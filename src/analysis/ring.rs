//! Ring construction, generator rebinding, and ring-specific type rules.

use m2_syn::nodes::{ExprNew as NewStatement, FloatLiteral, IntegerLiteral, Symbol};
use m2_syn::Token;

use crate::node_metadata::visit_expression_nodes;

use super::typechecker::{OperatorTypeQuery, TypeChecker};
use super::*;

/// Runtime binding shape produced for a ring generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingGeneratorKind {
    Direct,
    IndexedTable,
}

/// A ring-generator name and source node extracted from constructor syntax.
#[derive(Debug, Clone)]
struct RingGeneratorBinding<'tree> {
    name: String,
    kind: RingGeneratorKind,
    node: M2Node<'tree>,
}

/// Compact reference retained for later ring-generator rebinding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RingGenerator {
    name: ObjectName,
    kind: RingGeneratorKind,
}

/// Hardcoded static semantics that supplement indexed ring dispatch facts.
pub struct RingSemantics;

impl RingSemantics {
    /// Return the instance parent introduced by a source-defined ring value.
    pub fn value_parent(
        type_name: Option<&ObjectName>,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<ObjectName> {
        type_name
            .is_some_and(|type_name| knowledge.has_type_role(type_name, TypeRole::Ring))
            .then(|| ObjectName::new("RingElement"))
    }
}

impl Analysis {
    pub fn collect_ring_generator_bindings(
        &mut self,
        ring_name: &str,
        expression: (M2Node, Option<&Expr>),
        rebind_node: M2Node,
        scope_idx: usize,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) {
        let (expression, syntax) = expression;
        let mut nodes = Vec::new();
        visit_expression_nodes(expression, syntax, |node| nodes.push(node));
        let containers = nodes
            .into_iter()
            .filter(|node| node.is_space_application())
            .filter_map(|node| {
                let head = node.child_by_field_name("left")?;
                let variables = RingGeneratorBinding::constructor_variables(node)?;
                TypeChecker::new(self, knowledge)
                    .type_of(head, source, scope_idx)
                    .single()
                    .is_some_and(|head_type| knowledge.has_type_role(head_type, TypeRole::Ring))
                    .then_some(variables)
            })
            .collect::<Vec<_>>();

        let mut generators = Vec::new();
        for container in containers {
            for generator in RingGeneratorBinding::collect(container) {
                generators.push(RingGenerator {
                    name: ObjectName::new(&generator.name),
                    kind: generator.kind,
                });
                self.register_ring_generator(
                    ring_name,
                    &generator.name,
                    generator.kind,
                    generator.node,
                    source,
                );
            }
        }

        if generators.is_empty() {
            generators = self
                .ring_source_symbol(expression)
                .and_then(|source| self.registry.ring_generators.get(source).cloned())
                .unwrap_or_default();
            for generator in &generators {
                self.register_ring_generator(
                    ring_name,
                    generator.name.name(),
                    generator.kind,
                    rebind_node,
                    source,
                );
            }
        }

        self.registry
            .ring_generators
            .insert(ObjectName::new(ring_name), generators);
    }

    /// The variables a ring constructor spells out as plain symbols — `QQ[x, y]`
    /// yields `x` and `y`. Indexed families (`x_0`, `x_0..x_3`), generated tables
    /// (`Variables => 5`), and letter ranges (`a..d`) are not written as single
    /// symbols and are left out, so callers see exactly the names an author typed.
    pub(super) fn ring_constructor_symbol_variables<'tree>(
        &self,
        node: M2Node<'tree>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
        scope_idx: usize,
    ) -> Vec<M2Node<'tree>> {
        if !node.is_space_application() {
            return Vec::new();
        }
        let Some(head) = node.child_by_field_name("left") else {
            return Vec::new();
        };
        let Some(variables) = RingGeneratorBinding::constructor_variables(node) else {
            return Vec::new();
        };
        let constructs_a_ring = TypeChecker::new(self, knowledge)
            .type_of(head, source, scope_idx)
            .single()
            .is_some_and(|head_type| knowledge.has_type_role(head_type, TypeRole::Ring));
        if !constructs_a_ring {
            return Vec::new();
        }

        RingGeneratorBinding::collect(variables)
            .into_iter()
            .filter(|binding| {
                binding.kind == RingGeneratorKind::Direct && binding.node.is::<Symbol>()
            })
            .map(|binding| binding.node)
            .collect()
    }

    fn ring_source_symbol<'tree>(&self, expression: M2Node<'tree>) -> Option<&'tree str> {
        let expression = parenthesized_value(expression).unwrap_or(expression);
        if let Some(name) = symbol_node_text(expression) {
            return Some(name);
        }
        if expression.has_binary_operator::<Token![/]>() {
            return expression
                .child_by_field_name("left")
                .and_then(symbol_node_text);
        }
        None
    }

    fn register_ring_generator(
        &mut self,
        ring_name: &str,
        generator_name: &str,
        kind: RingGeneratorKind,
        node: M2Node,
        source: &(impl SourceNavigation + ?Sized),
    ) {
        match kind {
            RingGeneratorKind::Direct => {
                self.register_dynamic_global(
                    generator_name,
                    node,
                    ObjectName::new(ring_name),
                    None,
                    source,
                );
            }
            RingGeneratorKind::IndexedTable => {
                self.register_dynamic_global(
                    generator_name,
                    node,
                    ObjectName::new("IndexedVariableTable"),
                    Some(ObjectName::new(ring_name)),
                    source,
                );
            }
        }
    }

    fn register_dynamic_global(
        &mut self,
        name: &str,
        node: M2Node,
        type_name: ObjectName,
        indexed_element_type: Option<ObjectName>,
        source: &(impl SourceNavigation + ?Sized),
    ) {
        let position = source.position_for_node(node);
        let binding_id = self
            .binding_id_from_scope(name, 0, position)
            .filter(|binding_id| {
                self.binding_anchor(*binding_id)
                    .is_some_and(|binding| binding.scope_idx == 0)
            })
            .or_else(|| self.future_assignment_binding(name, 0, position));
        let fact = BindingFact {
            name: ObjectName::new(name),
            ..BindingFact::from_node(
                node,
                None,
                BindingRole::Ordinary,
                BindingEffect::Assign,
                0,
                true,
                source,
            )
        };
        let enrichment = BindingEnrichment {
            presentation_kind: SymbolKind::VARIABLE,
            inferred_type: Some(InferredType::exact_from_id(type_name)),
            object_id: None,
            indexed_element_type,
            parent_type: None,
        };
        let (binding_id, state_index) = if let Some(binding_id) = binding_id {
            let Some(mut state_index) = self.add_binding_state(binding_id, fact) else {
                return;
            };
            if let Some(binding) = self.registry.bindings.get_mut(binding_id.0 as usize) {
                let range = source.range_for_node(node);
                if range.start < binding.range.start {
                    binding.range = range;
                    let state = binding.states.remove(state_index);
                    binding.states.insert(0, state);
                    state_index = 0;
                }
            }
            self.reindex_binding_states(binding_id);
            (binding_id, state_index)
        } else {
            (self.add_binding(fact), 0)
        };
        self.enrich_binding_state(binding_id, state_index, enrichment);
    }
}

impl<'tree> RingGeneratorBinding<'tree> {
    fn constructor_variables(application: M2Node<'tree>) -> Option<M2Node<'tree>> {
        let argument = application.child_by_field_name("right")?;
        if argument.is_collection_expression() {
            return Some(argument);
        }
        argument
            .child_by_field_name("left")
            .filter(M2Node::is_collection_expression)
    }

    fn collect(container: M2Node<'tree>) -> Vec<Self> {
        let elements = container.collection_elements().collect::<Vec<_>>();
        let variable_base = elements
            .iter()
            .find_map(|element| Self::option_value(*element, "VariableBaseName"))
            .and_then(Self::base_name);
        let mut bindings = Vec::new();

        for element in elements {
            if element.is_option_assignment() {
                if let Some(variables) = Self::option_value(element, "Variables") {
                    if variables.is::<IntegerLiteral>() {
                        let name = variable_base.clone().unwrap_or_else(|| "p".to_string());
                        Self::push(
                            &mut bindings,
                            RingGeneratorBinding {
                                name,
                                kind: RingGeneratorKind::IndexedTable,
                                node: variables,
                            },
                        );
                    } else {
                        Self::collect_spec(variables, &mut bindings);
                    }
                }
                continue;
            }
            Self::collect_spec(element, &mut bindings);
        }

        bindings
    }

    fn option_value(node: M2Node<'tree>, key: &str) -> Option<M2Node<'tree>> {
        if !node.is_option_assignment() {
            return None;
        }
        node.child_by_field_name("left")
            .filter(|left| left.is::<Symbol>() && left.text() == key)?;
        node.child_by_field_name("right")
    }

    fn base_name(node: M2Node<'tree>) -> Option<String> {
        if node.is::<Symbol>() {
            Some(node.text().to_string())
        } else {
            node.string_literal_inner_text().map(ToString::to_string)
        }
    }

    fn collect_spec(node: M2Node<'tree>, bindings: &mut Vec<RingGeneratorBinding<'tree>>) {
        if node.is::<Symbol>() {
            Self::push(
                bindings,
                RingGeneratorBinding {
                    name: node.text().to_string(),
                    kind: RingGeneratorKind::Direct,
                    node,
                },
            );
            return;
        }

        if node.has_binary_operator::<Token![_]>() {
            if let Some(base) = node
                .child_by_field_name("left")
                .filter(|base| base.is::<Symbol>())
            {
                Self::push(
                    bindings,
                    RingGeneratorBinding {
                        name: base.text().to_string(),
                        kind: RingGeneratorKind::IndexedTable,
                        node: base,
                    },
                );
            }
            return;
        }

        if node.has_binary_operator::<Token![..]>() || node.has_binary_operator::<Token![..<]>() {
            let left = node.child_by_field_name("left");
            let right = node.child_by_field_name("right");
            if let (Some(left), Some(right)) = (left, right) {
                if let (Some(left_base), Some(right_base)) =
                    (Self::indexed_base(left), Self::indexed_base(right))
                {
                    if left_base.text() == right_base.text() {
                        Self::push(
                            bindings,
                            RingGeneratorBinding {
                                name: left_base.text().to_string(),
                                kind: RingGeneratorKind::IndexedTable,
                                node,
                            },
                        );
                        return;
                    }
                }

                if let Some(names) = Self::symbol_range(node, left, right) {
                    for name in names {
                        Self::push(
                            bindings,
                            RingGeneratorBinding {
                                name,
                                kind: RingGeneratorKind::Direct,
                                node,
                            },
                        );
                    }
                    return;
                }
            }
        }

        if node.is_collection_expression() {
            for element in node.collection_elements() {
                Self::collect_spec(element, bindings);
            }
        }
    }

    fn indexed_base(node: M2Node<'tree>) -> Option<M2Node<'tree>> {
        node.has_binary_operator::<Token![_]>()
            .then(|| node.child_by_field_name("left"))
            .flatten()
            .filter(|base| base.is::<Symbol>())
    }

    fn symbol_range(
        range: M2Node<'tree>,
        left: M2Node<'tree>,
        right: M2Node<'tree>,
    ) -> Option<Vec<String>> {
        if !left.is::<Symbol>() || !right.is::<Symbol>() {
            return None;
        }
        let [start] = left.text().as_bytes() else {
            return None;
        };
        let [end] = right.text().as_bytes() else {
            return None;
        };
        if !start.is_ascii_alphabetic()
            || !end.is_ascii_alphabetic()
            || start.is_ascii_lowercase() != end.is_ascii_lowercase()
            || start > end
        {
            return None;
        }

        let exclusive = range.has_binary_operator::<Token![..<]>();
        let stop = if exclusive {
            *end
        } else {
            end.saturating_add(1)
        };
        Some(
            (*start..stop)
                .map(|letter| (letter as char).to_string())
                .collect(),
        )
    }

    fn push(bindings: &mut Vec<RingGeneratorBinding<'tree>>, binding: RingGeneratorBinding<'tree>) {
        if !bindings
            .iter()
            .any(|existing| existing.name == binding.name)
        {
            bindings.push(binding);
        }
    }
}

impl<Knowledge: PositionedTypeKnowledge + ?Sized> TypeChecker<'_, '_, Knowledge> {
    /// Infer the ring-specific indexed-variable and quotient operator cases.
    pub fn ring_operator_type(
        &self,
        query: OperatorTypeQuery<'_>,
        left_name: &ObjectName,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<InferredType> {
        let promoted_value = parenthesized_value(query.left)?;
        if matches_token::<Token![_]>(query.operator)
            && (promoted_value.is::<IntegerLiteral>() || promoted_value.is::<FloatLiteral>())
        {
            let ring = parenthesized_value(query.right?)?;
            let ring_type = self.type_of(ring, source, query.scope_idx);
            if knowledge.has_type_role(ring_type.single()?, TypeRole::Ring) {
                let element_type = self
                    .resolved_ring_element_type(
                        ring,
                        source,
                        query.scope_idx,
                        knowledge,
                        &mut HashSet::new(),
                    )
                    .unwrap_or_else(|| ObjectName::new("RingElement"));
                return Some(InferredType::exact_from_id(element_type));
            }
        }

        if matches_token::<Token![_]>(query.operator)
            && left_name.as_ref() == "IndexedVariableTable"
        {
            if let Some(element_type) = self
                .get_binding_from_scope(
                    query.left.text(),
                    query.scope_idx,
                    source.position_for_node(query.left),
                )
                .and_then(|binding| binding.state.indexed_element_type.as_ref())
            {
                return Some(InferredType::exact(element_type.name()));
            }
            return Some(InferredType::upward("RingElement"));
        }

        if matches_token::<Token![/]>(query.operator)
            && knowledge.has_type_role(left_name, TypeRole::Ring)
        {
            let right_type = self.type_of(query.right?, source, query.scope_idx);
            if right_type.single()?.as_ref() == "ZZ" {
                return Some(InferredType::exact("QuotientRing"));
            }
        }

        None
    }

    fn resolved_ring_element_type(
        &self,
        ring: M2Node<'_>,
        source: &(impl SourceNavigation + ?Sized),
        scope_idx: usize,
        knowledge: &(impl TypeKnowledge + ?Sized),
        visited: &mut HashSet<BindingId>,
    ) -> Option<ObjectName> {
        let ring = parenthesized_value(ring)?;
        let name = symbol_node_text(ring)?;
        let position = source.position_for_node(ring);
        let Some(binding) =
            self.visible_source_binding_from_scope(name, scope_idx, position, knowledge)
        else {
            let name = ObjectName::new(name);
            self.resolve_type_id_at(&name, position, knowledge)?;
            return Some(name);
        };
        if !visited.insert(binding.binding_id) {
            return None;
        }

        let value_range = binding.state.value_range?;
        let root = ring.root();
        let value = root.descendant_for_point_range(
            source.point_for_position(value_range.start)?,
            source.point_for_position(value_range.end)?,
        )?;
        if source.range_for_node(value) != value_range {
            return None;
        }
        let value_scope_idx = self
            .find_scope_at(source.position_for_node(value))
            .unwrap_or(binding.state.scope_idx);
        if symbol_node_text(parenthesized_value(value)?).is_some() {
            return self.resolved_ring_element_type(
                value,
                source,
                value_scope_idx,
                knowledge,
                visited,
            );
        }

        (binding.scope_idx == 0
            && self.expression_introduces_ring_identity(value, source, value_scope_idx, knowledge))
        .then(|| binding.name.clone())
        .filter(|_| binding.state.source_type.is_some())
    }

    fn expression_introduces_ring_identity(
        &self,
        expression: M2Node<'_>,
        source: &(impl SourceNavigation + ?Sized),
        scope_idx: usize,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> bool {
        let Some(expression) = parenthesized_value(expression) else {
            return false;
        };
        if expression.is::<NewStatement>() {
            return true;
        }
        let introduces_identity =
            expression.is_space_application() || expression.has_binary_operator::<Token![/]>();
        let operand = introduces_identity
            .then(|| expression.child_by_field_name("left"))
            .flatten();
        operand.is_some_and(|operand| {
            self.type_of(operand, source, scope_idx)
                .single()
                .is_some_and(|type_name| knowledge.has_type_role(type_name, TypeRole::Ring))
        })
    }

    /// Square-bracket ring construction binds specially in Macaulay2 source:
    /// `R[x]/I` has a CST shaped like `R SPACE ([x] / I)`, while evaluation
    /// constructs `R[x]` before applying `/ I`. Preserve the parser's grouping,
    /// but lower that one type-directed application chain through the same
    /// dispatch table used for ordinary operators.
    pub fn ring_application_with_trailing_operator_type(
        &self,
        application_operator: &Operator,
        head: &InferredType,
        argument: M2Node,
        source: &(impl SourceNavigation + ?Sized),
        scope_idx: usize,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<InferredType> {
        let head_name = head.single()?;
        if !knowledge.has_type_role(head_name, TypeRole::Ring) {
            return None;
        }

        let operator = argument.binary_operator()?;
        let variables = argument.child_by_field_name("left")?;
        if !variables.is_collection_expression() {
            return None;
        }
        let trailing_operand = argument.child_by_field_name("right")?;

        let variables_type = self.type_of(variables, source, scope_idx);
        let ring_type = self.dispatch_codomain(
            knowledge,
            application_operator,
            &[head.clone(), variables_type],
            &[],
            source.position_for_node(argument),
        );
        let ring_name = ring_type.single()?;
        if !knowledge.has_type_role(ring_name, TypeRole::Ring) {
            return None;
        }

        let trailing_type = self.type_of(trailing_operand, source, scope_idx);
        Some(self.dispatch_codomain(
            knowledge,
            &Operator {
                token: ObjectName::new(operator),
                form: OperatorForm::Binary,
            },
            &[ring_type, trailing_type],
            &[],
            source.position_for_node(argument),
        ))
    }
}
