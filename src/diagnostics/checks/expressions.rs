//! The expression occasion: diagnostics spawned while visiting each typed
//! expression of a cell.
//!
//! [`ExpressionChecks`] visits a cell once. At each expression it runs the
//! checks that apply to the expression's alternative, in a fixed order, so a
//! document reports its findings in source order and, at one expression, in
//! the order of the rules below. Facts that depend on context the visitor
//! does not hold — rewrite sites and the targets of control transfers — are
//! computed for the cell first and looked up by expression.

use std::collections::HashMap;
use std::ptr;

use m2_syn::nodes::{AssignOperator, Expr, ExprAssign, ExprBinary, ExprIf, Literal, Symbol};
use m2_syn::visit::{self, Visit};
use m2_syn::{Delimiter, DelimiterValue, Span, Spanned, Token};

mod constant_values;
mod library_global_values;

use super::DiagnosticSink;
use crate::analysis::{
    Analysis, BindingId, CallOptionIssue, CallableHeadKind, ControlTransferTarget, Dispatch,
    InferenceSession, InfixSyntax, MethodCodomainEdit, MethodConstructor, MethodDeclarationOption,
    MethodHead, OptionDeclaration, RingVariableError, TypeChecker,
};
use crate::diagnostics::rewrites::{RewriteKind, RewriteSite, RewriteSites};
use crate::diagnostics::{catalog, DiagnosticRule, Finding};
use crate::document::FileProperties;
use crate::meta::BindingRole;
use crate::node_metadata::{ParsedCell, SyntaxSnapshot};
use crate::object_registry::{ObjectKnowledge, ObjectName, ObjectRegistry, ObjectRegistryView};
use crate::source::{DocumentSource, SourceNavigation, SyntaxNavigation, SyntaxSource};
use crate::syntax::{
    AssignmentOperator, AssignmentParts, AssignmentSyntax, AssignmentTarget, AssignmentTraversal,
    ControlSyntax, Destructuring, ExpressionSyntax, PackedArguments,
};
use crate::typesystem::{SubtypeEvidence, TypeKnowledge, TypeRole};

/// The callable whose argument `protect` makes immutable.
const PROTECT: &str = "protect";

/// The type name a `protect` argument must statically have.
const SYMBOL_TYPE: &str = "Symbol";

/// Inheritance predicate whose repeated calls indicate manual dispatch.
const INSTANCE: &str = "instance";

/// Number of parameter inheritance tests that suggests method dispatch.
const INSTANCE_DISPATCH_THRESHOLD: usize = 3;

/// Collection classes whose fixed length can encode positional record fields.
const RECORD_LIST_CLASSES: [&str; 3] = ["List", "BasicList", "VisibleList"];

/// What the expression occasion provides for every cell of one document.
pub(super) struct ExpressionContext<'a> {
    /// Semantic facts for the current document snapshot.
    pub analysis: &'a Analysis,
    /// Source coordinates of the syntax revision being checked.
    pub source: &'a SyntaxNavigation<'a, DocumentSource>,
    /// Object and type facts, positioned at each expression.
    pub knowledge: &'a ObjectRegistry,
    /// The first changed byte of a live update: expressions ending at or
    /// before it keep their published findings and are not checked again.
    pub first_change: Option<usize>,
}

/// Spawns every expression diagnostic of one retained cell.
pub(super) fn spawn<'a>(
    cell: &'a ParsedCell,
    context: &ExpressionContext<'a>,
    sink: &mut DiagnosticSink,
) {
    let Some(syntax) = cell.syntax() else {
        return;
    };
    let mut rewrites: HashMap<*const Expr, Vec<RewriteSite<'a>>> = HashMap::new();
    for site in RewriteSites::collect_analyzed(
        [syntax],
        context.source,
        context.analysis,
        context.knowledge,
    ) {
        rewrites
            .entry(ptr::from_ref(site.expression))
            .or_default()
            .push(site);
    }
    // A transfer is legal because of the function or loop enclosing it.
    // When the cell did not parse cleanly that structure is unknown, so a
    // missing target says the parse failed, not that the transfer is
    // misplaced.
    let transfers = if cell.parse_error().is_none() && cell.lexer_diagnostics().is_empty() {
        context
            .analysis
            .control_transfers([syntax], context.source, context.knowledge)
            .into_iter()
            .map(|transfer| (ptr::from_ref(transfer.transfer), transfer.target))
            .collect()
    } else {
        HashMap::new()
    };
    ExpressionChecks {
        context,
        snapshot: context.source.syntax(),
        rewrites,
        transfers,
        parameter_instance_tests: HashMap::new(),
        sink,
    }
    .visit_cell(syntax);
}

/// Visitor running the expression checks of one cell.
struct ExpressionChecks<'a, 'context> {
    /// Analysis, source, and knowledge shared by the cells of the document.
    context: &'context ExpressionContext<'a>,
    /// The syntax revision owning the visited cell.
    snapshot: &'a SyntaxSnapshot,
    /// Rewrite sites of the cell, by rewritten expression.
    rewrites: HashMap<*const Expr, Vec<RewriteSite<'a>>>,
    /// Control transfers of the cell, with the construct receiving each;
    /// absent when the cell's structure is unknown.
    transfers: HashMap<*const Expr, Option<ControlTransferTarget<'a>>>,
    /// Inheritance-test counts keyed by the resolved parameter binding.
    parameter_instance_tests: HashMap<BindingId, usize>,
    /// Findings spawned so far.
    sink: &'context mut DiagnosticSink,
}

impl<'a> Visit<'a> for ExpressionChecks<'a, '_> {
    fn visit_expr(&mut self, expression: &'a Expr) {
        self.instance_dispatch(expression);
        if let Some(bytes) = self.context.source.syntax_bytes(expression) {
            if self
                .context
                .first_change
                .is_none_or(|start| start < bytes.end)
            {
                self.check(expression);
            }
        }
        visit::visit_expr(self, expression);
    }

    fn visit_expr_assign(&mut self, assignment: &'a ExprAssign) {
        assignment.visit_installing(self);
    }
}

impl<'a> ExpressionChecks<'a, '_> {
    /// Runs every check on `expression`, in reporting order.
    fn check(&mut self, expression: &'a Expr) {
        let Some(position) = self.context.source.position_for_syntax(expression) else {
            return;
        };
        let knowledge = self.context.knowledge.at(position);
        self.required_values(expression, &knowledge);
        let assignment = match expression {
            Expr::Assign(assignment) => Some((assignment, assignment.parts())),
            _ => None,
        };
        let call = match expression {
            Expr::Binary(call) if matches!(call.op, m2_syn::nodes::BinaryOperator::Space(_)) => {
                Some(call)
            }
            _ => None,
        };
        self.missing_closing_delimiter(expression);
        self.rewrite(expression, RewriteKind::AmbiguousFloatMemberAccess);
        if let Some((node, parts)) = &assignment {
            self.multiple_assignment_targets(node, parts, &knowledge);
            self.colon_equal_part_assignment(parts);
            self.parallel_assignment(node, parts, &knowledge);
            self.protected_assignment(node, parts, &knowledge);
        }
        self.ring_variable(expression, &knowledge);
        self.constant_values(expression, &knowledge);
        self.float_key(expression, &knowledge);
        self.option_key_convention(expression, &knowledge);
        self.rewrite(expression, RewriteKind::RedundantControlParentheses);
        self.simplifications(expression);
        self.default_for_start(expression);
        if let Some(call) = call {
            self.ring_variable_naming(call, &knowledge);
        }
        if let Some((_, parts)) = &assignment {
            self.install_needs_colon_equals(expression, parts, &knowledge);
        }
        if let Some(call) = call {
            self.protect_computed_symbol(call, &knowledge);
        }
        if matches!(
            expression,
            Expr::Return(_) | Expr::Break(_) | Expr::Continue(_)
        ) {
            self.invalid_control_transfer(expression);
        }
        if let Some((_, parts)) = &assignment {
            self.explicit_install_required(expression, parts);
        }
        if let Expr::If(conditional) = expression {
            self.list_record_shape(conditional);
        }
        self.condition_type(expression, &knowledge);
        if let Some((node, _)) = &assignment {
            self.install_codomain(node, &knowledge);
        }
        if let Some(call) = call {
            self.call_arity(call, &knowledge);
            self.call_domain(call, &knowledge);
            self.call_options(call, &knowledge);
            for entry in call.right.arguments().into_iter().flatten() {
                self.option_symbol_convention(entry, &knowledge);
            }
        }
    }

    /// Requires names in value-only syntax slots to resolve in the declared
    /// environment. Bare names in symbolic positions, including option keys,
    /// enum-like values, and quoted symbols, remain legitimate symbols.
    fn required_values(&mut self, expression: &Expr, knowledge: &ObjectRegistryView<'_>) {
        match expression {
            Expr::Binary(call) if matches!(call.op, m2_syn::nodes::BinaryOperator::Space(_)) => {
                self.required_value(&call.left, "callable", knowledge);
                if call.left.symbol_name() == Some("method")
                    && self
                        .context
                        .source
                        .position_for_syntax(call)
                        .is_some_and(|position| {
                            self.context
                                .analysis
                                .visible_source_binding_at("method", position, knowledge)
                                .is_none()
                        })
                {
                    if let Some(typical) = call.method_option(MethodDeclarationOption::TypicalValue)
                    {
                        self.required_value(typical, "typical-value type", knowledge);
                    }
                }
            }
            Expr::New(node) => self.required_value(&node.class, "type", knowledge),
            Expr::If(node) => self.required_value(&node.cond, "Boolean condition", knowledge),
            Expr::While(node) => self.required_value(&node.cond, "Boolean condition", knowledge),
            Expr::Binary(index) if index.op.spelling() == <Token![#]>::SPELLING => {
                self.required_value(&index.left, "indexed value", knowledge)
            }
            Expr::Prefix(prefix) if prefix.op.spelling() == <Token![not]>::SPELLING => {
                self.required_value(&prefix.expr, "Boolean operand", knowledge)
            }
            _ => {}
        }
        if let Some(operands) = expression.infix_operands() {
            if matches!(
                operands.operator,
                <Token![and]>::SPELLING | <Token![or]>::SPELLING
            ) {
                self.required_value(operands.left, "Boolean operand", knowledge);
                self.required_value(operands.right, "Boolean operand", knowledge);
            }
        }
    }

    /// Reports an unbound name only when its position requires a resolved
    /// value. Forward global declarations and unknown-typed parameters are
    /// declarations, so neither is confused with an ambient free symbol.
    fn required_value(
        &mut self,
        expression: &Expr,
        role: &str,
        knowledge: &ObjectRegistryView<'_>,
    ) {
        let Some(Expr::Symbol(symbol)) = expression.unparenthesized() else {
            return;
        };
        let Some(position) = self.context.source.position_for_syntax(symbol) else {
            return;
        };
        let name = symbol.text();
        let analysis = self.context.analysis;
        if analysis
            .visible_source_binding_at(name, position, knowledge)
            .is_some()
            || knowledge.get_record(&ObjectName::new(name)).is_some()
            || analysis.bindings().any(|binding| {
                binding.name.name() == name && binding.scope_idx == 0 && binding.potential_export
            })
        {
            return;
        }
        self.spawn(&catalog::UNRESOLVED_VALUE, symbol,
            format!("Unresolved {role} `{name}`: no definition is available in this file's declared environment; declare or explicitly import it"));
    }

    /// Rejects floating-point keys in pairs and indexed reads or writes.
    fn float_key(&mut self, expression: &'a Expr, knowledge: &ObjectRegistryView<'_>) {
        let key = match expression {
            Expr::Option(option) => option.key.as_ref(),
            Expr::Binary(index) if index.op.spelling() == <Token![#]>::SPELLING => {
                index.right.as_ref()
            }
            Expr::Binary(operation) if operation.op.spelling() == <Token![#?]>::SPELLING => {
                operation.right.as_ref()
            }
            _ => return,
        };
        let literal = matches!(
            key.unparenthesized(),
            Some(Expr::Literal(Literal::Float(_)))
        );
        let inferred =
            self.context
                .analysis
                .infer_expression_static_type(key, self.context.source, knowledge);
        if literal
            || inferred
                .as_ref()
                .is_some_and(|ty| ty == &TypeRole::Real.object_name())
        {
            self.spawn(&catalog::FLOAT_KEY, key, "Floating-point values must not be used as keys: approximate equality makes lookups unreliable".to_string());
        }
    }

    /// Counts every inheritance test of a resolved function parameter, including
    /// compound conditions, validation guards, and nested expressions. Binding
    /// identity keeps shadowed names and separate functions independent. The
    /// third test carries one warning; later tests do not repeat it. Unchanged
    /// expressions contribute to the count without republishing old findings.
    fn instance_dispatch(&mut self, expression: &Expr) {
        if !matches!(expression, Expr::Binary(call) if matches!(call.op, m2_syn::nodes::BinaryOperator::Space(_)))
        {
            return;
        }
        let Some((subject, _)) = self.instance_operands(expression) else {
            return;
        };
        let Some(subject) = subject.unparenthesized().and_then(Expr::symbol_name) else {
            return;
        };
        let Some(position) = self.context.source.position_for_syntax(expression) else {
            return;
        };
        let knowledge = self.context.knowledge.at(position);
        let Some(binding) = self
            .context
            .analysis
            .visible_source_binding_at(subject, position, &knowledge)
        else {
            return;
        };
        if binding.role != BindingRole::Parameter {
            return;
        }
        let count = self
            .parameter_instance_tests
            .entry(binding.binding_id)
            .or_default();
        *count += 1;
        if *count != INSTANCE_DISPATCH_THRESHOLD {
            return;
        }
        if self.context.first_change.is_some_and(|start| {
            self.context
                .source
                .syntax_bytes(expression)
                .is_none_or(|bytes| bytes.end <= start)
        }) {
            return;
        }
        self.spawn(
            &catalog::MANUAL_TYPE_DISPATCH,
            expression,
            format!("Repeated class tests on `{subject}` implement manual dispatch; consider a method with one specialization per class."),
        );
    }

    /// Operands of an unshadowed inheritance predicate, without parameter restrictions.
    fn instance_operands<'test>(
        &self,
        expression: &'test Expr,
    ) -> Option<(&'test Expr, &'test Expr)> {
        let Expr::Binary(call) = expression.unparenthesized()? else {
            return None;
        };
        if !matches!(call.op, m2_syn::nodes::BinaryOperator::Space(_))
            || call.left.symbol_name()? != INSTANCE
        {
            return None;
        }
        let position = self.context.source.position_for_syntax(call)?;
        let knowledge = self.context.knowledge.at(position);
        if self
            .context
            .analysis
            .visible_source_binding_at(INSTANCE, position, &knowledge)
            .is_some()
        {
            return None;
        }
        let arguments = call.right.arguments();
        let [Some(subject), Some(class)] = arguments.as_slice() else {
            return None;
        };
        Some((subject, class))
    }

    /// A list-family check and fixed multi-field length suggest a record type.
    fn list_record_shape(&mut self, conditional: &ExprIf) {
        let mut terms = Vec::new();
        Self::condition_terms(&conditional.cond, &mut terms);
        let mut list_subjects = Vec::new();
        let mut length_subjects = Vec::new();
        for term in terms {
            if let Some((subject, class)) = self.instance_operands(term) {
                if RECORD_LIST_CLASSES
                    .iter()
                    .any(|role| class.symbol_name() == Some(*role))
                {
                    list_subjects.push(subject);
                }
            }
            if let Some(subject) = Self::fixed_record_length(term) {
                length_subjects.push(subject);
            }
        }
        if list_subjects.iter().any(|subject| {
            let subject = subject.unparenthesized().unwrap_or(subject);
            length_subjects.iter().any(|length_subject| {
                let length_subject = length_subject.unparenthesized().unwrap_or(length_subject);
                self.text(subject) == self.text(length_subject)
            })
        }) {
            self.spawn(
                &catalog::LIST_RECORD_SHAPE,
                &conditional.cond,
                "Consider making a struct-like type based on hash tables instead of a list.",
            );
        }
    }

    /// Leaves of boolean conditions, including negated validation predicates.
    fn condition_terms<'test>(expression: &'test Expr, terms: &mut Vec<&'test Expr>) {
        let Some(expression) = expression.unparenthesized() else {
            return;
        };
        if let Some(binary) = expression.infix_operands() {
            if matches!(binary.operator, "and" | "or") {
                Self::condition_terms(binary.left, terms);
                Self::condition_terms(binary.right, terms);
                return;
            }
        }
        if let Expr::Prefix(prefix) = expression {
            if prefix.op.spelling() == "not" {
                Self::condition_terms(&prefix.expr, terms);
                return;
            }
        }
        terms.push(expression);
    }

    /// Subject of a length equality or inequality against a literal integer above one.
    fn fixed_record_length(expression: &Expr) -> Option<&Expr> {
        let binary = expression.infix_operands()?;
        if !matches!(binary.operator, "==" | "!=") {
            return None;
        }
        for (length, count) in [(binary.left, binary.right), (binary.right, binary.left)] {
            let Some(Expr::Literal(Literal::Integer(integer))) = count.unparenthesized() else {
                continue;
            };
            if !integer.text().parse::<u64>().is_ok_and(|count| count > 1) {
                continue;
            }
            let Some(Expr::Prefix(prefix)) = length.unparenthesized() else {
                continue;
            };
            if prefix.op.spelling() == "#" {
                return Some(&prefix.expr);
            }
        }
        None
    }

    /// Spawns a finding of `rule` on `syntax`.
    fn spawn(
        &mut self,
        rule: &'static DiagnosticRule,
        syntax: &(impl Spanned + ?Sized),
        message: impl Into<String>,
    ) {
        if let Some(range) = self.context.source.range_for_syntax(syntax) {
            self.sink.spawn(rule, Finding::new(range, message));
        }
    }

    /// Source text of typed syntax.
    fn text(&self, syntax: &(impl Spanned + ?Sized)) -> &'a str {
        self.context
            .source
            .text_for_syntax(syntax)
            .unwrap_or_default()
    }

    /// A collection, parentheses, or parameter list whose closing delimiter
    /// is missing.
    fn missing_closing_delimiter(&mut self, expression: &Expr) {
        let delimiter = match expression {
            Expr::Sequence(node) => node.delimiter.delimiter(),
            Expr::List(node) => node.delimiter.delimiter(),
            Expr::Array(node) => node.delimiter.delimiter(),
            Expr::AngleBar(node) => node.delimiter.delimiter(),
            Expr::Paren(node) => node.delimiter.delimiter(),
            Expr::Function(node) => match &node.params {
                m2_syn::nodes::FunctionParams::List(list) => list.params.delimiter.delimiter(),
                _ => return,
            },
            _ => return,
        };
        self.missing_delimiter(&delimiter);
    }

    /// Spawns when `delimiter` was never closed in source.
    fn missing_delimiter(&mut self, delimiter: &Delimiter) {
        if let Some(closing) = delimiter.closing_span().filter(Span::is_empty) {
            self.spawn(&catalog::MISSING_NODE, &closing, "Missing: delimiter");
        }
    }

    /// Reports the rewrite of `kind` recorded for `expression`.
    fn rewrite(&mut self, expression: &Expr, kind: RewriteKind) {
        let Some(replacement) = self.site(expression, |site| site == kind) else {
            return;
        };
        let (rule, message) = match kind {
            RewriteKind::AmbiguousFloatMemberAccess => (
                &catalog::AMBIGUOUS_FLOAT_MEMBER_ACCESS,
                format!(
                    "This is parsed as application to a float literal; use `{replacement}` for member access"
                ),
            ),
            RewriteKind::RedundantControlParentheses => (
                &catalog::REDUNDANT_CONTROL_PARENTHESES,
                format!(
                    "Parentheses around this control expression are redundant; use `{replacement}`"
                ),
            ),
            _ => return,
        };
        self.spawn(rule, expression, message);
    }

    /// Flags a literal `from 0`, since numeric `for` loops start at zero by
    /// default. The recorded edit removes only the keyword and integer spelling,
    /// retaining comments between them and the remainder of the loop header.
    fn default_for_start(&mut self, expression: &Expr) {
        let Expr::For(loop_) = expression else {
            return;
        };
        let Some(m2_syn::nodes::ForIter::Range(range)) = &loop_.iter else {
            return;
        };
        let Some(bound) = &range.from_clause else {
            return;
        };
        let Expr::Literal(Literal::Integer(integer)) = bound.expr.as_ref() else {
            return;
        };
        if integer.text().parse::<u64>() != Ok(0) {
            return;
        }
        let source = self.context.source;
        let (Some(keyword), Some(value)) = (
            source.syntax_bytes(&bound.from_token),
            source.syntax_bytes(integer),
        ) else {
            return;
        };
        let end = value.start + integer.text().len();
        let range = source.range_for_bytes(keyword.start..end);
        let mut start = keyword.start;
        let text = source.text();
        while start > 0 && matches!(text.as_bytes()[start - 1], b' ' | b'\t') {
            start -= 1;
        }
        let trivia = &text[keyword.start + <Token![from]>::SPELLING.len()..value.start];
        let replacement = if trivia.trim().is_empty() { "" } else { trivia };
        self.sink.spawn(
            &catalog::REDUNDANT_FOR_START,
            Finding::new(range, "`for` starts at zero by default; omit `from 0`").with_fix(
                "Omit `from 0`",
                source.range_for_bytes(start..end),
                replacement,
            ),
        );
    }

    /// Reports the coalescences, inlined null bindings, and simplifications of
    /// `expression`, each under its site's rule. An inlined binding covers the
    /// binding statement as well as the conditional.
    fn simplifications(&mut self, expression: &Expr) {
        let findings = self
            .rewrites
            .get(&ptr::from_ref(expression))
            .into_iter()
            .flatten()
            .filter_map(|site| {
                let summary = match site.kind {
                    kind if kind.is_coalescence() => "This conditional can be simplified to",
                    RewriteKind::InlinedNullBinding => "This local only feeds a null test; use",
                    kind if kind.is_simplification() => "This expression can be simplified to",
                    _ => return None,
                };
                let range = match &site.extent {
                    Some(bytes) => self.context.source.range_for_bytes(bytes.clone()),
                    None => self.context.source.range_for_syntax(site.expression)?,
                };
                let caveat = if site.catches_errors {
                    ", which also yields the fallback where the tested value fails"
                } else {
                    ""
                };
                let message = format!("{summary} `{}`{caveat}", site.replacement);
                Some((site.rule(), Finding::new(range, message)))
            })
            .collect::<Vec<_>>();
        for (rule, finding) in findings {
            self.sink.spawn(rule, finding);
        }
    }

    /// The replacement of the first rewrite site at `expression` whose kind
    /// `accepts` takes.
    fn site(&self, expression: &Expr, accepts: impl Fn(RewriteKind) -> bool) -> Option<String> {
        self.rewrites
            .get(&ptr::from_ref(expression))?
            .iter()
            .find(|site| accepts(site.kind))
            .map(|site| site.replacement.clone())
    }

    /// Whether the assignment installs a method rather than assigning.
    fn installs(&self, assignment: &ExprAssign) -> bool {
        self.context
            .analysis
            .installation_for(assignment, self.context.source)
            .is_some()
    }

    /// A parallel assignment whose targets M2 cannot assign component-wise.
    fn multiple_assignment_targets(
        &mut self,
        assignment: &ExprAssign,
        parts: &AssignmentParts<'a>,
        knowledge: &ObjectRegistryView<'_>,
    ) {
        let AssignmentTarget::List(targets) = parts.target else {
            return;
        };
        let spelling = match parts.operator {
            AssignmentOperator::Local => <Token![:=]>::SPELLING,
            AssignmentOperator::Global => <Token![=]>::SPELLING,
            _ => return,
        };
        if self.installs(assignment)
            || self.targets_are_assignable(targets, parts.operator, knowledge)
        {
            return;
        }
        self.spawn(
            &catalog::MULTIPLE_ASSIGNMENT_TARGETS,
            targets.0,
            format!(
                "{spelling} parallel assignment targets must be symbols, installation \
                 targets such as `T + T` or `f ZZ`, parts such as `x#i` (with `=`), or \
                 nested lists"
            ),
        );
    }

    /// Whether every target of a parallel assignment is one M2 assigns
    /// component-wise: a symbol, a nested list of such targets, an `x <- y`
    /// target (grammatical; whether it installs anything is the `<-` check's
    /// concern), or an operator expression. Under `=` an operator expression is
    /// an ordinary assignment to that expression (`x + 1 = v`, `M_(0,0) = v`),
    /// always valid; under `:=` it must be an installation target, so a shape
    /// that installs nothing or a literal operand (`x + 1`) is rejected.
    fn targets_are_assignable(
        &self,
        targets: Destructuring<'_>,
        operator: AssignmentOperator,
        knowledge: &ObjectRegistryView<'_>,
    ) -> bool {
        targets.targets().into_iter().flatten().all(|target| {
            match (Destructuring::of(target), target.unparenthesized()) {
                (Some(nested), _) => self.targets_are_assignable(nested, operator, knowledge),
                (
                    _,
                    Some(
                        operation @ (Expr::Binary(_)
                        | Expr::Prefix(_)
                        | Expr::Postfix(_)
                        | Expr::Field(_)),
                    ),
                ) => {
                    operator == AssignmentOperator::Global
                        || self.is_installation_target(operation, knowledge)
                }
                _ => true,
            }
        })
    }

    /// Whether `target` can receive a `:=` installation: it has an
    /// installation shape and no operand is a literal, which can never be a
    /// type.
    fn is_installation_target(&self, target: &Expr, knowledge: &ObjectRegistryView<'_>) -> bool {
        let Some(position) = self.context.source.position_for_syntax(target) else {
            return false;
        };
        self.context
            .analysis
            .installation_shape(AssignmentTarget::Operator(target), position, knowledge)
            .is_some_and(|shape| {
                shape
                    .domain
                    .iter()
                    .all(|operand| !matches!(operand, Expr::Literal(_)))
            })
    }

    /// `x#i := v`, which cannot assign to a part.
    fn colon_equal_part_assignment(&mut self, parts: &AssignmentParts<'a>) {
        let AssignmentTarget::Operator(target @ Expr::Binary(index)) = parts.target else {
            return;
        };
        if index.op.spelling() != <Token![#]>::SPELLING {
            return;
        }
        if parts.operator != AssignmentOperator::Local {
            return;
        }
        let (Some(range), Some(operator)) = (
            self.context.source.range_for_syntax(target),
            parts
                .operator_span
                .as_ref()
                .and_then(|span| self.context.source.range_for_syntax(span)),
        ) else {
            return;
        };
        self.sink.spawn(
            &catalog::COLON_EQUAL_PART_ASSIGNMENT,
            Finding::new(
                range,
                "`:=` cannot assign to parts; use `=` for part assignment",
            )
            .with_fix(
                "Use `=` for part assignment",
                operator,
                <Token![=]>::SPELLING,
            ),
        );
    }

    /// A parallel assignment whose sides cannot be matched up: lists of
    /// different lengths, or a single value whose type cannot supply several.
    fn parallel_assignment(
        &mut self,
        assignment: &ExprAssign,
        parts: &AssignmentParts<'a>,
        knowledge: &ObjectRegistryView<'_>,
    ) {
        let AssignmentTarget::List(targets) = parts.target else {
            return;
        };
        if !matches!(
            parts.operator,
            AssignmentOperator::Global | AssignmentOperator::Local
        ) || self.installs(assignment)
        {
            return;
        }
        self.match_parallel_assignment(targets, parts.value, knowledge);
    }

    /// Matches a list of targets against the value assigned to it, descending
    /// into nested lists of equal length.
    fn match_parallel_assignment(
        &mut self,
        targets: Destructuring<'a>,
        value: &'a Expr,
        knowledge: &ObjectRegistryView<'_>,
    ) {
        let targets_written = targets.targets();
        let Some(values) = value.collection_elements() else {
            self.parallel_assignment_value_type(targets_written.len(), value, knowledge);
            return;
        };
        if targets_written.len() != values.len() {
            self.spawn(
                &catalog::PARALLEL_ASSIGNMENT_ARITY,
                targets.0,
                format!(
                    "parallel assignment binds {} targets but the right-hand side lists {}; their lengths must match",
                    targets_written.len(),
                    values.len()
                ),
            );
            return;
        }
        for (target, value) in targets_written.into_iter().zip(values) {
            if let (Some(nested), Some(value)) = (target.and_then(Destructuring::of), value) {
                self.match_parallel_assignment(nested, value, knowledge);
            }
        }
    }

    /// Spawns when a single value assigned to `target_count` targets has a
    /// type that cannot supply several values.
    fn parallel_assignment_value_type(
        &mut self,
        target_count: usize,
        value: &'a Expr,
        knowledge: &ObjectRegistryView<'_>,
    ) {
        if target_count < 2 {
            return;
        }
        let analysis = self.context.analysis;
        if let Some(Expr::Symbol(symbol)) = value.unparenthesized() {
            let Some(position) = self.context.source.position_for_syntax(symbol) else {
                return;
            };
            let has_source_binding = analysis
                .visible_source_binding_at(symbol.text(), position, knowledge)
                .is_some();
            let has_indexed_value = knowledge
                .get_record(&ObjectName::new(symbol.text()))
                .is_some();
            if !has_source_binding && !has_indexed_value {
                return;
            }
        }
        let Some(value_type) =
            analysis.infer_expression_static_type(value, self.context.source, knowledge)
        else {
            return;
        };
        if value_type == TypeRole::Thing.object_name()
            || knowledge.has_type_role(&value_type, TypeRole::VisibleList)
        {
            return;
        }
        self.spawn(
            &catalog::PARALLEL_ASSIGNMENT_TYPE,
            value,
            format!(
                "parallel assignment binds {target_count} targets, but the right-hand side has incompatible type `{}`",
                value_type.name()
            ),
        );
    }

    /// Rebinding a protected or package name outside the library's own code. Only names
    /// being rebound are checked; an indexed or member target changes a value,
    /// and an installation changes a method table rather than its binding.
    fn protected_assignment(
        &mut self,
        assignment: &ExprAssign,
        parts: &AssignmentParts<'a>,
        knowledge: &ObjectRegistryView<'_>,
    ) {
        if !matches!(
            parts.operator,
            AssignmentOperator::Global | AssignmentOperator::Augmented
        ) || self.installs(assignment)
        {
            return;
        }
        let symbols = match parts.target {
            AssignmentTarget::Augmented(target) => match target.unparenthesized() {
                Some(Expr::Symbol(symbol)) => vec![symbol],
                _ => Vec::new(),
            },
            target => target.bound_symbols(),
        };
        let value = matches!(
            (parts.operator, &parts.target),
            (AssignmentOperator::Global, AssignmentTarget::Symbol(_))
        )
        .then_some(parts.value);
        for symbol in symbols {
            self.rebound_library_name(symbol, value, knowledge);
        }
    }

    /// Spawns when `symbol` rebinds a library name rather than a local: a
    /// protected global, which Macaulay2 refuses, a package name, which it
    /// accepts and thereby loses the package, or a rebindable global given
    /// `value`, the whole value of a plain `=`, that the global cannot hold.
    fn rebound_library_name(
        &mut self,
        symbol: &Symbol,
        value: Option<&'a Expr>,
        knowledge: &ObjectRegistryView<'_>,
    ) {
        let analysis = self.context.analysis;
        let name = symbol.text();
        let Some(range) = self.context.source.range_for_syntax(symbol) else {
            return;
        };
        if analysis
            .assigned_binding_at(range)
            .or_else(|| analysis.get_binding_at(name, range.start))
            .is_some_and(|binding| binding.is_local())
        {
            return;
        }
        let object = ObjectName::new(name);
        let Some(record) = knowledge.get_record(&object) else {
            return;
        };
        let (rule, message) = if knowledge.binds_package(&object) {
            (
                &catalog::PACKAGE_REBINDING,
                format!(
                    "`{name}` names a package; rebinding it makes `package` return `null` for \
                     everything `{name}` defines, so choose another name"
                ),
            )
        } else if record.protected {
            (
                &catalog::PROTECTED_ASSIGNMENT,
                format!(
                    "Cannot assign to protected symbol `{name}`; a file implementing the \
                     library itself declares `{}`",
                    FileProperties::primary_code_directive()
                ),
            )
        } else {
            if let Some(value) = value {
                self.library_global_value(record, value, range, knowledge);
            }
            return;
        };
        self.sink.spawn(rule, Finding::new(range, message));
    }

    /// A ring variable subscript, `R_i` or `R_"x"`, selecting no variable.
    fn ring_variable(&mut self, expression: &'a Expr, knowledge: &ObjectRegistryView<'_>) {
        let Expr::Binary(binary) = expression else {
            return;
        };
        if binary
            .infix_operands()
            .is_none_or(|operands| operands.operator != <Token![_]>::SPELLING)
        {
            return;
        }
        let checker = TypeChecker::new(self.context.analysis, knowledge);
        let session = InferenceSession::new(&checker, self.context.source, self.snapshot);
        let Some(Err(error)) = session.ring_values(0).selected(expression) else {
            return;
        };
        let (rule, hint) = match &error {
            RingVariableError::IndexOutOfBounds(out_of_bounds) => (
                &catalog::RING_INDEX_OUT_OF_BOUNDS,
                match out_of_bounds.variable_count {
                    1 => "this ring has 1 variable".to_string(),
                    count => format!("this ring has {count} variables"),
                } + ", counting those of a polynomial coefficient ring",
            ),
            RingVariableError::NotFound { .. } => (
                &catalog::RING_VARIABLE_NOT_FOUND,
                "lookups by name find only the ring's variables, as its subscripts select them"
                    .to_string(),
            ),
        };
        self.spawn(rule, expression, format!("{error}; {hint}"));
    }

    /// Names and evaluated bindings in a function's option declaration list.
    fn option_key_convention(&mut self, expression: &Expr, knowledge: &ObjectRegistryView<'_>) {
        let Some((_, list)) = OptionDeclaration::declared_list(expression) else {
            return;
        };
        let entries = list
            .elems
            .as_ref()
            .map(PackedArguments::arguments)
            .unwrap_or_default();
        for entry in entries.into_iter().flatten() {
            self.option_symbol_convention(entry, knowledge);
        }
    }

    /// Checks the operands of one option in a declaration or a call.
    fn option_symbol_convention(&mut self, entry: &Expr, knowledge: &ObjectRegistryView<'_>) {
        let Expr::Option(option) = entry else { return };
        for (operand, key) in [(&option.key, true), (&option.value, false)] {
            let Expr::Symbol(symbol) = operand.as_ref() else {
                continue;
            };
            let name = symbol.text();
            let Some(range) = self.context.source.range_for_syntax(symbol) else {
                continue;
            };
            let bound = self
                .context
                .analysis
                .visible_source_binding_at(name, range.start, knowledge)
                .is_some();
            let indexed = knowledge.get_record(&ObjectName::new(name)).is_some();
            if bound && key {
                self.sink.spawn(&catalog::BOUND_OPTION_KEY, Finding::new(range, format!("Option key `{name}` evaluates its existing binding instead of naming a symbol")));
            }
            if bound || indexed || super::naming::follows_case(name, true) {
                continue;
            }
            let role = if key {
                "Option name"
            } else {
                "Symbol option value"
            };
            let mut finding = Finding::new(range, format!("{role} `{name}` should use PascalCase"));
            if key && name.starts_with(|first: char| first.is_ascii_lowercase()) {
                let mut capitalized = name.to_string();
                capitalized[..1].make_ascii_uppercase();
                if super::naming::follows_case(&capitalized, true) {
                    finding = finding.with_fix("Capitalize option key", range, capitalized);
                }
            }
            self.sink.spawn(&catalog::OPTION_KEY_CONVENTION, finding);
        }
    }

    /// Multi-character ring variables, which read as an indexed family but
    /// are unrelated symbols.
    fn ring_variable_naming(&mut self, call: &'a ExprBinary, knowledge: &ObjectRegistryView<'_>) {
        let analysis = self.context.analysis;
        let Some(position) = self.context.source.position_for_syntax(call) else {
            return;
        };
        let scope = analysis.find_scope_at(position).unwrap_or(0);
        for variable in
            analysis.ring_constructor_symbol_variables(call, self.context.source, knowledge, scope)
        {
            if let Some(message) = ring_variable_naming_message(variable.text()) {
                self.spawn(&catalog::RING_VARIABLE_NAMING, variable, message);
            }
        }
    }

    /// `f T = x -> …`, a method installation written with `=`.
    fn install_needs_colon_equals(
        &mut self,
        expression: &Expr,
        parts: &AssignmentParts<'a>,
        knowledge: &ObjectRegistryView<'_>,
    ) {
        if parts.operator != AssignmentOperator::Global || !matches!(parts.value, Expr::Function(_))
        {
            return;
        }
        let analysis = self.context.analysis;
        let source = self.context.source;
        let Some(position) = source.position_for_syntax(expression) else {
            return;
        };
        let Some(shape) = analysis.installation_shape(parts.target, position, knowledge) else {
            return;
        };
        let MethodHead::Function(name) = shape.head else {
            return;
        };
        if analysis.callable_head_kind(name.name(), position, knowledge)
            == CallableHeadKind::Unknown
        {
            return;
        }
        let (Some(range), Some(operator)) = (
            source.range_for_syntax(expression),
            parts
                .operator_span
                .as_ref()
                .and_then(|span| source.range_for_syntax(span)),
        ) else {
            return;
        };
        let name = name.name();
        self.sink.spawn(
            &catalog::INSTALL_NEEDS_COLON_EQUALS,
            Finding::new(
                range,
                format!(
                    "Installing a method on `{name}` must use `:=`, not `=`: M2 rejects this \
                     (\"no method for storing values of function {name}\"). Use `:=`."
                ),
            )
            .with_fix(
                "Use `:=` for method installation",
                operator,
                <Token![:=]>::SPELLING,
            ),
        );
    }

    /// `protect` applied to an expression, which chooses the protected
    /// symbol only at run time.
    fn protect_computed_symbol(
        &mut self,
        call: &'a ExprBinary,
        knowledge: &ObjectRegistryView<'_>,
    ) {
        let Expr::Symbol(callable) = call.left.as_ref() else {
            return;
        };
        if callable.text() != PROTECT {
            return;
        }
        let analysis = self.context.analysis;
        let source = self.context.source;
        let argument = call.right.as_ref();
        if source
            .position_for_syntax(callable)
            .is_none_or(|position| analysis.binding_id_at(callable.text(), position).is_some())
            || matches!(
                argument.unparenthesized(),
                Some(Expr::Quote(_) | Expr::Symbol(_))
            )
        {
            return;
        }
        if analysis
            .infer_expression_static_type(argument, source, knowledge)
            .is_none_or(|type_id| type_id.name() == SYMBOL_TYPE)
        {
            self.spawn(
                &catalog::PROTECT_COMPUTED_SYMBOL,
                argument,
                "`protect` evaluates this expression to choose a Symbol at runtime; \
                 the protected symbol is not statically apparent",
            );
        }
    }

    /// A `return`, `break`, or `continue` that no enclosing function or loop
    /// receives.
    fn invalid_control_transfer(&mut self, transfer: &Expr) {
        let Some(&target) = self.transfers.get(&ptr::from_ref(transfer)) else {
            return;
        };
        if target.is_some_and(|target| target.accepts(transfer)) {
            return;
        }
        let message = match transfer {
            Expr::Return(_) => "`return` can only be used inside a function body",
            Expr::Break(_) => {
                "`break` can only be used inside a loop body or an `apply`/`scan` callback"
            }
            Expr::Continue(_)
                if transfer.transferred_value().is_some()
                    && matches!(
                        target,
                        Some(
                            ControlTransferTarget::DoLoop(_)
                                | ControlTransferTarget::LoopCallback { .. }
                        )
                    ) =>
            {
                "`continue` with a value requires a `list` clause"
            }
            Expr::Continue(_) => "`continue` can only be used inside a `list` or `do` loop body",
            _ => return,
        };
        match transfer {
            Expr::Return(node) => {
                self.spawn(
                    &catalog::INVALID_CONTROL_TRANSFER,
                    &node.return_token,
                    message,
                );
            }
            Expr::Break(node) => {
                self.spawn(
                    &catalog::INVALID_CONTROL_TRANSFER,
                    &node.break_token,
                    message,
                );
            }
            Expr::Continue(node) => {
                self.spawn(
                    &catalog::INVALID_CONTROL_TRANSFER,
                    &node.continue_token,
                    message,
                );
            }
            _ => {}
        }
    }

    /// A `<-` method written as an assignment, which M2 never installs.
    fn explicit_install_required(&mut self, expression: &Expr, parts: &AssignmentParts<'a>) {
        let installs_function = parts.value.assigned_function().is_some();
        let classical_left_arrow_install = match parts.target {
            // `x <- (y := f)`
            AssignmentTarget::Evaluated(_) => matches!(
                parts.value.unparenthesized(),
                Some(Expr::Assign(inner))
                    if inner.parts().operator == AssignmentOperator::Local
                        && inner.parts().value.assigned_function().is_some()
            ),
            AssignmentTarget::Expression(Expr::Assign(inner)) => {
                parts.operator == AssignmentOperator::Local
                    && installs_function
                    && matches!(inner.op, AssignOperator::Evaluated(_))
            }
            _ => false,
        };
        if classical_left_arrow_install {
            self.spawn(
                &catalog::EXPLICIT_INSTALL_REQUIRED,
                expression,
                "Methods for `<-` must be installed with `installMethod(symbol <-, Type, function)`",
            );
        }
        let AssignmentTarget::List(list) = parts.target else {
            return;
        };
        for target in left_arrow_targets(list) {
            self.spawn(
                &catalog::EXPLICIT_INSTALL_REQUIRED,
                target,
                "`x <- y` cannot be an assignment target: `<-` is overloaded only with \
                 `installMethod(symbol <-, Type, function)`",
            );
        }
    }

    /// An `if` or `while` condition whose type cannot be `Boolean`.
    fn condition_type(&mut self, expression: &'a Expr, knowledge: &ObjectRegistryView<'_>) {
        let (construct, condition) = match expression {
            Expr::If(node) => ("if", node.cond.as_ref()),
            Expr::While(node) => ("while", node.cond.as_ref()),
            _ => return,
        };
        let Some(range) = self.context.source.range_for_syntax(condition) else {
            return;
        };
        let analysis = self.context.analysis;
        let checker = TypeChecker::new(analysis, knowledge);
        let session = InferenceSession::new(&checker, self.context.source, self.snapshot);
        let scope = analysis.find_scope_at(range.start).unwrap_or(0);
        let actual = session.infer(condition, scope);
        if actual.possibility_by(&TypeRole::Boolean.object_name(), |candidate, bound| {
            checker.subtype_evidence(candidate, bound, range.start, knowledge)
        }) != SubtypeEvidence::Disproven
        {
            return;
        }
        self.sink.spawn(
            &catalog::CONDITION_TYPE,
            Finding::new(
                range,
                format!(
                    "{construct} condition must have type `Boolean`, but this expression has type `{}`",
                    actual.label().unwrap_or_else(|| "unknown".to_string())
                ),
            ),
        );
    }

    /// A method installation whose codomain inference can supply, or whose
    /// annotated codomain contradicts the inferred result type.
    fn install_codomain(&mut self, assignment: &ExprAssign, knowledge: &ObjectRegistryView<'_>) {
        if let Some(range) = self
            .context
            .analysis
            .redundant_method_codomain(assignment, self.context.source)
        {
            self.sink.spawn(
                &catalog::REDUNDANT_METHOD_CODOMAIN,
                Finding::new(
                    range,
                    "This annotation repeats the method's TypicalValue and can be removed.",
                )
                .with_fix("Remove redundant codomain annotation", range, ""),
            );
        }
        let Some(deduction) = self.context.analysis.method_codomain_deduction(
            assignment,
            self.context.source,
            knowledge,
        ) else {
            return;
        };
        let codomain = &deduction.codomain;
        match (&deduction.edit, &deduction.annotated_codomain) {
            (MethodCodomainEdit::Add(insertion), _) => self.sink.spawn(
                &catalog::INSTALL_CODOMAIN_MISSING,
                Finding::new(
                    deduction.diagnostic_range,
                    format!(
                        "This method's lambda has the deducible codomain `{codomain}`. Add the codomain annotation."
                    ),
                )
                .with_fix(
                    "Add codomain annotation",
                    *insertion,
                    format!("{codomain} {} ", <Token![=>]>::SPELLING),
                ),
            ),
            (MethodCodomainEdit::Replace, Some(annotated)) => self.sink.spawn(
                &catalog::INSTALL_CODOMAIN_MISMATCH,
                Finding::new(
                    deduction.diagnostic_range,
                    format!(
                        "This method's inferred result type `{codomain}` is incompatible with its annotated codomain `{annotated}`."
                    ),
                ),
            ),
            (MethodCodomainEdit::Replace, None) => {}
        }
    }

    /// Checks fixed source-lambda arity when the argument pack's size is
    /// apparent. A computed Sequence may have any size, so stays unchecked.
    fn call_arity(&mut self, call: &ExprBinary, knowledge: &ObjectRegistryView<'_>) {
        let Some(name) = call.left.symbol_name() else {
            return;
        };
        let Some(position) = self.context.source.position_for_syntax(call) else {
            return;
        };
        let analysis = self.context.analysis;
        let Some(function) = analysis
            .function_at(name, position)
            .filter(|function| !function.is_method_function())
        else {
            return;
        };
        let Some(Dispatch::Fixed(expected)) = function.dispatch else {
            return;
        };
        let argument = call.right.unparenthesized().unwrap_or(&call.right);
        let arguments = argument.arguments();
        if !matches!(argument, Expr::Sequence(_))
            && analysis
                .infer_expression_static_type(argument, self.context.source, knowledge)
                .is_none_or(|class| knowledge.has_type_role(&class, TypeRole::Sequence))
        {
            return;
        }
        let actual = arguments.into_iter().filter(|argument| {
            !matches!(argument, Some(Expr::Option(option)) if function.declared_options.as_ref()
                .is_some_and(|options| options.iter().any(|accepted| Some(accepted.name.name()) == option.key.symbol_name())))
        }).count();
        if actual != expected {
            self.spawn(
                &catalog::CALL_ARITY_MISMATCH,
                call,
                format!("`{name}` takes {expected} argument(s), but this call supplies {actual}"),
            );
        }
    }

    /// A call whose argument types no known method domain accepts.
    fn call_domain(&mut self, call: &ExprBinary, knowledge: &ObjectRegistryView<'_>) {
        let Some(arguments) = TypeChecker::new(self.context.analysis, knowledge)
            .disjoint_call_argument_types(call, self.context.source, knowledge)
        else {
            return;
        };
        self.spawn(
            &catalog::CALL_DOMAIN_MISMATCH,
            call,
            format!(
                "No known method domain matches these argument types: ({})",
                arguments
                    .iter()
                    .map(|argument| argument.label().unwrap_or_else(|| "unknown".into()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }

    /// Options a call passes that its function does not accept: unknown
    /// names, and values outside the documented types or possible symbols.
    fn call_options(&mut self, call: &'a ExprBinary, knowledge: &ObjectRegistryView<'_>) {
        let checker = TypeChecker::new(self.context.analysis, knowledge);
        let issues = checker.call_option_issues(call, self.context.source, knowledge);
        if issues.is_empty() {
            return;
        }
        let function = self.text(call.left.as_ref());
        let quoted = |names: &[ObjectName]| {
            names
                .iter()
                .map(|name| format!("`{}`", name.name()))
                .collect::<Vec<_>>()
                .join(", ")
        };
        for issue in issues {
            let (rule, syntax, message, fix) = match issue {
                CallOptionIssue::UnknownName {
                    key,
                    suggestion,
                    accepted,
                } => {
                    let name = self.text(key);
                    let message = match &suggestion {
                        Some(suggestion) => format!(
                            "`{name}` is not an option of `{function}`; did you mean `{}`?",
                            suggestion.name()
                        ),
                        None if accepted.is_empty() => {
                            format!("`{function}` takes no options, so `{name}` is rejected")
                        }
                        None => format!(
                            "`{name}` is not an option of `{function}`, which accepts {}",
                            quoted(&accepted)
                        ),
                    };
                    (&catalog::UNKNOWN_OPTION, key, message, suggestion)
                }
                CallOptionIssue::ValueType {
                    value,
                    option,
                    actual,
                    accepted,
                } => (
                    &catalog::OPTION_VALUE_MISMATCH,
                    value,
                    format!(
                        "Option `{}` of `{function}` takes {}, but this value has type `{}`",
                        option.name(),
                        quoted(&accepted),
                        actual.label().unwrap_or_else(|| "unknown".to_string())
                    ),
                    None,
                ),
                CallOptionIssue::ValueNotPossible {
                    value,
                    option,
                    possible,
                } => (
                    &catalog::OPTION_VALUE_MISMATCH,
                    value,
                    format!(
                        "Option `{}` of `{function}` takes one of {}, not `{}`",
                        option.name(),
                        quoted(&possible),
                        self.text(value)
                    ),
                    None,
                ),
            };
            let Some(range) = self.context.source.range_for_syntax(syntax) else {
                continue;
            };
            let mut finding = Finding::new(range, message);
            if let Some(suggestion) = fix {
                finding = finding.with_fix(
                    format!("Replace with `{}`", suggestion.name()),
                    range,
                    suggestion.name(),
                );
            }
            self.sink.spawn(rule, finding);
        }
    }
}

/// The `x <- y` targets of a parallel assignment list, at any nesting depth.
/// The `x <- y` targets of a parallel assignment list, at any nesting depth.
fn left_arrow_targets(list: Destructuring<'_>) -> Vec<&Expr> {
    list.targets()
        .into_iter()
        .flatten()
        .flat_map(|target| match target.unparenthesized() {
            Some(Expr::Assign(assignment))
                if matches!(assignment.op, AssignOperator::Evaluated(_)) =>
            {
                vec![target]
            }
            _ => Destructuring::of(target).map_or_else(Vec::new, left_arrow_targets),
        })
        .collect()
}

/// The trailing run of digits in a name, with the name before it — `x0`
/// splits into `x` and `0`. `None` when the name has no trailing digits
/// (`xx`) or is only digits, neither of which names an index the author meant
/// to write.
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

/// Why a multi-character ring variable is worth flagging: `x0` and `x1` are
/// two unrelated symbols that merely look sequential, while `x_0` and `x_1`
/// are one indexed family — subscriptable, printable as `x`, and writable as
/// the range `x_0..x_9`. Single-character names need no such treatment.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::DocumentSnapshot;

    /// The rewrite sites of a document whose kind `accepts` takes.
    fn rewrite_sites(document: &DocumentSnapshot, accepts: fn(RewriteKind) -> bool) -> Vec<String> {
        let cells = document
            .syntax()
            .cells()
            .iter()
            .filter_map(ParsedCell::syntax);
        RewriteSites::collect(cells, document)
            .into_iter()
            .filter(|site| accepts(site.kind))
            .map(|site| site.replacement)
            .collect()
    }

    fn option_findings(builtins: &ObjectRegistry, source: &str) -> Vec<(&'static str, String)> {
        let document = DocumentSnapshot::from_text(source.to_string(), builtins).unwrap();
        document
            .diagnostics()
            .iter()
            .filter(|diagnostic| {
                [
                    catalog::UNKNOWN_OPTION.code,
                    catalog::OPTION_VALUE_MISMATCH.code,
                ]
                .contains(&diagnostic.rule.code)
            })
            .map(|diagnostic| {
                (
                    diagnostic.rule.code.as_str(),
                    diagnostic.finding.message.clone(),
                )
            })
            .collect()
    }

    #[test]
    fn source_declared_options_reject_unknown_names() {
        let builtins = ObjectRegistry::load(crate::catalog::test_corpus());
        let declarations = concat!(
            "f = method(Options => {Verbose => false, Limit => 10})\n",
            "f ZZ := o -> n -> n\n",
            "g = {Depth => 2} >> o -> x -> x\n",
            "h = method(Options => false)\n",
            "h ZZ := o -> n -> n\n",
            "e = method(Options => {})\n",
            "e ZZ := o -> n -> n\n",
        );
        let findings = |call: &str| option_findings(&builtins, &format!("{declarations}{call}\n"));

        assert_eq!(
            findings("f(1, Verbos => true)"),
            [(
                "E16",
                "`Verbos` is not an option of `f`; did you mean `Verbose`?".into()
            )]
        );
        assert_eq!(
            findings("f(1, Limit => 3, Strategy => 2)"),
            [(
                "E16",
                "`Strategy` is not an option of `f`, which accepts `Verbose`, `Limit`".into()
            )]
        );
        assert_eq!(
            findings("g(3, Dept => 1)"),
            [(
                "E16",
                "`Dept` is not an option of `g`; did you mean `Depth`?".into()
            )]
        );
        assert_eq!(
            findings("e(2, Any => 1)"),
            [("E16", "`e` takes no options, so `Any` is rejected".into())]
        );
        assert!(findings("f(1, Verbose => true, Limit => 2)").is_empty());
        assert!(
            findings("h(2, Any => 1)").is_empty(),
            "`Options => false` accepts any name"
        );
        assert!(
            findings("f = 3\nf(1, Bogus => 2)").is_empty(),
            "a rebound name is no longer the method"
        );
    }

    #[test]
    fn catalog_option_tables_check_names_and_values() {
        let corpus = format!(
            "{}\n{}\n{}\n",
            crate::catalog::test_corpus().trim_end(),
            concat!(
                r#"{"kind":"methodFunction","name":"probeTable","package":"$Core$Core","#,
                r#""class":"$Core$MethodFunctionWithOptions","option_passing":"table","#,
                r#""methods":[{"domain":["$Core$ZZ"]}],"options":["#,
                r#"{"key":"Limit","acceptedTypes":["$Core$ZZ","$Core$InfiniteNumber"]},"#,
                r#"{"key":"Strategy","possibleValues":["Fast","Slow"]},"#,
                r#"{"key":"Verify","acceptedTypes":["$Core$Boolean"]}]}"#,
            ),
            concat!(
                r#"{"kind":"methodFunction","name":"probeForwarded","package":"$Core$Core","#,
                r#""class":"$Core$MethodFunctionWithOptions","option_passing":"forwarded","#,
                r#""methods":[{"domain":["$Core$ZZ"],"option_passing":"table","#,
                r#""options":[{"key":"Generic"}]},{"domain":["$Core$String"]}]}"#,
            ),
        );
        let builtins = ObjectRegistry::load(&corpus);
        let findings = |call: &str| option_findings(&builtins, &format!("{call}\n"));

        assert_eq!(
            findings("probeTable(1, Limt => 2)"),
            [(
                "E16",
                "`Limt` is not an option of `probeTable`; did you mean `Limit`?".into()
            )]
        );
        assert!(
            findings("probeTable(1, Limit => infinity, Verify => true, Strategy => Fast)")
                .is_empty()
        );
        assert_eq!(
            findings("probeTable(1, Verify => 3)"),
            [(
                "T06",
                "Option `Verify` of `probeTable` takes `Boolean`, but this value has type `ZZ`"
                    .into()
            )]
        );
        assert_eq!(
            findings("probeTable(1, Strategy => Medium)"),
            [(
                "T06",
                "Option `Strategy` of `probeTable` takes one of `Fast`, `Slow`, not `Medium`"
                    .into()
            )]
        );
        assert!(
            findings("s := Medium\nprobeTable(1, Strategy => s)").is_empty(),
            "a variable's symbol is not judged by its name"
        );

        assert!(findings("probeForwarded(1, Generic => true)").is_empty());
        assert_eq!(
            findings("probeForwarded(1, Generik => true)"),
            [(
                "E16",
                "`Generik` is not an option of `probeForwarded`; did you mean `Generic`?".into()
            )]
        );
        assert!(
            findings("probeForwarded(\"text\", Bogus => 1)").is_empty(),
            "an installation without a recorded table is not checked"
        );
    }

    #[test]
    fn call_domain_mismatch_names_the_failing_argument_types() {
        let builtins = ObjectRegistry::load(crate::catalog::test_corpus());
        for (source, expected) in [
            ("f = method()\nf ZZ := x -> x\nf \"text\"\n", "(String)"),
            ("f = method()\nf ZZ := x -> x\nf(1,2)\n", "(ZZ, ZZ)"),
            (
                "f = method(Binary => true)\nf(ZZ,ZZ) := List => (x,y) -> {x,y}\nf(1,2,3)\n",
                "(List, ZZ)",
            ),
        ] {
            let document = DocumentSnapshot::from_text(source.into(), &builtins).unwrap();
            let diagnostic = document
                .diagnostics()
                .iter()
                .find(|diagnostic| diagnostic.rule.code == catalog::CALL_DOMAIN_MISMATCH.code)
                .expect("call must have disjoint argument types");
            assert_eq!(
                diagnostic.finding.message,
                format!("No known method domain matches these argument types: {expected}")
            );
        }
    }

    #[test]
    fn ring_variables_prefer_indexed_names_over_multicharacter_symbols() {
        let builtins = ObjectRegistry::load(crate::catalog::test_corpus());
        let messages = |source: &str| {
            let document = DocumentSnapshot::from_text(source.to_string(), &builtins)
                .expect("fixture should parse");
            document
                .diagnostics()
                .iter()
                .filter(|reported| reported.rule.code == catalog::RING_VARIABLE_NAMING.code)
                .map(|reported| reported.finding.message.clone())
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
    fn conditionals_in_both_branches_have_no_simplification() {
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

        let simplifications = rewrite_sites(&document, RewriteKind::is_simplification);
        assert!(
            simplifications.is_empty(),
            "swapping two nested conditionals is not a simplification: {simplifications:?}"
        );
    }

    /// What compile-time evaluation proves is reported: matrices that cannot
    /// combine, a division by a zero written out, and a condition computed
    /// from literals; names that merely hold constants are settings, and
    /// inexact or unknown operands prove nothing.
    #[test]
    fn determined_values_prove_failures_and_constant_conditions() {
        let builtins = ObjectRegistry::load(crate::catalog::test_corpus());
        let codes = |text: &str| {
            DocumentSnapshot::from_text(text.to_string(), &builtins)
                .expect("fixture should parse")
                .diagnostics()
                .iter()
                .map(|reported| reported.rule.code)
                .filter(|code| {
                    [
                        catalog::MATRIX_SHAPE_MISMATCH.code,
                        catalog::DIVISION_BY_ZERO.code,
                        catalog::CONSTANT_CONDITION.code,
                    ]
                    .contains(code)
                })
                .collect::<Vec<_>>()
        };
        let matrices = "A = matrix {{1, 2, 3}, {4, 5, 6}}\nB = matrix {{1, 2}, {3, 4}}\n";
        let mismatch = [catalog::MATRIX_SHAPE_MISMATCH.code];

        assert_eq!(codes(&format!("{matrices}A * A\n")), mismatch);
        assert_eq!(codes(&format!("{matrices}A + B\n")), mismatch);
        assert_eq!(codes(&format!("{matrices}B * A\n")), []);
        assert_eq!(codes(&format!("{matrices}transpose A * A\n")), []);
        assert_eq!(codes(&format!("{matrices}A + A\n")), []);
        assert_eq!(codes("1/0\n"), [catalog::DIVISION_BY_ZERO.code]);
        assert_eq!(codes("1.5/0\n"), [catalog::DIVISION_BY_ZERO.code]);
        assert_eq!(codes("1/0.\n"), [], "an inexact zero divides to infinity");
        assert_eq!(codes("1.0/0.\n"), []);
        assert_eq!(codes("x/0\n"), [], "an unknown dividend proves nothing");
        for text in ["if 2^10 > 1000 then 1 else 2\n", "while 1 > 2 do 3\n"] {
            assert_eq!(codes(text), [catalog::CONSTANT_CONDITION.code], "{text:?}");
        }
        for text in [
            "if true then 1\n",
            "n = 4\nif n > 3 then 1\n",
            "DEBUG = false\nif not DEBUG then 1\n",
            "if f x then 1\n",
        ] {
            assert_eq!(codes(text), [], "{text:?}");
        }
    }

    /// A simplification must change the code it rewrites. A condition's span
    /// reaches the `then` keyword and so carries the separating space, which
    /// used to make `not name` compare unequal to itself and propose the
    /// untouched line back.
    #[test]
    fn simplification_is_not_reported_when_the_rewrite_changes_nothing() {
        let text = concat!(
            "f = () -> (\n",
            "    if not completed then lineNumber = previousLine - 1;\n",
            "    g();\n",
            ");\n",
        );
        let builtins = ObjectRegistry::default();
        let document =
            DocumentSnapshot::from_text(text.to_string(), &builtins).expect("fixture should parse");

        assert!(
            document
                .diagnostics()
                .iter()
                .all(|diagnostic| matches!(diagnostic.rule.code,
                    code if code == catalog::UNRESOLVED_VALUE.code || code == catalog::UNINTRODUCED_SYMBOL.code)),
            "an already-simplest condition must not be reported: {:?}",
            document
                .diagnostics()
                .iter()
                .map(|reported| &reported.finding.message)
                .collect::<Vec<_>>()
        );

        assert_eq!(
            rewrite_sites(&document, |kind| kind.is_simplification()
                || kind.is_coalescence()),
            Vec::<String>::new(),
            "a rewrite equal to the original is not a simplification"
        );
    }

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
                .filter(|reported| reported.finding.message.contains("can only be used inside"))
                .map(|reported| reported.finding.message.clone())
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
        let document =
            DocumentSnapshot::from_text("return 5\n".to_string(), &ObjectRegistry::default())
                .expect("fixture should parse");

        assert!(
            document
                .diagnostics()
                .iter()
                .any(|reported| reported.finding.message.contains("can only be used inside")),
            "a top-level return is still a scope violation: {:?}",
            document
                .diagnostics()
                .iter()
                .map(|reported| &reported.finding.message)
                .collect::<Vec<_>>()
        );
    }
}
