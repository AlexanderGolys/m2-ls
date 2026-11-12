//! Diagnostics that follow from values the program determines: what
//! compile-time evaluation proves certain about an expression, such as
//! matrices whose dimensions cannot combine, a division by zero, or a
//! condition that has one value whatever else happens.

use m2_syn::nodes::{Expr, ExprLiteral, ExprOp, ExprOpBin};
use m2_syn::visit::{self, Visit};
use m2_syn::Token;

use super::ExpressionChecks;
use crate::analysis::{InferenceSession, InfixSyntax, KnownValue, MatrixShape, TypeChecker};
use crate::diagnostics::catalog;
use crate::object_registry::ObjectRegistryView;
use crate::source::SourceNavigation;
use crate::syntax::ExpressionSyntax;

/// The class name of a matrix.
const MATRIX_CLASS: &str = "Matrix";

/// The calls that give a matrix its dimensions.
const MATRIX_BUILDERS: [&str; 2] = ["matrix", "transpose"];

/// Finds a name the program defines among the symbols of an expression.
struct DefinedNames<'check, 'a, 'context> {
    /// The checks whose analysis answers which names the program defines.
    checks: &'check ExpressionChecks<'a, 'context>,
    /// Whether such a name was found.
    found: bool,
}

impl<'ast> Visit<'ast> for DefinedNames<'_, '_, '_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        if let Expr::Symbol(symbol) = expression {
            self.found |= self
                .checks
                .context
                .source
                .range_for_syntax(symbol)
                .and_then(|range| {
                    self.checks
                        .context
                        .analysis
                        .get_binding_at(symbol.text(), range.start)
                })
                .is_some();
        }
        visit::visit_expr(self, expression);
    }
}

impl<'a> ExpressionChecks<'a, '_> {
    /// Runs the checks that need a determined value on `expression`, each
    /// only where a cheap look at the syntax finds it possible.
    pub(super) fn constant_values(
        &mut self,
        expression: &'a Expr,
        knowledge: &ObjectRegistryView<'_>,
    ) {
        match expression {
            Expr::If(node) => self.constant_condition("if", &node.condition, knowledge),
            Expr::While(node) => self.constant_condition("while", &node.condition, knowledge),
            Expr::Op(ExprOp::Bin(binary)) if !matches!(binary, ExprOpBin::Adj(_)) => {
                let Some(operands) = binary.infix_operands() else {
                    return;
                };
                if operands.operator == <Token![/]>::SPELLING {
                    self.division_by_zero(expression, operands.left, operands.right, knowledge);
                } else if [
                    <Token![+]>::SPELLING,
                    <Token![-]>::SPELLING,
                    <Token![*]>::SPELLING,
                ]
                .contains(&operands.operator)
                {
                    self.matrix_shapes(
                        expression,
                        operands.operator,
                        operands.left,
                        operands.right,
                        knowledge,
                    );
                }
            }
            _ => {}
        }
    }

    /// The value `expression` certainly has, if the program determines it.
    fn determined_value(
        &self,
        expression: &'a Expr,
        knowledge: &ObjectRegistryView<'_>,
    ) -> Option<KnownValue> {
        let checker = TypeChecker::new(self.context.analysis, knowledge);
        let session = InferenceSession::new(&checker, self.context.source, self.snapshot);
        let value = session.ring_values(0).constant(expression);
        value
    }

    /// A number divided by an exact zero written out, which Macaulay2
    /// rejects; only an inexact zero, `0.`, divides to infinity.
    fn division_by_zero(
        &mut self,
        expression: &'a Expr,
        dividend: &'a Expr,
        divisor: &'a Expr,
        knowledge: &ObjectRegistryView<'_>,
    ) {
        let writes_exact_zero = matches!(
            divisor.unparenthesized(),
            Some(literal @ Expr::Literal(ExprLiteral::Integer(_)))
                if self.text(literal).trim() == "0"
        );
        let is_number = matches!(
            dividend.unparenthesized(),
            Some(Expr::Literal(ExprLiteral::Float(_)))
        ) || matches!(
            self.determined_value(dividend, knowledge),
            Some(KnownValue::Number(_))
        );
        if !writes_exact_zero || !is_number {
            return;
        }
        self.spawn(
            &catalog::DIVISION_BY_ZERO,
            expression,
            "Division by an exact zero raises an error in Macaulay2; only an inexact zero, `0.`, divides to infinity",
        );
    }

    /// Whether `operand` is a matrix the syntax or its binding's type says so.
    fn may_be_matrix(&self, operand: &Expr) -> bool {
        match operand.unparenthesized() {
            Some(Expr::Op(ExprOp::Bin(ExprOpBin::Adj(call)))) => call
                .left_operand
                .symbol_name()
                .is_some_and(|name| MATRIX_BUILDERS.contains(&name)),
            Some(Expr::Symbol(symbol)) => self
                .context
                .source
                .range_for_syntax(symbol)
                .and_then(|range| {
                    self.context
                        .analysis
                        .get_binding_at(symbol.text(), range.start)
                })
                .and_then(|binding| binding.state.inferred_type.as_ref()?.label())
                .is_some_and(|label| label == MATRIX_CLASS),
            _ => false,
        }
    }

    /// A sum, difference or product of matrices whose dimensions the program
    /// determines and which do not combine.
    fn matrix_shapes(
        &mut self,
        expression: &'a Expr,
        operator: &str,
        left: &'a Expr,
        right: &'a Expr,
        knowledge: &ObjectRegistryView<'_>,
    ) {
        if !self.may_be_matrix(left) || !self.may_be_matrix(right) {
            return;
        }
        let (Some(KnownValue::Matrix(left_shape)), Some(KnownValue::Matrix(right_shape))) = (
            self.determined_value(left, knowledge),
            self.determined_value(right, knowledge),
        ) else {
            return;
        };
        let reason = if operator == <Token![*]>::SPELLING {
            (left_shape.columns != right_shape.rows).then(|| {
                format!(
                    "the first has {} columns, the second {} rows",
                    left_shape.columns, right_shape.rows
                )
            })
        } else {
            (left_shape != right_shape)
                .then(|| "a sum or difference needs matrices of one shape".to_string())
        };
        if let Some(reason) = reason {
            self.spawn(
                &catalog::MATRIX_SHAPE_MISMATCH,
                expression,
                Self::mismatch_message(operator, left_shape, right_shape, &reason),
            );
        }
    }

    /// The message of a mismatch between the shapes of two matrices.
    fn mismatch_message(
        operator: &str,
        left: MatrixShape,
        right: MatrixShape,
        reason: &str,
    ) -> String {
        format!("Cannot compute the {left} {operator} the {right}: {reason}")
    }

    /// A condition of `if` or `while` computed from literals alone, which has
    /// one value.
    fn constant_condition(
        &mut self,
        construct: &str,
        condition: &'a Expr,
        knowledge: &ObjectRegistryView<'_>,
    ) {
        let computes = matches!(
            condition.unparenthesized(),
            Some(Expr::Op(_) | Expr::Control(_))
        );
        // A condition on a name the program defines is a setting, `not DEBUG`,
        // however constant that name is.
        let mut names = DefinedNames {
            checks: self,
            found: false,
        };
        names.visit_expr(condition);
        if !computes || names.found {
            return;
        }
        let Some(KnownValue::Boolean(value)) = self.determined_value(condition, knowledge) else {
            return;
        };
        let consequence = match (construct, value) {
            ("if", true) => "the `else` branch never runs",
            ("if", false) => "the `then` branch never runs",
            (_, true) => "the loop runs until something leaves it",
            (_, false) => "the loop body never runs",
        };
        self.spawn(
            &catalog::CONSTANT_CONDITION,
            condition,
            format!("This `{construct}` condition is always `{value}`: {consequence}"),
        );
    }
}
