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
    AssignOperator, Cell, Elements, ElseClause, ExceptClause, Expr, ExprBinary, ExprDebug, ExprIf,
    ExprParen, ExprTiming, ExprTry, ExprWhile, FromClause, Literal, OfClause, Stmt, ThenClause,
    ToClause, TryFallback, WhenClause,
};
use m2_syn::visit::{self, Visit};
use m2_syn::{Spanned, Token};

use crate::analysis::{Analysis, InfixSyntax};
use crate::diagnostics::{catalog, DiagnosticRule};
use crate::node_metadata::binding_precedence;
use crate::object_registry::ObjectRegistry;
use crate::source::{ByteRange, SourceNavigation, SyntaxSource};
use crate::syntax::{AssignmentSyntax, ClauseSyntax, ControlSyntax, ExpressionSyntax};

mod assigned_branches;
mod augmented_assignment;
mod boolean_comparison;
mod composition;
mod constructor_call;
mod null_binding;
mod unary_call;

use boolean_comparison::BooleanComparison;
use composition::{LambdaComposition, MappingComposition};
use constructor_call::ConstructorCall;
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
    /// A local bound only to be tested against `null`, which `??` folds away.
    InlinedNullBinding,
    /// An unused null guard whose branch returns from the function.
    NullReturnGuard,
    /// Mapping a callback consisting of two composed unary function calls.
    MappingComposition,
    /// A lambda forwarding its parameter through two unary function calls.
    LambdaComposition,
    /// A Boolean expression strictly compared with `true` or `false`.
    BooleanComparison,
    /// A callback forwarding its parameter after a fixed first argument.
    PartialApplicationLambda,
    /// A `new ... from ...` expression using a self-initializing metaclass.
    SelfInitializingConstructor,
    /// A header constructor with list or sequence data.
    HeaderConstructor,
    /// A wrapper constructor with list or sequence data.
    WrapperConstructor,
    /// An `if` with a `null` branch.
    NullBranch,
    /// A `try` repeating its value or falling back to `null`.
    TrySimplification,
    /// A bare `try` as the left operand of `??`, which catches errors itself.
    CoalescedTry,
    /// An `x = x op e` that the augmented assignment `x op= e` shortens.
    AugmentedAssignment,
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
                | Self::LambdaComposition
                | Self::BooleanComparison
                | Self::PartialApplicationLambda
                | Self::SelfInitializingConstructor
                | Self::HeaderConstructor
                | Self::WrapperConstructor
                | Self::NullBranch
                | Self::TrySimplification
                | Self::CoalescedTry
                | Self::AugmentedAssignment
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
            Self::InlinedNullBinding => &catalog::INLINE_NULL_BINDING,
            Self::AugmentedAssignment => &catalog::PREFER_AUGMENTED_ASSIGNMENT,
            Self::NullBranch => &catalog::UNNECESSARY_NULL_BRANCH,
            Self::NullReturnGuard => &catalog::NULL_RETURN_GUARD,
            Self::MappingComposition => &catalog::MAPPED_COMPOSITION,
            Self::LambdaComposition => &catalog::COMPOSED_LAMBDA,
            Self::BooleanComparison => &catalog::REDUNDANT_BOOLEAN_COMPARISON,
            Self::PartialApplicationLambda => &catalog::PARTIAL_APPLICATION_LAMBDA,
            Self::SelfInitializingConstructor => &catalog::SELF_INITIALIZING_CONSTRUCTOR,
            Self::HeaderConstructor => &catalog::HEADER_CONSTRUCTOR,
            Self::WrapperConstructor => &catalog::WRAPPER_CONSTRUCTOR,
            Self::TrySimplification => &catalog::SIMPLIFIABLE_TRY,
            Self::CoalescedTry => &catalog::REDUNDANT_COALESCED_TRY,
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
    /// Bytes the replacement covers when that is more than the expression: the
    /// statements an inlining rewrite absorbs before it. The expression is
    /// where the rewrite is reported and found.
    pub extent: Option<ByteRange>,
    /// Whether the replacement turns an error into a fallback value:
    /// `if h.k =!= null then h.k else v` fails when `h` has no key `k`, while
    /// its replacement `h.k ?? v` yields `v`.
    pub catches_errors: bool,
}

impl RewriteSite<'_> {
    /// The rule reporting the site: its kind's, unless it catches errors.
    pub fn rule(&self) -> &'static DiagnosticRule {
        if self.catches_errors {
            &catalog::ERROR_CATCHING_REWRITE
        } else {
            self.kind.rule()
        }
    }
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
            .filter_map(|mut site| {
                let applicable = match site.kind {
                    RewriteKind::MappingComposition => MappingComposition::of(site.expression)
                        .is_some_and(|mapping| mapping.is_applicable(analysis, source, knowledge)),
                    RewriteKind::BooleanComparison => BooleanComparison::of(site.expression)
                        .is_some_and(|comparison| {
                            comparison.is_applicable(analysis, source, knowledge)
                        }),
                    RewriteKind::SelfInitializingConstructor | RewriteKind::LambdaComposition => {
                        let range = source.syntax_bytes(site.expression)?;
                        let drops_comment = source
                            .syntax()
                            .cells()
                            .iter()
                            .flat_map(|cell| cell.trivia())
                            .any(|comment| {
                                source.syntax_bytes(comment).is_some_and(|bytes| {
                                    range.start <= bytes.start
                                        && bytes.end <= range.end
                                        && source.text_for_syntax(comment).is_some_and(|text| {
                                            !site.replacement.contains(text.trim())
                                        })
                                })
                            });
                        if drops_comment {
                            return None;
                        }
                        if site.kind == RewriteKind::SelfInitializingConstructor {
                            site.kind = ConstructorCall::of(site.expression)?
                                .kind(analysis, source, knowledge)?;
                        }
                        true
                    }
                    _ => true,
                };
                applicable.then_some(site)
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
            extent: None,
            catches_errors: self.catches_errors(kind, expression),
        });
    }

    /// Whether the `kind` rewrite of `expression` replaces a null test whose
    /// subject may fail by a `??`, which turns that failure into its fallback.
    fn catches_errors(&self, kind: RewriteKind, expression: &Expr) -> bool {
        let Expr::If(conditional) = expression else {
            return false;
        };
        (kind.is_coalescence() || kind == RewriteKind::NullReturnGuard)
            && self
                .null_test(conditional)
                .is_some_and(|test| test.fallible)
    }

    /// Marks `value` as the slot a control construct delimits.
    fn control_slot(&mut self, value: &'ast Expr, owner: &'ast dyn Spanned) {
        self.control_slots.push((ptr::from_ref(value), owner));
    }

    /// The rewrites of one expression, in the order diagnostics report them.
    fn rewrite(&mut self, expression: &'ast Expr) {
        let replacement = self.constructor_call(expression);
        self.push(
            RewriteKind::SelfInitializingConstructor,
            expression,
            None,
            replacement,
        );
        let replacement = self.lambda_composition(expression);
        self.push(
            RewriteKind::LambdaComposition,
            expression,
            None,
            replacement,
        );
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
        if let Expr::Binary(call) = expression {
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
            // Parentheses around statements are a block, not a grouping, and
            // parentheses spanning lines free their content from the newline
            // rule: without them `from (f)⏎ \ L` ends at the line break.
            if matches!(expression, Expr::Paren(holder) if holder.stmts.is_empty())
                && !self.text(expression).contains('\n')
            {
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
        let replacement = self.augmented_assignment(expression);
        self.push(
            RewriteKind::AugmentedAssignment,
            expression,
            None,
            replacement,
        );
        let replacement = self.coalesced_try(expression);
        self.push(RewriteKind::CoalescedTry, expression, None, replacement);
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
    fn ambiguous_float_member_access(&self, call: &ExprBinary) -> Option<String> {
        if !matches!(call.op, m2_syn::nodes::BinaryOperator::Space(_)) {
            return None;
        }
        let name = call.left.symbol_name()?;
        let Expr::Literal(Literal::Float(float)) = call.right.as_ref() else {
            return None;
        };
        let adjacent = self.source.syntax_bytes(call.left.as_ref())?.end
            == self.source.syntax_bytes(float)?.start;
        let fractional = self.text(float).strip_prefix('.')?;
        (adjacent && !fractional.is_empty() && fractional.chars().all(|ch| ch.is_ascii_digit()))
            .then(|| format!("{name}#{fractional}"))
    }

    /// Call syntax for a named constructor, with parentheses where an argument
    /// would otherwise escape the application or capture surrounding syntax.
    /// The target of an installation keeps its form: `new HLG from String :=
    /// f` installs the constructor `HLG "Operator"` calls, while `HLG String
    /// := f` would install a method that call never reaches.
    fn constructor_call(&self, expression: &Expr) -> Option<String> {
        if self.parent.is_some_and(|parent| {
            matches!(parent, Expr::Assign(assignment) if ptr::eq(assignment.left.as_ref(), expression))
        }) {
            return None;
        }
        let construction = ConstructorCall::of(expression)?;
        let value = construction.value;
        let grouped = !matches!(
            value,
            Expr::Symbol(_)
                | Expr::Literal(_)
                | Expr::List(_)
                | Expr::Array(_)
                | Expr::Sequence(_)
                | Expr::AngleBar(_)
                | Expr::Paren(_)
        ) && !matches!(value, Expr::Binary(binary) if matches!(binary.op, m2_syn::nodes::BinaryOperator::Space(_)));
        let replacement = format!(
            "{} {}",
            self.text(construction.class).trim(),
            self.parenthesized_if(value, grouped)
        );
        let nested = self.parent.is_some_and(|parent| {
            matches!(
                parent,
                Expr::Binary(_) | Expr::Prefix(_) | Expr::Postfix(_) | Expr::Field(_)
            )
        });
        Some(if nested {
            format!("({replacement})")
        } else {
            replacement
        })
    }

    /// Composition notation for a lambda passing its parameter through two
    /// named unary functions. Parentheses protect the composition when the
    /// lambda is an operand of an enclosing call or operator.
    fn lambda_composition(&self, expression: &Expr) -> Option<String> {
        let composition = LambdaComposition::of(expression)?;
        if !composition.has_function_names() {
            return None;
        }
        let replacement = format!(
            "{} @@ {}",
            self.text(composition.outer).trim(),
            self.text(composition.inner).trim()
        );
        let needs_parentheses = self.parent.is_some_and(|parent| {
            matches!(
                parent,
                Expr::Binary(_) | Expr::Prefix(_) | Expr::Postfix(_) | Expr::Field(_)
            )
        });
        Some(if needs_parentheses {
            format!("({replacement})")
        } else {
            replacement
        })
    }

    /// Partial application for `y -> f(x, y)`, with a fixed symbol argument.
    /// A symbol callee and fixed symbol avoid moving computed expressions out
    /// of the callback. Neither may refer to the disappearing parameter.
    fn partial_application_lambda(&self, expression: &Expr) -> Option<String> {
        let Expr::Function(lambda) = expression else {
            return None;
        };
        let m2_syn::nodes::FunctionParams::Variadic(parameter) = &lambda.params else {
            return None;
        };
        let call = UnarySymbolCall::application(&lambda.body)?;
        let function = UnarySymbolCall::grouped_value(&call.left)?;
        if !UnarySymbolCall::has_function_name(function) {
            return None;
        }
        let arguments = UnarySymbolCall::grouped_value(&call.right)?.arguments();
        let [Some(fixed), Some(forwarded)] = arguments.as_slice() else {
            return None;
        };
        let fixed = UnarySymbolCall::grouped_value(fixed)?;
        let forwarded = UnarySymbolCall::grouped_value(forwarded)?;
        let parameter = parameter.text();
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
            matches!(
                parent,
                Expr::Binary(_) | Expr::Prefix(_) | Expr::Postfix(_) | Expr::Field(_)
            ) && parent.infix_operands().is_none_or(|operands| {
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
            || matches!(collection, Expr::Function(_));
        let replacement = format!(
            "{} / {} @@ {}",
            self.parenthesized_if(collection, grouped),
            self.text(mapping.outer).trim(),
            self.text(mapping.inner).trim(),
        );
        let needs_parentheses = self.parent.is_some_and(|parent| {
            !matches!(parent, Expr::Assign(_) | Expr::Function(_) | Expr::Paren(_))
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
            Expr::Assign(assignment) if matches!(assignment.op, AssignOperator::Global(_)) => {
                let conditional @ Expr::If(test) = assignment.right.as_ref() else {
                    return None;
                };
                let (subject, fallback) = self.coalescence_parts(test)?;
                let target = assignment.left.symbol_name()?;
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
                        let (subject, fallback) = self.coalescence_parts(conditional)?;
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
        if guard.then_clause.is_some() {
            return None;
        }
        let Some(TryFallback::Else(error_fallback)) = &guard.fallback else {
            return None;
        };
        let conditional = guard.expr.unparenthesized()?;
        let Expr::If(test) = conditional else {
            return None;
        };
        let (subject, fallback) = self.coalescence_parts(test)?;
        self.same_expression(fallback, error_fallback.clause_value())
            .then(|| (conditional, self.coalescence_expression(subject, fallback)))
    }

    /// `e ?? v` for `(try e) ?? v`: an error in `e` yields `v` either way, through
    /// the `try`'s `null` or directly through `??`.
    fn coalesced_try(&self, expression: &Expr) -> Option<String> {
        let operands = expression.infix_operands()?;
        (operands.operator == <Token![??]>::SPELLING)
            .then(|| guarded_value(operands.left))
            .flatten()
            .map(|subject| self.coalescence_expression(subject, operands.right))
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

    /// The `??` form of an `if` assigning either a fallback when its subject is
    /// null or its subject when non-null. An omitted branch can be replaced by
    /// the target's previous value only when the conditional's result is unused.
    /// A complete fallback chain can instead yield the value assigned on each
    /// path, retaining statements before terminal assignments inside their blocks.
    fn coalescing_assignment(
        &self,
        conditional: &ExprIf,
        discarded: bool,
    ) -> Option<(RewriteKind, String)> {
        let NullTest {
            subject,
            null_branch,
            ..
        } = self.null_test(conditional)?;
        let then_value = conditional.then_clause.clause_value();
        let Some(else_clause) = &conditional.else_clause else {
            // `if x === null then x = v` yields `null` where `x ??= v` yields `x`,
            // so the two agree only when nothing uses the `if`'s own value.
            let (target, operator, fallback) = symbol_assignment(then_value)?;
            if null_branch == NullBranch::Else {
                return (discarded
                    && operator == <Token![=]>::SPELLING
                    && self.same_expression(fallback, subject))
                .then(|| {
                    (
                        RewriteKind::Coalescence,
                        format!(
                            "{target} = {} ?? {target}",
                            self.coalescence_subject(subject)
                        ),
                    )
                });
            }
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
        let (repeated_target, repeated_operator, repeated_subject) =
            symbol_assignment(subject_branch)?;
        if !self.same_expression(repeated_subject, subject) {
            return None;
        }
        let Some((target, operator, fallback)) = symbol_assignment(fallback_branch) else {
            if repeated_operator != <Token![=]>::SPELLING {
                return None;
            }
            let fallback = self.assigned_branch_value(fallback_branch, repeated_target)?;
            return Some((
                RewriteKind::Coalescence,
                format!(
                    "{repeated_target} = {} ?? ({fallback})",
                    self.coalescence_subject(subject)
                ),
            ));
        };
        if repeated_target != target || repeated_operator != operator {
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
            ..
        } = self.null_test(conditional)?;
        let fallback = conditional.then_clause.clause_value().unparenthesized()?;
        (null_branch == NullBranch::Then && matches!(fallback, Expr::Return(_)))
            .then(|| self.coalescence_expression(subject, fallback))
    }

    /// `fallback` as the right operand of `??`, parenthesized when it binds more
    /// loosely than `??` and would otherwise take the coalescence as its own left
    /// operand, as `x ?? y -> y` reads `(x ?? y) -> y`.
    fn coalescence_operand(&self, fallback: &Expr) -> String {
        let binds_looser = matches!(fallback, Expr::Function(_))
            || precedence_relative_to_coalescence(fallback).is_some_and(Ordering::is_lt);
        self.parenthesized_if(fallback, binds_looser)
    }

    /// `subject` as the left operand of `??`, parenthesized unless it binds more
    /// tightly: `??` groups to the right, so `a or b ?? v` reads `a or (b ?? v)`,
    /// and a keyword expression such as `try e` would take `?? v` as part of `e`.
    fn coalescence_subject(&self, subject: &Expr) -> String {
        let binds_tighter = !matches!(subject, Expr::Function(_))
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

    /// The expression an `if` compares with `null` using `===` or `=!=`, seen
    /// through a bare `try`: `(try f x) === null` tests `f x`.
    fn null_test<'test>(&self, conditional: &'test ExprIf) -> Option<NullTest<'test>> {
        let operands = conditional.cond.infix_operands()?;
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
        Some(NullTest {
            subject: guarded_value(tested).or_else(|| tested.unparenthesized())?,
            null_branch,
            fallible: may_fail(tested),
        })
    }

    /// The subject and fallback of a conditional coalescence expression.
    fn coalescence_parts<'test>(
        &self,
        conditional: &'test ExprIf,
    ) -> Option<(&'test Expr, &'test Expr)> {
        let NullTest {
            subject,
            null_branch,
            ..
        } = self.null_test(conditional)?;
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
                self.text(conditional.cond.as_ref()).trim_end(),
                self.text(then_branch)
            ));
        }
        is_null(then_branch).then(|| {
            format!(
                "if {} then {}",
                self.negated_condition(&conditional.cond),
                self.text(else_branch),
            )
        })
    }

    /// A `try` without a `then` repeating its value, or without an `else null`.
    fn try_statement(&self, guard: &ExprTry) -> Option<String> {
        let condition_text = self.text(guard.expr.as_ref());
        let consequence_text = guard
            .then_clause
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
        let simplified = self.simplified_condition(&conditional.cond)?;
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
            self.negated_condition(&conditional.cond),
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
        if let Some(value) = expression.transferred_value() {
            self.control_slot(value, expression);
        }
        let parent = self.parent.replace(expression);
        visit::visit_expr(self, expression);
        self.parent = parent;
    }

    fn visit_cell(&mut self, cell: &'ast Cell) {
        if let Elements::Single(value) = &cell.elems {
            self.discarded.insert(ptr::from_ref(value.as_ref()));
        }
        visit::visit_cell(self, cell);
    }

    fn visit_stmt(&mut self, statement: &'ast Stmt) {
        if let Elements::Single(value) = &statement.elems {
            self.discarded.insert(ptr::from_ref(value.as_ref()));
        }
        visit::visit_stmt(self, statement);
    }

    fn visit_expr_paren(&mut self, node: &'ast ExprParen) {
        self.null_bindings(node);
        visit::visit_expr_paren(self, node);
    }

    fn visit_expr_if(&mut self, node: &'ast ExprIf) {
        self.control_slot(&node.cond, node);
        visit::visit_expr_if(self, node);
    }

    fn visit_expr_while(&mut self, node: &'ast ExprWhile) {
        self.control_slot(&node.cond, node);
        visit::visit_expr_while(self, node);
    }

    fn visit_when_clause(&mut self, node: &'ast WhenClause) {
        self.control_slot(&node.expr, node);
        visit::visit_when_clause(self, node);
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
        self.control_slot(&node.expr, node);
        visit::visit_expr_try(self, node);
    }

    fn visit_except_clause(&mut self, node: &'ast ExceptClause) {
        self.control_slot(&node.expr, node);
        visit::visit_except_clause(self, node);
    }

    fn visit_of_clause(&mut self, node: &'ast OfClause) {
        self.control_slot(&node.expr, node);
        visit::visit_of_clause(self, node);
    }

    fn visit_from_clause(&mut self, node: &'ast FromClause) {
        self.control_slot(&node.expr, node);
        visit::visit_from_clause(self, node);
    }

    fn visit_to_clause(&mut self, node: &'ast ToClause) {
        self.control_slot(&node.expr, node);
        visit::visit_to_clause(self, node);
    }

    fn visit_expr_debug(&mut self, node: &'ast ExprDebug) {
        if let Some(value) = node.expr.as_deref() {
            self.control_slot(value, node);
        }
        visit::visit_expr_debug(self, node);
    }

    fn visit_expr_timing(&mut self, node: &'ast ExprTiming) {
        self.control_slot(&node.expr, node);
        visit::visit_expr_timing(self, node);
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

/// What an `if` compares with `null`, and the branch that runs when it is null.
/// `if (try f x) === null then v else f x` tests the subject `f x`, which is
/// not fallible because the `try` turns its errors into `null`.
#[derive(Debug, Clone, Copy)]
struct NullTest<'ast> {
    /// Expression whose behavior the rewrite preserves.
    subject: &'ast Expr,
    /// Branch returning the null value in a conditional rewrite.
    null_branch: NullBranch,
    /// Whether an error in the test escapes the conditional, where `??`
    /// would turn it into the fallback.
    fallible: bool,
}

/// Whether `expression` is the symbol `null`.
fn is_null(expression: &Expr) -> bool {
    expression.symbol_name() == Some("null")
}

/// The target name, operator, and value of `x = v` or `x := v`.
fn symbol_assignment(expression: &Expr) -> Option<(&str, &'static str, &Expr)> {
    let Expr::Assign(assignment) = expression else {
        return None;
    };
    let name = assignment.left.symbol_name()?;
    let operator = match assignment.op {
        AssignOperator::Global(_) => <Token![=]>::SPELLING,
        AssignOperator::Local(_) => <Token![:=]>::SPELLING,
        _ => return None,
    };
    Some((name, operator, &assignment.right))
}

/// The expression a bare `try e` evaluates, seen through parentheses; errors
/// in it yield `null`.
fn guarded_value(expression: &Expr) -> Option<&Expr> {
    match expression.unparenthesized()? {
        Expr::Try(guard) if guard.then_clause.is_none() && guard.fallback.is_none() => {
            guard.expr.unparenthesized()
        }
        _ => None,
    }
}

/// Whether evaluating `expression` may raise an error: anything but a symbol,
/// which always has a value, or a bare `try`, which turns errors into `null`.
fn may_fail(expression: &Expr) -> bool {
    guarded_value(expression).is_none()
        && !matches!(expression.unparenthesized(), Some(Expr::Symbol(_)))
}

/// The operand of a `not`.
fn negated_operand(expression: &Expr) -> Option<&Expr> {
    match expression {
        Expr::Prefix(prefix) if prefix.op.spelling() == <Token![not]>::SPELLING => {
            Some(&prefix.expr)
        }
        _ => None,
    }
}

/// The single value parentheses hold, or the expression itself.
fn unwrap_parentheses(expression: &Expr) -> &Expr {
    match expression {
        Expr::Paren(ExprParen {
            stmts,
            expr: Some(value),
            ..
        }) if stmts.is_empty() => value,
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
        | Expr::Break(_)
        | Expr::Continue(_)
        | Expr::Return(_)
        | Expr::Catch(_)
        | Expr::Throw(_)
        | Expr::Trap(_)
        | Expr::Debug(_)
        | Expr::Timing(_) => true,
        Expr::Prefix(prefix) => prefix
            .op
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

    /// The coalescence rewrites of `text`, of either kind, that keep its errors.
    fn coalescences(text: &str) -> Vec<(RewriteKind, String)> {
        coalescences_catching_errors(text, false)
    }

    /// The coalescence rewrites of `text`, of either kind, that turn an error
    /// into the fallback exactly when `catches_errors`.
    fn coalescences_catching_errors(
        text: &str,
        catches_errors: bool,
    ) -> Vec<(RewriteKind, String)> {
        let snapshot = SyntaxSnapshot::parse(text, None);
        let mut source = DocumentSource::new(text.to_owned());
        source.register_syntax(&snapshot);
        RewriteSites::collect(
            snapshot.cells().iter().filter_map(|cell| cell.syntax()),
            &source,
        )
        .into_iter()
        .filter(|site| site.kind.is_coalescence() && site.catches_errors == catches_errors)
        .map(|site| (site.kind, site.replacement))
        .collect()
    }

    /// An assignment repeating its symbol target as the left operand of an
    /// operator with an augmented form becomes that augmented assignment.
    #[test]
    fn a_target_repeated_as_the_left_becomes_an_augmented_assignment() {
        let augmented = |text: &str| rewrites(text, RewriteKind::AugmentedAssignment);

        assert_eq!(augmented("prefix = prefix | \"X\"\n"), ["prefix |= \"X\""]);
        assert_eq!(augmented("n = n + 1\n"), ["n += 1"]);
        assert_eq!(augmented("n = n - (a or b)\n"), ["n -= (a or b)"]);
        assert_eq!(augmented("n = n // 2\n"), ["n //= 2"]);
        assert_eq!(augmented("L = L ++ {1}\n"), ["L ++= {1}"]);
        assert_eq!(augmented("n = n + n\n"), ["n += n"]);
        for text in [
            "n = n < 3\n",
            "n = n == 3\n",
            "n = n or b\n",
            "n = n ? b\n",
            "n = n + 1 + 2\n",
            "n = b + n\n",
            "n = m + 1\n",
            "n = n#0\n",
            "n := n + 1\n",
            "n#0 = n#0 + 1\n",
        ] {
            assert_eq!(augmented(text), Vec::<String>::new(), "{text:?}");
        }
    }

    /// Parentheses holding statements before their value are a block: removing
    /// them would drop the statements. Parentheses spanning lines keep their
    /// content from ending at a line break. Neither is ever redundant.
    #[test]
    fn parentheses_around_statements_or_lines_are_not_redundant() {
        let redundant = RewriteKind::RedundantControlParentheses;

        assert_eq!(
            rewrites(
                "f = x -> if x then 1 else (c = 2; d = 3; c + d)\n",
                redundant
            ),
            Vec::<String>::new()
        );
        assert_eq!(
            rewrites(
                "f = v -> new M from ((e -> {e})\n     \\ toList v)\n",
                redundant
            ),
            Vec::<String>::new(),
            "without them the line break ends `new M from (e -> {{e}})`"
        );
        assert_eq!(
            rewrites("f = x -> if x then 1 else (2)\n", redundant),
            ["2"],
            "a plain grouping still is"
        );
    }

    /// A local bound only to be tested against `null` folds into the `??`
    /// using it, whether the conditional assigns or yields; the plain
    /// coalescence rewrite covers the conditional without the local.
    #[test]
    fn a_null_tested_local_folds_into_the_coalescence_using_it() {
        let inlined = |text: &str| rewrites(text, RewriteKind::InlinedNullBinding);

        assert_eq!(
            inlined("f = x -> (v := g x; if v =!= null then y = v else y = 0; y)\n"),
            ["y = g x ?? 0"]
        );
        assert_eq!(
            inlined("f = x -> (v := g x; if v === null then y = 0 else y = v; y)\n"),
            ["y = g x ?? 0"]
        );
        assert_eq!(
            inlined("f = x -> (v := a or b; if v =!= null then y := v else y := 0; y)\n"),
            ["y := (a or b) ?? 0"],
            "a looser subject keeps its parentheses"
        );
        assert_eq!(
            inlined("f = x -> (v := try g x; if v =!= null then y = v else y = 0; y)\n"),
            ["y = g x ?? 0"],
            "the `try` is what `??` does with errors already"
        );
        assert_eq!(
            inlined("f = x -> (v := g x; if v =!= null then v else 0)\n"),
            ["g x ?? 0"],
            "a block's own value"
        );
        assert_eq!(
            coalescences("f = x -> (v := g x; if v =!= null then y = v else y = 0; y)\n"),
            [],
            "the folded conditional carries no coalescence hint of its own"
        );
    }

    /// The local stays when anything else uses it, and the conditional stays
    /// when its branches are not the local and a fallback.
    #[test]
    fn a_local_used_elsewhere_or_a_branching_conditional_is_not_folded() {
        for text in [
            "f = x -> (v := g x; if v =!= null then y = v else y = 0; v + 1)\n",
            "f = x -> (v := g x; if v =!= null then y = v else y = v + 1; y)\n",
            "f = x -> (v := g x; if v =!= null then y = v else z = 0; y)\n",
            "f = x -> (v := g x; if v =!= null then y = v else y = 0; w := 1; v)\n",
            "f = x -> (v := g v; if v =!= null then y = v else y = 0; y)\n",
            "f = x -> (v := g x; w := 1; if v =!= null then y = v else y = 0; y)\n",
            "f = x -> (v = g x; if v =!= null then y = v else y = 0; y)\n",
            "f = x -> (v := g x; if v =!= null then y = v; y)\n",
            "f = x -> (v := g x; if v =!= null then v else 0; 1)\n",
        ] {
            assert_eq!(
                rewrites(text, RewriteKind::InlinedNullBinding),
                Vec::<String>::new(),
                "{text:?}"
            );
        }
    }

    /// A chain whose branches do different things is no coalescence, with or
    /// without a local, and a coalescence already written needs no rewrite.
    #[test]
    fn multiway_branches_and_written_coalescences_stay_untouched() {
        let group_end = "groupEnd = (source, i, place) -> (\n    expected := {closingBracket#(source#i)};\n    j := i + 1;\n    while #expected > 0 do (\n        stop := opaqueEnd(source, j);\n        if stop =!= null then\n            j = stop\n        else if isNameStart source#j then\n            j = pastQuotedBracket(source, j)\n        else (\n            c := source#j;\n            if closingBracket#?c then\n                expected = append(expected, closingBracket#c)\n            else if member(c, bracketSpellings) then\n                expected = drop(expected, -1);\n            j += 1\n        )\n    );\n    j\n)\n";
        let trailing = "trailingLineComment = text -> (\n    i := 0;\n    comment := null;\n    while i < #text do (\n        stop := opaqueEnd(text, i);\n        comment = if stop =!= null and substring(i, 2, text) == \"--\" then i;\n        i = stop ?? i + 1\n    );\n    if comment =!= null and opaqueEnd(text, comment) == #text then\n        comment\n)\n";

        for text in [group_end, trailing] {
            assert_eq!(coalescences(text), []);
            assert_eq!(
                rewrites(text, RewriteKind::InlinedNullBinding),
                Vec::<String>::new()
            );
            assert_eq!(
                rewrites(text, RewriteKind::RedundantControlParentheses),
                Vec::<String>::new()
            );
        }
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
    /// through. The first three coalesce only as error-catching rewrites.
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
        assert_eq!(
            coalescences_catching_errors("y = if h.Text =!= null then h.Text else 0\n", true),
            [(RewriteKind::Coalescence, "h.Text ?? 0".to_string())]
        );
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
