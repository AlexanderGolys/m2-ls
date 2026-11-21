//! `formatting.spacing.operators.*`: the binary operators the formatter
//! writes spaced, those it writes tight, those it writes as the document
//! mostly does, and that everything listed nowhere keeps its spacing as
//! written.
//!
//! ```toml
//! [formatting.spacing.operators]
//! spaced = [":=", "=", "->", "=="]  # a := b
//! tight = ["^", "_", "#"]           # a^b
//! detect = ["+", "-"]               # a + b in a file that mostly spaces +
//! ```
//!
//! A list left unset is its default minus what the other lists name, so
//! `tight = ["+"]` alone takes `+` out of the default `detect` list (see
//! [`EXCLUSIVE_LISTS`]). An item several lists name is spaced, else tight.
//!
//! Operators spelled as words (`or`, `and`, `xor`, `not`, `SPACE`, and their
//! qualified `Core$or` forms) are always spaced, as `1 or 2` and `not x`;
//! listing one changes nothing. A prefix operator is tight to its operand
//! (`-x`, `#x`) unless `spaced` lists it, as `<< x`; a postfix one is tight.

use std::borrow::Borrow;
use std::collections::HashSet;
use std::fmt;

use m2_syn::nodes::{TokenParseRule, TOKEN_PARSE_RULES};
use serde::de::Error;
use serde::{Deserialize, Deserializer};

pub use super::formatting::Spacing;

/// The package qualifier a word operator may be written with: `Core$or`.
const CORE_QUALIFIER: &str = "Core$";

/// Lists naming the same kind of item written different ways, as JSON
/// pointers into the settings section. An item a user lists in one leaves
/// the others' defaults.
pub const EXCLUSIVE_LISTS: [&[&str]; 1] = [&[
    "/formatting/spacing/operators/spaced",
    "/formatting/spacing/operators/tight",
    "/formatting/spacing/operators/detect",
]];

/// A binary operator the grammar declares, as written in source: `"+"`,
/// `"^**"` or `"=="`. Its hash is its spelling's, so a set of them is looked
/// up by `&str`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OperatorSpelling(&'static str);

impl TryFrom<&str> for OperatorSpelling {
    type Error = String;
    fn try_from(spelling: &str) -> Result<Self, String> {
        let bare = spelling.strip_prefix(CORE_QUALIFIER).unwrap_or(spelling);
        let index = TOKEN_PARSE_RULES
            .binary_search_by_key(&bare, |(declared, _)| declared)
            .map_err(|_| format!("`{spelling}` is not an operator"))?;
        let (spelling, rule) = TOKEN_PARSE_RULES[index];
        matches!(
            rule,
            TokenParseRule::Binary(..) | TokenParseRule::PrefixBinary(..)
        )
        .then_some(Self(spelling))
        .ok_or_else(|| format!("`{spelling}` is not a binary operator"))
    }
}

impl<'de> Deserialize<'de> for OperatorSpelling {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?.as_str()).map_err(D::Error::custom)
    }
}

impl Borrow<str> for OperatorSpelling {
    fn borrow(&self) -> &str {
        self.0
    }
}

impl fmt::Display for OperatorSpelling {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

/// How an operator's spacing is decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OperatorSpacing {
    /// Always this way.
    Fixed(Spacing),
    /// As the document mostly writes the operator.
    Detect,
}

/// `formatting.spacing.operators.*`: the binary operators written spaced,
/// tight, or as each document mostly writes them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperatorSpacingSettings {
    /// `a + b`.
    pub spaced: HashSet<OperatorSpelling>,
    /// `a^b`.
    pub tight: HashSet<OperatorSpelling>,
    /// As the document mostly does, spaced when it says too little.
    pub detect: HashSet<OperatorSpelling>,
}

impl OperatorSpacingSettings {
    /// How the binary `operator` is spaced; `None` keeps it as written. A
    /// word operator is always spaced: written tight it runs into its
    /// operands (`notx` is a name, and nobody means `1or 2`).
    pub fn binary(&self, operator: &str) -> Option<OperatorSpacing> {
        if operator.starts_with(char::is_alphabetic) || self.spaced.contains(operator) {
            Some(OperatorSpacing::Fixed(Spacing::Spaced))
        } else if self.tight.contains(operator) {
            Some(OperatorSpacing::Fixed(Spacing::Tight))
        } else {
            self.detect
                .contains(operator)
                .then_some(OperatorSpacing::Detect)
        }
    }

    /// How a prefix or postfix `operator` is spaced from its operand.
    pub fn unary(&self, operator: &str) -> Spacing {
        if operator.starts_with(char::is_alphabetic) || self.spaced.contains(operator) {
            Spacing::Spaced
        } else {
            Spacing::Tight
        }
    }
}
