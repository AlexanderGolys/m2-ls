//! Unary symbol calls whose grouping can be removed without losing statements.

use m2_syn::nodes::{Expr, ExprOp, ExprOpBin, ExprOpBinAdj};

use crate::syntax::ExpressionSyntax;

/// One supplied argument passed to a named callee, with only grouping holders.
#[derive(Debug, Clone, Copy)]
pub struct UnarySymbolCall<'ast> {
    /// Symbol expression naming the called operation.
    pub function: &'ast Expr,
    /// Sole supplied argument, without grouping parentheses.
    pub argument: &'ast Expr,
}

impl<'ast> UnarySymbolCall<'ast> {
    /// Recognizes `f x` and `f(x)`, rejecting extra arguments and parentheses
    /// containing statements before the returned expression.
    pub fn of(expression: &'ast Expr) -> Option<Self> {
        let call = Self::application(expression)?;
        let function = Self::grouped_value(&call.left_operand)?;
        function.symbol_name()?;
        let arguments = Self::grouped_value(&call.right_operand)?.arguments();
        let [Some(argument)] = arguments.as_slice() else {
            return None;
        };
        Some(Self {
            function,
            argument: Self::grouped_value(argument)?,
        })
    }

    /// Typed application after removing only statement-free grouping holders.
    pub fn application(expression: &Expr) -> Option<&ExprOpBinAdj> {
        match Self::grouped_value(expression)? {
            Expr::Op(ExprOp::Bin(ExprOpBin::Adj(call))) => Some(call),
            _ => None,
        }
    }

    /// Removes parentheses used solely for grouping. A statement-bearing or
    /// empty holder cannot be discarded by a source simplification.
    pub fn grouped_value(mut expression: &Expr) -> Option<&Expr> {
        while let Expr::Holder(holder) = expression {
            if !holder.muted.is_empty() {
                return None;
            }
            expression = holder.value.as_deref()?;
        }
        Some(expression)
    }

    /// Whether the name follows the lowercase function convention, excluding
    /// PascalCase type constructors without requiring a proven Function type.
    pub fn has_function_name(expression: &Expr) -> bool {
        expression
            .symbol_name()
            .and_then(|name| name.chars().next())
            .is_some_and(char::is_lowercase)
    }
}
