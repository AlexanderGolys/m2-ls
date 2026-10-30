//! Typed structural queries shared by expression inference and call handling.

use crate::analysis::{MethodDeclarationOption, MethodOptions};
use crate::typesystem::TypeRole;
use m2_syn::nodes::*;

/// The value produced by each generated assignment alternative.
pub trait AssignmentValue {
    /// Borrows the assigned expression without recovering it through navigation.
    fn assigned_value(&self) -> &Expr;
}

impl AssignmentValue for ExprAssign {
    fn assigned_value(&self) -> &Expr {
        match self {
            Self::Simple(ExprAssignSimple::Global(node)) => &node.value,
            Self::Simple(ExprAssignSimple::Local(node)) => &node.value,
            Self::Parallel(ExprAssignParallel::Global(node)) => &node.value,
            Self::Parallel(ExprAssignParallel::Local(node)) => &node.value,
            Self::Evaluated(node) => &node.value,
            Self::Augmented(node) => &node.value,
            Self::Install(ExprAssignInstall::Method(node)) => &node.function,
            Self::Install(ExprAssignInstall::Op(node)) => &node.function,
            Self::Install(ExprAssignInstall::New(node)) => &node.function,
            Self::Op(node) => &node.value_expr,
        }
    }
}

/// Expression structure relevant to argument passing and nominal class inference.
pub trait ExpressionSyntax {
    /// Peels holders that contribute a value; an empty holder has no value.
    fn unparenthesized(&self) -> Option<&Expr>;
    /// Symbol spelling when this exact expression is a symbol, without peeling holders.
    fn symbol_name(&self) -> Option<&str>;
    /// Positional arguments, retaining empty components as `None`.
    fn arguments(&self) -> Vec<Option<&Expr>>;
}

impl ExpressionSyntax for Expr {
    fn unparenthesized(&self) -> Option<&Expr> {
        let mut expression = self;
        while let Self::Holder(holder) = expression {
            expression = holder.value.as_deref()?;
        }
        Some(expression)
    }

    fn symbol_name(&self) -> Option<&str> {
        match self {
            Self::Symbol(symbol) => Some(symbol.text()),
            _ => None,
        }
    }

    fn arguments(&self) -> Vec<Option<&Expr>> {
        let expression = self.unparenthesized().unwrap_or(self);
        match expression {
            Self::Collection(ExprCollection::Sequence(sequence)) => sequence
                .elements
                .as_ref()
                .map_or_else(Vec::new, PackedArguments::arguments),
            _ => vec![Some(expression)],
        }
    }
}

/// Value slots in a generated packed argument list.
pub trait PackedArguments {
    /// Returns expressions and explicit empty slots, excluding muted statements.
    fn arguments(&self) -> Vec<Option<&Expr>>;
}
impl PackedArguments for PackedContent {
    fn arguments(&self) -> Vec<Option<&Expr>> {
        match self {
            Self::Expr(expression) => vec![Some(expression)],
            Self::Pack(pack) => pack
                .pack
                .iter()
                .map(|component| match component {
                    Component::Expr(expression) => Some(expression.as_ref()),
                    Component::Empty(_) => None,
                })
                .collect(),
        }
    }
}

/// A typed call of `method`, whose own options determine its runtime class.
pub trait MethodConstructor {
    /// The option's last value among this call's immediate arguments.
    fn method_option(&self, option: MethodDeclarationOption) -> Option<&Expr>;
    /// Runtime class when this call is a method constructor.
    fn method_class(&self) -> Option<TypeRole>;
}
impl MethodConstructor for ExprOpBinAdj {
    fn method_option(&self, option: MethodDeclarationOption) -> Option<&Expr> {
        self.right_operand
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
        if self.left_operand.symbol_name() != Some("method") {
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
    /// Canonical operator spelling with attached trivia excluded.
    pub operator: String,
}

/// Recognizes explicit infix structure without classifying navigation nodes.
pub trait InfixSyntax {
    /// Typed operands of an explicit infix expression; implicit application is excluded.
    fn infix_operands(&self) -> Option<InfixOperands<'_>>;
}

impl InfixSyntax for Expr {
    fn infix_operands(&self) -> Option<InfixOperands<'_>> {
        match self {
            Self::Op(ExprOp::Bin(binary)) => binary.infix_operands(),
            _ => None,
        }
    }
}

impl InfixSyntax for ExprOpBin {
    fn infix_operands(&self) -> Option<InfixOperands<'_>> {
        use crate::analysis::syntax_roles::OperatorSyntax;
        use m2_syn::Token;
        let (left, right, operator) = match self {
            Self::Adj(_) => return None,
            Self::Regular(binary) => (
                &binary.left_operand,
                &binary.right_operand,
                binary.operator.operator_spelling()?,
            ),
            Self::Index(index) => (
                &index.receiver,
                &index.index,
                <m2_syn::Token![#]>::SPELLING.to_owned(),
            ),
            Self::Member(ExprOpBinMember::Dot(member)) => (
                &member.receiver,
                &member.member,
                <m2_syn::Token![.]>::SPELLING.to_owned(),
            ),
            Self::Member(ExprOpBinMember::Optional(member)) => (
                &member.receiver,
                &member.member,
                <m2_syn::Token![.?]>::SPELLING.to_owned(),
            ),
        };
        Some(InfixOperands {
            left,
            right,
            operator,
        })
    }
}
