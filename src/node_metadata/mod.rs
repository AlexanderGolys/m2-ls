//! Typed, grammar-local access to Tree-sitter nodes used throughout the server.

pub mod markdown;
mod node;
mod parser;

use m2_syn::{Span, Spanned};

pub use node::{visit_expression_nodes, visit_source_nodes, M2Node, M2SyntaxKind, SyntaxNodeId};
pub use parser::{M2Parser, M2Tree};

pub fn syntax_byte_range(syntax: &(impl Spanned + ?Sized)) -> Option<(usize, usize)> {
    span_byte_range(syntax.span())
}

fn span_byte_range(span: Span) -> Option<(usize, usize)> {
    span.bounds()
}

pub fn matches_token<T: m2_syn::Token>(text: &str) -> bool {
    text == T::SPELLING
}

pub fn token_spelling<T: m2_syn::Token>() -> &'static str {
    T::SPELLING
}

#[cfg(test)]
mod tests;
