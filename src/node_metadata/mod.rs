//! Shared native syntax ownership and typed document navigation.

mod cells;
pub mod markdown;
mod native;
mod node;
mod parser;
mod syntax_ref;

pub use native::TypedSource;
pub use node::{visit_expression_nodes, visit_source_nodes, SyntaxKind, SyntaxNode, SyntaxNodeId};
pub use parser::{SyntaxParser, SyntaxTree};

pub fn matches_token<T: m2_syn::Token>(text: &str) -> bool {
    text == T::SPELLING
}

pub fn token_spelling<T: m2_syn::Token>() -> &'static str {
    T::SPELLING
}

#[cfg(test)]
mod tests;

pub use cells::SyntaxSnapshot;
pub use syntax_ref::SyntaxType;
