//! Retained parsed cells, the immutable syntax snapshot built from them, the
//! parentheses their parses do not need, and Markdown parsing.

use m2_syn::nodes::{TokenParseRule, TOKEN_PARSE_RULES};

mod cells;
pub mod markdown;
mod parentheses;

/// Whether the supplied spelling equals the requested generated token's canonical spelling.
pub fn matches_token<T: m2_syn::Token>(text: &str) -> bool {
    text == T::SPELLING
}

/// The parse rule of the token spelled `spelling`.
fn parse_rule(spelling: &str) -> Option<TokenParseRule> {
    let index = TOKEN_PARSE_RULES
        .binary_search_by_key(&spelling, |(token, _)| token)
        .ok()?;
    Some(TOKEN_PARSE_RULES[index].1)
}

/// The parsing precedence of the token spelled `spelling`: how tightly it
/// binds the operand on its left, `58` for `*` against `50` for `+`.
pub fn binding_precedence(spelling: &str) -> Option<u8> {
    Some(match parse_rule(spelling)? {
        TokenParseRule::Precedence(precedence)
        | TokenParseRule::Postfix(precedence)
        | TokenParseRule::Binary(precedence, _)
        | TokenParseRule::Prefix(precedence, _)
        | TokenParseRule::PrefixBinary(precedence, _, _)
        | TokenParseRule::Unary(precedence, _) => precedence,
    })
}

/// How tightly the binary operator spelled `spelling` binds its right
/// operand: an operator binding a following one of its own precedence, as
/// `->` (`14`, binding at `13`), groups to the right.
pub fn binary_strength(spelling: &str) -> Option<u8> {
    match parse_rule(spelling)? {
        TokenParseRule::Binary(_, strength) | TokenParseRule::PrefixBinary(_, strength, _) => {
            Some(strength)
        }
        _ => None,
    }
}

pub use cells::{ParsedCell, SyntaxSnapshot};
pub use parentheses::{CallParentheses, UngroupedText};
