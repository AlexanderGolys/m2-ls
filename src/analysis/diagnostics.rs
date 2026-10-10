//! Diagnostic detection over completed document analysis.

use super::*;
use m2_syn::nodes::{
    ExprFor as ForLoop, ExprIf as IfStatement, ExprLambda as LambdaExpression,
    ExprQuote as QuoteExpression, ExprTry as TryStatement, ExprWhile as WhileLoop, FloatLiteral,
    Symbol,
};
use m2_syn::Token;

/// Which operator a parallel assignment uses; it decides what an operator
/// target means (an installation under `:=`, an assignment under `=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParallelAssignment {
    /// `(...) := ...`
    Local,
    /// `(...) = ...`
    Global,
}

/// The inputs every per-node check reads: the node under inspection, the
/// analysis that findings are recorded into, and the source and type knowledge
/// positioned at that node.
struct NodeDiagnosticContext<'analysis, 'tree, 'source, Source: ?Sized, Knowledge: ?Sized> {
    analysis: &'analysis mut Analysis,
    node: M2Node<'tree>,
    source: &'source Source,
    knowledge: &'source Knowledge,
}

/// The inputs every per-installation check reads. Findings go to a separate
/// list because the installation is borrowed from the analysis itself.
struct InstallationDiagnosticContext<'analysis, 'source, Knowledge: ?Sized> {
    analysis: &'analysis Analysis,
    installation: &'analysis MethodInstallation,
    knowledge: &'source Knowledge,
    diagnostics: &'analysis mut Vec<M2Diagnostic>,
}

impl<
        Source: SourceNavigation + ?Sized,
        Knowledge: TypeKnowledge + PositionedTypeKnowledge + ?Sized,
    > NodeDiagnosticContext<'_, '_, '_, Source, Knowledge>
{
    /// Runs every check that inspects a single syntax node.
    fn run_checks(&mut self) {
        self.syntax_error();
        self.missing_node();
        self.ambiguous_float_member_access();
        self.multiple_assignment_targets();
        self.colon_equal_part_assignment();
        self.parallel_assignment();
        self.option_key_convention();
        self.redundant_control_parentheses();
        self.prefer_coalescence();
        self.simplifiable_expression();
        self.ring_variable_naming();
        self.install_needs_colon_equals();
        self.protect_argument();
        self.invalid_control_transfer();
        self.explicit_install_required();
        self.condition_type();
    }

    fn syntax_error(&mut self) {
        if self.node.is_error() && !self.node.is_recoverable_control_transfer_error() {
            self.analysis
                .diagnostics
                .push(DiagnosticKind::SyntaxError.at(
                    self.source.remainder_of_line_range(self.node.start_byte()),
                    "Syntax error",
                ));
        }
    }

    fn missing_node(&mut self) {
        if self.node.is_missing() {
            self.analysis
                .diagnostics
                .push(DiagnosticKind::MissingNode.at(
                    self.source.range_for_node(self.node),
                    format!("Missing: {}", self.node.syntax_label()),
                ));
        }
    }

    fn ambiguous_float_member_access(&mut self) {
        let Some(replacement) = ambiguous_float_member_access_rewrite(self.node) else {
            return;
        };
        self.analysis.diagnostics.push(DiagnosticKind::AmbiguousFloatMemberAccess.at(
            self.source.range_for_node(self.node),
            format!(
                "This is parsed as application to a float literal; use `{replacement}` for member access"
            ),
        ));
    }

    fn multiple_assignment_targets(&mut self) {
        if !self.node.is_assignment() {
            return;
        }
        let Some(left) = self.node.child_by_field_name("left") else {
            return;
        };
        let Some(operator) = self.node.child_by_field_name("operator") else {
            return;
        };
        let operator = operator.text();
        let assignment = if matches_token::<Token![:=]>(operator) {
            ParallelAssignment::Local
        } else if matches_token::<Token![=]>(operator) {
            ParallelAssignment::Global
        } else {
            return;
        };
        if self
            .analysis
            .installation_for(self.node, self.source)
            .is_none()
            && !self.parallel_targets_are_assignable(left, assignment)
        {
            self.analysis
                .diagnostics
                .push(DiagnosticKind::MultipleAssignmentTargets.at(
                    self.source.range_for_node(left),
                    format!(
                        "{operator} parallel assignment targets must be symbols, installation \
                         targets such as `T + T` or `f ZZ`, parts such as `x#i` (with `=`), or \
                         nested lists"
                    ),
                ));
        }
    }

    /// Whether every target of a parallel assignment is one M2 assigns
    /// component-wise: a symbol, a nested list of such targets, an `x <- y`
    /// target (grammatical; whether it installs anything is the `<-` check's
    /// concern), or an operator expression. Under `=` an operator expression is
    /// an ordinary assignment to that expression (`x + 1 = v`, `M_(0,0) = v`),
    /// always valid; under `:=` it must be an installation target, so a shape
    /// that installs nothing or a literal operand (`x + 1`) is rejected.
    fn parallel_targets_are_assignable(
        &self,
        targets: M2Node,
        assignment: ParallelAssignment,
    ) -> bool {
        if !targets.is_collection_expression() {
            return true;
        }
        targets.collection_elements().all(|target| {
            target.is::<Symbol>()
                || target.has_binary_operator::<Token![<-]>()
                || (target.is_collection_expression()
                    && self.parallel_targets_are_assignable(target, assignment))
                || (target.is_operator_expression()
                    && (assignment == ParallelAssignment::Global
                        || self.is_installation_target(target)))
        })
    }

    /// Whether `target` can receive a `:=` installation: it has an installation
    /// shape and no operand is a literal, which can never be a type.
    fn is_installation_target(&self, target: M2Node) -> bool {
        let position = self.source.position_for_node(target);
        self.analysis
            .installation_shape(target, position, self.knowledge)
            .is_some_and(|(_, operands)| operands.iter().all(|operand| !operand.is_literal()))
    }

    fn colon_equal_part_assignment(&mut self) {
        if !self.node.has_binary_operator::<Token![:=]>() {
            return;
        }
        let Some(left) = self.node.child_by_field_name("left") else {
            return;
        };
        if left.has_binary_operator::<Token![#]>() {
            self.analysis
                .diagnostics
                .push(DiagnosticKind::ColonEqualPartAssignment.at(
                    self.source.range_for_node(left),
                    "`:=` cannot assign to parts; use `=` for part assignment",
                ));
        }
    }

    fn option_key_convention(&mut self) {
        self.analysis
            .diagnose_option_key_convention(self.node, self.source);
    }

    fn redundant_control_parentheses(&mut self) {
        let Some(inner) = redundant_control_parentheses_inner(self.node) else {
            return;
        };
        self.analysis
            .diagnostics
            .push(DiagnosticKind::RedundantControlParentheses.at(
                self.source.range_for_node(self.node),
                format!(
                    "Parentheses around this control expression are redundant; use `{}`",
                    inner.text()
                ),
            ));
    }

    fn prefer_coalescence(&mut self) {
        let Some(replacement) = simplification_of(self.node, coalescence_rewrite(self.node)) else {
            return;
        };
        self.analysis
            .diagnostics
            .push(DiagnosticKind::PreferCoalescence.at(
                self.source.range_for_node(self.node),
                format!("This conditional can be simplified to `{replacement}`"),
            ));
    }

    fn simplifiable_expression(&mut self) {
        let rewrites = if self.node.is::<IfStatement>() {
            [
                if_null_branch_rewrite(self.node),
                if_condition_rewrite(self.node),
                else_if_chain_rewrite(self.node),
            ]
        } else if self.node.is::<TryStatement>() {
            [try_statement_rewrite(self.node), None, None]
        } else {
            return;
        };
        let can_simplify = rewrites
            .into_iter()
            .any(|rewrite| simplification_of(self.node, rewrite).is_some());
        if can_simplify {
            self.analysis
                .diagnostics
                .push(DiagnosticKind::SimplifiableExpression.at(
                    self.source.range_for_node(self.node),
                    "This expression can be simplified",
                ));
        }
    }

    fn ring_variable_naming(&mut self) {
        let scope_idx = self
            .analysis
            .find_scope_at(self.source.position_for_node(self.node))
            .unwrap_or(0);
        let variables = self.analysis.ring_constructor_symbol_variables(
            self.node,
            self.source,
            self.knowledge,
            scope_idx,
        );
        for variable in variables {
            let Some(message) = ring_variable_naming_message(variable.text()) else {
                continue;
            };
            self.analysis.diagnostics.push(
                DiagnosticKind::RingVariableNaming
                    .at(self.source.range_for_node(variable), message),
            );
        }
    }

    fn install_needs_colon_equals(&mut self) {
        let position = self.source.position_for_node(self.node);
        let Some(name) =
            self.analysis
                .illegal_equals_install_head(self.node, position, self.knowledge)
        else {
            return;
        };
        self.analysis
            .diagnostics
            .push(DiagnosticKind::InstallNeedsColonEquals.at(
                self.source.range_for_node(self.node),
                format!(
                    "Installing a method on `{name}` must use `:=`, not `=`: M2 rejects this \
                 (\"no method for storing values of function {name}\"). Use `:=`."
                ),
            ));
    }

    fn protect_argument(&mut self) {
        self.analysis
            .diagnose_protect_argument(self.node, self.source, self.knowledge);
    }

    fn invalid_control_transfer(&mut self) {
        self.analysis
            .diagnose_control_transfer(self.node, self.source, self.knowledge);
    }

    fn explicit_install_required(&mut self) {
        let is_classical_left_arrow_install = if self.node.has_binary_operator::<Token![<-]>() {
            self.node
                .child_by_field_name("right")
                .filter(|right| right.has_binary_operator::<Token![:=]>())
                .and_then(|right| right.child_by_field_name("right"))
                .and_then(assigned_lambda)
                .is_some()
        } else {
            self.node.has_binary_operator::<Token![:=]>()
                && self
                    .node
                    .child_by_field_name("left")
                    .and_then(parenthesized_value)
                    .is_some_and(|left| left.has_binary_operator::<Token![<-]>())
                && self
                    .node
                    .child_by_field_name("right")
                    .and_then(assigned_lambda)
                    .is_some()
        };
        if is_classical_left_arrow_install {
            self.analysis.diagnostics.push(DiagnosticKind::ExplicitInstallRequired.at(
                self.source.range_for_node(self.node),
                "Methods for `<-` must be installed with `installMethod(symbol <-, Type, function)`",
            ));
        }
        let parallel_targets = self
            .node
            .is_assignment()
            .then(|| self.node.child_by_field_name("left"))
            .flatten()
            .filter(|left| left.is_collection_expression());
        for target in parallel_targets.map(left_arrow_targets).unwrap_or_default() {
            self.analysis
                .diagnostics
                .push(DiagnosticKind::ExplicitInstallRequired.at(
                    self.source.range_for_node(target),
                    "`x <- y` cannot be an assignment target: `<-` is overloaded only with \
                 `installMethod(symbol <-, Type, function)`",
                ));
        }
    }

    fn condition_type(&mut self) {
        self.analysis
            .diagnose_condition_type(self.node, self.source, self.knowledge);
    }

    fn parallel_assignment(&mut self) {
        if !self.node.is_assignment()
            || !(self.node.has_binary_operator::<Token![=]>()
                || self.node.has_binary_operator::<Token![:=]>())
            || self
                .analysis
                .installation_for(self.node, self.source)
                .is_some()
        {
            return;
        }
        let (Some(left), Some(right)) = (
            self.node.child_by_field_name("left"),
            self.node.child_by_field_name("right"),
        ) else {
            return;
        };
        self.analysis
            .validate_parallel_assignment(left, right, self.source, self.knowledge);
    }
}

impl<Knowledge: TypeKnowledge + ?Sized> InstallationDiagnosticContext<'_, '_, Knowledge> {
    /// Runs every check that inspects one method installation.
    fn run_checks(&mut self) {
        self.install_no_effect();
        self.operator_not_flexible();
        self.install_arity();
    }

    fn install_no_effect(&mut self) {
        let method = &self.installation.method;
        if let MethodHead::Operator(operator) = &method.head {
            if operator.form == OperatorForm::Binary
                && matches_token::<Token![??]>(operator.token.name())
                && self.installation.expected_rhs_arity() == method.domain.len()
            {
                self.diagnostics.push(DiagnosticKind::InstallNoEffect.at(
                    self.installation.span,
                    "Installing a binary `??` method has no effect: M2 records the method, but `x ?? y` never dispatches to it. Install the prefix form `?? X := x -> ...` to customize how `X` behaves on the left of `??`.",
                ));
                return;
            }
        }
        if let MethodHead::Operator(operator) = &method.head {
            if matches_token::<Token![<-]>(operator.token.name()) {
                if let Some(message) = self.left_arrow_install_without_effect() {
                    self.diagnostics
                        .push(DiagnosticKind::InstallNoEffect.at(self.installation.span, message));
                }
                return;
            }
        }
        let MethodHead::Function(name) = &method.head else {
            return;
        };
        if self.analysis.callable_head_kind(
            name.name(),
            self.installation.span.start,
            self.knowledge,
        ) != CallableHeadKind::PlainFunction
        {
            return;
        }
        self.diagnostics.push(DiagnosticKind::InstallNoEffect.at(
            self.installation.span,
            format!(
                "Installing a method on `{name}` has no effect: `{name}` is not a method \
                 function. Define it with `{name} = method()` to make method installations take effect."
            ),
        ));
    }

    /// Why a `<-` installation is never called, if it is not. M2 evaluates
    /// `x <- v` by assigning a symbol `x` directly and otherwise looking up the
    /// one-type method `symbol <-` in the class of `x`
    /// (`assigntofun` in M2's `d/evaluate.d`), so only a single non-symbol type
    /// can receive the call; `installMethod` still accepts any other domain.
    fn left_arrow_install_without_effect(&self) -> Option<String> {
        let domain = &self.installation.method.domain;
        let [target] = domain.as_slice() else {
            return Some(format!(
                "Installing a `<-` method for {} types has no effect: M2 dispatches `x <- v` on the \
                 class of `x` alone, so this method is stored but never called. Install it for one \
                 type with `installMethod(symbol <-, Type, (x, v) -> ...)`.",
                domain.len()
            ));
        };
        self.knowledge
            .has_type_role(target, TypeRole::Symbol)
            .then(|| {
                format!(
                    "Installing a `<-` method on `{target}` has no effect: a symbol on the left of \
                     `<-` is assigned directly, before any method lookup, so `<-` can only be \
                     overloaded for types that are not symbols."
                )
            })
    }

    fn operator_not_flexible(&mut self) {
        if self.installation.syntax == MethodInstallationSyntax::InstallMethod {
            return;
        }
        let MethodHead::Operator(operator) = &self.installation.method.head else {
            return;
        };
        if self
            .analysis
            .operator_form_is_flexible(operator, self.knowledge)
            != Some(false)
        {
            return;
        }
        self.diagnostics.push(DiagnosticKind::OperatorNotFlexible.at(
            self.installation.span,
            format!(
                "Cannot install a method on the {} operator `{}`: it is not flexible, so M2 rejects the assignment.",
                operator.form, operator.token
            ),
        ));
    }

    fn install_arity(&mut self) {
        let Some(Dispatch::Fixed(actual)) = self.installation.rhs_lambda_dispatch else {
            return;
        };
        let expected = self.installation.expected_rhs_arity();
        if actual == expected {
            return;
        }
        self.diagnostics.push(DiagnosticKind::InstallArity.at(
            self.installation.span,
            format!(
                "This method's function takes {actual} argument(s) but the installation expects \
                 {expected}. Match the domain arity or use a variadic `x -> …`."
            ),
        ));
    }
}

impl Analysis {
    /// Runs every diagnostic check over the document and records the findings:
    /// per-node checks in source order, then installation and codomain checks,
    /// then the document-wide unused-binding check.
    pub fn collect_diagnostics(
        &mut self,
        root: M2Node,
        syntax: Option<&SourceFile>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl PositionedTypeKnowledge + ?Sized),
    ) {
        let installations_enabled = knowledge.at_position(pos!()).is_available();
        let mut installation_diagnostics = Vec::new();
        if installations_enabled {
            for installation in &self.registry.installations {
                let knowledge = knowledge.at_position(installation.span.start);
                InstallationDiagnosticContext {
                    analysis: self,
                    installation,
                    knowledge: &knowledge,
                    diagnostics: &mut installation_diagnostics,
                }
                .run_checks();
            }
        }
        visit_source_nodes(root, syntax, |node| {
            self.diagnose_node(node, source, knowledge);
            if installations_enabled {
                self.diagnose_installation_codomain(
                    node,
                    source,
                    knowledge,
                    &mut installation_diagnostics,
                );
            }
        });
        self.diagnostics.extend(installation_diagnostics);
        self.diagnose_unused_bindings(root, source);
    }

    /// Reports a method installation whose codomain annotation inference can
    /// supply, or whose annotation contradicts the inferred result type.
    fn diagnose_installation_codomain(
        &self,
        node: M2Node,
        source: &(impl SourceNavigation + ?Sized),
        knowledge_provider: &(impl PositionedTypeKnowledge + ?Sized),
        out: &mut Vec<M2Diagnostic>,
    ) {
        if !node.is_assignment() {
            return;
        }
        let knowledge = knowledge_provider.at_position(source.position_for_node(node));
        let Some(deduction) = self.method_codomain_deduction(node, source, &knowledge) else {
            return;
        };
        let codomain = &deduction.codomain;
        let finding = match (&deduction.edit, &deduction.annotated_codomain) {
            (MethodCodomainEdit::Add(_), _) => DiagnosticKind::InstallCodomainMissing.at(
                deduction.diagnostic_range,
                format!(
                    "This method's lambda has the deducible codomain `{codomain}`. Add the codomain annotation."
                ),
            ),
            (MethodCodomainEdit::Replace, Some(annotated)) => DiagnosticKind::InstallCodomainMismatch.at(
                deduction.diagnostic_range,
                format!(
                    "This method's inferred result type `{codomain}` is incompatible with its annotated codomain `{annotated}`."
                ),
            ),
            (MethodCodomainEdit::Replace, None) => return,
        };
        out.push(finding);
    }

    fn illegal_equals_install_head(
        &self,
        node: M2Node,
        position: Position,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<String> {
        if !node.has_binary_operator::<Token![=]>() {
            return None;
        }
        let right = node.child_by_field_name("right")?;
        if !right.is::<LambdaExpression>() {
            return None;
        }
        let left = node.child_by_field_name("left")?;
        let (MethodHead::Function(name), _) = self.installation_shape(left, position, knowledge)?
        else {
            return None;
        };
        (self.callable_head_kind(name.name(), position, knowledge) != CallableHeadKind::Unknown)
            .then(|| name.name().to_string())
    }

    fn operator_form_is_flexible(
        &self,
        operator: &Operator,
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) -> Option<bool> {
        knowledge
            .get_record(&operator.token)?
            .operator_info()
            .map(|operator_info| operator_info.is_flexible(operator.form))
    }
}

impl Analysis {
    fn diagnose_node(
        &mut self,
        node: M2Node,
        source: &(impl SourceNavigation + ?Sized),
        knowledge_provider: &(impl PositionedTypeKnowledge + ?Sized),
    ) {
        let knowledge = knowledge_provider.at_position(source.position_for_node(node));
        NodeDiagnosticContext {
            analysis: self,
            node,
            source,
            knowledge: &knowledge,
        }
        .run_checks();
    }

    fn diagnose_condition_type(
        &mut self,
        node: M2Node,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) {
        let construct = if node.is::<IfStatement>() {
            "if"
        } else if node.is::<WhileLoop>() {
            "while"
        } else {
            return;
        };
        let Some(condition) = node.child_by_field_name("condition") else {
            return;
        };
        let position = source.position_for_node(condition);
        let scope_idx = self.find_scope_at(position).unwrap_or(0);
        let checker = TypeChecker::new(self, knowledge);
        let actual = checker.type_of(condition, source, scope_idx);
        if actual.possibility_by(&TypeRole::Boolean.object_name(), |candidate, bound| {
            checker.subtype_evidence(candidate, bound, position, knowledge)
        }) != SubtypeEvidence::Disproven
        {
            return;
        }
        self.diagnostics.push(DiagnosticKind::ConditionType.at(
            source.range_for_node(condition),
            format!(
                "{construct} condition must have type `Boolean`, but this expression has type `{}`",
                actual.label().unwrap_or_else(|| "unknown".to_string())
            ),
        ));
    }

    fn diagnose_control_transfer(
        &mut self,
        node: M2Node,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) {
        if !node.is_control_transfer() {
            return;
        }
        // The enclosing function or loop is what makes a transfer legal. Inside a
        // region the grammar could not parse that structure is unknown, so a
        // missing target says the parse failed, not that the transfer is
        // misplaced — and claiming otherwise reports valid code as an error.
        if node.ancestors().any(|ancestor| ancestor.is_error()) {
            return;
        }
        let target = self.control_transfer_target(node, source, knowledge);
        if target.is_some_and(|target| target.accepts(node)) {
            return;
        }

        let message = if node.is_return_expr() {
            "`return` can only be used inside a function body"
        } else if node.is_break_expr() {
            "`break` can only be used inside a loop body or an `apply`/`scan` callback"
        } else if node.control_transfer_value().is_some()
            && matches!(
                target,
                Some(ControlTransferTarget::DoLoop(_) | ControlTransferTarget::LoopCallback { .. })
            )
        {
            "`continue` with a value requires a `list` clause"
        } else {
            "`continue` can only be used inside a `list` or `do` loop body"
        };
        let keyword = node.child(0).unwrap_or(node);
        self.diagnostics.push(
            DiagnosticKind::InvalidControlTransfer.at(source.range_for_node(keyword), message),
        );
    }

    fn diagnose_protect_argument(
        &mut self,
        node: M2Node,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) {
        if !node.is_space_application() {
            return;
        }
        let (Some(callable), Some(argument)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            return;
        };
        if !callable.is::<Symbol>() || callable.text() != "protect" {
            return;
        }
        if self
            .binding_id_at(callable.text(), source.position_for_node(callable))
            .is_some()
        {
            return;
        }

        if argument.is::<QuoteExpression>() {
            return;
        }
        if argument.is::<Symbol>() {
            let name = argument.text();
            let position = source.position_for_node(argument);
            let has_source_binding = self.binding_id_at(name, position).is_some();
            let has_builtin_binding = knowledge.get_record(&ObjectName::new(name)).is_some();
            if has_source_binding || has_builtin_binding {
                self.diagnostics
                    .push(DiagnosticKind::ProtectAssignedSymbol.at(
                        source.range_for_node(argument),
                        format!(
                            "`protect {name}` evaluates the current value of `{name}`; \
                         use `protect symbol {name}` to protect the symbol itself"
                        ),
                    ));
            }
            return;
        }
        let inferred = self.infer_expression_static_type(argument, source, knowledge);
        if inferred
            .as_ref()
            .is_none_or(|type_id| type_id.name() == "Symbol")
        {
            self.diagnostics
                .push(DiagnosticKind::ProtectComputedSymbol.at(
                    source.range_for_node(argument),
                    "`protect` evaluates this expression to choose a Symbol at runtime; \
                 the protected symbol is not statically apparent",
                ));
        }
    }

    fn diagnose_option_key_convention(
        &mut self,
        node: M2Node,
        source: &(impl SourceNavigation + ?Sized),
    ) {
        if !node.has_binary_operator::<Token![=>]>() {
            return;
        }
        let Some(key) = node.child_by_field_name("left") else {
            return;
        };
        if !key.is::<Symbol>() {
            return;
        }
        let key_text = key.text();
        let starts_lowercase = key_text
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase());
        if !starts_lowercase || !is_function_option_context(node) {
            return;
        }
        self.diagnostics
            .push(DiagnosticKind::OptionKeyConvention.at(
                source.range_for_node(key),
                format!("Option key `{key_text}` should be capitalized by Macaulay2 convention"),
            ));
    }

    fn validate_parallel_assignment(
        &mut self,
        left: M2Node,
        right: M2Node,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl TypeKnowledge + ?Sized),
    ) {
        if !left.is_collection_expression() {
            return;
        }

        let target_nodes = left.collection_elements().collect::<Vec<_>>();
        if !right.is_collection_expression() {
            if target_nodes.len() < 2 || !knowledge.is_available() {
                return;
            }
            let mut value = right;
            while value.is_holder() {
                let Some(inner) = value.final_value_child() else {
                    break;
                };
                value = inner;
            }
            if value.is::<Symbol>() {
                let position = source.position_for_node(value);
                let has_source_binding = self
                    .visible_source_binding_at(value.text(), position, knowledge)
                    .is_some();
                let has_indexed_value = knowledge
                    .get_record(&ObjectName::new(value.text()))
                    .is_some();
                if !has_source_binding && !has_indexed_value {
                    return;
                }
            }
            let Some(right_type) = self.infer_expression_static_type(right, source, knowledge)
            else {
                return;
            };
            if right_type == TypeRole::Thing.object_name()
                || knowledge.has_type_role(&right_type, TypeRole::VisibleList)
            {
                return;
            }
            self.diagnostics.push(DiagnosticKind::ParallelAssignmentType.at(
                source.range_for_node(right),
                format!(
                    "parallel assignment binds {} targets, but the right-hand side has incompatible type `{}`",
                    target_nodes.len(),
                    right_type.name()
                ),
            ));
            return;
        }

        let value_nodes = right.collection_elements().collect::<Vec<_>>();
        if target_nodes.len() != value_nodes.len() {
            self.diagnostics.push(DiagnosticKind::ParallelAssignmentArity.at(
                source.range_for_node(left),
                format!(
                    "parallel assignment binds {} targets but the right-hand side lists {}; their lengths must match",
                    target_nodes.len(),
                    value_nodes.len()
                ),
            ));
            return;
        }

        for (target, value) in target_nodes.iter().zip(value_nodes.iter()) {
            self.validate_parallel_assignment(*target, *value, source, knowledge);
        }
    }

    fn diagnose_unused_bindings(
        &mut self,
        root: M2Node,
        source: &(impl SourceNavigation + ?Sized),
    ) {
        let mut used_bindings = HashSet::new();
        for node in root.symbols() {
            let name = node.text();
            let position = source.position_for_node(node);
            if let Some(binding_id) = self.binding_id_at(name, position) {
                if let Some(binding) = self.get_binding_at(name, position) {
                    let node_range = source.range_for_node(node);
                    if node_range != binding.range {
                        used_bindings.insert(binding_id);
                    }
                }
            }
        }

        let diagnostics = self
            .bindings()
            .filter(|binding| binding.role == BindingRole::Ordinary)
            .filter(|binding| !binding.potential_export)
            .filter(|binding| !used_bindings.contains(&binding.binding_id))
            .filter_map(|binding| {
                let name = binding.name.name();
                if name.starts_with('_') {
                    return None;
                }
                let noun = if binding.state.presentation_kind == SymbolKind::FUNCTION {
                    "function"
                } else {
                    "variable"
                };
                Some(
                    DiagnosticKind::UnusedBinding
                        .at(binding.range, format!("Unused {noun} {name}")),
                )
            })
            .collect::<Vec<_>>();
        self.diagnostics.extend(diagnostics);
    }
}

fn is_function_option_context(option: M2Node<'_>) -> bool {
    let mut current = option;
    while let Some(parent) = current.parent() {
        if parent.is::<Sequence>() {
            return true;
        }
        if parent.is::<List>() || parent.is::<Array>() || parent.is::<AngleBarList>() {
            return false;
        }
        current = parent;
    }
    false
}

pub fn redundant_control_parentheses_inner(node: M2Node<'_>) -> Option<M2Node<'_>> {
    if !node.is_holder() {
        return None;
    }
    let parent = node.parent()?;
    let is_field = |field| {
        parent
            .child_by_field_name(field)
            .is_some_and(|value| value.id() == node.id())
    };
    let is_control_expression = if parent.is::<IfStatement>() || parent.is::<WhileLoop>() {
        is_field("condition")
    } else if parent.is::<TryStatement>() {
        is_field("value")
    } else if parent.is_iteration_range() {
        ["iterated_collection", "range_start", "range_end"]
            .into_iter()
            .any(is_field)
    } else if parent.is::<ForLoop>() {
        is_field("filter")
    } else {
        false
    };
    is_control_expression
        .then(|| parenthesized_value(node))
        .flatten()
}

pub fn coalescence_rewrite(node: M2Node<'_>) -> Option<String> {
    if node.has_binary_operator::<Token![=]>() {
        let target = node.child_by_field_name("left")?;
        let conditional = node.child_by_field_name("right")?;
        if !target.is::<Symbol>() || !conditional.is::<IfStatement>() {
            return None;
        }
        let (subject, fallback) = coalescence_parts(conditional)?;
        return (target.text() == subject.text())
            .then(|| format!("{} ??= {}", target.text(), fallback.text()));
    }
    if !node.is::<IfStatement>() {
        return None;
    }
    if node
        .parent()
        .is_some_and(|parent| coalescence_rewrite(parent).is_some())
    {
        return None;
    }
    let (subject, fallback) = coalescence_parts(node)?;
    Some(format!("{} ?? {}", subject.text(), fallback.text()))
}

pub fn if_null_branch_rewrite(if_node: M2Node<'_>) -> Option<String> {
    let condition = if_node.child_by_field_name("condition")?;
    let then_branch = clause_of::<ThenClause>(if_node).and_then(clause_value)?;
    let else_branch = clause_of::<ElseClause>(if_node).and_then(clause_value)?;

    if is_null_value(else_branch) {
        return Some(format!(
            "if {} then {}",
            condition.text().trim_end(),
            then_branch.text()
        ));
    }

    if is_null_value(then_branch) && !is_null_value(else_branch) {
        return Some(format!(
            "if {} then {}",
            negated_condition_text(condition),
            else_branch.text(),
        ));
    }

    None
}

pub fn try_statement_rewrite(try_node: M2Node<'_>) -> Option<String> {
    let condition = try_node.child_by_field_name("value")?;
    let consequence = clause_of::<ThenClause>(try_node).and_then(clause_value);
    let else_clause = clause_of::<ElseClause>(try_node);
    let condition_text = condition.text();
    let consequence_text = consequence.map(|node| node.text());

    if consequence_text == Some(condition_text) && else_clause.is_none() {
        return Some(format!("try {condition_text}"));
    }

    if let Some(alternative) = else_clause.and_then(clause_value) {
        if is_null_value(alternative) {
            let mut simplified = format!("try {condition_text}");
            if let Some(consequence_text) = consequence_text {
                simplified.push_str(" then ");
                simplified.push_str(consequence_text);
            }
            return Some(simplified);
        }
    }

    None
}

pub fn if_condition_rewrite(if_node: M2Node<'_>) -> Option<String> {
    let condition = if_node.child_by_field_name("condition")?;
    let simplified = simplify_condition(condition)?;
    let then_branch = clause_of::<ThenClause>(if_node).and_then(clause_value)?;
    let else_clause = clause_of::<ElseClause>(if_node);

    let mut replacement = format!("if {} then {}", simplified, then_branch.text());
    if let Some(else_clause) = else_clause {
        replacement.push(' ');
        replacement.push_str(else_clause.text());
    }
    Some(replacement)
}

pub fn else_if_chain_rewrite(if_node: M2Node<'_>) -> Option<String> {
    flatten_then_if_chain(if_node).or_else(|| flatten_parenthesized_else_if_chain(if_node))
}

/// Moves a conditional nested in the `then` branch to the `else` position by
/// negating the condition. When the `else` branch is itself a conditional, the
/// swap only trades one nested branch for the other: the result is flattenable
/// back into the original, so the quick fix would oscillate between the two
/// forms without ever clearing its diagnostic.
fn flatten_then_if_chain(if_node: M2Node<'_>) -> Option<String> {
    let condition = if_node.child_by_field_name("condition")?;
    let then_branch = clause_of::<ThenClause>(if_node).and_then(clause_value)?;
    let nested_if = unwrap_parentheses(then_branch);
    if !nested_if.is::<IfStatement>() {
        return None;
    }
    let else_branch = clause_of::<ElseClause>(if_node).and_then(clause_value)?;
    if unwrap_parentheses(else_branch).is::<IfStatement>() {
        return None;
    }
    let nested_replacement =
        else_if_chain_rewrite(nested_if).unwrap_or_else(|| nested_if.text().to_string());

    Some(format!(
        "if {} then {} else {}",
        negated_condition_text(condition),
        else_branch.text(),
        nested_replacement
    ))
}

fn flatten_parenthesized_else_if_chain(if_node: M2Node<'_>) -> Option<String> {
    let else_branch = clause_of::<ElseClause>(if_node).and_then(clause_value)?;
    let nested_if = unwrap_parentheses(else_branch);
    if !nested_if.is::<IfStatement>() {
        return None;
    }

    let nested_replacement = else_if_chain_rewrite(nested_if);
    let removes_parentheses = nested_if.id() != else_branch.id();
    if !removes_parentheses && nested_replacement.is_none() {
        return None;
    }

    let replacement = nested_replacement.unwrap_or_else(|| nested_if.text().to_string());
    let start = else_branch.start_byte() - if_node.start_byte();
    let end = else_branch.end_byte() - if_node.start_byte();
    let mut flattened = if_node.text().to_string();
    flattened.replace_range(start..end, &replacement);
    Some(flattened)
}

/// The trailing run of digits in a name, with the name before it — `x0` splits
/// into `x` and `0`. `None` when the name has no trailing digits (`xx`) or is
/// only digits, neither of which names an index the author meant to write.
fn split_trailing_index(name: &str) -> Option<(&str, &str)> {
    let index_start = name
        .char_indices()
        .rev()
        .take_while(|(_, character)| character.is_ascii_digit())
        .last()
        .map(|(byte, _)| byte)?;
    let (base, index) = name.split_at(index_start);
    (!base.is_empty()).then_some((base, index))
}

/// Why a multi-character ring variable is worth flagging: `x0` and `x1` are two
/// unrelated symbols that merely look sequential, while `x_0` and `x_1` are one
/// indexed family — subscriptable, printable as `x`, and writable as the range
/// `x_0..x_9`. Single-character names need no such treatment.
fn ring_variable_naming_message(name: &str) -> Option<String> {
    if name.chars().count() <= 1 {
        return None;
    }
    Some(match split_trailing_index(name) {
        Some((base, index)) => format!(
            "Ring variable `{name}` is a multi-character symbol, not an indexed \
             variable; use `{base}_{index}` so the generators form one indexed \
             family (`{base}_0`, `{base}_1`, ...)"
        ),
        None => format!(
            "Ring variable `{name}` is a multi-character symbol; prefer a \
             single-character name, or an indexed variable such as `{}_0`",
            name.chars()
                .next()
                .expect("a multi-character name is non-empty")
        ),
    })
}

/// A rewrite counts as a simplification only when it actually changes the
/// expression. Proposing the original text back raises a diagnostic that its own
/// quick fix cannot clear, so the user is told to fix code that is already in its
/// simplest form and the warning survives applying the action.
fn simplification_of(node: M2Node<'_>, rewrite: Option<String>) -> Option<String> {
    rewrite.filter(|replacement| replacement.trim() != node.text().trim())
}

fn simplify_condition(node: M2Node<'_>) -> Option<String> {
    // A condition's span runs up to the `then` keyword, so it carries the
    // separating whitespace. Comparing against the untrimmed text would make
    // every `not name` look like a change and propose itself back.
    let original = node.text().trim();
    if !node.is_prefix_expr() {
        return None;
    }
    let operator = node.child_by_field_name("operator")?;
    if !matches_token::<Token![not]>(operator.text()) {
        return None;
    }
    let child = node
        .named_children()
        .find(|child| child.id() != operator.id())?;
    let inner = unwrap_parentheses(child);
    let simplified = negated_condition_text(inner);
    (simplified != original).then_some(simplified)
}

fn unwrap_parentheses(node: M2Node<'_>) -> M2Node<'_> {
    if node.is_holder() && node.child_count() == 3 {
        if let Some(inner) = node.child(1) {
            return inner;
        }
    }
    node
}

/// The negation of a condition, without the whitespace a condition's span
/// carries up to the `then` keyword; callers supply their own separators, so
/// keeping it would widen the gap before `then` on every rewrite.
fn negated_condition_text(node: M2Node<'_>) -> String {
    if node.is_prefix_expr() {
        if let Some(operator) = node.child_by_field_name("operator") {
            if matches_token::<Token![not]>(operator.text()) {
                if let Some(child) = node
                    .named_children()
                    .find(|child| child.id() != operator.id())
                {
                    return child.text().trim_end().to_string();
                }
            }
        }
    }

    if let Some(negated_operator) = node.binary_operator().and_then(negated_binary_operator) {
        if let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) {
            return format!(
                "{} {} {}",
                left.text(),
                negated_operator,
                right.text().trim_end()
            );
        }
    }

    let condition = node.text().trim_end();
    if node.is_binary_expr() {
        format!("not ({condition})")
    } else {
        format!("not {condition}")
    }
}

fn negated_binary_operator(operator: &str) -> Option<&'static str> {
    if matches_token::<Token![==]>(operator) {
        Some(token_spelling::<Token![!=]>())
    } else if matches_token::<Token![!=]>(operator) {
        Some(token_spelling::<Token![==]>())
    } else if matches_token::<Token![===]>(operator) {
        Some(token_spelling::<Token![=!=]>())
    } else if matches_token::<Token![=!=]>(operator) {
        Some(token_spelling::<Token![===]>())
    } else if matches_token::<Token![<]>(operator) {
        Some(token_spelling::<Token![>=]>())
    } else if matches_token::<Token![<=]>(operator) {
        Some(token_spelling::<Token![>]>())
    } else if matches_token::<Token![>]>(operator) {
        Some(token_spelling::<Token![<=]>())
    } else if matches_token::<Token![>=]>(operator) {
        Some(token_spelling::<Token![<]>())
    } else {
        None
    }
}

fn coalescence_parts(if_statement: M2Node<'_>) -> Option<(M2Node<'_>, M2Node<'_>)> {
    let condition = if_statement.child_by_field_name("condition")?;
    let operator = condition.binary_operator()?;
    let left = condition.child_by_field_name("left")?;
    let right = condition.child_by_field_name("right")?;
    let (subject, null_when_true) = match (is_null_value(left), is_null_value(right)) {
        (true, false) if matches_token::<Token![===]>(operator) => (right, true),
        (false, true) if matches_token::<Token![===]>(operator) => (left, true),
        (true, false) if matches_token::<Token![=!=]>(operator) => (right, false),
        (false, true) if matches_token::<Token![=!=]>(operator) => (left, false),
        _ => return None,
    };
    if !subject.is::<Symbol>() {
        return None;
    }
    let then_value = clause_value(clause_of::<ThenClause>(if_statement)?)?;
    let else_value = clause_value(clause_of::<ElseClause>(if_statement)?)?;
    let (fallback, repeated_subject) = if null_when_true {
        (then_value, else_value)
    } else {
        (else_value, then_value)
    };
    let repeated_subject = parenthesized_value(repeated_subject)?;
    (repeated_subject.is::<Symbol>() && repeated_subject.text() == subject.text())
        .then_some((subject, fallback))
}

fn is_null_value(node: M2Node<'_>) -> bool {
    node.is::<Symbol>() && node.text() == "null"
}

/// The `x <- y` targets of a parallel assignment, at any nesting depth.
fn left_arrow_targets(node: M2Node<'_>) -> Vec<M2Node<'_>> {
    node.collection_elements()
        .flat_map(|target| {
            if target.has_binary_operator::<Token![<-]>() {
                vec![target]
            } else if target.is_collection_expression() {
                left_arrow_targets(target)
            } else {
                Vec::new()
            }
        })
        .collect()
}

pub fn ambiguous_float_member_access_rewrite(node: M2Node<'_>) -> Option<String> {
    if !node.is_space_application() {
        return None;
    }

    let left = node.child_by_field_name("left")?;
    let right = node.child_by_field_name("right")?;
    if symbol_node_text(left).is_none()
        || !right.is::<FloatLiteral>()
        || left.end_byte() != right.start_byte()
    {
        return None;
    }

    let member_index = member_index_for_ambiguous_float_literal(right.text())?;
    Some(format!("{}#{member_index}", left.text()))
}

fn member_index_for_ambiguous_float_literal(float_text: &str) -> Option<String> {
    let fractional_part = float_text.strip_prefix('.')?;
    (!fractional_part.is_empty() && fractional_part.chars().all(|ch| ch.is_ascii_digit()))
        .then(|| fractional_part.to_string())
}

#[cfg(test)]
mod tests {
    use super::member_index_for_ambiguous_float_literal;

    use super::*;
    use crate::document::DocumentSnapshot;
    use crate::object_registry::ObjectRegistry;

    /// A control transfer is legal because of the function or loop enclosing it.
    /// When the grammar cannot parse that enclosure the structure is unknown, so
    /// reporting a scope violation turns a parser limitation into a false error
    /// on valid code. M2 v1.26.05 accepts all three shapes below and each returns
    /// `5`; the grammar currently fails on the lambda, so only the honest
    /// "Syntax error" may remain.
    #[test]
    fn control_transfers_in_unparsed_regions_report_no_scope_violation() {
        let builtins = ObjectRegistry::default();
        for source in [
            "f = () -> (\n    return -- note\n    5;\n)\n",
            "f = () -> (\n    return -- note\n    5\n)\n",
            "f = () -> (\n    return\n    5\n)\n",
            "f = () -> (\n    while true do (\n        break -- note\n        5\n    )\n)\n",
        ] {
            let document = DocumentSnapshot::from_text(source.to_string(), &builtins)
                .expect("fixture should parse");
            let scope_claims = document
                .diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.message.contains("can only be used inside"))
                .map(|diagnostic| diagnostic.message.clone())
                .collect::<Vec<_>>();

            assert!(
                scope_claims.is_empty(),
                "{source:?} must not be reported as a misplaced transfer: {scope_claims:?}"
            );
        }
    }

    #[test]
    fn misplaced_control_transfers_are_still_reported() {
        // The guard above must not silence transfers in code that does parse.
        let document = DocumentSnapshot::from_text(
            "return 5
"
            .to_string(),
            &ObjectRegistry::default(),
        )
        .expect("fixture should parse");

        assert!(
            document
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("can only be used inside")),
            "a top-level return is still a scope violation: {:?}",
            document
                .diagnostics()
                .iter()
                .map(|diagnostic| &diagnostic.message)
                .collect::<Vec<_>>()
        );
    }

    /// The installation and assignment-shape diagnostics a source reports,
    /// against the real builtin catalog.
    fn installation_findings(builtins: &ObjectRegistry, source: &str) -> Vec<DiagnosticKind> {
        let document = DocumentSnapshot::from_text(source.to_string(), builtins)
            .expect("fixture should parse");
        document
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.kind)
            .filter(|kind| {
                matches!(
                    kind,
                    DiagnosticKind::InstallNoEffect
                        | DiagnosticKind::InstallArity
                        | DiagnosticKind::MultipleAssignmentTargets
                        | DiagnosticKind::ExplicitInstallRequired
                )
            })
            .collect()
    }

    /// M2 evaluates `x <- v` by assigning a symbol `x` directly and otherwise
    /// calling the one-type method `symbol <-` of `class x` with `(x, v)`
    /// (`assigntofun` in M2's `d/evaluate.d`). `installMethod` accepts any other
    /// `<-` domain, but nothing ever calls those methods.
    #[test]
    fn left_arrow_methods_take_effect_only_for_one_non_symbol_type() {
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let findings = |installation: &str| {
            installation_findings(
                &builtins,
                &format!("T = new Type of BasicList\n{installation}\n"),
            )
        };

        assert_eq!(findings("installMethod(symbol <-, T, (x, v) -> v)"), []);
        assert_eq!(
            findings("installMethod(symbol <-, T, T, (x, y, v) -> v)"),
            [DiagnosticKind::InstallNoEffect],
            "a two-type method is never dispatched, and its three parameters are the right arity"
        );
        assert_eq!(
            findings("installMethod(symbol <-, Symbol, (x, v) -> v)"),
            [DiagnosticKind::InstallNoEffect]
        );
        assert_eq!(
            findings("installMethod(symbol <-, Keyword, (x, v) -> v)"),
            [DiagnosticKind::InstallNoEffect],
            "a keyword is a symbol, so it is assigned before any lookup too"
        );
    }

    /// Parallel assignment assigns each target by its own kind: symbols bind,
    /// parts store, and operator or method targets install, all in one statement.
    #[test]
    fn parallel_assignment_targets_are_assigned_component_wise() {
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let findings = |source: &str| installation_findings(&builtins, source);

        assert_eq!(
            findings(concat!(
                "T = new Type of BasicList\n",
                "g = method()\n",
                "(a, T + T, g ZZ, (b, c)) := (1, (x, y) -> x, n -> n, (3, 4))\n",
            )),
            []
        );
        assert_eq!(
            findings("L = new MutableList from {0, 0}\n(p, L#0, (q, w)) = (1, 2, (3, 4))\n"),
            []
        );
        assert_eq!(
            findings("(a, 1) = (2, 3)\n"),
            [DiagnosticKind::MultipleAssignmentTargets]
        );
        assert_eq!(
            findings("T = new Type of BasicList\n(T <- T, d) := ((x, y, v) -> v, 8)\n"),
            [DiagnosticKind::ExplicitInstallRequired],
            "a `<-` target is grammatical but never installs a callable method"
        );
    }

    #[test]
    fn ring_variables_prefer_indexed_names_over_multicharacter_symbols() {
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let messages = |source: &str| {
            let document = DocumentSnapshot::from_text(source.to_string(), &builtins)
                .expect("fixture should parse");
            document
                .diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.message.starts_with("Ring variable"))
                .map(|diagnostic| diagnostic.message.clone())
                .collect::<Vec<_>>()
        };

        // A trailing index is what the author meant; name it as one.
        let indexed = messages("R = QQ[x0, x1, x2]\n");
        assert_eq!(
            indexed.len(),
            3,
            "each multi-character variable is reported"
        );
        assert!(
            indexed[0].contains("`x_0`") && indexed[2].contains("`x_2`"),
            "the suggestion carries the index across: {indexed:?}"
        );

        // No trailing digits, so there is no index to suggest.
        let doubled = messages("R = QQ[xx]\n");
        assert_eq!(doubled.len(), 1);
        assert!(
            doubled[0].contains("single-character") && doubled[0].contains("`x_0`"),
            "an unindexed multi-character name suggests both routes: {doubled:?}"
        );

        // Everything already idiomatic stays quiet.
        for quiet in [
            "R = QQ[x, y, z]\n",
            "R = QQ[x_0, x_1]\n",
            "R = QQ[x_0..x_3]\n",
            "R = QQ[a..d]\n",
            "R = QQ[x, Degrees => {1}]\n",
            "counter = 0\nlongName = 1\n",
        ] {
            assert!(
                messages(quiet).is_empty(),
                "{quiet:?} must not be reported: {:?}",
                messages(quiet)
            );
        }
    }

    /// A conditional nested in both branches has no flatter else-if form: moving
    /// the `then` conditional to the `else` position just swaps the two, and the
    /// swapped form proposes the original back, so the quick fix oscillated.
    #[test]
    fn conditionals_in_both_branches_are_not_reported_as_flattenable() {
        let text = concat!(
            "f = (label, raw) -> (\n",
            "    if label == \"Token\" then {}\n",
            "    else if not isNode raw then if #raw == 0 then {} else toList(0 .. #raw - 1)\n",
            "    else if #raw <= 1 then {}\n",
            "    else toList(1 .. #raw - 1)\n",
            "    )\n",
        );
        let builtins = ObjectRegistry::default();
        let document =
            DocumentSnapshot::from_text(text.to_string(), &builtins).expect("fixture should parse");

        let simplifications = document
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.kind == DiagnosticKind::SimplifiableExpression)
            .map(|diagnostic| &diagnostic.message)
            .collect::<Vec<_>>();
        assert!(
            simplifications.is_empty(),
            "swapping two nested conditionals is not a simplification: {simplifications:?}"
        );
    }

    /// A simplification diagnostic must be clearable by its own quick fix. A
    /// condition's span reaches the `then` keyword and so carries the separating
    /// space, which used to make `not name` compare unequal to itself and propose
    /// the untouched line back — the warning then survived applying the action.
    #[test]
    fn simplification_is_not_reported_when_the_rewrite_changes_nothing() {
        let text = concat!(
            "f = () -> (\n",
            "    if not completed then lineNumber = lineNumber - 1;\n",
            "    g();\n",
            ");\n",
        );
        let builtins = ObjectRegistry::default();
        let document =
            DocumentSnapshot::from_text(text.to_string(), &builtins).expect("fixture should parse");

        assert!(
            document.diagnostics().is_empty(),
            "an already-simplest condition must not be reported: {:?}",
            document
                .diagnostics()
                .iter()
                .map(|diagnostic| &diagnostic.message)
                .collect::<Vec<_>>()
        );

        let if_node = document
            .root_node()
            .descendants()
            .find(|node| node.is::<IfStatement>())
            .expect("fixture contains an if");
        for rewrite in [
            if_null_branch_rewrite(if_node),
            if_condition_rewrite(if_node),
            else_if_chain_rewrite(if_node),
            coalescence_rewrite(if_node),
        ] {
            assert_eq!(
                simplification_of(if_node, rewrite.clone()),
                None,
                "a rewrite equal to the original is not a simplification (got {rewrite:?})"
            );
        }
    }

    #[test]
    fn ambiguous_member_access_helper_requires_dot_prefixed_float() {
        assert_eq!(
            member_index_for_ambiguous_float_literal(".3"),
            Some("3".to_string())
        );
        assert_eq!(
            member_index_for_ambiguous_float_literal(".123"),
            Some("123".to_string())
        );
        assert_eq!(member_index_for_ambiguous_float_literal("3"), None);
        assert_eq!(member_index_for_ambiguous_float_literal("."), None);
        assert_eq!(member_index_for_ambiguous_float_literal(".3e2"), None);
    }
}
