//! Ring construction, generator rebinding, and ring-specific type rules.

use m2_syn::nodes::{Array, ExprNew as NewStatement, FloatLiteral, IntegerLiteral, Symbol};
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
    node: SyntaxNode<'tree>,
    indices: Option<IndexSelection>,
}

/// Compact reference retained for later ring-generator rebinding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RingGenerator {
    name: ObjectName,
    kind: RingGeneratorKind,
    indices: Option<IndexSelection>,
}

/// Source-ordered writes to an indexed variable table. Ranges stay compact even
/// when their Cartesian product contains many generators.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexedGeneratorTypes {
    entries: Vec<(IndexSelection, ObjectName)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum IndexKey {
    Integer(i64),
    String(String),
    List(Vec<IndexKey>),
    Sequence(Vec<IndexKey>),
}

impl IndexKey {
    fn from_node(node: SyntaxNode<'_>) -> Option<Self> {
        let node = parenthesized_value(node)?;
        if node.is::<IntegerLiteral>() {
            return node.text().parse().ok().map(Self::Integer);
        }
        if node.is_prefix_expr() && node.operator_node()?.text() == "-" {
            let Self::Integer(value) = Self::from_node(node.unary_operand()?)? else {
                return None;
            };
            return value.checked_neg().map(Self::Integer);
        }
        if let Some(text) = node.string_literal_inner_text() {
            // Escaped strings require evaluation; do not compare their raw spelling.
            return (!text.contains('\\')).then(|| Self::String(text.to_string()));
        }
        if node.is::<List>() || node.is::<Sequence>() {
            let items = node
                .collection_elements()
                .map(Self::from_node)
                .collect::<Option<Vec<_>>>()?;
            return Some(if node.is::<List>() {
                Self::List(items)
            } else {
                Self::Sequence(items)
            });
        }
        None
    }

    fn in_range(&self, start: &Self, end: &Self, exclusive: bool) -> bool {
        match (self, start, end) {
            (Self::Integer(value), Self::Integer(start), Self::Integer(end)) => {
                start <= value && if exclusive { value < end } else { value <= end }
            }
            (Self::List(values), Self::List(starts), Self::List(ends))
            | (Self::Sequence(values), Self::Sequence(starts), Self::Sequence(ends)) => {
                values.len() == starts.len()
                    && values.len() == ends.len()
                    && values
                        .iter()
                        .zip(starts)
                        .zip(ends)
                        .all(|((value, start), end)| value.in_range(start, end, exclusive))
            }
            (Self::String(value), Self::String(start), Self::String(end)) => {
                value.chars().count() == start.chars().count()
                    && value.chars().count() == end.chars().count()
                    && value.chars().zip(start.chars()).zip(end.chars()).all(
                        |((value, start), end)| {
                            start <= value && if exclusive { value < end } else { value <= end }
                        },
                    )
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum IndexSelection {
    Exact(IndexKey),
    Range {
        start: IndexKey,
        end: IndexKey,
        exclusive: bool,
    },
}

impl IndexSelection {
    fn contains(&self, key: &IndexKey) -> bool {
        match self {
            Self::Exact(index) => index == key,
            Self::Range {
                start,
                end,
                exclusive,
            } => key.in_range(start, end, *exclusive),
        }
    }
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
        expression: (SyntaxNode, Option<&Expr>),
        rebind_node: SyntaxNode,
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
                let head = node.binary_parts().map(|parts| parts.left)?;
                let variables = RingGeneratorBinding::constructor_variables(node)?;
                TypeChecker::new(self, knowledge)
                    .type_of(head, source, scope_idx)
                    .single()
                    .is_some_and(|head_type| {
                        knowledge.has_type_role(head_type, TypeRole::Ring)
                            || knowledge
                                .is_subtype(head_type, &ObjectName::new("InexactFieldFamily"))
                    })
                    .then_some(variables)
            })
            .collect::<Vec<_>>();

        let mut generators = Vec::new();
        for container in containers {
            for generator in RingGeneratorBinding::collect(container) {
                generators.push(RingGenerator {
                    name: ObjectName::new(&generator.name),
                    kind: generator.kind,
                    indices: generator.indices.clone(),
                });
                self.register_ring_generator(
                    ring_name,
                    &generator.name,
                    generator.kind,
                    generator.indices,
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
                    generator.indices.clone(),
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
    pub(crate) fn ring_constructor_symbol_variables<'tree>(
        &self,
        node: SyntaxNode<'tree>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
        scope_idx: usize,
    ) -> Vec<SyntaxNode<'tree>> {
        if !node.is_space_application() {
            return Vec::new();
        }
        let Some(head) = node.binary_parts().map(|parts| parts.left) else {
            return Vec::new();
        };
        let Some(variables) = RingGeneratorBinding::constructor_variables(node) else {
            return Vec::new();
        };
        let constructs_a_ring = TypeChecker::new(self, knowledge)
            .type_of(head, source, scope_idx)
            .single()
            .is_some_and(|head_type| {
                knowledge.has_type_role(head_type, TypeRole::Ring)
                    || knowledge.is_subtype(head_type, &ObjectName::new("InexactFieldFamily"))
            });
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

    fn ring_source_symbol<'tree>(&self, expression: SyntaxNode<'tree>) -> Option<&'tree str> {
        let expression = parenthesized_value(expression).unwrap_or(expression);
        if let Some(name) = symbol_node_text(expression) {
            return Some(name);
        }
        if expression.has_binary_operator::<Token![/]>() {
            return expression
                .binary_parts()
                .map(|parts| parts.left)
                .and_then(symbol_node_text);
        }
        None
    }

    /// Whether an indexed access selects a generator recorded by a ring constructor.
    pub(super) fn is_indexed_ring_generator(
        &self,
        node: SyntaxNode<'_>,
        source: &(impl SourceNavigation + ?Sized),
    ) -> bool {
        if !node.has_binary_operator::<Token![_]>() {
            return false;
        }
        let Some(parts) = node.binary_parts() else {
            return false;
        };
        let Some(base) = parenthesized_value(parts.left).filter(|base| base.is::<Symbol>()) else {
            return false;
        };
        let Some(index) = IndexKey::from_node(parts.right) else {
            return false;
        };
        let position = source.position_for_node(base);
        self.get_binding_from_scope(
            base.text(),
            self.find_scope_at(position).unwrap_or(0),
            position,
        )
        .and_then(|binding| binding.state.indexed_element_type.as_ref())
        .is_some_and(|elements| {
            elements
                .entries
                .iter()
                .any(|(selection, _)| selection.contains(&index))
        })
    }

    fn register_ring_generator(
        &mut self,
        ring_name: &str,
        generator_name: &str,
        kind: RingGeneratorKind,
        indices: Option<IndexSelection>,
        node: SyntaxNode,
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
                let mut elements = self
                    // Several disjoint specs can be rebound at the same anchor
                    // (for example by a quotient). Include earlier writes there.
                    .get_binding_from_scope(generator_name, 0, source.range_for_node(node).end)
                    .and_then(|binding| binding.state.indexed_element_type.clone())
                    .unwrap_or_default();
                if let Some(indices) = indices {
                    elements.entries.push((indices, ObjectName::new(ring_name)));
                }
                self.register_dynamic_global(
                    generator_name,
                    node,
                    ObjectName::new("IndexedVariableTable"),
                    Some(elements),
                    source,
                );
            }
        }
    }

    fn register_dynamic_global(
        &mut self,
        name: &str,
        node: SyntaxNode,
        type_name: ObjectName,
        indexed_element_type: Option<IndexedGeneratorTypes>,
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
        let is_ring_generator = enrichment.indexed_element_type.is_none();
        self.enrich_binding_state(binding_id, state_index, enrichment);
        self.registry.bindings[binding_id.0 as usize].states[state_index].is_ring_generator =
            is_ring_generator;
    }
}

impl<'tree> RingGeneratorBinding<'tree> {
    fn constructor_variables(application: SyntaxNode<'tree>) -> Option<SyntaxNode<'tree>> {
        let argument = parenthesized_value(application.binary_parts().map(|parts| parts.right)?)?;
        let is_constructor_argument =
            |node: &SyntaxNode<'tree>| node.is::<Array>() || node.is::<List>();
        if is_constructor_argument(&argument) {
            return Some(argument);
        }
        argument
            .binary_parts()
            .map(|parts| parts.left)
            .filter(is_constructor_argument)
    }

    fn collect(container: SyntaxNode<'tree>) -> Vec<Self> {
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
                                indices: variables
                                    .text()
                                    .parse::<i64>()
                                    .ok()
                                    .filter(|n| *n > 0)
                                    .map(|n| IndexSelection::Range {
                                        start: IndexKey::Integer(0),
                                        end: IndexKey::Integer(n - 1),
                                        exclusive: false,
                                    }),
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

    fn option_value(node: SyntaxNode<'tree>, key: &str) -> Option<SyntaxNode<'tree>> {
        if !node.is_option_assignment() {
            return None;
        }
        node.binary_parts()
            .map(|parts| parts.left)
            .filter(|left| left.is::<Symbol>() && left.text() == key)?;
        node.binary_parts().map(|parts| parts.right)
    }

    fn base_name(node: SyntaxNode<'tree>) -> Option<String> {
        if node.is::<Symbol>() {
            Some(node.text().to_string())
        } else {
            node.string_literal_inner_text().map(ToString::to_string)
        }
    }

    fn collect_spec(node: SyntaxNode<'tree>, bindings: &mut Vec<RingGeneratorBinding<'tree>>) {
        let Some(node) = parenthesized_value(node) else {
            return;
        };
        if node.is::<Symbol>() {
            Self::push(
                bindings,
                RingGeneratorBinding {
                    name: node.text().to_string(),
                    kind: RingGeneratorKind::Direct,
                    node,
                    indices: None,
                },
            );
            return;
        }

        if node.has_binary_operator::<Token![_]>() {
            if let Some(base) = node
                .binary_parts()
                .map(|parts| parts.left)
                .and_then(parenthesized_value)
                .filter(|base| base.is::<Symbol>())
            {
                Self::push(
                    bindings,
                    RingGeneratorBinding {
                        name: base.text().to_string(),
                        kind: RingGeneratorKind::IndexedTable,
                        node: base,
                        indices: node
                            .binary_parts()
                            .map(|parts| parts.right)
                            .and_then(IndexKey::from_node)
                            .map(IndexSelection::Exact),
                    },
                );
            }
            return;
        }

        if node.has_binary_operator::<Token![..]>() || node.has_binary_operator::<Token![..<]>() {
            let left = node
                .binary_parts()
                .map(|parts| parts.left)
                .and_then(parenthesized_value);
            let right = node
                .binary_parts()
                .map(|parts| parts.right)
                .and_then(parenthesized_value);
            if let (Some(left), Some(right)) = (left, right) {
                if let (Some(left_base), Some(right_base)) =
                    (Self::indexed_base(left), Self::indexed_base(right))
                {
                    let indices = left
                        .binary_parts()
                        .map(|parts| parts.right)
                        .and_then(IndexKey::from_node)
                        .zip(
                            right
                                .binary_parts()
                                .map(|parts| parts.right)
                                .and_then(IndexKey::from_node),
                        )
                        .map(|(start, end)| IndexSelection::Range {
                            start,
                            end,
                            exclusive: node.has_binary_operator::<Token![..<]>(),
                        });
                    if let Some(names) = Self::symbol_range(node, left_base, right_base) {
                        for name in names {
                            Self::push(
                                bindings,
                                RingGeneratorBinding {
                                    name,
                                    kind: RingGeneratorKind::IndexedTable,
                                    node,
                                    indices: indices.clone(),
                                },
                            );
                        }
                    }
                    return;
                }

                if let Some(names) = Self::symbol_range(node, left, right) {
                    for name in names {
                        Self::push(
                            bindings,
                            RingGeneratorBinding {
                                name,
                                kind: RingGeneratorKind::Direct,
                                node,
                                indices: None,
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

    fn indexed_base(node: SyntaxNode<'tree>) -> Option<SyntaxNode<'tree>> {
        let node = parenthesized_value(node)?;
        node.has_binary_operator::<Token![_]>()
            .then(|| node.binary_parts().map(|parts| parts.left))
            .flatten()
            .filter(|base| base.is::<Symbol>())
    }

    fn symbol_range(
        range: SyntaxNode<'tree>,
        left: SyntaxNode<'tree>,
        right: SyntaxNode<'tree>,
    ) -> Option<Vec<String>> {
        if !left.is::<Symbol>() || !right.is::<Symbol>() {
            return None;
        }
        let names = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
        // Equal endpoints also support names outside the standard vars alphabet.
        if left.text() == right.text() {
            return Some(if range.has_binary_operator::<Token![..<]>() {
                Vec::new()
            } else {
                vec![left.text().to_string()]
            });
        }
        let start = names.chars().position(|c| c.to_string() == left.text())?;
        let end = names.chars().position(|c| c.to_string() == right.text())?;
        let stop = end + usize::from(!range.has_binary_operator::<Token![..<]>());
        Some(
            names
                .chars()
                .skip(start)
                .take(stop.saturating_sub(start))
                .map(|c| c.to_string())
                .collect(),
        )
    }

    fn push(bindings: &mut Vec<RingGeneratorBinding<'tree>>, binding: RingGeneratorBinding<'tree>) {
        if !bindings.iter().any(|existing| {
            existing.name == binding.name
                && existing.kind == binding.kind
                && existing.indices == binding.indices
        }) {
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
            if let Some(elements) = self
                .get_binding_from_scope(
                    query.left.text(),
                    query.scope_idx,
                    source.position_for_node(query.left),
                )
                .and_then(|binding| binding.state.indexed_element_type.as_ref())
            {
                if let Some(index) = query.right.and_then(IndexKey::from_node) {
                    return Some(
                        elements
                            .entries
                            .iter()
                            .rev()
                            .find(|(selection, _)| selection.contains(&index))
                            .map_or_else(
                                || InferredType::exact("IndexedVariable"),
                                |(_, ring)| InferredType::exact_from_id(ring.clone()),
                            ),
                    );
                }
                return Some(elements.entries.iter().fold(
                    InferredType::exact("IndexedVariable"),
                    |types, (_, ring)| types | InferredType::exact_from_id(ring.clone()),
                ));
            }
            return Some(InferredType::exact("IndexedVariable"));
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
        ring: SyntaxNode<'_>,
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
        expression: SyntaxNode<'_>,
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
            .then(|| expression.binary_parts().map(|parts| parts.left))
            .flatten();
        operand.is_some_and(|operand| {
            self.type_of(operand, source, scope_idx)
                .single()
                .is_some_and(|type_name| knowledge.has_type_role(type_name, TypeRole::Ring))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::DocumentSnapshot;
    use crate::object_registry::ObjectRegistry;
    use std::sync::OnceLock;

    fn document(source: &str) -> DocumentSnapshot {
        static BUILTINS: OnceLock<ObjectRegistry> = OnceLock::new();
        let builtins =
            BUILTINS.get_or_init(|| ObjectRegistry::load(include_str!("../data/m2-index.jsonl")));
        DocumentSnapshot::from_text(source.to_string(), builtins).expect("fixture should parse")
    }

    /// Check complete expressions, including indexed accesses, rather than the
    /// types of their symbol children or the presence of an unrelated inlay hint.
    fn assert_types(setup: &str, probes: &[(&str, &str)]) {
        let source = format!(
            "{setup}\n{}\n",
            probes
                .iter()
                .map(|(expr, _)| *expr)
                .collect::<Vec<_>>()
                .join("\n")
        );
        let document = document(&source);
        assert!(
            !document.root_node().has_error(),
            "invalid fixture: {source}"
        );
        let checker = TypeChecker::new(document.analysis(), document.object_registry());
        let first_line = setup.lines().count();
        for (offset, (expression, expected)) in probes.iter().enumerate() {
            let line = (first_line + offset) as u32;
            let node = document
                .root_node()
                .descendants()
                .filter(|node| {
                    node.text() == *expression && document.position_for_node(*node).line == line
                })
                .last()
                .unwrap_or_else(|| panic!("missing probe {expression:?} on line {line}: {source}"));
            let inferred = checker.type_of(node, &document, 0);
            assert_eq!(
                inferred.single().map(ObjectName::name),
                Some(*expected),
                "wrong type for {expression:?} after {setup:?}: {inferred:?}"
            );
        }
    }

    #[test]
    fn polynomial_constructors_recognize_coefficient_rings_and_spacing() {
        for constructor in [
            "QQ[x]",
            "ZZ[x]",
            "(ZZ/101)[x]",
            "ZZ/101[x]",
            "(QQ)[x]",
            "((QQ))[x]",
            "QQ [x]",
            "QQ\t[x]",
            "QQ[ x , y ]",
            "QQ([x])",
            "QQ[(((x)))]",
            "QQ[(x,y)]",
            "QQ[\r\nx,\r\ny\r\n]",
            "QQ{ x, y }",
            "QQ[{x,y}]",
            "QQ[\n -- generators\n x,\n y\n]",
        ] {
            assert_types(
                &format!("R = {constructor}"),
                &[("R", "PolynomialRing"), ("x", "R")],
            );
        }
    }

    #[test]
    fn polynomial_constructors_recognize_options_and_empty_variable_sets() {
        for constructor in [
            "QQ[]",
            "QQ{}",
            "QQ[{}]",
            "QQ[Variables => 0]",
            "QQ[x, Degrees => {2}]",
            "QQ[x, MonomialOrder => Lex]",
            "QQ[MonomialOrder => Lex, x]",
            "QQ[Variables => {x,y}]",
            "QQ[Variables => [x,y]]",
            "QQ[Variables => 3]",
            "QQ[Variables => 3, VariableBaseName => v]",
            "QQ[VariableBaseName => \"v\", Variables => 3]",
        ] {
            assert_types(&format!("R = {constructor}"), &[("R", "PolynomialRing")]);
        }
        assert_types("R = QQ[Variables => {x,y}]", &[("x", "R"), ("y", "R")]);
        assert_types(
            "R = QQ[Variables => 2]",
            &[
                ("p", "IndexedVariableTable"),
                ("p_0", "R"),
                ("p_1", "R"),
                ("p_2", "IndexedVariable"),
            ],
        );
        assert_types(
            "R = QQ[VariableBaseName => \"v\", Variables => 2]",
            &[("v_1", "R"), ("v_2", "IndexedVariable")],
        );
    }

    #[test]
    fn polynomial_constructors_follow_aliases_and_existing_ring_heads() {
        for setup in [
            "P = QQ\nR = P[x]",
            "P := QQ\nR := P[x]",
            "P = QQ\nA = P\nR = A[x]",
            "P = 1\nP = QQ\nR = P[x]",
            "P = QQ[t]\nR = P[x]",
            "P = QQ[t]\nI = ideal(t^2)\nA = P/I\nR = A[x]",
            "R = QQ[t][x]",
            "R = (QQ[t])[x]",
            "R = (QQ[x])",
            "R = ((QQ[x]))",
        ] {
            assert_types(setup, &[("R", "PolynomialRing"), ("x", "R")]);
        }
    }

    #[test]
    fn polynomial_constructors_preserve_trailing_operator_dispatch() {
        for setup in [
            "I = ideal(0)\nR = QQ[x]/I",
            "I = ideal(0)\nR = (QQ[x])/I",
            "I = ideal(0)\nR = QQ[x] / I",
            "I = ideal(0)\nR = (QQ[x]/I)",
            "A = QQ[t]\nI = ideal(t^2)\nR = A[x]/I",
        ] {
            assert_types(setup, &[("R", "QuotientRing"), ("x", "R")]);
        }
        assert_types("M = QQ[x]^2", &[("M", "Module")]);
    }

    #[test]
    fn polynomial_constructor_detection_does_not_confuse_other_applications() {
        // Brackets alone, a familiar name, or a Ring result from an ordinary
        // function call must not turn its arguments into ring generators.
        for setup in [
            "R = [x,y]",
            "R = {x,y}",
            "R = (x,y)",
            "f = args -> args\nR = f[x]",
            "f = args -> args\nR = f{x}",
            "f = args -> QQ\nR = f[x]",
            "R = unknownHead[x]",
            "R = P[x]\nP = QQ",
            "P = 1\nR = P[x]\nP = QQ",
            "R = QQ(x,y)",
            "R = QQ<|x|>",
            "P = 7\nR = P[x]",
            "P = \"QQ\"\nR = P[x]",
            "P = QQ\nP = 7\nR = P[x]",
            "QQ = 7\nR = QQ[x]",
            "R = \"QQ[x]\"",
            "-- R = QQ[x]\nR = 1",
        ] {
            assert_types(setup, &[("x", "Symbol")]);
        }
    }

    #[test]
    fn polynomial_constructor_symbol_detection_uses_the_head_type_and_scope() {
        for (source, expected) in [
            ("R = QQ[longName]", vec!["longName"]),
            ("R = QQ[((longName))]", vec!["longName"]),
            ("R = QQ([longName])", vec!["longName"]),
            ("P = QQ\nR = P[longName]", vec!["longName"]),
            ("P = QQ\nf = () -> P[longName]", vec!["longName"]),
            ("f = () -> (P := QQ; P[longName])", vec!["longName"]),
            ("R = (ZZ/101)[longName]", vec!["longName"]),
            ("R = QQ[longName]/ideal(0)", vec!["longName"]),
            ("R = QQ[x]/ideal(x^2)", vec!["x"]),
            (
                "R = QQ[longName, otherName, x_1, a..c]",
                vec!["longName", "otherName"],
            ),
            ("R = QQ[Degrees => {longName}, x]", vec!["x"]),
            ("R = QQ[Variables => {longName, x_1}]", vec!["longName"]),
            ("R = QQ[x_1..y_3, Variables => 2]", vec![]),
            ("R = [longName]", vec![]),
            ("R = QQ(longName, otherName)", vec![]),
            ("R = QQ<|longName|>", vec![]),
            ("R = unknownHead[longName]", vec![]),
            ("f = x -> x\nR = f[longName]", vec![]),
            ("P = QQ\nf = P -> P[longName]", vec![]),
            ("P = QQ\nf = () -> (P := 1; P[longName])", vec![]),
            ("P = QQ\nP = 1\nR = P[longName]", vec![]),
        ] {
            let document = document(source);
            let found = document
                .root_node()
                .descendants()
                .flat_map(|node| {
                    let scope = document
                        .analysis()
                        .find_scope_at(document.position_for_node(node))
                        .unwrap_or(0);
                    document.analysis().ring_constructor_symbol_variables(
                        node,
                        &document,
                        document.object_registry(),
                        scope,
                    )
                })
                .map(|node| node.text().to_string())
                .collect::<Vec<_>>();
            assert_eq!(found, expected, "wrong constructor symbols in {source:?}");
        }
    }

    #[test]
    fn polynomial_generator_ranges_cover_boundaries_and_index_shapes() {
        for (constructor, probes) in [
            (
                "QQ[a..c]",
                vec![("a", "R"), ("b", "R"), ("c", "R"), ("d", "Symbol")],
            ),
            ("QQ[a..<d]", vec![("c", "R"), ("d", "Symbol")]),
            (
                "QQ[z..B]",
                vec![("z", "R"), ("A", "R"), ("B", "R"), ("C", "Symbol")],
            ),
            ("QQ[longName..longName]", vec![("longName", "R")]),
            ("QQ[c..a]", vec![("a", "Symbol"), ("c", "Symbol")]),
            (
                "QQ[((x)_1)]",
                vec![
                    ("x", "IndexedVariableTable"),
                    ("x_1", "R"),
                    ("x_2", "IndexedVariable"),
                ],
            ),
            (
                "QQ[(x_1)..(x_3)]",
                vec![("x_1", "R"), ("x_3", "R"), ("x_4", "IndexedVariable")],
            ),
            (
                "QQ[x_1, x_3]",
                vec![("x_1", "R"), ("x_2", "IndexedVariable"), ("x_3", "R")],
            ),
            (
                "QQ[x_(-2)..x_1]",
                vec![
                    ("x_(-3)", "IndexedVariable"),
                    ("x_(-2)", "R"),
                    ("x_0", "R"),
                    ("x_2", "IndexedVariable"),
                ],
            ),
            (
                "QQ[x_{1,2}]",
                vec![
                    ("x_{1,2}", "R"),
                    ("x_{2,1}", "IndexedVariable"),
                    ("x_(1,2)", "IndexedVariable"),
                ],
            ),
            (
                "QQ[x_(1,2)]",
                vec![("x_(1,2)", "R"), ("x_{1,2}", "IndexedVariable")],
            ),
            (
                "QQ[x_\"a\"]",
                vec![("x_\"a\"", "R"), ("x_\"b\"", "IndexedVariable")],
            ),
            (
                "QQ[x_1..z_1000000000]",
                vec![
                    ("x_1", "R"),
                    ("y_500000000", "R"),
                    ("z_1000000000", "R"),
                    ("z_1000000001", "IndexedVariable"),
                ],
            ),
        ] {
            assert_types(&format!("R = {constructor}"), &probes);
        }
    }

    #[test]
    fn polynomial_cartesian_ranges_preserve_holes_and_dimension_boundaries() {
        assert_types(
            "R = QQ[x_{1,2}..z_{2,4}, x_{4,1}, y_7, a..c, w]",
            &[
                ("x_{1,2}", "R"),
                ("y_{2,3}", "R"),
                ("z_{2,4}", "R"),
                ("x_{0,3}", "IndexedVariable"),
                ("x_{1,5}", "IndexedVariable"),
                ("x_{1}", "IndexedVariable"),
                ("x_{1,2,3}", "IndexedVariable"),
                ("x_{4,1}", "R"),
                ("y_{4,1}", "IndexedVariable"),
                ("y_7", "R"),
                ("x_7", "IndexedVariable"),
                ("w", "R"),
                ("b", "R"),
            ],
        );
    }
}
