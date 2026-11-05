//! Transient static type inference for a completed semantic snapshot.
//!
//! A checker borrows the source analysis and its positioned knowledge provider.
//! Typed sessions own expression caches keyed by borrowed AST identity and scope;
//! inferred results are not
//! written back into the syntax tree. Local bindings, explicit domain/codomain
//! annotations, and external method tables contribute exact or upward-closed
//! result ranges. Unknown evidence remains distinct from a proven mismatch.
//!
//! Dispatch selection bounds the Cartesian products of union-typed arguments,
//! filters compatible signatures, and prefers strictly more specific domains
//! before combining candidate codomains.

use crate::builtin_index::CallableInfo;
use crate::node_metadata::SyntaxSnapshot;
use std::ops::Deref;

use m2_syn::nodes::{Expr, ExprCollection, ExprOpBinAdj};

use super::inference::session::{ExpressionTypes, InferenceSession};
use super::inference::InferType;
use super::*;
use crate::object_registry::TypeStore;
use crate::source::{SyntaxNavigation, SyntaxSource};

/// Shared callable and type knowledge for typed sessions and diagnostic queries.
pub struct TypeChecker<'analysis, 'knowledge, Knowledge: ?Sized> {
    /// Semantic facts for the current document snapshot.
    analysis: &'analysis Analysis,
    /// Object and type knowledge queried at each source position.
    knowledge_provider: &'knowledge Knowledge,
}

/// The class or singleton object a method-domain slot denotes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DispatchIdentity {
    /// A runtime class identity or its catalog data.
    Type(TypeId),
    /// A singleton runtime object identity.
    Object(ObjectId),
}

/// An exact dispatch identity or the types derived from a domain class.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DispatchRange {
    /// A single known identity, value, or type point.
    Exact(DispatchIdentity),
    /// A runtime class together with its possible subtypes.
    Upward(TypeId),
}

/// A candidate signature with its domain names resolved to object identities.
struct ResolvedDispatchDomain {
    /// Position of this candidate in the callable's signature list.
    signature_index: usize,
    /// Resolved identities for the method-domain arguments.
    slots: Vec<DispatchIdentity>,
}

impl<'analysis, 'knowledge, Knowledge: ?Sized> TypeChecker<'analysis, 'knowledge, Knowledge> {
    /// Borrows completed analysis and positioned knowledge; typed sessions own their caches.
    pub fn new(analysis: &'analysis Analysis, knowledge_provider: &'knowledge Knowledge) -> Self {
        Self {
            analysis,
            knowledge_provider,
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
    /// The inferred type of every expression of `syntax`, each after the
    /// operands it depends on.
    pub fn expression_types<'ast>(
        &self,
        syntax: &'ast SyntaxSnapshot,
        source: &(impl SourceNavigation + ?Sized),
        knowledge_provider: &(impl PositionedTypeKnowledge + ?Sized),
    ) -> Vec<(&'ast Expr, InferredType)> {
        let checker = TypeChecker::new(self, knowledge_provider);
        let source = SyntaxNavigation::new(source.source(), syntax);
        let session = InferenceSession::new(&checker, &source, syntax);
        ExpressionTypes::collect(&session, syntax)
    }

    /// Whether the class has a known proper subtype at this source position.
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
                self.object_relations(knowledge)
                    .has_strict_subtype_id(&type_id)
            })
    }
}

impl<Knowledge: PositionedTypeKnowledge + ?Sized> TypeChecker<'_, '_, Knowledge> {
    /// Object and type knowledge visible at one source position.
    pub fn knowledge_at(&self, position: Position) -> Knowledge::Knowledge<'_> {
        self.knowledge_provider.at_position(position)
    }

    /// Result range determined by positioned external object facts for this expression.
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

    /// Resolves dispatch identity at against the source position and available object knowledge.
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

    /// Dispatch products derived from the available type evidence.
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

    /// Signature indices compatible with the known argument identities and domains.
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
        let order = self.object_relations(knowledge);
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

    /// Catalog signatures compatible with the call's argument types, separated from excluded candidates.
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

    /// Infers typed syntax in one lexical scope of the revision `source` maps.
    pub fn infer_type<'ast, T: InferType + ?Sized>(
        &self,
        syntax: &'ast T,
        source: &'ast (impl SyntaxSource + ?Sized),
        scope_idx: usize,
    ) -> InferredType {
        let session = InferenceSession::new(self, source, source.syntax());
        syntax.infer(&session.context(scope_idx))
    }

    /// Forms the union of result possibilities contributed by the input expressions.
    pub fn join_types(
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

    /// Whether `name` resolves to a function tracked in the local registry — a
    /// lambda binding or a local method declaration. Such a head is known to be a
    /// function without consulting external type facts.
    pub fn local_function_at(
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

    /// Dispatch codomain after resolving the available source and catalog identities.
    pub fn resolved_dispatch_codomain(
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

    /// Resolves external call return type against the source position and available object knowledge.
    pub fn resolve_external_call_return_type(
        &self,
        callable: &ObjectName,
        argument_types: &[InferredType],
        _options: &[LiteralOption],
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<InferredType> {
        let record = knowledge.get_record(callable)?;
        let callable_info = record.callable()?;
        if knowledge.has_type_role(&record.class, TypeRole::MethodFunctionBinary)
            && argument_types.len() > 2
        {
            // The catalog includes the generic Sequence wrapper without a
            // codomain. An annotated Sequence method overrides its binary fold.
            let sequence = [InferredType::exact("Sequence")];
            let signatures = self.external_dispatch_domains(callable_info, knowledge);
            let candidates =
                self.dispatch_candidate_indices(&signatures, &sequence, position, knowledge);
            let candidates = effective_external_candidate_indices(callable_info, &candidates);
            if candidates
                .iter()
                .any(|index| callable_info.methods[*index].codomain.is_some())
            {
                return Some(self.external_candidates_return_type(
                    callable_info,
                    &candidates,
                    position,
                    knowledge,
                ));
            }
            let mut result = argument_types[0].clone();
            for argument in &argument_types[1..] {
                result = self.resolve_external_call_return_type(
                    callable,
                    &[result, argument.clone()],
                    _options,
                    position,
                    knowledge,
                )?;
            }
            return Some(result);
        }
        let callable = callable_info;
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

    /// Dispatch domains supplied by the external object catalog.
    fn external_dispatch_domains(
        &self,
        callable: &CallableInfo,
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

    /// Candidates return type supplied by the external object catalog.
    fn external_candidates_return_type(
        &self,
        callable: &CallableInfo,
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

    /// Argument types and literal options of a call.
    pub fn call_facts<'ast>(
        &self,
        call: &'ast ExprOpBinAdj,
        source: &'ast (impl SyntaxSource + ?Sized),
        scope_idx: usize,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> CallStaticFacts {
        self.argument_facts(
            &call.right_operand,
            source,
            scope_idx,
            call.left_operand.symbol_name(),
            knowledge,
        )
    }

    /// Argument types and literal options of `arguments`, passed to `callable`.
    fn argument_facts<'ast>(
        &self,
        arguments: &'ast Expr,
        source: &'ast (impl SyntaxSource + ?Sized),
        scope_idx: usize,
        callable: Option<&str>,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> CallStaticFacts {
        let session = InferenceSession::new(self, source, source.syntax());
        session
            .context(scope_idx)
            .call_facts(arguments, callable, knowledge)
    }

    /// Reports only calls whose known domains are disjoint from every possible
    /// argument type. Missing type or signature facts leave the call unchecked.
    pub fn disjoint_call_argument_types(
        &self,
        call: &ExprOpBinAdj,
        source: &(impl SyntaxSource + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<Vec<InferredType>> {
        let name = call.left_operand.symbol_name()?;
        let position = source.position_for_syntax(call)?;
        let scope = self.find_scope_at(position).unwrap_or(0);
        let function = self.local_function_at(name, scope, position, knowledge);
        let indexed_name = ObjectName::new(name);
        let record = if let Some(function) = function {
            if !function.is_method_function() {
                return None;
            }
            function
                .indexed_callable
                .as_ref()
                .and_then(|name| knowledge.get_record(name))
        } else {
            if self
                .visible_source_binding_from_scope(name, scope, position, knowledge)
                .is_some()
            {
                return None;
            }
            knowledge.get_record(&indexed_name)
        };
        if function.is_none()
            && !record
                .and_then(|record| record.callable())
                .is_some_and(|callable| callable.is_method_function())
        {
            return None;
        }
        let facts = self.call_facts(call, source, scope, knowledge);
        let binary = function
            .is_some_and(|function| function.runtime_class == Some(TypeRole::MethodFunctionBinary))
            || record.is_some_and(|record| {
                knowledge.has_type_role(&record.class, TypeRole::MethodFunctionBinary)
            });
        if binary && facts.argument_types.len() > 2 {
            let sequence = [InferredType::exact("Sequence")];
            if function.is_some_and(|function| {
                !self
                    .local_call_candidate_installations(function, &sequence, position, knowledge)
                    .is_empty()
            }) {
                return None;
            }
            if let Some(callable) = record.and_then(|record| record.callable()) {
                let signatures = self.external_dispatch_domains(callable, knowledge);
                if self
                    .dispatch_candidate_indices(&signatures, &sequence, position, knowledge)
                    .iter()
                    .any(|index| callable.methods[*index].codomain.is_some())
                {
                    return None;
                }
            }
            let mut result = facts.argument_types[0].clone();
            for argument in &facts.argument_types[1..] {
                let pair = [result, argument.clone()];
                if self.method_domain_is_disjoint(function, record, &pair, position, knowledge) {
                    return Some(pair.to_vec());
                }
                result = if let Some(function) = function {
                    self.resolve_local_call_return_type(function, &pair, position, knowledge)
                } else {
                    self.resolve_external_call_return_type(
                        &indexed_name,
                        &pair,
                        &facts.literal_options,
                        position,
                        knowledge,
                    )
                }
                .unwrap_or_else(InferredType::unknown);
            }
            return None;
        }
        self.method_domain_is_disjoint(function, record, &facts.argument_types, position, knowledge)
            .then_some(facts.argument_types)
    }

    /// The options `node`, a call `f args`, passes that `f` does not accept
    /// as given. Only calls of a function whose complete option list is known
    /// are checked: one declaring a literal option list in source, or a
    /// catalog function recording its option table, directly or for the one
    /// installation the call reaches.
    pub fn call_option_issues<'ast>(
        &self,
        call: &'ast ExprOpBinAdj,
        source: &(impl SyntaxSource + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Vec<CallOptionIssue<'ast>> {
        let Some(name) = call.left_operand.symbol_name() else {
            return Vec::new();
        };
        let arguments = call
            .right_operand
            .unparenthesized()
            .unwrap_or(&call.right_operand);
        let passed = match arguments {
            Expr::Collection(ExprCollection::Sequence(_)) => {
                arguments.collection_elements().unwrap_or_default()
            }
            _ => vec![Some(arguments)],
        };
        let passed = passed
            .into_iter()
            .flatten()
            .filter_map(|argument| match argument {
                Expr::Option(option) if matches!(option.key.as_ref(), Expr::Symbol(_)) => {
                    Some(option)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let Some(position) = source
            .position_for_syntax(call)
            .filter(|_| !passed.is_empty())
        else {
            return Vec::new();
        };
        let scope = self.find_scope_at(position).unwrap_or(0);
        let Some(accepted) =
            self.accepted_call_options(name, call, scope, position, source, knowledge)
        else {
            return Vec::new();
        };
        let mut issues = Vec::new();
        for option in passed {
            let key = option.key.as_ref();
            let key_name = key.symbol_name().unwrap_or_default();
            let Some(spec) = accepted.iter().find(|spec| spec.name.name() == key_name) else {
                issues.push(CallOptionIssue::UnknownName {
                    key,
                    suggestion: Self::closest_option_name(key_name, &accepted),
                    accepted: accepted.iter().map(|spec| spec.name.clone()).collect(),
                });
                continue;
            };
            if let Some(issue) =
                self.option_value_issue(spec, &option.value, scope, source, knowledge)
            {
                issues.push(issue);
            }
        }
        issues
    }

    /// The complete option list of the function `name` names at a call with
    /// `arguments`, or `None` when it is not known.
    fn accepted_call_options(
        &self,
        name: &str,
        call: &ExprOpBinAdj,
        scope: usize,
        position: Position,
        source: &(impl SyntaxSource + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<Vec<CallableOption>> {
        let callable =
            if let Some(function) = self.local_function_at(name, scope, position, knowledge) {
                if let Some(options) = &function.declared_options {
                    return Some(options.clone());
                }
                function.indexed_callable.clone()?
            } else {
                if self
                    .visible_source_binding_from_scope(name, scope, position, knowledge)
                    .is_some()
                {
                    return None;
                }
                ObjectName::new(name)
            };
        let info = knowledge.get_record(&callable)?.callable()?;
        match info.option_passing? {
            OptionPassing::Table => Some(info.options.clone()),
            OptionPassing::Forwarded => {
                let facts = self.call_facts(call, source, scope, knowledge);
                self.external_call_signature_facts(&callable, &facts, position, knowledge)?
                    .pinned?
                    .options
            }
            _ => None,
        }
    }

    /// A value `spec` does not accept: one whose type is disproven for every
    /// accepted type, or an unbound symbol outside its possible values.
    fn option_value_issue<'ast>(
        &self,
        spec: &CallableOption,
        value: &'ast Expr,
        scope: usize,
        source: &(impl SyntaxSource + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<CallOptionIssue<'ast>> {
        let position = source.position_for_syntax(value)?;
        if let Some(symbol) = value
            .symbol_name()
            .filter(|_| !spec.possible_values.is_empty())
        {
            let evaluates_to_itself = self
                .visible_source_binding_from_scope(symbol, scope, position, knowledge)
                .is_none()
                && knowledge
                    .get_record(&ObjectName::new(symbol))
                    .is_none_or(|record| record.class == TypeRole::Symbol.object_name());
            if evaluates_to_itself {
                return (!spec
                    .possible_values
                    .iter()
                    .any(|possible| possible.name() == symbol))
                .then(|| CallOptionIssue::ValueNotPossible {
                    value,
                    option: spec.name.clone(),
                    possible: spec.possible_values.clone(),
                });
            }
        }
        if spec.accepted_types.is_empty() {
            return None;
        }
        let actual = self.infer_type(value, source, scope);
        let disproven = spec.accepted_types.iter().all(|accepted| {
            actual.possibility_by(accepted, |candidate, bound| {
                self.subtype_evidence(candidate, bound, position, knowledge)
            }) == SubtypeEvidence::Disproven
        });
        disproven.then(|| CallOptionIssue::ValueType {
            value,
            option: spec.name.clone(),
            actual,
            accepted: spec.accepted_types.clone(),
        })
    }

    /// The accepted option name a misspelled `key` most likely means: the one
    /// with the fewest single-character edits, ignoring case, provided they
    /// change at most a third of the key.
    fn closest_option_name(key: &str, accepted: &[CallableOption]) -> Option<ObjectName> {
        let key = key.to_lowercase().chars().collect::<Vec<_>>();
        let edits = |name: &str| {
            let name = name.to_lowercase().chars().collect::<Vec<_>>();
            let mut previous = (0..=name.len()).collect::<Vec<_>>();
            for (row, key_char) in key.iter().enumerate() {
                let mut current = vec![row + 1];
                for (column, name_char) in name.iter().enumerate() {
                    let substitution = previous[column] + usize::from(key_char != name_char);
                    current.push(
                        substitution
                            .min(previous[column + 1] + 1)
                            .min(current[column] + 1),
                    );
                }
                previous = current;
            }
            previous[name.len()]
        };
        accepted
            .iter()
            .map(|option| (edits(option.name.name()), &option.name))
            .filter(|(distance, _)| *distance <= (key.len() / 3).max(1))
            .min_by_key(|(distance, _)| *distance)
            .map(|(_, name)| name.clone())
    }

    /// Domain is disjoint associated with the recognized method installation or signature.
    fn method_domain_is_disjoint(
        &self,
        function: Option<&FunctionInfo>,
        record: Option<&crate::builtin_index::Record>,
        arguments: &[InferredType],
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> bool {
        let Some(products) = self.inferred_dispatch_products(arguments, position, knowledge) else {
            return false;
        };
        if products.is_empty() {
            return false;
        }
        let mut has_domains = false;
        if let Some(function) = function {
            let installations = self
                .analysis
                .method_installations_for_at(function, position);
            has_domains |= !installations.is_empty();
            if installations.iter().any(|installation| {
                installation
                    .method
                    .domain
                    .iter()
                    .any(|name| self.resolve_type_id_at(name, position, knowledge).is_none())
            }) || !self
                .local_call_candidate_installations(function, arguments, position, knowledge)
                .is_empty()
            {
                return false;
            }
        }
        if let Some(callable) = record.and_then(|record| record.callable()) {
            has_domains |= !callable.methods.is_empty();
            let signatures = self.external_dispatch_domains(callable, knowledge);
            // Dispatch by an object rather than its class requires value facts
            // that a nominal argument type alone cannot rule out.
            if signatures.iter().any(|signature| {
                signature.slots.len() == arguments.len()
                    && signature
                        .slots
                        .iter()
                        .any(|slot| matches!(slot, DispatchIdentity::Object(_)))
            }) || !self
                .dispatch_candidate_indices(&signatures, arguments, position, knowledge)
                .is_empty()
            {
                return false;
            }
        }
        has_domains
    }

    /// Whether the callable receives the argument pack as one sequence value.
    pub fn callable_receives_sequence(
        &self,
        name: &str,
        scope_idx: usize,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> bool {
        if let Some(function) = self.local_function_at(name, scope_idx, position, knowledge) {
            return function.dispatch == Some(Dispatch::Variadic)
                || function.runtime_class == Some(TypeRole::MethodFunctionSingle);
        }

        knowledge
            .get_record(&ObjectName::new(name))
            .and_then(|record| record.callable())
            .is_some_and(|callable| callable.receives_sequence)
    }

    /// Resolves local call return type against the source position and available object knowledge.
    pub fn resolve_local_call_return_type(
        &self,
        function: &FunctionInfo,
        argument_types: &[InferredType],
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<InferredType> {
        if function.runtime_class == Some(TypeRole::MethodFunctionBinary)
            && argument_types.len() > 2
        {
            let sequence = [InferredType::exact("Sequence")];
            if !self
                .local_call_candidate_installations(function, &sequence, position, knowledge)
                .is_empty()
            {
                return self
                    .resolve_local_call_return_type(function, &sequence, position, knowledge);
            }
            if let Some(indexed_callable) = &function.indexed_callable {
                // Catalog Sequence specializations take precedence over the
                // generic fold when no source specialization replaces them.
                if function.installations.is_empty() {
                    return self.resolve_external_call_return_type(
                        indexed_callable,
                        argument_types,
                        &[],
                        position,
                        knowledge,
                    );
                }
            }
            let mut result = argument_types[0].clone();
            for argument in &argument_types[1..] {
                result = self.resolve_local_call_return_type(
                    function,
                    &[result, argument.clone()],
                    position,
                    knowledge,
                )?;
            }
            return Some(result);
        }
        let candidates =
            self.local_call_candidate_installations(function, argument_types, position, knowledge);
        if candidates.is_empty() {
            if let Some(callable) = &function.indexed_callable {
                return self.resolve_external_call_return_type(
                    callable,
                    argument_types,
                    &[],
                    position,
                    knowledge,
                );
            }
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

    /// Identities of source methods compatible with the inferred argument tuple.
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

    /// Source methods compatible with the inferred argument tuple and current visibility.
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

    /// Whether positioned source and external type facts prove the subtype relationship.
    pub fn is_subtype(
        &self,
        actual: &ObjectName,
        expected: &ObjectName,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> bool {
        self.subtype_evidence(actual, expected, position, knowledge) == SubtypeEvidence::Proven
    }

    /// Available proof that the child type belongs to the parent's subtype family.
    pub fn subtype_evidence(
        &self,
        actual: &ObjectName,
        expected: &ObjectName,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> SubtypeEvidence {
        let order = self.object_relations(knowledge);
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

    /// Resolves source type id against the source position and available object knowledge.
    fn resolve_source_type_id(&self, name: &ObjectName, position: Position) -> Option<TypeId> {
        self.get_binding_at(name.name(), position)?
            .state
            .source_type
            .clone()
    }

    /// Resolves type id at against the source position and available object knowledge.
    pub fn resolve_type_id_at(
        &self,
        name: &ObjectName,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<TypeId> {
        self.resolve_source_type_id(name, position)
            .or_else(|| knowledge.resolve_type_id(name))
    }

    /// Resolves type at against the source position and available object knowledge.
    pub fn resolve_type_at(
        &self,
        name: &ObjectName,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<Type> {
        let type_id = self.resolve_type_id_at(name, position, knowledge)?;
        Type::from_id(name.clone(), type_id, &self.object_relations(knowledge))
    }
}
/// External candidate indices remaining after applying source visibility and dispatch specificity.
fn effective_external_candidate_indices(
    callable: &CallableInfo,
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

/// Removes candidates whose dispatch domains strictly contain another applicable domain.
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

/// Upper bound on Cartesian products explored while resolving union-typed dispatch arguments.
const MAX_DISPATCH_PRODUCTS: usize = 256;

/// Whether a class or singleton argument identity satisfies a domain slot.
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

/// Finds evidence that two dispatch domains can accept a common argument tuple.
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

/// Whether all argument identities satisfy the corresponding dispatch-domain slots.
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

/// Whether one dispatch domain is strictly more specific than another.
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
