//! Binding enrichment: the second analysis pass.
//!
//! Once the first pass has bound every name, this pass reads the values bound
//! to them. Each assignment contributes the type of its value, the callable
//! it creates, the method installation it makes, and the ring generators its
//! ring declares; each lambda types its parameters from the installation
//! receiving its arguments. The pass follows generated fields from each
//! assignment, lambda, and call it visits.

use m2_syn::nodes::{Expr, ExprAssign, ExprLambda, ExprOp, ExprOpBin, ExprOpBinAdj, Symbol};
use m2_syn::visit::{self, Visit};

use super::first_pass::BindingFact;
use super::installation::{InstalledFunction, MethodDeclaration};
use super::ring::RingSemantics;
use super::*;
use crate::source::SyntaxSource;
use crate::syntax::{
    AssignmentOperator, AssignmentParts, AssignmentSyntax, AssignmentTarget, BoundSymbols,
    ClauseSyntax, LambdaSyntax,
};

impl Analysis {
    /// Traverses typed syntax to enrich callable and binding type facts.
    pub(super) fn enrich_types(
        &mut self,
        source: &(impl SyntaxSource + ?Sized),
        knowledge_provider: &(impl PositionedTypeKnowledge + ?Sized),
    ) {
        source.syntax().visit(&mut TypeEnricher {
            analysis: self,
            source,
            knowledge_provider,
        });
    }

    /// Types a lambda's parameters by the domain of the installation
    /// receiving its arguments.
    fn enrich_lambda(&mut self, lambda: &ExprLambda, source: &(impl SourceNavigation + ?Sized)) {
        let parameter_types = self.argument_lambda_domain(lambda, source).cloned();
        let parameters = lambda.bound_symbols();
        let typed_parameters = parameter_types
            .as_deref()
            .filter(|types| types.len() == parameters.len());
        for (index, parameter) in parameters.into_iter().enumerate() {
            self.enrich_symbol(
                parameter,
                source,
                BindingEnrichment {
                    presentation_kind: SymbolKind::VARIABLE,
                    inferred_type: typed_parameters
                        .and_then(|types| types.get(index))
                        .cloned()
                        .map(InferredType::upward_from_id),
                    object_id: None,
                    indexed_element_type: None,
                    parent_type: None,
                },
            );
        }
    }

    /// Records what one assignment binds: the value's type and callable,
    /// the method it installs, and the ring generators its ring declares.
    fn enrich_assignment(
        &mut self,
        assignment: &ExprAssign,
        source: &(impl SyntaxSource + ?Sized),
        scope_idx: usize,
        knowledge_provider: &(impl PositionedTypeKnowledge + ?Sized),
    ) {
        let Some(position) = source.position_for_syntax(assignment) else {
            return;
        };
        let parts = assignment.parts();
        let value = parts.value;
        let knowledge = knowledge_provider.at_position(position);
        let installation_id = self.record_method_installation(assignment, source, &knowledge);
        let declaration = MethodDeclaration::of(value);
        let inferred_type = match declaration {
            Some(declaration) => {
                InferredType::exact_from_id(declaration.runtime_class().object_name())
            }
            None => TypeChecker::new(self, knowledge_provider).infer_type(value, source, scope_idx),
        };
        let type_name = inferred_type.single();
        let parent_type = source
            .position_for_syntax(value)
            .and_then(|position| self.declared_type_parent(value, type_name, position, &knowledge));
        let presentation_kind = if matches!(value, Expr::Lambda(_))
            || declaration.is_some()
            || type_name
                .is_some_and(|type_name| knowledge.has_type_role(type_name, TypeRole::Function))
        {
            SymbolKind::FUNCTION
        } else if type_name.is_some_and(|type_name| {
            parts.target.bytes(source).is_some_and(|target| {
                TypeChecker::new(self, &knowledge).is_subtype(
                    type_name,
                    &TypeRole::Type.object_name(),
                    source.position_for_byte(target.start),
                    &knowledge,
                )
            })
        }) {
            SymbolKind::CLASS
        } else {
            SymbolKind::VARIABLE
        };
        let assigned_symbol = match parts.target {
            AssignmentTarget::Symbol(symbol) => Some(symbol),
            _ => None,
        };
        let object_id = assigned_symbol
            .and_then(|_| self.callable_object_for_value(value, source, scope_idx, &knowledge));

        if !matches!(parts.target, AssignmentTarget::New(_))
            && matches!(
                parts.operator,
                AssignmentOperator::Local | AssignmentOperator::Global
            )
        {
            self.enrich_definitions(
                parts.target,
                source,
                BindingEnrichment {
                    presentation_kind,
                    inferred_type: Some(inferred_type.clone()),
                    object_id,
                    indexed_element_type: None,
                    parent_type,
                },
            );
        }

        if let Some(ring) = assigned_symbol {
            if RingSemantics::may_be_ring(&inferred_type, &knowledge) {
                self.collect_ring_generator_bindings(ring, value, scope_idx, source, &knowledge);
            }
        }

        if parts.operator == AssignmentOperator::Global {
            if let AssignmentTarget::Operator(ExprOp::Bin(ExprOpBin::Regular(target))) =
                parts.target
            {
                let value_type = type_name
                    .cloned()
                    .unwrap_or_else(|| TypeRole::Thing.object_name());
                self.register_indexed_assignment(target, value_type, source);
            }
        }

        self.record_assignment_fact(assignment, &parts, scope_idx, installation_id, source);
    }

    /// Records an assignment fact for later editor queries: installations,
    /// writes to indexed variables, and callables assigned through an
    /// operator expression.
    fn record_assignment_fact(
        &mut self,
        assignment: &ExprAssign,
        parts: &AssignmentParts<'_>,
        scope_idx: usize,
        installation_id: Option<MethodInstallationId>,
        source: &(impl SourceNavigation + ?Sized),
    ) {
        let kind = match (installation_id, parts.operator, parts.target) {
            (Some(id), _, _) => AssignmentFactKind::MethodInstallation(id),
            (
                None,
                AssignmentOperator::Global,
                AssignmentTarget::Operator(ExprOp::Bin(ExprOpBin::Regular(target))),
            ) if target.operator.spelling() == <m2_syn::Token![_]>::SPELLING => {
                AssignmentFactKind::IndexedVariable
            }
            (None, AssignmentOperator::Global, AssignmentTarget::Operator(ExprOp::Bin(_))) => {
                AssignmentFactKind::ScopedCallable
            }
            _ => return,
        };
        let (Some(span), Some(target)) = (
            source.range_for_syntax(assignment),
            parts.target.bytes(source),
        ) else {
            return;
        };
        self.registry.assignment_facts.push(AssignmentFact {
            label: source.text()[target.clone()].trim_end().to_string(),
            span,
            target_span: source.range_for_bytes(target),
            value_span: source.range_for_syntax(parts.value),
            scope_idx,
            kind,
        });
    }

    /// Enriches the symbols an assignment target binds. A structured target
    /// binds parts of the value, so the value's type and callable do not
    /// apply to them.
    fn enrich_definitions(
        &mut self,
        target: AssignmentTarget<'_>,
        source: &(impl SourceNavigation + ?Sized),
        enrichment: BindingEnrichment,
    ) {
        let structured_target = !matches!(target, AssignmentTarget::Symbol(_));
        for symbol in target.bound_symbols() {
            let enrichment = if structured_target {
                BindingEnrichment {
                    inferred_type: None,
                    object_id: None,
                    ..enrichment.clone()
                }
            } else {
                enrichment.clone()
            };
            self.enrich_symbol(symbol, source, enrichment);
        }
    }

    /// A `newPackage` name string binds its global symbol to the new package.
    fn enrich_package_declaration(
        &mut self,
        call: &ExprOpBinAdj,
        source: &(impl SourceNavigation + ?Sized),
    ) {
        let Some(binding_state) = BindingFact::package_name_bytes(call, source).and_then(|name| {
            self.registry
                .binding_states_by_range
                .get(&source_range_key(source.range_for_bytes(name)))
                .copied()
        }) else {
            return;
        };
        self.enrich_binding_state(
            binding_state.binding,
            binding_state.state,
            BindingEnrichment {
                presentation_kind: SymbolKind::NAMESPACE,
                inferred_type: Some(InferredType::exact_from_id(TypeRole::Package.object_name())),
                object_id: None,
                indexed_element_type: None,
                parent_type: None,
            },
        );
    }

    /// Enriches the binding state a symbol writes.
    fn enrich_symbol(
        &mut self,
        symbol: &Symbol,
        source: &(impl SourceNavigation + ?Sized),
        enrichment: BindingEnrichment,
    ) {
        let Some(binding_state) = source.range_for_syntax(symbol).and_then(|range| {
            self.registry
                .binding_states_by_range
                .get(&source_range_key(range))
                .copied()
        }) else {
            return;
        };
        self.enrich_binding_state(binding_state.binding, binding_state.state, enrichment);
    }

    /// Type parent explicitly provided by the source declaration.
    fn declared_type_parent(
        &self,
        value: &Expr,
        type_name: Option<&ObjectName>,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<TypeId> {
        if let Some(parent) = ring::RingSemantics::value_parent(type_name, knowledge) {
            return TypeChecker::new(self, knowledge)
                .resolve_type_id_at(&parent, position, knowledge);
        }
        let Expr::New(new) = value else {
            return None;
        };
        let type_name = type_name.filter(|type_name| {
            TypeChecker::new(self, knowledge).is_subtype(
                type_name,
                &TypeRole::Type.object_name(),
                position,
                knowledge,
            )
        })?;
        let parent = new
            .parent_class
            .as_ref()
            .and_then(|parent| parent.clause_value().symbol_name())
            .map(ObjectName::new)
            .unwrap_or_else(|| type_name.clone());
        TypeChecker::new(self, knowledge).resolve_type_id_at(&parent, position, knowledge)
    }

    /// Callable identity attached to a source value, recognizing aliases and function declarations.
    fn callable_object_for_value(
        &mut self,
        value: &Expr,
        source: &(impl SourceNavigation + ?Sized),
        scope_idx: usize,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<CallableObjectId> {
        let value = value.unparenthesized()?;
        if let Expr::Symbol(symbol) = value {
            return self.callable_object_for_symbol(symbol, source, scope_idx, knowledge);
        }
        let function = if let Some(declaration) = MethodDeclaration::of(value) {
            FunctionInfo {
                typical_value: declaration.typical_value(),
                installations: Vec::new(),
                dispatch: None,
                runtime_class: Some(declaration.runtime_class()),
                indexed_callable: None,
                kind: LocalFunctionKind::Method(declaration.options()),
                declared_options: declaration.declared_options(),
            }
        } else if let Expr::Lambda(lambda) = value {
            FunctionInfo {
                typical_value: None,
                installations: Vec::new(),
                runtime_class: None,
                indexed_callable: None,
                dispatch: Some(lambda.dispatch()),
                kind: LocalFunctionKind::Plain,
                declared_options: None,
            }
        } else if let Some(InstalledFunction::OptionDeclaring { declaration, .. }) =
            InstalledFunction::of(value)
        {
            FunctionInfo {
                typical_value: None,
                installations: Vec::new(),
                runtime_class: None,
                indexed_callable: None,
                dispatch: None,
                kind: LocalFunctionKind::Plain,
                declared_options: OptionDeclaration::declared_options(&declaration.left_operand),
            }
        } else {
            return None;
        };
        let id = CallableObjectId(self.registry.callable_objects.len());
        self.registry.callable_objects.push(function);
        Some(id)
    }

    /// Callable identity of a named value: the local binding's, or the
    /// catalog callable's, registered once per catalog object.
    fn callable_object_for_symbol(
        &mut self,
        symbol: &Symbol,
        source: &(impl SourceNavigation + ?Sized),
        scope_idx: usize,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<CallableObjectId> {
        if let Some(binding) = source
            .position_for_syntax(symbol)
            .and_then(|position| self.get_binding_from_scope(symbol.text(), scope_idx, position))
        {
            if binding.scope_idx != 0
                || !knowledge.shadows_source(&binding.name, binding.state.span.start)
            {
                return binding.state.object_id;
            }
        }

        let indexed_id = knowledge.resolve_object(&ObjectName::new(symbol.text()))?;
        if let Some(object_id) = self
            .registry
            .indexed_callable_objects
            .get(&indexed_id)
            .copied()
        {
            return Some(object_id);
        }
        let record = knowledge.object(&indexed_id)?;
        let callable = record.callable()?;
        let function = FunctionInfo {
            typical_value: callable
                .typical_value
                .as_ref()
                .and_then(|type_id| knowledge.type_name(type_id))
                .cloned(),
            installations: Vec::new(),
            dispatch: None,
            runtime_class: [
                TypeRole::MethodFunctionSingle,
                TypeRole::MethodFunctionBinary,
            ]
            .into_iter()
            .find(|role| knowledge.has_type_role(&record.class, *role)),
            indexed_callable: Some(record.name.clone()),
            kind: match callable.kind {
                CallableKind::MethodFunction => {
                    LocalFunctionKind::Method(Self::catalog_method_options(record, knowledge))
                }
                CallableKind::Function | CallableKind::Operator(_) => LocalFunctionKind::Plain,
            },
            declared_options: None,
        };
        let object_id = CallableObjectId(self.registry.callable_objects.len());
        self.registry.callable_objects.push(function);
        self.registry
            .indexed_callable_objects
            .insert(indexed_id, object_id);
        Some(object_id)
    }
}

/// AST visitor recording types and installations after binding collection.
struct TypeEnricher<'analysis, 'source, 'knowledge, Source: ?Sized, Knowledge: ?Sized> {
    /// Semantic facts for the current document snapshot.
    analysis: &'analysis mut Analysis,
    /// Source navigation over the visited syntax revision.
    source: &'source Source,
    /// Object and type knowledge queried at each source position.
    knowledge_provider: &'knowledge Knowledge,
}

impl<'ast, Source, Knowledge> Visit<'ast> for TypeEnricher<'_, '_, '_, Source, Knowledge>
where
    Source: SyntaxSource + ?Sized,
    Knowledge: PositionedTypeKnowledge + ?Sized,
{
    fn visit_expr_assign(&mut self, node: &'ast ExprAssign) {
        let scope_idx = self
            .source
            .position_for_syntax(node)
            .and_then(|position| self.analysis.find_scope_at(position))
            .unwrap_or(0);
        self.analysis
            .enrich_assignment(node, self.source, scope_idx, self.knowledge_provider);
        visit::visit_expr_assign(self, node);
    }

    fn visit_expr_lambda(&mut self, node: &'ast ExprLambda) {
        self.analysis.enrich_lambda(node, self.source);
        visit::visit_expr_lambda(self, node);
    }

    fn visit_expr_op_bin_adj(&mut self, call: &'ast ExprOpBinAdj) {
        if let Some(position) = self.source.position_for_syntax(call) {
            let knowledge = self.knowledge_provider.at_position(position);
            let _ = self
                .analysis
                .record_install_method_call(call, self.source, &knowledge);
            self.analysis.enrich_package_declaration(call, self.source);
            self.analysis
                .bind_anonymous_ring_generators(call, self.source, &knowledge);
        }
        visit::visit_expr_op_bin_adj(self, call);
    }
}
