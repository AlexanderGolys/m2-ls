//! Transient static type inference for a completed semantic snapshot.

use std::cell::RefCell;
use std::ops::Deref;

use m2_syn::nodes::{
    AngleBarList, Array, ExprAssign as Assignment, ExprAssignEvaluated as EvaluatedAssignment,
    ExprAssignInstall as Installation, ExprAssignOp as OperatorAssignment,
    ExprAssignParallel as ParallelAssignment, ExprAssignSimple as SimpleBinding,
    ExprFor as ForLoop, ExprIf as IfStatement, ExprLambda as LambdaExpression,
    ExprNew as NewStatement, ExprOpBinAdj as AdjacentExpr, ExprOption as OptionExpression,
    ExprQuote as QuoteExpression, ExprTry as TryStatement, ExprWhile as WhileLoop, FloatLiteral,
    IntegerLiteral, List, LoopBody, RawStringLiteral, Sequence, StringLiteral,
};
use m2_syn::visit::{self, Visit};
use m2_syn::Token;

use super::*;
use crate::node_metadata::SyntaxNodeId;
use crate::object_registry::TypeStore;

/// Partial-order view combining source and external type edges without copying.
struct SourceTypeOrder<'analysis, Knowledge: ?Sized> {
    source: &'analysis SourceTypeFacts,
    external: &'analysis Knowledge,
}

impl<Knowledge: TypeKnowledge + ?Sized> TypeStore for SourceTypeOrder<'_, Knowledge> {
    fn parent_type_id(&self, type_id: &TypeId) -> Option<TypeId> {
        self.source
            .data
            .get(type_id)
            .and_then(|data| data.parent.clone())
            .or_else(|| {
                self.external
                    .object(type_id.object())?
                    .type_info()
                    .and_then(|data| data.parent.clone())
            })
    }

    fn has_strict_subtype_id(&self, type_id: &TypeId) -> bool {
        self.source
            .data
            .iter()
            .any(|(candidate, data)| candidate != type_id && data.parent.as_ref() == Some(type_id))
            || self.external.has_strict_subtype_id(type_id)
    }
}

pub struct TypeChecker<'analysis, 'knowledge, Knowledge: ?Sized> {
    analysis: &'analysis Analysis,
    knowledge_provider: &'knowledge Knowledge,
    type_cache: RefCell<HashMap<SyntaxNodeId, InferredType>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DispatchIdentity {
    Type(TypeId),
    Object(ObjectId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DispatchRange {
    Exact(DispatchIdentity),
    Upward(TypeId),
}

struct ResolvedDispatchDomain {
    signature_index: usize,
    slots: Vec<DispatchIdentity>,
}

enum TypeSubstitution<'tree> {
    Exact(ObjectName),
    Follow(M2Node<'tree>),
    Union(Vec<Self>),
    Symbol(M2Node<'tree>),
    Dispatch(M2Node<'tree>),
    Unknown,
}

/// One transient operator-inference request passed to specialized rule sets.
#[derive(Debug, Clone, Copy)]
pub struct OperatorTypeQuery<'tree> {
    pub operator: &'tree str,
    pub left: M2Node<'tree>,
    pub right: Option<M2Node<'tree>>,
    pub scope_idx: usize,
}

impl<'analysis, 'knowledge, Knowledge: ?Sized> TypeChecker<'analysis, 'knowledge, Knowledge> {
    pub fn new(analysis: &'analysis Analysis, knowledge_provider: &'knowledge Knowledge) -> Self {
        Self {
            analysis,
            knowledge_provider,
            type_cache: RefCell::new(HashMap::new()),
        }
    }
}

impl<Knowledge: ?Sized> Deref for TypeChecker<'_, '_, Knowledge> {
    type Target = Analysis;

    fn deref(&self) -> &Self::Target {
        self.analysis
    }
}

impl Analysis {
    pub fn for_each_expression_type<'tree>(
        &self,
        root: M2Node<'tree>,
        syntax: Option<&SourceFile>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge_provider: &(impl PositionedTypeKnowledge + ?Sized),
        mut visit: impl FnMut(M2Node<'tree>, InferredType),
    ) {
        let checker = TypeChecker::new(self, knowledge_provider);
        let mut nodes = Vec::new();
        visit_source_nodes(root, syntax, |node| nodes.push(node));
        for node in nodes.into_iter().rev() {
            let substitution = type_substitution(node);
            if matches!(substitution, TypeSubstitution::Unknown) {
                continue;
            }
            let position = source.position_for_node(node);
            let scope_idx = self.find_scope_at(position).unwrap_or(0);
            visit(node, checker.type_of(node, source, scope_idx));
        }
    }

    pub fn has_strict_subtype_at(
        &self,
        name: &ObjectName,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> bool {
        let checker = TypeChecker::new(self, knowledge);
        checker
            .resolve_type_id_at(name, position, knowledge)
            .is_some_and(|type_id| {
                SourceTypeOrder {
                    source: &self.registry.source_types,
                    external: knowledge,
                }
                .has_strict_subtype_id(&type_id)
            })
    }

    pub(super) fn enrich_types_cst(
        &mut self,
        node: M2Node,
        source: &(impl SourceNavigation + ?Sized),
        current_scope_idx: usize,
        assignment_scope_idx: usize,
        knowledge_provider: &(impl PositionedTypeKnowledge + ?Sized),
    ) {
        let knowledge = knowledge_provider.at_position(source.position_for_node(node));
        let _ = self.record_install_method_call(node, source, &knowledge);
        let mut next_scope_idx = current_scope_idx;
        let mut next_assignment_scope_idx = assignment_scope_idx;

        if node.is::<LambdaExpression>() {
            next_scope_idx = self.registry.scopes.owned_by(node, source);
            next_assignment_scope_idx = next_scope_idx;
            self.enrich_lambda_node(node, source);
        } else if node.is::<ForLoop>() {
            next_scope_idx = self.registry.scopes.owned_by(node, source);
        } else if node.is_assignment() {
            self.enrich_assignment_node(node, None, source, current_scope_idx, knowledge_provider);
        }

        for child in node.children() {
            let (child_scope_idx, child_assignment_scope_idx) =
                match control_flow_scope(node, child) {
                    Some(scope_kind) => {
                        let scope_idx = self.registry.scopes.owned_by(child, source);
                        let assignment_scope_idx =
                            scope_kind.assignment_scope(scope_idx, next_assignment_scope_idx);
                        (scope_idx, assignment_scope_idx)
                    }
                    None => (next_scope_idx, next_assignment_scope_idx),
                };
            self.enrich_types_cst(
                child,
                source,
                child_scope_idx,
                child_assignment_scope_idx,
                knowledge_provider,
            );
        }
    }

    fn enrich_lambda_node(&mut self, node: M2Node, source: &(impl SourceNavigation + ?Sized)) {
        if let Some(parameters) = node.child_by_field_name("parameters") {
            let parameter_types = method_installation_parameter_types_for_function(node);
            self.enrich_parameters(parameters, source, parameter_types.as_deref());
        }
    }

    fn enrich_assignment_node(
        &mut self,
        node: M2Node,
        syntax: Option<&Assignment>,
        source: &(impl SourceNavigation + ?Sized),
        scope_idx: usize,
        knowledge_provider: &(impl PositionedTypeKnowledge + ?Sized),
    ) {
        let Some((left, operator, value)) = assignment_parts(node) else {
            return;
        };
        let right = Some(value);
        let knowledge = knowledge_provider.at_position(source.position_for_node(node));
        let installation_id = self.record_method_installation(node, source, &knowledge);
        let inferred_type = right.map(|right| {
            if method_declaration_typical_value(right).is_some() || is_method_call(right) {
                InferredType::exact_from_id(TypeRole::MethodFunction.object_name())
            } else {
                TypeChecker::new(self, knowledge_provider).type_of(right, source, scope_idx)
            }
        });
        let type_name = inferred_type.as_ref().and_then(InferredType::single);
        let parent_type = right.and_then(|right| {
            self.declared_type_parent(
                right,
                type_name,
                source.position_for_node(right),
                &knowledge,
            )
        });
        let presentation_kind = if right.is_some_and(|right| {
            right.is::<LambdaExpression>()
                || method_declaration_typical_value(right).is_some()
                || is_method_call(right)
        }) || type_name
            .is_some_and(|type_name| knowledge.has_type_role(type_name, TypeRole::Function))
        {
            SymbolKind::FUNCTION
        } else if type_name.is_some_and(|type_name| {
            TypeChecker::new(self, &knowledge).is_subtype(
                type_name,
                &TypeRole::Type.object_name(),
                source.position_for_node(left),
                &knowledge,
            )
        }) {
            SymbolKind::CLASS
        } else {
            SymbolKind::VARIABLE
        };
        let object_id = right
            .zip(single_symbol_assignment_target(left))
            .and_then(|(right, _)| {
                self.callable_object_for_value(right, source, scope_idx, &knowledge)
            });

        if !left.is::<NewStatement>()
            && (matches_token::<Token![:=]>(operator.text())
                || matches_token::<Token![=]>(operator.text()))
        {
            self.enrich_definitions(
                left,
                source,
                BindingEnrichment {
                    presentation_kind,
                    inferred_type: inferred_type.clone(),
                    object_id,
                    indexed_element_type: None,
                    parent_type,
                },
            );
        }

        if let (Some(right), Some(type_name), Some(ring_name)) =
            (right, type_name, single_symbol_assignment_target(left))
        {
            if knowledge.has_type_role(type_name, TypeRole::Ring) {
                self.collect_ring_generator_bindings(
                    ring_name,
                    (right, syntax.map(assignment_value)),
                    left,
                    scope_idx,
                    source,
                    &knowledge,
                );
            }
        }

        self.record_assignment_fact(node, left, right, scope_idx, installation_id, source);
    }

    pub(super) fn enrich_types_typed(
        &mut self,
        syntax: &SourceFile,
        root: M2Node,
        source: &(impl SourceNavigation + ?Sized),
        knowledge_provider: &(impl PositionedTypeKnowledge + ?Sized),
    ) {
        TypedTypeEnricher {
            analysis: self,
            root,
            source,
            knowledge_provider,
        }
        .visit_source_file(syntax);
    }
}

struct TypedTypeEnricher<'analysis, 'tree, 'source, 'knowledge, Source: ?Sized, Knowledge: ?Sized> {
    analysis: &'analysis mut Analysis,
    root: M2Node<'tree>,
    source: &'source Source,
    knowledge_provider: &'knowledge Knowledge,
}

impl<Source, Knowledge> TypedTypeEnricher<'_, '_, '_, '_, Source, Knowledge>
where
    Source: SourceNavigation + ?Sized,
    Knowledge: PositionedTypeKnowledge + ?Sized,
{
    fn record_install_method_call(&mut self, node: M2Node) {
        let knowledge = self
            .knowledge_provider
            .at_position(self.source.position_for_node(node));
        let _ = self
            .analysis
            .record_install_method_call(node, self.source, &knowledge);
    }
}

impl<'ast, Source, Knowledge> Visit<'ast> for TypedTypeEnricher<'_, '_, '_, '_, Source, Knowledge>
where
    Source: SourceNavigation + ?Sized,
    Knowledge: PositionedTypeKnowledge + ?Sized,
{
    fn visit_expr_assign(&mut self, node: &'ast Assignment) {
        if let Some(assignment) = self.root.descendant_for_syntax(node) {
            let scope_idx = self
                .analysis
                .find_scope_at(self.source.position_for_node(assignment))
                .unwrap_or(0);
            self.analysis.enrich_assignment_node(
                assignment,
                Some(node),
                self.source,
                scope_idx,
                self.knowledge_provider,
            );
        }
        visit::visit_expr_assign(self, node);
    }

    fn visit_expr_lambda(&mut self, node: &'ast LambdaExpression) {
        if let Some(lambda) = self.root.descendant_for_syntax(node) {
            self.analysis.enrich_lambda_node(lambda, self.source);
        }
        visit::visit_expr_lambda(self, node);
    }

    fn visit_expr_op_bin_adj(&mut self, node: &'ast AdjacentExpr) {
        if let Some(call) = self.root.descendant_for_syntax(node) {
            self.record_install_method_call(call);
        }
        visit::visit_expr_op_bin_adj(self, node);
    }
}

fn assignment_value(node: &Assignment) -> &Expr {
    match node {
        Assignment::Simple(SimpleBinding::Global(node)) => &node.value,
        Assignment::Simple(SimpleBinding::Local(node)) => &node.value,
        Assignment::Parallel(ParallelAssignment::Global(node)) => &node.value,
        Assignment::Parallel(ParallelAssignment::Local(node)) => &node.value,
        Assignment::Evaluated(EvaluatedAssignment { value, .. }) => value,
        Assignment::Augmented(node) => &node.value,
        Assignment::Install(Installation::Method(node)) => &node.function,
        Assignment::Install(Installation::Op(node)) => &node.function,
        Assignment::Install(Installation::New(node)) => &node.function,
        Assignment::Op(OperatorAssignment { value_expr, .. }) => value_expr,
    }
}

impl<Knowledge: PositionedTypeKnowledge + ?Sized> TypeChecker<'_, '_, Knowledge> {
    pub fn inferred_external_type(
        &self,
        type_id: TypeId,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> InferredType {
        let Some(name) = knowledge.type_name(&type_id).cloned() else {
            return InferredType::unknown();
        };
        Type::from_id(name.clone(), type_id, knowledge)
            .map(InferredType::upward_type)
            .unwrap_or_else(|| InferredType::upward_from_id(name))
    }

    fn resolve_dispatch_identity_at(
        &self,
        name: &ObjectName,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<DispatchIdentity> {
        self.resolve_type_id_at(name, position, knowledge)
            .map(DispatchIdentity::Type)
            .or_else(|| knowledge.resolve_object(name).map(DispatchIdentity::Object))
    }

    fn inferred_dispatch_products(
        &self,
        arguments: &[InferredType],
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<Vec<Vec<DispatchRange>>> {
        let mut products = vec![Vec::new()];
        for argument in arguments {
            let mut alternatives = Vec::new();
            for point in argument.exact_points() {
                alternatives.push(DispatchRange::Exact(
                    self.resolve_dispatch_identity_at(point, position, knowledge)?,
                ));
            }
            for generator in argument.upward_generators() {
                alternatives.push(DispatchRange::Upward(
                    self.resolve_type_id_at(generator, position, knowledge)?,
                ));
            }
            if alternatives.is_empty() {
                return Some(Vec::new());
            }

            let product_count = products.len().checked_mul(alternatives.len())?;
            if product_count > MAX_DISPATCH_PRODUCTS {
                return None;
            }
            products = products
                .into_iter()
                .flat_map(|product| {
                    alternatives.iter().map(move |alternative| {
                        let mut product = product.clone();
                        product.push(alternative.clone());
                        product
                    })
                })
                .collect();
        }
        Some(products)
    }

    fn dispatch_candidate_indices(
        &self,
        signatures: &[ResolvedDispatchDomain],
        arguments: &[InferredType],
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Vec<usize> {
        let Some(products) = self.inferred_dispatch_products(arguments, position, knowledge) else {
            return signatures
                .iter()
                .filter(|signature| signature.slots.len() == arguments.len())
                .map(|signature| signature.signature_index)
                .collect();
        };
        let order = SourceTypeOrder {
            source: &self.registry.source_types,
            external: knowledge,
        };
        let mut witnesses = Vec::new();

        for product in products {
            for signature in signatures {
                if signature.slots.len() != product.len() {
                    continue;
                }
                let witness = signature
                    .slots
                    .iter()
                    .zip(&product)
                    .map(|(expected, actual)| {
                        dispatch_intersection_witness(actual, expected, &order)
                    })
                    .collect::<Option<Vec<_>>>();
                if let Some(witness) = witness {
                    if !witnesses.contains(&witness) {
                        witnesses.push(witness);
                    }
                }
            }
        }

        let mut candidates = Vec::new();
        for witness in witnesses {
            let matching = signatures
                .iter()
                .filter(|signature| dispatch_domain_matches(&signature.slots, &witness, &order))
                .collect::<Vec<_>>();
            let minimal = minimal_candidates(matching, |other, candidate| {
                dispatch_domain_strictly_smaller(&other.slots, &candidate.slots, &order)
            });
            candidates.extend(
                minimal
                    .into_iter()
                    .map(|signature| signature.signature_index),
            );
        }
        candidates.sort_unstable();
        candidates.dedup();
        candidates
    }

    pub fn external_call_signature_facts(
        &self,
        callable: &ObjectName,
        facts: &CallStaticFacts,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<CallSignatureFacts> {
        let callable = knowledge.get_record(callable)?.callable()?;
        let signatures = self.external_dispatch_domains(callable, knowledge);
        let dispatch_candidates = self.dispatch_candidate_indices(
            &signatures,
            &facts.argument_types,
            position,
            knowledge,
        );
        let candidates = effective_external_candidate_indices(callable, &dispatch_candidates);
        let mut possible = candidates
            .iter()
            .filter_map(|candidate| callable.methods.get(*candidate).cloned())
            .collect::<Vec<_>>();
        let excluded = callable
            .methods
            .iter()
            .enumerate()
            .filter(|(index, method)| {
                method.domain.len() == facts.argument_types.len()
                    && !dispatch_candidates.contains(index)
            })
            .map(|(_, method)| method.clone())
            .collect();
        let pinned = (possible.len() == 1).then(|| possible.remove(0));

        Some(CallSignatureFacts {
            pinned,
            possible,
            excluded,
        })
    }

    pub fn type_of(
        &self,
        node: M2Node,
        source: &(impl SourceNavigation + ?Sized),
        scope_idx: usize,
    ) -> InferredType {
        if let Some(inferred) = self.type_cache.borrow().get(&node.id()).cloned() {
            return inferred;
        }
        let position = source.position_for_node(node);
        let knowledge = self.knowledge_provider.at_position(position);
        let inferred = self.evaluate_type_substitution(
            type_substitution(node),
            source,
            scope_idx,
            position,
            &knowledge,
        );
        self.type_cache
            .borrow_mut()
            .insert(node.id(), inferred.clone());
        inferred
    }
}

fn type_substitution(node: M2Node<'_>) -> TypeSubstitution<'_> {
    use TypeSubstitution::{Dispatch, Exact, Follow, Symbol, Union, Unknown};

    let exact = |name| Exact(ObjectName::new(name));

    match node {
        node if node.is::<LambdaExpression>() => exact("FunctionClosure"),
        node if method_declaration_typical_value(node).is_some() || is_method_call(node) => {
            Exact(TypeRole::MethodFunction.object_name())
        }
        node if node.is::<List>() => exact("List"),
        node if node.is::<Array>() => exact("Array"),
        node if node.is::<AngleBarList>() => exact("AngleBarList"),
        node if node.is_sequence() => exact("Sequence"),
        node if node.is_nothing_value() => exact("Nothing"),
        node if node.is_holder() => parenthesized_value(node)
            .map(Follow)
            .unwrap_or_else(|| exact("Nothing")),
        node if node.is::<StringLiteral>() || node.is::<RawStringLiteral>() => {
            Exact(TypeRole::String.object_name())
        }
        node if node.is::<IntegerLiteral>() => exact("ZZ"),
        node if node.is::<FloatLiteral>() => exact("RR"),
        node if node.is::<QuoteExpression>() => exact("Symbol"),
        node if node.is::<m2_syn::nodes::Symbol>() => Symbol(node),
        node if node.is_assignment() => node
            .child_by_field_name("right")
            .map(Follow)
            .unwrap_or(Unknown),
        node if node.is::<OptionExpression>() => exact("Option"),
        node if node.is_adjacent_expr()
            || node.is_binary_expr()
            || node.is_prefix_expr()
            || node.is_postfix_expr() =>
        {
            Dispatch(node)
        }
        node if node.is::<NewStatement>() => node
            .child_by_field_name("type")
            .filter(|type_node| type_node.is::<m2_syn::nodes::Symbol>())
            .map(|type_node| Exact(ObjectName::new(type_node.text())))
            .unwrap_or(Unknown),
        node if node.is::<IfStatement>() => {
            let then_value = clause_of::<ThenClause>(node)
                .and_then(clause_value)
                .map(Follow)
                .unwrap_or(Unknown);
            let else_value = clause_of::<ElseClause>(node)
                .and_then(clause_value)
                .map(Follow)
                .unwrap_or_else(|| exact("Nothing"));
            Union(vec![then_value, else_value])
        }
        node if node.is::<TryStatement>() => {
            let body = node.child_by_field_name("value");
            let success = clause_of::<ThenClause>(node)
                .and_then(clause_value)
                .or(body)
                .map(Follow)
                .unwrap_or(Unknown);
            let failure = clause_of::<ElseClause>(node)
                .and_then(clause_value)
                .map(Follow)
                .unwrap_or_else(|| exact("Nothing"));
            Union(vec![success, failure])
        }
        node if node.is::<ForLoop>() => {
            let lists = node
                .named_children()
                .find(|child| child.is::<LoopBody>())
                .and_then(|body| body.child_by_field_name("listed_value"))
                .is_some();
            if lists {
                exact("List")
            } else {
                exact("Nothing")
            }
        }
        node if node.is::<WhileLoop>() => exact("Nothing"),
        node if node.is_control_transfer() => node
            .control_transfer_value()
            .map(Follow)
            .unwrap_or_else(|| exact("Nothing")),
        node if node.is_debug_expr() => node.named_children().next().map(Follow).unwrap_or(Unknown),
        _ => Unknown,
    }
}

impl<Knowledge: PositionedTypeKnowledge + ?Sized> TypeChecker<'_, '_, Knowledge> {
    fn evaluate_type_substitution(
        &self,
        substitution: TypeSubstitution<'_>,
        source: &(impl SourceNavigation + ?Sized),
        scope_idx: usize,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> InferredType {
        match substitution {
            TypeSubstitution::Exact(type_name) => self
                .resolve_type_at(&type_name, position, knowledge)
                .map(InferredType::exact_type)
                .unwrap_or_else(|| InferredType::exact_from_id(type_name)),
            TypeSubstitution::Follow(node) => self.type_of(node, source, scope_idx),
            TypeSubstitution::Union(substitutions) => substitutions.into_iter().fold(
                InferredType::diverges(),
                |inferred, substitution| {
                    self.join_types(
                        inferred,
                        self.evaluate_type_substitution(
                            substitution,
                            source,
                            scope_idx,
                            position,
                            knowledge,
                        ),
                        position,
                        knowledge,
                    )
                },
            ),
            TypeSubstitution::Symbol(node) => self.symbol_type(node, source, scope_idx, knowledge),
            TypeSubstitution::Dispatch(node)
                if node.is_adjacent_expr() || node.is_binary_expr() =>
            {
                self.binary_expression_type(node, source, scope_idx, knowledge)
            }
            TypeSubstitution::Dispatch(node) if node.is_prefix_expr() || node.is_postfix_expr() => {
                self.unary_operator_type(node, source, scope_idx, knowledge)
            }
            TypeSubstitution::Dispatch(_) => InferredType::unknown(),
            TypeSubstitution::Unknown => InferredType::unknown(),
        }
    }

    fn join_types(
        &self,
        left: InferredType,
        right: InferredType,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> InferredType {
        let left = left.resolved_by(|name| self.resolve_type_at(name, position, knowledge));
        let right = right.resolved_by(|name| self.resolve_type_at(name, position, knowledge));
        left | right
    }

    /// A symbol's type, in precedence order: an in-scope user binding (which
    /// overrides an earlier visible unprotected name), then the visible object's
    /// recorded class, then `Symbol` (an
    /// unbound name evaluates to its own `Symbol` in M2).
    fn symbol_type(
        &self,
        node: M2Node,
        source: &(impl SourceNavigation + ?Sized),
        scope_idx: usize,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> InferredType {
        let name = node.text();
        if let Some(binding) = self.visible_source_binding_from_scope(
            name,
            scope_idx,
            source.position_for_node(node),
            knowledge,
        ) {
            return binding
                .state
                .inferred_type
                .as_ref()
                .cloned()
                .unwrap_or_else(InferredType::unknown);
        }

        if let Some(reference) = OutputReference::parse(name) {
            return self.output_reference_type(node, reference, source);
        }

        if let Some(record) = knowledge.get_record(&ObjectName::new(name)) {
            return InferredType::exact_from_id(record.class.clone());
        }

        InferredType::exact("Symbol")
    }

    fn output_reference_type(
        &self,
        node: M2Node,
        reference: OutputReference,
        source: &(impl SourceNavigation + ?Sized),
    ) -> InferredType {
        let Some(output) = reference.referenced_value(node) else {
            return InferredType::exact("Symbol");
        };
        let output_scope = self
            .find_scope_at(source.position_for_node(output))
            .unwrap_or(0);
        self.type_of(output, source, output_scope)
    }

    /// A binary expression's type. Juxtaposition `a SPACE b` is application
    /// (handled by [`Self::application_type`]); the function-dependent operators
    /// `_` (currying) and `@@` (composition) are computed here when their
    /// function-position operand is a `Function`; everything else dispatches
    /// through the M2 type table.
    fn binary_expression_type(
        &self,
        node: M2Node,
        source: &(impl SourceNavigation + ?Sized),
        scope_idx: usize,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> InferredType {
        if node.is_space_application() {
            return self.application_type(node, source, scope_idx, knowledge);
        }

        let operator = node.binary_operator();
        let left = node.child_by_field_name("left");
        let right = node.child_by_field_name("right");

        if let Some(operator) = operator {
            if let Some(result) =
                self.special_operator_type(operator, left, right, source, scope_idx, knowledge)
            {
                return result;
            }
        }

        let (Some(left), Some(right), Some(operator)) =
            (left, right, Operator::from_expression(node))
        else {
            return InferredType::unknown();
        };
        let left_type = self.type_of(left, source, scope_idx);
        let right_type = self.type_of(right, source, scope_idx);
        let position = source.position_for_node(node);
        let arguments = [left_type, right_type];
        if is_comparison_operator(operator.token.name()) {
            return self
                .resolved_dispatch_codomain(knowledge, &operator, &arguments, &[], position)
                .unwrap_or_else(|| InferredType::exact_from_id(TypeRole::Boolean.object_name()));
        }
        self.dispatch_codomain(knowledge, &operator, &arguments, &[], position)
    }

    /// The function-dependent operators, whose result depends on the specific
    /// function value (M2 has no dependent types, so the M2 table cannot express
    /// them): currying `f _ x` (`f_x(y) := f(x, y)`) and composition `f @@ g`
    /// both yield a `FunctionClosure` when the function-position operand is a
    /// `Function`. `None` falls through to ordinary dispatch (so `M_i`, `L_i`,
    /// … keep their table behavior).
    fn special_operator_type(
        &self,
        operator: &str,
        left: Option<M2Node>,
        right: Option<M2Node>,
        source: &(impl SourceNavigation + ?Sized),
        scope_idx: usize,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<InferredType> {
        let left = left?;
        let left_type = self.type_of(left, source, scope_idx);
        let left_name = left_type.single()?;

        if matches_token::<Token![_]>(operator) && left_name.as_ref() == "Symbol" {
            return Some(InferredType::exact("IndexedVariable"));
        }

        let query = OperatorTypeQuery {
            operator,
            left,
            right,
            scope_idx,
        };
        if let Some(result) = self.ring_operator_type(query, left_name, source, knowledge) {
            return Some(result);
        }

        if (matches_token::<Token![_]>(operator) || matches_token::<Token![@@]>(operator))
            && knowledge.has_type_role(left_name, TypeRole::Function)
        {
            return Some(InferredType::exact("FunctionClosure"));
        }

        None
    }

    /// Application `f SPACE x`. A `Function` head delegates to the head's own
    /// signatures available in the current object environment,
    /// stepping beyond the M2 table whose `(Function, Thing)` row only yields
    /// `Thing`. A non-`Function` head dispatches `SPACE` through the table
    /// (`Ring × Array → PolynomialRing`).
    fn application_type(
        &self,
        node: M2Node,
        source: &(impl SourceNavigation + ?Sized),
        scope_idx: usize,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> InferredType {
        let (Some(callable_node), Some(argument_node)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            return InferredType::unknown();
        };
        let Some(operator) = Operator::from_expression(node) else {
            return InferredType::unknown();
        };
        let callable_name = symbol_node_text(callable_node);
        let call_facts = self.infer_call_facts_for_callable(
            argument_node,
            source,
            scope_idx,
            callable_name,
            knowledge,
        );

        // A locally-defined function is known to be a function from the registry
        // alone, so its application resolves without external type facts: its
        // signatures give the codomain, and an undocumented one yields `Thing`
        // (applying a function gives at least a Thing).
        if let Some(callable) = callable_name {
            let position = source.position_for_node(callable_node);
            if let Some(function) = self.local_function_at(callable, scope_idx, position, knowledge)
            {
                return self
                    .resolve_local_call_return_type(
                        function,
                        &call_facts.argument_types,
                        position,
                        knowledge,
                    )
                    .unwrap_or_else(InferredType::unknown);
            }
            if callable == "error"
                && self
                    .visible_source_binding_from_scope(callable, scope_idx, position, knowledge)
                    .is_none()
                && knowledge.get_record(&ObjectName::new(callable)).is_some()
            {
                return InferredType::diverges();
            }
        }

        // Otherwise the lattice decides whether the head is a function (delegating
        // to its signatures) or another SPACE method (`Ring × Array →
        // PolynomialRing`).
        let head = self.type_of(callable_node, source, scope_idx);
        let head_is_function = head
            .single()
            .is_some_and(|head| knowledge.has_type_role(head, TypeRole::Function));
        if head_is_function {
            if let Some(callable) = callable_name {
                if let Some(return_type) = self.resolve_external_call_return_type(
                    &ObjectName::new(callable),
                    &call_facts.argument_types,
                    &call_facts.literal_options,
                    source.position_for_node(node),
                    knowledge,
                ) {
                    return return_type;
                }
            }
            // Applying a function yields at least a Thing.
            return InferredType::unknown();
        }

        if let Some(result) = self.ring_application_with_trailing_operator_type(
            &operator,
            &head,
            argument_node,
            source,
            scope_idx,
            knowledge,
        ) {
            return result;
        }

        let argument_type = self.type_of(argument_node, source, scope_idx);
        self.dispatch_codomain(
            knowledge,
            &operator,
            &[head, argument_type],
            &call_facts.literal_options,
            source.position_for_node(node),
        )
    }

    /// Whether `name` resolves to a function tracked in the local registry — a
    /// lambda binding or a local method declaration. Such a head is known to be a
    /// function without consulting external type facts.
    fn local_function_at(
        &self,
        name: &str,
        scope_idx: usize,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<&FunctionInfo> {
        let binding = self.get_binding_from_scope(name, scope_idx, position)?;
        if binding.scope_idx == 0
            && knowledge.shadows_source(&binding.name, binding.state.span.start)
        {
            return None;
        }
        self.function_for_binding(binding)
    }

    /// A prefix/postfix operator's type: `typicalValue(op, operand)`.
    fn unary_operator_type(
        &self,
        node: M2Node,
        source: &(impl SourceNavigation + ?Sized),
        scope_idx: usize,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> InferredType {
        let (Some(operator), Some(operand)) = (
            Operator::from_expression(node),
            node.child_by_field_name("operand"),
        ) else {
            return InferredType::unknown();
        };
        let operand_type = self.type_of(operand, source, scope_idx);
        self.dispatch_codomain(
            knowledge,
            &operator,
            &[operand_type],
            &[],
            source.position_for_node(node),
        )
    }

    /// Dispatch `callable` on `args` through the M2 type table. A matched but
    /// undocumented codomain is `Thing` (≡ a null `typicalValue` under the
    /// lower-bound reading) — approximated by "the callable/operator resolves to
    /// a known object, so it dispatches"; an unidentifiable head stays `Unknown`.
    pub fn dispatch_codomain(
        &self,
        knowledge: &(impl TypeKnowledge + ?Sized),
        operator: &Operator,
        args: &[InferredType],
        options: &[LiteralOption],
        position: Position,
    ) -> InferredType {
        if let Some(return_type) =
            self.resolved_dispatch_codomain(knowledge, operator, args, options, position)
        {
            return return_type;
        }
        if knowledge.get_record(&operator.token).is_some() {
            return InferredType::unknown();
        }
        InferredType::unknown()
    }

    fn resolved_dispatch_codomain(
        &self,
        knowledge: &(impl TypeKnowledge + ?Sized),
        operator: &Operator,
        args: &[InferredType],
        options: &[LiteralOption],
        position: Position,
    ) -> Option<InferredType> {
        if let Some(function) = self.registry.operator_functions.get(operator) {
            if let Some(return_type) =
                self.resolve_local_call_return_type(function, args, position, knowledge)
            {
                return Some(return_type);
            }
        }
        self.resolve_external_call_return_type(&operator.token, args, options, position, knowledge)
    }

    fn resolve_external_call_return_type(
        &self,
        callable: &ObjectName,
        argument_types: &[InferredType],
        _options: &[LiteralOption],
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<InferredType> {
        let callable = knowledge.get_record(callable)?.callable()?;
        let signatures = self.external_dispatch_domains(callable, knowledge);
        let candidates =
            self.dispatch_candidate_indices(&signatures, argument_types, position, knowledge);
        let candidates = effective_external_candidate_indices(callable, &candidates);

        if candidates.is_empty() {
            callable
                .typical_value
                .clone()
                .map(|codomain| self.inferred_external_type(codomain, knowledge))
        } else {
            Some(self.external_candidates_return_type(callable, &candidates, position, knowledge))
        }
    }

    fn external_dispatch_domains(
        &self,
        callable: &crate::builtin_index::CallableInfo,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Vec<ResolvedDispatchDomain> {
        callable
            .methods
            .iter()
            .enumerate()
            .map(|(signature_index, method)| ResolvedDispatchDomain {
                signature_index,
                slots: method
                    .domain
                    .iter()
                    .map(|object| {
                        knowledge.type_id(object).map_or_else(
                            || DispatchIdentity::Object(object.clone()),
                            DispatchIdentity::Type,
                        )
                    })
                    .collect(),
            })
            .collect()
    }

    fn external_candidates_return_type(
        &self,
        callable: &crate::builtin_index::CallableInfo,
        candidates: &[usize],
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> InferredType {
        candidates
            .iter()
            .fold(InferredType::diverges(), |result, candidate| {
                let inferred = callable
                    .methods
                    .get(*candidate)
                    .and_then(|method| callable.effective_codomain(method))
                    .map_or_else(InferredType::unknown, |(codomain, _)| {
                        self.inferred_external_type(codomain.clone(), knowledge)
                    });
                self.join_types(result, inferred, position, knowledge)
            })
    }

    pub fn infer_call_facts(
        &self,
        node: M2Node,
        source: &(impl SourceNavigation + ?Sized),
        scope_idx: usize,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> CallStaticFacts {
        self.infer_call_facts_for_callable(node, source, scope_idx, None, knowledge)
    }

    fn infer_call_facts_for_callable(
        &self,
        node: M2Node,
        source: &(impl SourceNavigation + ?Sized),
        scope_idx: usize,
        callable: Option<&str>,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> CallStaticFacts {
        // A single parenthesized argument `f(x)` / `f(opt => v)` denotes its inner
        // value; peel it so the argument is classified like a bare argument.
        let node = parenthesized_value(node).unwrap_or(node);
        let receives_sequence = callable.is_some_and(|name| {
            self.callable_receives_sequence(
                name,
                scope_idx,
                source.position_for_node(node),
                knowledge,
            )
        });
        if node.is::<Sequence>() && !receives_sequence {
            let mut facts = CallStaticFacts::default();
            for child in node.collection_elements() {
                if let Some(option) = literal_option_assignment(child) {
                    facts.literal_options.push(option);
                } else {
                    facts
                        .argument_types
                        .push(self.type_of(child, source, scope_idx));
                }
            }
            return facts;
        }

        if let Some(option) = literal_option_assignment(node) {
            return CallStaticFacts {
                argument_types: Vec::new(),
                literal_options: vec![option],
            };
        }

        CallStaticFacts {
            argument_types: vec![self.type_of(node, source, scope_idx)],
            literal_options: Vec::new(),
        }
    }

    fn callable_receives_sequence(
        &self,
        name: &str,
        scope_idx: usize,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> bool {
        let local_dispatch = self
            .local_function_at(name, scope_idx, position, knowledge)
            .and_then(|function| function.dispatch);
        if local_dispatch == Some(Dispatch::Variadic) {
            return true;
        }

        knowledge
            .get_record(&ObjectName::new(name))
            .and_then(|record| record.callable())
            .is_some_and(|callable| callable.receives_sequence)
    }

    fn resolve_local_call_return_type(
        &self,
        function: &FunctionInfo,
        argument_types: &[InferredType],
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<InferredType> {
        let candidates =
            self.local_call_candidate_installations(function, argument_types, position, knowledge);
        if candidates.is_empty() {
            function
                .typical_value
                .clone()
                .map(InferredType::upward_from_id)
        } else {
            Some(
                candidates
                    .into_iter()
                    .fold(InferredType::diverges(), |result, installation| {
                        let inferred = installation
                            .method
                            .codomain
                            .as_ref()
                            .or(function.typical_value.as_ref())
                            .cloned()
                            .map_or_else(InferredType::unknown, InferredType::upward_from_id);
                        self.join_types(result, inferred, position, knowledge)
                    }),
            )
        }
    }

    pub fn local_call_candidate_installation_ids(
        &self,
        function: &FunctionInfo,
        argument_types: &[InferredType],
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Vec<MethodInstallationId> {
        self.local_call_candidate_installations(function, argument_types, position, knowledge)
            .into_iter()
            .map(|installation| installation.id)
            .collect()
    }

    fn local_call_candidate_installations(
        &self,
        function: &FunctionInfo,
        argument_types: &[InferredType],
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Vec<&MethodInstallation> {
        let installations = self
            .analysis
            .method_installations_for_at(function, position);
        let signatures = installations
            .iter()
            .enumerate()
            .filter_map(|(signature_index, installation)| {
                Some(ResolvedDispatchDomain {
                    signature_index,
                    slots: installation
                        .method
                        .domain
                        .iter()
                        .map(|name| {
                            self.resolve_type_id_at(name, position, knowledge)
                                .map(DispatchIdentity::Type)
                        })
                        .collect::<Option<Vec<_>>>()?,
                })
            })
            .collect::<Vec<_>>();
        let candidates =
            self.dispatch_candidate_indices(&signatures, argument_types, position, knowledge);
        candidates
            .into_iter()
            .map(|candidate| installations[candidate])
            .collect()
    }

    pub fn is_subtype(
        &self,
        actual: &ObjectName,
        expected: &ObjectName,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> bool {
        self.subtype_evidence(actual, expected, position, knowledge) == SubtypeEvidence::Proven
    }

    pub fn subtype_evidence(
        &self,
        actual: &ObjectName,
        expected: &ObjectName,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> SubtypeEvidence {
        let order = SourceTypeOrder {
            source: &self.registry.source_types,
            external: knowledge,
        };
        let Some((actual, expected)) = self
            .resolve_type_id_at(actual, position, knowledge)
            .zip(self.resolve_type_id_at(expected, position, knowledge))
        else {
            return SubtypeEvidence::Unknown;
        };
        if order.is_subtype_id(&actual, &expected) {
            SubtypeEvidence::Proven
        } else {
            SubtypeEvidence::Disproven
        }
    }

    fn resolve_source_type_id(&self, name: &ObjectName, position: Position) -> Option<TypeId> {
        self.get_binding_at(name.name(), position)?
            .state
            .source_type
            .clone()
    }

    pub fn resolve_type_id_at(
        &self,
        name: &ObjectName,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<TypeId> {
        self.resolve_source_type_id(name, position)
            .or_else(|| knowledge.resolve_type_id(name))
    }

    fn resolve_type_at(
        &self,
        name: &ObjectName,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<Type> {
        let type_id = self.resolve_type_id_at(name, position, knowledge)?;
        Type::from_id(
            name.clone(),
            type_id,
            &SourceTypeOrder {
                source: &self.registry.source_types,
                external: knowledge,
            },
        )
    }
}

fn is_comparison_operator(operator: &str) -> bool {
    [
        token_spelling::<Token![==]>(),
        token_spelling::<Token![===]>(),
        token_spelling::<Token![!=]>(),
        token_spelling::<Token![=!=]>(),
        token_spelling::<Token![<]>(),
        token_spelling::<Token![<=]>(),
        token_spelling::<Token![>]>(),
        token_spelling::<Token![>=]>(),
    ]
    .contains(&operator)
}

fn effective_external_candidate_indices(
    callable: &crate::builtin_index::CallableInfo,
    candidates: &[usize],
) -> Vec<usize> {
    let mut selected = Vec::new();
    for candidate in candidates {
        let Some(method) = callable.methods.get(*candidate) else {
            continue;
        };
        if candidates.iter().any(|other| {
            other < candidate
                && callable
                    .methods
                    .get(*other)
                    .is_some_and(|other| other.domain == method.domain)
        }) {
            continue;
        }

        let same_domain = candidates.iter().copied().filter(|other| {
            callable
                .methods
                .get(*other)
                .is_some_and(|other| other.domain == method.domain)
        });
        let explicit = same_domain
            .clone()
            .filter(|other| callable.methods[*other].codomain.is_some())
            .collect::<Vec<_>>();
        if explicit.is_empty() {
            selected.push(*candidate);
        } else {
            for explicit in explicit {
                if !selected
                    .iter()
                    .any(|selected| callable.methods[*selected] == callable.methods[explicit])
                {
                    selected.push(explicit);
                }
            }
        }
    }
    selected
}

fn minimal_candidates<T: Copy>(
    mut candidates: Vec<T>,
    strictly_smaller: impl Fn(T, T) -> bool,
) -> Vec<T> {
    let originals = candidates.clone();
    candidates.retain(|candidate| {
        !originals
            .iter()
            .copied()
            .any(|other| strictly_smaller(other, *candidate))
    });
    candidates
}

const MAX_DISPATCH_PRODUCTS: usize = 256;

fn dispatch_identity_matches(
    actual: &DispatchIdentity,
    expected: &DispatchIdentity,
    order: &(impl TypeStore + ?Sized),
) -> bool {
    actual == expected
        || matches!((actual, expected),
            (DispatchIdentity::Type(actual), DispatchIdentity::Type(expected))
                if order.is_subtype_id(actual, expected))
}

fn dispatch_intersection_witness(
    actual: &DispatchRange,
    expected: &DispatchIdentity,
    order: &(impl TypeStore + ?Sized),
) -> Option<DispatchIdentity> {
    match (actual, expected) {
        (DispatchRange::Exact(actual), expected) => {
            dispatch_identity_matches(actual, expected, order).then(|| actual.clone())
        }
        (DispatchRange::Upward(actual), DispatchIdentity::Type(expected))
            if order.is_subtype_id(expected, actual) =>
        {
            Some(DispatchIdentity::Type(expected.clone()))
        }
        (DispatchRange::Upward(actual), DispatchIdentity::Type(expected))
            if order.is_subtype_id(actual, expected) =>
        {
            Some(DispatchIdentity::Type(actual.clone()))
        }
        (DispatchRange::Upward(_), DispatchIdentity::Object(_)) => None,
        (DispatchRange::Upward(_), DispatchIdentity::Type(_)) => None,
    }
}

fn dispatch_domain_matches(
    expected: &[DispatchIdentity],
    actual: &[DispatchIdentity],
    order: &(impl TypeStore + ?Sized),
) -> bool {
    expected.len() == actual.len()
        && expected
            .iter()
            .zip(actual)
            .all(|(expected, actual)| dispatch_identity_matches(actual, expected, order))
}

fn dispatch_domain_strictly_smaller(
    smaller: &[DispatchIdentity],
    bigger: &[DispatchIdentity],
    order: &(impl TypeStore + ?Sized),
) -> bool {
    if smaller.len() != bigger.len() || smaller == bigger {
        return false;
    }
    smaller
        .iter()
        .zip(bigger)
        .all(|(smaller, bigger)| dispatch_identity_matches(smaller, bigger, order))
}
