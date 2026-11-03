//! Retained parsed cells, the immutable syntax snapshot built from them, and Markdown parsing.

mod cells;
pub mod markdown;

/// Whether the supplied spelling equals the requested generated token's canonical spelling.
pub fn matches_token<T: m2_syn::Token>(text: &str) -> bool {
    text == T::SPELLING
}

pub use cells::{ParsedCell, SyntaxSnapshot};
