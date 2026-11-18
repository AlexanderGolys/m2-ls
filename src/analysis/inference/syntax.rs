//! Typed structural queries shared by expression inference and call handling.

use crate::analysis::{MethodDeclarationOption, MethodOptions};
use crate::syntax::ExpressionSyntax;
use crate::typesystem::TypeRole;
use m2_syn::{nodes::*, Span, Spanned};

/// The value produced by each generated assignment alternative.
pub trait AssignmentValue {
    /// Borrows the assigned expression without recovering it through navigation.
    fn assigned_value(&self) -> &Expr;
}

impl AssignmentValue for ExprAssign {
    fn assigned_value(&self) -> &Expr {
        &self.right
    }
}

/// A typed call of `method`, whose own options determine its runtime class.
pub trait MethodConstructor {
    /// The option's last value among this call's immediate arguments.
    fn method_option(&self, option: MethodDeclarationOption) -> Option<&Expr>;
    /// Runtime class when this call is a method constructor.
    fn method_class(&self) -> Option<TypeRole>;
}
impl MethodConstructor for ExprBinary {
    fn method_option(&self, option: MethodDeclarationOption) -> Option<&Expr> {
        self.right
            .arguments()
            .into_iter()
            .flatten()
            .filter_map(|argument| match argument {
                Expr::Option(assignment) if assignment.key.symbol_name() == Some(option.name()) => {
                    Some(assignment.value.as_ref())
                }
                _ => None,
            })
            .next_back()
    }
    fn method_class(&self) -> Option<TypeRole> {
        if self.left.symbol_name() != Some("method") {
            return None;
        }
        let symbol = |option| {
            self.method_option(option)
                .and_then(ExpressionSyntax::symbol_name)
        };
        let options = match symbol(MethodDeclarationOption::Options) {
            Some("null") => MethodOptions::Absent,
            Some("true") => MethodOptions::Forwarded,
            _ if self
                .method_option(MethodDeclarationOption::Options)
                .is_some() =>
            {
                MethodOptions::Table
            }
            _ => MethodOptions::Absent,
        };
        Some(if symbol(MethodDeclarationOption::Binary) == Some("true") {
            TypeRole::MethodFunctionBinary
        } else if symbol(MethodDeclarationOption::Dispatch).is_some_and(|name| {
            name == TypeRole::Thing.object_name().name()
                || name == TypeRole::Type.object_name().name()
        }) {
            TypeRole::MethodFunctionSingle
        } else if options != MethodOptions::Absent {
            TypeRole::MethodFunctionWithOptions
        } else {
            TypeRole::MethodFunction
        })
    }
}

/// Typed value operands of an explicit infix operation used for application precedence.
pub struct InfixOperands<'ast> {
    /// Operand to which the delimited application binds first.
    pub left: &'ast Expr,
    /// Operand evaluated after the application result.
    pub right: &'ast Expr,
    /// Canonical operator spelling.
    pub operator: &'static str,
    /// Where the operator is written.
    pub operator_span: Option<Span>,
}

/// Recognizes explicit infix structure without classifying navigation nodes.
pub trait InfixSyntax {
    /// Typed operands of an explicit infix expression; implicit application is excluded.
    fn infix_operands(&self) -> Option<InfixOperands<'_>>;
}

impl InfixSyntax for Expr {
    fn infix_operands(&self) -> Option<InfixOperands<'_>> {
        match self {
            Self::Binary(binary) => binary.infix_operands(),
            _ => None,
        }
    }
}

impl InfixSyntax for ExprBinary {
    fn infix_operands(&self) -> Option<InfixOperands<'_>> {
        if matches!(self.op, BinaryOperator::Space(_)) {
            return None;
        }
        Some(InfixOperands {
            left: &self.left,
            right: &self.right,
            operator: self.op.spelling(),
            operator_span: self.op.try_span(),
        })
    }
}
