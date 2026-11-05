//! Source rewrites shared by diagnostics and their quick fixes: the shorter or
//! clearer form a flagged expression has, as replacement text.
//!
//! [`RewriteSites`] visits typed syntax once and records every rewrite it
//! finds, with the context that decides it: whether parentheses delimit a
//! control expression, whether a conditional's value is discarded, and
//! whether an enclosing expression already rewrites it. Diagnostics report
//! the sites; code actions offer the sites around the cursor.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::ptr;

use m2_syn::nodes::{
    Cell, ElseClause, Expr, ExprAssign, ExprAssignSimple, ExprControl, ExprDebug, ExprIf,
    ExprLambda, ExprLiteral, ExprOp, ExprOpBin, ExprOpBinAdj, ExprProfile, ExprTry, ExprWhile,
    FromInstance, Holder, OfClass, PackedContent, RangeBoundFrom, RangeBoundTo, Statement,
    ThenClause, TokenParseRule, TryFallback, TryFallbackExcept, WhenCondition, TOKEN_PARSE_RULES,
};
use m2_syn::visit::{self, Visit};
use m2_syn::{Spanned, Token};

use crate::analysis::{Analysis, InfixSyntax};
use crate::diagnostics::{catalog, DiagnosticRule};
use crate::object_registry::ObjectRegistry;
use crate::source::{ByteRange, SourceNavigation, SyntaxSource};
use crate::syntax::{AssignmentSyntax, ClauseSyntax, ControlSyntax, ExpressionSyntax};

mod boolean_comparison;
mod composition;
mod unary_call;

use boolean_comparison::BooleanComparison;
use composition::MappingComposition;
use unary_call::UnarySymbolCall;

/// What a rewrite site simplifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RewriteKind {
    /// `x .3`, an application to a float literal, meant as `x#3`.
    AmbiguousFloatMemberAccess,
    /// Parentheses around a control expression its keyword already delimits.
    RedundantControlParentheses,
    /// A null test that `??=` expresses.
    CoalescingAssignment,
    /// A null test that `??` expresses.
    Coalescence,
    /// An unused null guard whose branch returns from the function.
    NullReturnGuard,
    /// Mapping a callback consisting of two composed unary function calls.
    MappingComposition,
    /// A Boolean expression strictly compared with `true` or `false`.
    BooleanComparison,
    /// A callback doing nothing but forwarding its parameter to a named function.
    ForwardingLambda,
    /// A callback forwarding its parameter after a fixed first argument.
    PartialApplicationLambda,
    /// An `if` with a `null` branch.
    NullBranch,
    /// A `try` repeating its value or falling back to `null`.
    TrySimplification,
    /// An `if` whose negated condition simplifies.
    IfCondition,
    /// Nested conditionals that flatten into an `else if` chain.
    ElseIfChain,
}

impl RewriteKind {
    /// Whether the rewrite shortens an expression, reported as one
    /// simplifiable expression.
    pub fn is_simplification(self) -> bool {
        matches!(
            self,
            Self::NullReturnGuard
                | Self::MappingComposition
                | Self::BooleanComparison
                | Self::ForwardingLambda
                | Self::PartialApplicationLambda
                | Self::NullBranch
                | Self::TrySimplification
                | Self::IfCondition
                | Self::ElseIfChain
        )
    }

    /// The independently configurable diagnostic reporting this rewrite.
    pub fn rule(self) -> &'static DiagnosticRule {
        match self {
            Self::AmbiguousFloatMemberAccess => &catalog::AMBIGUOUS_FLOAT_MEMBER_ACCESS,
            Self::RedundantControlParentheses => &catalog::REDUNDANT_CONTROL_PARENTHESES,
            Self::CoalescingAssignment | Self::Coalescence => &catalog::PREFER_COALESCENCE,
            Self::NullBranch => &catalog::UNNECESSARY_NULL_BRANCH,
            Self::NullReturnGuard => &catalog::NULL_RETURN_GUARD,
            Self::MappingComposition => &catalog::MAPPED_COMPOSITION,
            Self::BooleanComparison => &catalog::REDUNDANT_BOOLEAN_COMPARISON,
            Self::ForwardingLambda => &catalog::FORWARDING_LAMBDA,
            Self::PartialApplicationLambda => &catalog::PARTIAL_APPLICATION_LAMBDA,
            Self::TrySimplification => &catalog::SIMPLIFIABLE_TRY,
            Self::IfCondition => &catalog::SIMPLIFIABLE_IF_CONDITION,
            Self::ElseIfChain => &catalog::FLATTENABLE_IF_CHAIN,
        }
    }

    /// Whether the rewrite turns a null test into a coalescence.
    pub fn is_coalescence(self) -> bool {
        matches!(self, Self::CoalescingAssignment | Self::Coalescence)
    }
}

/// One rewrite of one expression.
#[derive(Debug, Clone)]
pub struct RewriteSite<'ast> {
    /// What the rewrite simplifies.
    pub kind: RewriteKind,
    /// The rewritten expression.
    pub expression: &'ast Expr,
    /// Bytes of the construct a rewrite inside it belongs to: the control
    /// expression whose parentheses are redundant.
    pub owner: Option<ByteRange>,
    /// Source text replacing the expression.
    pub replacement: String,
}

/// Collects the rewrite sites of typed syntax in source order.
pub struct RewriteSites<'ast, 'source, Source: ?Sized> {
    /// Source text and coordinates of the visited revision.
    source: &'source Source,
    /// Expressions in a control slot, with the construct owning the slot.
    control_slots: Vec<(*const Expr, &'ast dyn Spanned)>,
    /// Expressions whose value nothing uses: a cell's or a statement's.
    discarded: HashSet<*const Expr>,
    /// Conditionals an enclosing coalescence already rewrites.
    covered: HashSet<*const Expr>,
    /// Sites found so far.
    sites: Vec<RewriteSite<'ast>>,
    /// Parent of the current expression, for grouping an operator replacement.
    parent: Option<&'ast Expr>,
}

impl<'ast, 'source, Source: SourceNavigation + ?Sized> RewriteSites<'ast, 'source, Source> {
    /// The rewrite sites of the given cells, in source order.
    pub fn collect(
        cells: impl IntoIterator<Item = &'ast Cell>,
        source: &'source Source,
    ) -> Vec<RewriteSite<'ast>> {
        let mut collector = Self {
            source,
            control_slots: Vec::new(),
            discarded: HashSet::new(),
            covered: HashSet::new(),
            sites: Vec::new(),
            parent: None,
        };
        for cell in cells {
            collector.visit_cell(cell);
        }
        collector.sites
    }

    /// Collects rewrites and validates those requiring name and type facts.
    /// Use this entry point for diagnostics and actions so both offer the same
    /// edits, including when a hint is suppressed or fixing the whole file.
    pub fn collect_analyzed(
        cells: impl IntoIterator<Item = &'ast Cell>,
        source: &'source Source,
        analysis: &Analysis,
        knowledge: &ObjectRegistry,
    ) -> Vec<RewriteSite<'ast>>
    where
        Source: SyntaxSource,
    {
        Self::collect(cells, source)
            .into_iter()
            .filter(|site| match site.kind {
                RewriteKind::MappingComposition => MappingComposition::of(site.expression)
                    .is_some_and(|mapping| mapping.is_applicable(analysis, source, knowledge)),
                RewriteKind::BooleanComparison => BooleanComparison::of(site.expression)
                    .is_some_and(|comparison| {
                        comparison.is_applicable(analysis, source, knowledge)
                    }),
                _ => true,
            })
            .collect()
    }

    /// Source text of typed syntax.
    fn text(&self, syntax: &(impl Spanned + ?Sized)) -> &'source str {
        self.source.text_for_syntax(syntax).unwrap_or_default()
    }

    /// Records a rewrite unless it reproduces the expression.
    fn push(
        &mut self,
        kind: RewriteKind,
        expression: &'ast Expr,
        owner: Option<&'ast dyn Spanned>,
        replacement: Option<String>,
    ) {
        let owner = owner.and_then(|owner| self.source.syntax_bytes(owner));
        let Some(replacement) =
            replacement.filter(|replacement| replacement.trim() != self.text(expression).trim())
        else {
            return;
        };
        self.sites.push(RewriteSite {
            kind,
            expression,
            owner,
            replacement,
        });
    }

    /// Marks `value` as the slot a control construct delimits.
    fn control_slot(&mut self, value: &'ast Expr, owner: &'ast dyn Spanned) {
        self.control_slots.push((ptr::from_ref(value), owner));
    }

    /// The rewrites of one expression, in the order diagnostics report them.
    fn rewrite(&mut self, expression: &'ast Expr) {
        let replacement = self.forwarding_lambda(expression);
        self.push(RewriteKind::ForwardingLambda, expression, None, replacement);
        let replacement = self.partial_application_lambda(expression);
        self.push(
            RewriteKind::PartialApplicationLambda,
            expression,
            None,
            replacement,
        );
        let replacement = self.boolean_comparison(expression);
        self.push(
            RewriteKind::BooleanComparison,
            expression,
            None,
            replacement,
        );
        let replacement = self.mapping_composition(expression);
        self.push(
            RewriteKind::MappingComposition,
            expression,
            None,
            replacement,
        );
        if let Expr::Op(ExprOp::Bin(ExprOpBin::Adj(call))) = expression {
            let replacement = self.ambiguous_float_member_access(call);
            self.push(
                RewriteKind::AmbiguousFloatMemberAccess,
                expression,
                None,
                replacement,
            );
        }
        if let Some(&(_, owner)) = self
            .control_slots
            .iter()
            .find(|(slot, _)| ptr::eq(*slot, expression))
        {
            if let Expr::Holder(_) = expression {
                let replacement = expression
                    .unparenthesized()
                    .map(|inner| self.text(inner).to_string());
                self.push(
                    RewriteKind::RedundantControlParentheses,
                    expression,
                    Some(owner),
                    replacement,
                );
            }
        }
        let coalescence = self.coalescence(expression);
        let coalesces = coalescence.is_some();
        if let Some((kind, replacement)) = coalescence {
            self.push(kind, expression, None, Some(replacement));
        }
        if coalesces {
            return;
        }
        match expression {
            Expr::If(conditional) => {
                let discarded = self.discarded.contains(&ptr::from_ref(expression));
                let rewrites = [
                    (
                        RewriteKind::NullReturnGuard,
                        self.null_return_guard(conditional, discarded),
                    ),
                    (RewriteKind::NullBranch, self.null_branch(conditional)),
                    (RewriteKind::IfCondition, self.if_condition(conditional)),
                    (RewriteKind::ElseIfChain, self.else_if_chain(conditional)),
                ];
                for (kind, replacement) in rewrites {
                    self.push(kind, expression, None, replacement);
                }
            }
            Expr::Try(guard) => {
                let replacement = self.try_statement(guard);
                self.push(
                    RewriteKind::TrySimplification,
                    expression,
                    None,
                    replacement,
                );
            }
            _ => {}
        }
    }

    /// `x#3` for `x .3`, an application to a float literal written right
    /// after a name.
    fn ambiguous_float_member_access(&self, call: &ExprOpBinAdj) -> Option<String> {
        let name = call.left_operand.symbol_name()?;
        let Expr::Literal(ExprLiteral::Float(float)) = call.right_operand.as_ref() else {
            return None;
        };
        let adjacent = self.source.syntax_bytes(call.left_operand.as_ref())?.end
            == self.source.syntax_bytes(float)?.start;
        let fractional = self.text(float).strip_prefix('.')?;
        (adjacent && !fractional.is_empty() && fractional.chars().all(|ch| ch.is_ascii_digit()))
            .then(|| format!("{name}#{fractional}"))
    }

    /// The named function supplied by `x -> f x` or `x -> f(x)`. A direct
    /// variadic parameter forwards the same argument value as the original
    /// function. Fixed parameter lists, computed callees, type constructors,
    /// extra arguments, and statement-bearing holders keep their wrappers.
    fn forwarding_lambda(&self, expression: &Expr) -> Option<String> {
        let Expr::Lambda(ExprLambda::Variadic(lambda)) = expression else {
            return None;
        };
        let call = UnarySymbolCall::of(&lambda.body)?;
        let function = call.function.symbol_name()?;
        (call.argument.symbol_name()? == lambda.argument.text()
            && function != lambda.argument.text()
            && UnarySymbolCall::has_function_name(call.function))
        .then(|| self.text(call.function).trim().to_string())
    }

    /// Partial application for `y -> f(x, y)`, with a fixed symbol argument.
    /// A symbol callee and fixed symbol avoid moving computed expressions out
    /// of the callback. Neither may refer to the disappearing parameter.
    fn partial_application_lambda(&self, expression: &Expr) -> Option<String> {
        let Expr::Lambda(ExprLambda::Variadic(lambda)) = expression else {
            return None;
        };
        let call = UnarySymbolCall::application(&lambda.body)?;
        let function = UnarySymbolCall::grouped_value(&call.left_operand)?;
        if !UnarySymbolCall::has_function_name(function) {
            return None;
        }
        let arguments = UnarySymbolCall::grouped_value(&call.right_operand)?.arguments();
        let [Some(fixed), Some(forwarded)] = arguments.as_slice() else {
            return None;
        };
        let fixed = UnarySymbolCall::grouped_value(fixed)?;
        let forwarded = UnarySymbolCall::grouped_value(forwarded)?;
        let parameter = lambda.argument.text();
        (forwarded.symbol_name()? == parameter
            && fixed.symbol_name()? != parameter
            && function.symbol_name()? != parameter)
            .then(|| format!("{}_{}", self.text(function).trim(), self.text(fixed).trim()))
    }

    /// A Boolean comparison as its subject or its negation. Keep the subject's
    /// grouping when dropping the comparison, and reuse comparison inversion
    /// and double-negation removal when negating. An enclosing non-Boolean
    /// operator may need parentheses around the shorter expression.
    fn boolean_comparison(&self, expression: &Expr) -> Option<String> {
        let comparison = BooleanComparison::of(expression)?;
        if !comparison.negated {
            return Some(self.text(comparison.subject).trim().to_string());
        }
        let replacement = self.negated_condition(comparison.subject.unparenthesized()?);
        let needs_parentheses = self.parent.is_some_and(|parent| {
            matches!(parent, Expr::Op(_))
                && parent.infix_operands().is_none_or(|operands| {
                    !matches!(
                        operands.operator,
                        <Token![and]>::SPELLING | <Token![or]>::SPELLING
                    )
                })
        });
        Some(if needs_parentheses {
            format!("({replacement})")
        } else {
            replacement
        })
    }

    /// Operator notation for mapping a callback made of two unary calls.
    /// Parentheses preserve an input expression's grouping and prevent an
    /// enclosing call or operator from capturing the replacement's `/`.
    fn mapping_composition(&self, expression: &Expr) -> Option<String> {
        let mapping = MappingComposition::of(expression)?;
        let collection = mapping.collection;
        let grouped = collection.infix_operands().is_some()
            || starts_with_keyword(collection)
            || matches!(collection, Expr::Lambda(_));
        let replacement = format!(
            "{} / {} @@ {}",
            self.parenthesized_if(collection, grouped),
            self.text(mapping.outer).trim(),
            self.text(mapping.inner).trim(),
        );
        let needs_parentheses = self.parent.is_some_and(|parent| {
            !matches!(parent, Expr::Assign(_) | Expr::Lambda(_) | Expr::Holder(_))
        });
        Some(if needs_parentheses {
            format!("({replacement})")
        } else {
            replacement
        })
    }

    /// The `??` form of a null test at `expression`:
    /// - `x = if x === null then v else x` becomes `x ??= v`;
    /// - `if x === null then x = v`, whose own value is unused, becomes `x ??= v`;
    /// - `if x === null then y = v else y = x` becomes `y = x ?? v`;
    /// - `if x === null then v else x` becomes `x ?? v`;
    /// - `if (try e) === null then v else e` and `try (if e === null then v else e)
    ///   else v` become `e ?? v`, since `??` turns errors in its left side into the
    ///   fallback just as the `try` does.
    ///
    /// A conditional an enclosing rewrite covers carries no rewrite of its own.
    fn coalescence(&mut self, expression: &'ast Expr) -> Option<(RewriteKind, String)> {
        match expression {
            Expr::Try(guard) => {
                let (conditional, replacement) = self.guarded_conditional(guard)?;
                self.covered.insert(ptr::from_ref(conditional));
                Some((RewriteKind::Coalescence, replacement))
            }
            Expr::Assign(ExprAssign::Simple(ExprAssignSimple::Global(assignment))) => {
                let conditional @ Expr::If(test) = assignment.value.as_ref() else {
                    return None;
                };
                let (subject, fallback) = self.coalescence_parts(test, SubjectErrors::Propagate)?;
                let target = assignment.variable.text();
                (target == self.text(subject)).then(|| {
                    self.covered.insert(ptr::from_ref(conditional));
                    (
                        RewriteKind::CoalescingAssignment,
                        format!("{target} ??= {}", self.text(fallback)),
                    )
                })
            }
            Expr::If(conditional) if !self.covered.contains(&ptr::from_ref(expression)) => {
                let discarded = self.discarded.contains(&ptr::from_ref(expression));
                self.coalescing_assignment(conditional, discarded)
                    .or_else(|| {
                        let (subject, fallback) =
                            self.coalescence_parts(conditional, SubjectErrors::Propagate)?;
                        Some((
                            RewriteKind::Coalescence,
                            self.coalescence_expression(subject, fallback),
                        ))
                    })
            }
            _ => None,
        }
    }

    /// `try (if e === null then v else e) else v`, whose `try` turns errors in `e`
    /// into the fallback as `??` does. Its `else` must repeat the fallback, or an
    /// error would yield something else. Returns the conditional with the
    /// replacement.
    fn guarded_conditional(&self, guard: &'ast ExprTry) -> Option<(&'ast Expr, String)> {
        if guard.on_success.is_some() {
            return None;
        }
        let Some(TryFallback::Else(error_fallback)) = &guard.fallback else {
            return None;
        };
        let conditional = guard.value.unparenthesized()?;
        let Expr::If(test) = conditional else {
            return None;
        };
        let (subject, fallback) = self.coalescence_parts(test, SubjectErrors::Caught)?;
        self.same_expression(fallback, error_fallback.clause_value())
            .then(|| (conditional, self.coalescence_expression(subject, fallback)))
    }

    /// `subject ?? fallback`, each side parenthesized where it would otherwise
    /// regroup around `??`.
    fn coalescence_expression(&self, subject: &Expr, fallback: &Expr) -> String {
        format!(
            "{} ?? {}",
            self.coalescence_subject(subject),
            self.coalescence_operand(fallback)
        )
    }

    /// The `??` form of an `if` that assigns a fallback when its subject is null.
    fn coalescing_assignment(
        &self,
        conditional: &ExprIf,
        discarded: bool,
    ) -> Option<(RewriteKind, String)> {
        let NullTest {
            subject,
            null_branch,
        } = self.null_test(conditional, SubjectErrors::Propagate)?;
        let then_value = conditional.then_clause.clause_value();
        let Some(else_clause) = &conditional.else_clause else {
            // `if x === null then x = v` yields `null` where `x ??= v` yields `x`,
            // so the two agree only when nothing uses the `if`'s own value.
            let (target, operator, fallback) = symbol_assignment(then_value)?;
            return (null_branch == NullBranch::Then
                && discarded
                && operator == <Token![=]>::SPELLING
                && target == self.text(subject))
            .then(|| {
                (
                    RewriteKind::CoalescingAssignment,
                    format!("{} ??= {}", self.text(subject), self.text(fallback)),
                )
            });
        };
        let else_value = else_clause.clause_value();
        let (fallback_branch, subject_branch) = match null_branch {
            NullBranch::Then => (then_value, else_value),
            NullBranch::Else => (else_value, then_value),
        };
        let (target, operator, fallback) = symbol_assignment(fallback_branch)?;
        let (repeated_target, repeated_operator, repeated_subject) =
            symbol_assignment(subject_branch)?;
        if repeated_target != target
            || repeated_operator != operator
            || !self.same_expression(repeated_subject, subject)
        {
            return None;
        }
        if operator == <Token![=]>::SPELLING && target == self.text(subject) {
            return Some((
                RewriteKind::CoalescingAssignment,
                format!("{} ??= {}", self.text(subject), self.text(fallback)),
            ));
        }
        Some((
            RewriteKind::Coalescence,
            format!(
                "{target} {operator} {} ?? {}",
                self.coalescence_subject(subject),
                self.coalescence_operand(fallback)
            ),
        ))
    }

    /// A null guard returning early, with no `else` and an unused result.
    /// A non-null subject falls through in both forms, but only the original
    /// yields `null`, so a caller consuming that result must keep the `if`.
    /// Reuse the subject's error restrictions because `??` also catches errors.
    fn null_return_guard(&self, conditional: &ExprIf, discarded: bool) -> Option<String> {
        if !discarded || conditional.else_clause.is_some() {
            return None;
        }
        let NullTest {
            subject,
            null_branch,
        } = self.null_test(conditional, SubjectErrors::Propagate)?;
        let fallback = conditional.then_clause.clause_value().unparenthesized()?;
        (null_branch == NullBranch::Then
            && matches!(fallback, Expr::Control(ExprControl::Return(_))))
        .then(|| self.coalescence_expression(subject, fallback))
    }

    /// `fallback` as the right operand of `??`, parenthesized when it binds more
    /// loosely than `??` and would otherwise take the coalescence as its own left
    /// operand, as `x ?? y -> y` reads `(x ?? y) -> y`.
    fn coalescence_operand(&self, fallback: &Expr) -> String {
        let binds_looser = matches!(fallback, Expr::Lambda(_))
            || precedence_relative_to_coalescence(fallback).is_some_and(Ordering::is_lt);
        self.parenthesized_if(fallback, binds_looser)
    }

    /// `subject` as the left operand of `??`, parenthesized unless it binds more
    /// tightly: `??` groups to the right, so `a or b ?? v` reads `a or (b ?? v)`,
    /// and a keyword expression such as `try e` would take `?? v` as part of `e`.
    fn coalescence_subject(&self, subject: &Expr) -> String {
        let binds_tighter = !matches!(subject, Expr::Lambda(_))
            && !starts_with_keyword(subject)
            && precedence_relative_to_coalescence(subject).is_none_or(Ordering::is_gt);
        self.parenthesized_if(subject, !binds_tighter)
    }

    /// The trimmed text of `expression`, in parentheses when `parenthesize` holds.
    fn parenthesized_if(&self, expression: &Expr, parenthesize: bool) -> String {
        let text = self.text(expression).trim();
        if parenthesize {
            format!("({text})")
        } else {
            text.to_string()
        }
    }

    /// Whether `left` and `right` spell the same expression, ignoring enclosing
    /// parentheses.
    fn same_expression(&self, left: &Expr, right: &Expr) -> bool {
        left.unparenthesized()
            .zip(right.unparenthesized())
            .is_some_and(|(left, right)| self.text(left).trim() == self.text(right).trim())
    }

    /// The expression an `if` compares with `null` using `===` or `=!=`. `??`
    /// treats an error in its left side like `null`, so the subject must be one
    /// whose errors are handled the same way: a symbol, which never fails to
    /// evaluate, an expression under a bare `try` (`(try f x) === null`), or any
    /// expression when a `try` around the conditional catches its errors.
    fn null_test<'test>(
        &self,
        conditional: &'test ExprIf,
        subject_errors: SubjectErrors,
    ) -> Option<NullTest<'test>> {
        let operands = conditional.condition.infix_operands()?;
        let tested = match (is_null(operands.left), is_null(operands.right)) {
            (true, false) => operands.right,
            (false, true) => operands.left,
            _ => return None,
        };
        let null_branch = if operands.operator == <Token![===]>::SPELLING {
            NullBranch::Then
        } else if operands.operator == <Token![=!=]>::SPELLING {
            NullBranch::Else
        } else {
            return None;
        };
        let subject = match guarded_value(tested) {
            Some(guarded) => guarded,
            None if subject_errors == SubjectErrors::Caught
                || matches!(tested, Expr::Symbol(_)) =>
            {
                tested.unparenthesized()?
            }
            None => return None,
        };
        Some(NullTest {
            subject,
            null_branch,
        })
    }

    /// The subject and fallback of a conditional coalescence expression.
    fn coalescence_parts<'test>(
        &self,
        conditional: &'test ExprIf,
        subject_errors: SubjectErrors,
    ) -> Option<(&'test Expr, &'test Expr)> {
        let NullTest {
            subject,
            null_branch,
        } = self.null_test(conditional, subject_errors)?;
        let then_value = conditional.then_clause.clause_value();
        let else_value = conditional.else_clause.as_ref()?.clause_value();
        let (fallback, repeated_subject) = match null_branch {
            NullBranch::Then => (then_value, else_value),
            NullBranch::Else => (else_value, then_value),
        };
        self.same_expression(repeated_subject, subject)
            .then_some((subject, fallback))
    }

    /// An `if` without its `null` branch: `if c then v` for `if c then v else
    /// null`, and the negated condition for `if c then null else v`.
    fn null_branch(&self, conditional: &ExprIf) -> Option<String> {
        let then_branch = conditional.then_clause.clause_value();
        let else_branch = conditional.else_clause.as_ref()?.clause_value();
        if is_null(else_branch) {
            return Some(format!(
                "if {} then {}",
                self.text(conditional.condition.as_ref()).trim_end(),
                self.text(then_branch)
            ));
        }
        is_null(then_branch).then(|| {
            format!(
                "if {} then {}",
                self.negated_condition(&conditional.condition),
                self.text(else_branch),
            )
        })
    }

    /// A `try` without a `then` repeating its value, or without an `else null`.
    fn try_statement(&self, guard: &ExprTry) -> Option<String> {
        let condition_text = self.text(guard.value.as_ref());
        let consequence_text = guard
            .on_success
            .as_ref()
            .map(|clause| self.text(clause.clause_value()));
        let alternative = match &guard.fallback {
            Some(TryFallback::Else(clause)) => Some(clause.clause_value()),
            Some(TryFallback::Except(_)) => return None,
            None => None,
        };

        if consequence_text == Some(condition_text) && alternative.is_none() {
            return Some(format!("try {condition_text}"));
        }
        alternative
            .filter(|alternative| is_null(alternative))
            .map(|_| {
                let mut simplified = format!("try {condition_text}");
                if let Some(consequence_text) = consequence_text {
                    simplified.push_str(" then ");
                    simplified.push_str(consequence_text);
                }
                simplified
            })
    }

    /// An `if` whose condition loses a negation: `if a != b then x` for `if not
    /// (a == b) then x`.
    fn if_condition(&self, conditional: &ExprIf) -> Option<String> {
        let simplified = self.simplified_condition(&conditional.condition)?;
        let mut replacement = format!(
            "if {} then {}",
            simplified,
            self.text(conditional.then_clause.clause_value())
        );
        if let Some(else_clause) = &conditional.else_clause {
            replacement.push(' ');
            replacement.push_str(self.text(else_clause));
        }
        Some(replacement)
    }

    /// Nested conditionals as one `else if` chain.
    fn else_if_chain(&self, conditional: &ExprIf) -> Option<String> {
        self.flatten_then_if_chain(conditional)
            .or_else(|| self.flatten_parenthesized_else_if_chain(conditional))
    }

    /// Moves a conditional nested in the `then` branch to the `else` position by
    /// negating the condition. When the `else` branch is itself a conditional, the
    /// swap only trades one nested branch for the other: the result is flattenable
    /// back into the original, so the quick fix would oscillate between the two
    /// forms without ever clearing its diagnostic.
    fn flatten_then_if_chain(&self, conditional: &ExprIf) -> Option<String> {
        let Expr::If(nested) = unwrap_parentheses(conditional.then_clause.clause_value()) else {
            return None;
        };
        let else_branch = conditional.else_clause.as_ref()?.clause_value();
        if matches!(unwrap_parentheses(else_branch), Expr::If(_)) {
            return None;
        }
        let nested_replacement = self
            .else_if_chain(nested)
            .unwrap_or_else(|| self.text(nested).to_string());
        Some(format!(
            "if {} then {} else {}",
            self.negated_condition(&conditional.condition),
            self.text(else_branch),
            nested_replacement
        ))
    }

    /// Flattens a parenthesized conditional in the `else` branch into the chain.
    fn flatten_parenthesized_else_if_chain(&self, conditional: &ExprIf) -> Option<String> {
        let else_branch = conditional.else_clause.as_ref()?.clause_value();
        let nested_branch = unwrap_parentheses(else_branch);
        let Expr::If(nested) = nested_branch else {
            return None;
        };
        let nested_replacement = self.else_if_chain(nested);
        let removes_parentheses = !ptr::eq(nested_branch, else_branch);
        if !removes_parentheses && nested_replacement.is_none() {
            return None;
        }
        let replacement = nested_replacement.unwrap_or_else(|| self.text(nested).to_string());
        let start = self.source.syntax_bytes(conditional)?.start;
        let branch = self.source.syntax_bytes(else_branch)?;
        let mut flattened = self.text(conditional).to_string();
        flattened.replace_range(branch.start - start..branch.end - start, &replacement);
        Some(flattened)
    }

    /// Shorter condition text preserving the condition's Boolean value: a
    /// negated comparison inverted, or a double negation cancelled.
    fn simplified_condition(&self, condition: &Expr) -> Option<String> {
        let original = self.text(condition).trim();
        let inner = unwrap_parentheses(negated_operand(condition)?);
        // Only invert a comparison or cancel a second `not`. Adding grouping or
        // normalizing whitespace around an unchanged negation is not simpler.
        let inverts_comparison = inner
            .infix_operands()
            .and_then(|operands| negated_comparison(operands.operator))
            .is_some();
        if !inverts_comparison && negated_operand(inner).is_none() {
            return None;
        }
        let simplified = self.negated_condition(inner);
        (simplified != original).then_some(simplified)
    }

    /// The negation of a condition, without the whitespace a condition's span
    /// carries up to the `then` keyword; callers supply their own separators.
    fn negated_condition(&self, condition: &Expr) -> String {
        if let Some(operand) = negated_operand(condition) {
            return self.text(operand).trim_end().to_string();
        }
        if let Some(operands) = condition.infix_operands() {
            if let Some(negated) = negated_comparison(operands.operator) {
                return format!(
                    "{} {} {}",
                    self.text(operands.left),
                    negated,
                    self.text(operands.right).trim_end()
                );
            }
        }
        let text = self.text(condition).trim_end();
        let needs_parentheses = condition.infix_operands().is_some_and(|operands| {
            binding_precedence(operands.operator)
                .zip(binding_precedence(<Token![not]>::SPELLING))
                .is_none_or(|(operand, negation)| operand <= negation)
        });
        if needs_parentheses {
            format!("not ({text})")
        } else {
            format!("not {text}")
        }
    }
}

impl<'ast, Source: SourceNavigation + ?Sized> Visit<'ast> for RewriteSites<'ast, '_, Source> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        self.rewrite(expression);
        let parent = self.parent.replace(expression);
        visit::visit_expr(self, expression);
        self.parent = parent;
    }

    fn visit_cell(&mut self, cell: &'ast Cell) {
        if let PackedContent::Expr(value) = &cell.content {
            self.discarded.insert(ptr::from_ref(value.as_ref()));
        }
        visit::visit_cell(self, cell);
    }

    fn visit_statement(&mut self, statement: &'ast Statement) {
        if let PackedContent::Expr(value) = &statement.content {
            self.discarded.insert(ptr::from_ref(value.as_ref()));
        }
        visit::visit_statement(self, statement);
    }

    fn visit_expr_if(&mut self, node: &'ast ExprIf) {
        self.control_slot(&node.condition, node);
        visit::visit_expr_if(self, node);
    }

    fn visit_expr_while(&mut self, node: &'ast ExprWhile) {
        self.control_slot(&node.condition, node);
        visit::visit_expr_while(self, node);
    }

    fn visit_when_condition(&mut self, node: &'ast WhenCondition) {
        self.control_slot(&node.condition, node);
        visit::visit_when_condition(self, node);
    }

    fn visit_then_clause(&mut self, node: &'ast ThenClause) {
        self.control_slot(&node.expr, node);
        visit::visit_then_clause(self, node);
    }

    fn visit_else_clause(&mut self, node: &'ast ElseClause) {
        self.control_slot(&node.expr, node);
        visit::visit_else_clause(self, node);
    }

    fn visit_expr_try(&mut self, node: &'ast ExprTry) {
        self.control_slot(&node.value, node);
        visit::visit_expr_try(self, node);
    }

    fn visit_try_fallback_except(&mut self, node: &'ast TryFallbackExcept) {
        self.control_slot(&node.value, node);
        visit::visit_try_fallback_except(self, node);
    }

    fn visit_of_class(&mut self, node: &'ast OfClass) {
        self.control_slot(&node.parent, node);
        visit::visit_of_class(self, node);
    }

    fn visit_from_instance(&mut self, node: &'ast FromInstance) {
        self.control_slot(&node.instance, node);
        visit::visit_from_instance(self, node);
    }

    fn visit_range_bound_from(&mut self, node: &'ast RangeBoundFrom) {
        self.control_slot(&node.value, node);
        visit::visit_range_bound_from(self, node);
    }

    fn visit_range_bound_to(&mut self, node: &'ast RangeBoundTo) {
        self.control_slot(&node.value, node);
        visit::visit_range_bound_to(self, node);
    }

    fn visit_expr_control(&mut self, node: &'ast ExprControl) {
        if let Some(value) = node.transferred_value() {
            self.control_slot(value, node);
        }
        visit::visit_expr_control(self, node);
    }

    fn visit_expr_debug(&mut self, node: &'ast ExprDebug) {
        let value = match node {
            ExprDebug::Test(debug) => debug.value.as_deref(),
            ExprDebug::Step(debug) => debug.value.as_deref(),
            ExprDebug::Finish(debug) => debug.value.as_deref(),
            ExprDebug::Shield(debug) => debug.value.as_deref(),
            ExprDebug::Breakpoint(debug) => debug.value.as_deref(),
        };
        if let Some(value) = value {
            self.control_slot(value, node);
        }
        visit::visit_expr_debug(self, node);
    }

    fn visit_expr_profile(&mut self, node: &'ast ExprProfile) {
        let value = match node {
            ExprProfile::ElapsedTime(profile) => &profile.value,
            ExprProfile::ElapsedTiming(profile) => &profile.value,
            ExprProfile::Profile(profile) => &profile.value,
            ExprProfile::Time(profile) => &profile.value,
            ExprProfile::Timing(profile) => &profile.value,
        };
        self.control_slot(value, node);
        visit::visit_expr_profile(self, node);
    }
}

/// Which branch of an `if` runs when its subject is null.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NullBranch {
    /// The conditional's then branch.
    Then,
    /// The conditional's else branch.
    Else,
}

/// Whether errors raised while evaluating a null test's subject are caught by
/// a `try` around the whole conditional.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubjectErrors {
    /// The failure propagates beyond this expression.
    Propagate,
    /// The failure is handled by this expression's fallback.
    Caught,
}

/// What an `if` compares with `null`, and the branch that runs when it is null.
#[derive(Debug, Clone, Copy)]
struct NullTest<'ast> {
    /// Expression whose behavior the rewrite preserves.
    subject: &'ast Expr,
    /// Branch returning the null value in a conditional rewrite.
    null_branch: NullBranch,
}

/// Whether `expression` is the symbol `null`.
fn is_null(expression: &Expr) -> bool {
    expression.symbol_name() == Some("null")
}

/// The target name, operator, and value of `x = v` or `x := v`.
fn symbol_assignment(expression: &Expr) -> Option<(&str, &'static str, &Expr)> {
    let Expr::Assign(ExprAssign::Simple(assignment)) = expression else {
        return None;
    };
    match assignment {
        ExprAssignSimple::Global(assignment) => Some((
            assignment.variable.text(),
            <Token![=]>::SPELLING,
            assignment.value.as_ref(),
        )),
        ExprAssignSimple::Local(assignment) => Some((
            assignment.variable.text(),
            <Token![:=]>::SPELLING,
            assignment.value.as_ref(),
        )),
    }
}

/// The expression a bare `try e` evaluates, seen through parentheses; errors
/// in it yield `null`.
fn guarded_value(expression: &Expr) -> Option<&Expr> {
    match expression.unparenthesized()? {
        Expr::Try(guard) if guard.on_success.is_none() && guard.fallback.is_none() => {
            guard.value.unparenthesized()
        }
        _ => None,
    }
}

/// The operand of a `not`.
fn negated_operand(expression: &Expr) -> Option<&Expr> {
    match expression {
        Expr::Op(ExprOp::Prefix(prefix))
            if prefix.operator.spelling() == <Token![not]>::SPELLING =>
        {
            Some(&prefix.operand)
        }
        _ => None,
    }
}

/// The single value parentheses hold, or the expression itself.
fn unwrap_parentheses(expression: &Expr) -> &Expr {
    match expression {
        Expr::Holder(Holder {
            muted,
            value: Some(value),
            ..
        }) if muted.is_empty() => value,
        _ => expression,
    }
}

/// Whether an expression starts with a keyword, which would take a following
/// operator into its own operand.
fn starts_with_keyword(expression: &Expr) -> bool {
    match expression {
        Expr::If(_)
        | Expr::For(_)
        | Expr::While(_)
        | Expr::Try(_)
        | Expr::New(_)
        | Expr::Control(_)
        | Expr::Catch(_)
        | Expr::Throw(_)
        | Expr::Trap(_)
        | Expr::Debug(_)
        | Expr::Profile(_) => true,
        Expr::Op(ExprOp::Prefix(prefix)) => prefix
            .operator
            .spelling()
            .starts_with(|first: char| first.is_alphabetic()),
        _ => false,
    }
}

/// How the binary operator at the top of `expression` binds compared with
/// `??`; `None` when it is no binary expression with a known precedence.
fn precedence_relative_to_coalescence(expression: &Expr) -> Option<Ordering> {
    let coalescence = binding_precedence(<Token![??]>::SPELLING)?;
    let operator = match expression {
        Expr::Assign(assignment) => match assignment.parts().operator {
            crate::syntax::AssignmentOperator::Global => <Token![=]>::SPELLING,
            crate::syntax::AssignmentOperator::Local => <Token![:=]>::SPELLING,
            crate::syntax::AssignmentOperator::Evaluated => <Token![<-]>::SPELLING,
            _ => return None,
        },
        Expr::Option(_) => <Token![=>]>::SPELLING,
        _ => expression.infix_operands()?.operator,
    };
    Some(binding_precedence(operator)?.cmp(&coalescence))
}

/// The parsing precedence of the operator spelled `spelling`.
fn binding_precedence(spelling: &str) -> Option<u8> {
    let index = TOKEN_PARSE_RULES
        .binary_search_by_key(&spelling, |(token, _)| token)
        .ok()?;
    Some(match TOKEN_PARSE_RULES[index].1 {
        TokenParseRule::Precedence(precedence)
        | TokenParseRule::Postfix(precedence)
        | TokenParseRule::Binary(precedence, _)
        | TokenParseRule::Prefix(precedence, _)
        | TokenParseRule::PrefixBinary(precedence, _, _)
        | TokenParseRule::Unary(precedence, _) => precedence,
    })
}

/// Equivalent comparison operator after logical negation, when one exists.
fn negated_comparison(operator: &str) -> Option<&'static str> {
    [
        (<Token![==]>::SPELLING, <Token![!=]>::SPELLING),
        (<Token![!=]>::SPELLING, <Token![==]>::SPELLING),
        (<Token![===]>::SPELLING, <Token![=!=]>::SPELLING),
        (<Token![=!=]>::SPELLING, <Token![===]>::SPELLING),
        (<Token![<]>::SPELLING, <Token![>=]>::SPELLING),
        (<Token![<=]>::SPELLING, <Token![>]>::SPELLING),
        (<Token![>]>::SPELLING, <Token![<=]>::SPELLING),
        (<Token![>=]>::SPELLING, <Token![<]>::SPELLING),
    ]
    .into_iter()
    .find_map(|(comparison, negation)| (comparison == operator).then_some(negation))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_metadata::SyntaxSnapshot;
    use crate::source::DocumentSource;

    /// Every rewrite of `kind` in `text`, as replacement text.
    fn rewrites(text: &str, kind: RewriteKind) -> Vec<String> {
        let snapshot = SyntaxSnapshot::parse(text, None);
        let mut source = DocumentSource::new(text.to_owned());
        source.register_syntax(&snapshot);
        RewriteSites::collect(
            snapshot.cells().iter().filter_map(|cell| cell.syntax()),
            &source,
        )
        .into_iter()
        .filter(|site| site.kind == kind)
        .map(|site| site.replacement)
        .collect()
    }

    /// The coalescence rewrites of `text`, of either kind.
    fn coalescences(text: &str) -> Vec<(RewriteKind, String)> {
        let snapshot = SyntaxSnapshot::parse(text, None);
        let mut source = DocumentSource::new(text.to_owned());
        source.register_syntax(&snapshot);
        RewriteSites::collect(
            snapshot.cells().iter().filter_map(|cell| cell.syntax()),
            &source,
        )
        .into_iter()
        .filter(|site| site.kind.is_coalescence())
        .map(|site| (site.kind, site.replacement))
        .collect()
    }

    #[test]
    fn negating_a_condition_only_groups_operators_looser_than_not() {
        for (condition, negation) in [
            ("linkStyles#?style", "not linkStyles#?style"),
            ("first and second", "not (first and second)"),
            ("first or second", "not (first or second)"),
            ("isNode raw", "not isNode raw"),
        ] {
            let text = format!("if {condition} then null else value\n");
            assert_eq!(
                rewrites(&text, RewriteKind::NullBranch),
                [format!("if {negation} then value")]
            );
        }
    }

    /// `??` turns an error in its left side into the fallback, as a `try`
    /// around the tested expression or the whole conditional does, so those
    /// forms coalesce too, for any expression rather than only a symbol.
    #[test]
    fn try_guarded_null_tests_coalesce_like_the_error_catching_coalescence() {
        let expression = |text: &str| vec![(RewriteKind::Coalescence, text.to_string())];

        assert_eq!(
            coalescences("y = if (try f x) === null then 0 else f x\n"),
            expression("f x ?? 0")
        );
        assert_eq!(
            coalescences("if (try x) === null then x = 1\n"),
            [(RewriteKind::CoalescingAssignment, "x ??= 1".to_string())]
        );
        assert_eq!(
            coalescences("y = try (if f x === null then 0 else f x) else 0\n"),
            expression("f x ?? 0")
        );
        assert_eq!(
            coalescences("y = try (if x === null then 0 else x) else 0\n"),
            expression("x ?? 0"),
            "the enclosing try carries the one hint"
        );
        assert_eq!(
            coalescences("y = if (try (a or b)) === null then 0 else (a or b)\n"),
            expression("(a or b) ?? 0"),
            "`??` groups to the right, so an equally loose left side keeps parentheses"
        );
    }

    /// Without a `try` on the tested expression an error in it propagates,
    /// where `??` would swallow it; a `try` without the matching `else` yields
    /// `null` on error rather than the fallback; and a `try` around an
    /// assignment also swallows errors from the fallback, which `??=` lets
    /// through.
    #[test]
    fn null_tests_whose_errors_behave_differently_do_not_coalesce() {
        for text in [
            "y = if f x === null then 0 else f x\n",
            "y = try (if f x === null then 0 else f x)\n",
            "y = try (if f x === null then 0 else f x) else 1\n",
            "try if x === null then x = 1\n",
        ] {
            assert_eq!(coalescences(text), [], "{text:?}");
        }
    }

    #[test]
    fn an_if_that_only_assigns_a_fallback_becomes_a_coalescing_assignment() {
        let assignment = |text: &str| vec![(RewriteKind::CoalescingAssignment, text.to_string())];

        assert_eq!(
            coalescences(
                "f = () -> (\n    if typeInfo === null then\n        typeInfo = class value;\n    typeInfo\n    )\n"
            ),
            assignment("typeInfo ??= class value")
        );
        assert_eq!(
            coalescences("if x === null then x = 1\n"),
            assignment("x ??= 1"),
            "a top-level statement's value is unused"
        );
        assert_eq!(
            coalescences("if null === x then x = 1 else x = x\n"),
            assignment("x ??= 1")
        );
    }

    #[test]
    fn assigning_either_the_subject_or_a_fallback_becomes_a_coalescence() {
        let expression = |text: &str| vec![(RewriteKind::Coalescence, text.to_string())];

        assert_eq!(
            coalescences("if c === null then d = e else d = c\n"),
            expression("d = c ?? e")
        );
        assert_eq!(
            coalescences("if c =!= null then d := c else d := e\n"),
            expression("d := c ?? e")
        );
        assert_eq!(
            coalescences("y = if x === null then a -> a else x\n"),
            expression("x ?? (a -> a)"),
            "a fallback looser than `??` keeps its own parentheses"
        );
    }

    /// `if x === null then x = v` yields `null` when `x` is set, and `x ??= v`
    /// yields `x`, so the rewrite applies only where nothing uses that value.
    #[test]
    fn coalescing_assignment_leaves_ifs_whose_value_is_used_or_that_declare() {
        for text in [
            "f = () -> (if x === null then x = 1)\n",
            "y = (if x === null then x = 1)\n",
            "if x === null then x := 1\n",
            "if x =!= null then x = 1\n",
            "if x === null then y = 1\n",
        ] {
            assert_eq!(coalescences(text), [], "{text:?}");
        }
    }

    #[test]
    fn ambiguous_member_access_requires_an_adjacent_dot_prefixed_float() {
        for (text, expected) in [
            ("x.3\n", vec!["x#3".to_string()]),
            ("x.123\n", vec!["x#123".to_string()]),
            ("x .3\n", Vec::new()),
            ("x.3e2\n", Vec::new()),
        ] {
            assert_eq!(
                rewrites(text, RewriteKind::AmbiguousFloatMemberAccess),
                expected,
                "{text:?}"
            );
        }
    }
}
