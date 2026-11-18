//! Typed structural operations used when collecting source semantic roles.

use m2_syn::{
    nodes::{Expr, ExprBinary, ExprField, ExprQuote, Literal, Symbol},
    Spanned, Token,
};

/// An access whose key denotes a property rather than a binding reference.
pub trait PropertyAccess {
    /// The literal property key, when this access has one.
    fn property_key(&self) -> Option<&dyn Spanned>;
}

impl PropertyAccess for ExprField {
    fn property_key(&self) -> Option<&dyn Spanned> {
        Some(&self.member)
    }
}

impl PropertyAccess for ExprBinary {
    fn property_key(&self) -> Option<&dyn Spanned> {
        let property_operator = matches!(
            self.op.spelling(),
            <Token![#]>::SPELLING | <Token![#?]>::SPELLING
        );
        (property_operator
            && matches!(
                self.right.as_ref(),
                Expr::Literal(Literal::String(_) | Literal::RawString(_))
            ))
        .then_some(self.right.as_ref() as &dyn Spanned)
    }
}

/// A quote form naming a symbol without evaluating its binding.
pub trait QuotedName {
    /// The generated symbol field shared by quote alternatives.
    fn quoted_name(&self) -> &Symbol;
}

impl QuotedName for ExprQuote {
    fn quoted_name(&self) -> &Symbol {
        &self.symbol
    }
}
