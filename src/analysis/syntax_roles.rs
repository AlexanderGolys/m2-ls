//! Typed structural operations used when collecting source semantic roles.

use m2_syn::{
    nodes::{
        Expr, ExprLiteral, ExprOpBinIndex, ExprOpBinMember, ExprOpBinRegular, ExprQuote, Symbol,
    },
    Lex, Token,
};

/// An access whose key denotes a property rather than a binding reference.
pub trait PropertyAccess {
    /// The literal property key, when this access has one.
    fn property_key(&self) -> Option<&Expr>;
}

impl PropertyAccess for ExprOpBinMember {
    fn property_key(&self) -> Option<&Expr> {
        let member = match self {
            Self::Dot(access) => access.member.as_ref(),
            Self::Optional(access) => access.member.as_ref(),
        };
        matches!(member, Expr::Symbol(_)).then_some(member)
    }
}

impl PropertyAccess for ExprOpBinIndex {
    fn property_key(&self) -> Option<&Expr> {
        self.index.string_property_key()
    }
}

impl PropertyAccess for ExprOpBinRegular {
    fn property_key(&self) -> Option<&Expr> {
        let spelling = self.operator.operator_spelling()?;
        (spelling == <m2_syn::Token![#?]>::SPELLING)
            .then(|| self.right_operand.string_property_key())?
    }
}

/// Literal expressions accepted as indexed property keys.
trait StringPropertyKey {
    /// Returns this expression only when it is a string literal.
    fn string_property_key(&self) -> Option<&Expr>;
}

impl StringPropertyKey for Expr {
    fn string_property_key(&self) -> Option<&Expr> {
        matches!(
            self,
            Expr::Literal(ExprLiteral::String(_) | ExprLiteral::RawString(_))
        )
        .then_some(self)
    }
}

/// A quote form naming a symbol without evaluating its binding.
pub trait QuotedName {
    /// The generated symbol field shared by quote alternatives.
    fn quoted_name(&self) -> &Symbol;
}

impl QuotedName for ExprQuote {
    fn quoted_name(&self) -> &Symbol {
        match self {
            Self::Symbol(quote) => &quote.symbol,
            Self::Global(quote) => &quote.symbol,
            Self::Local(quote) => &quote.symbol,
            Self::ThreadLocal(quote) => &quote.symbol,
            Self::ThreadVariable(quote) => &quote.symbol,
        }
    }
}

/// Canonical lexical spelling of a generated operator, excluding attached trivia.
pub trait OperatorSyntax: Lex {
    /// Projects the existing lexical token without parsing or scanning its source.
    fn operator_spelling(&self) -> Option<String> {
        self.to_token_stream()
            .into_iter()
            .next()?
            .spelling()
            .map(str::to_owned)
    }
}
impl OperatorSyntax for m2_syn::nodes::BinaryOperator {}
impl OperatorSyntax for m2_syn::nodes::PrefixOperator {}
impl OperatorSyntax for m2_syn::nodes::PostfixOperator {}
