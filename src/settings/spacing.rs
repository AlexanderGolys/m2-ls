//! `formatting.spacing.operators.*`: the binary operators the formatter
//! writes spaced and those it writes tight, an operator listed in neither
//! keeping its spacing as written, and the prefix operators written apart
//! from their operand.
//!
//! ```toml
//! [formatting.spacing.operators]
//! spaced = [":=", "=", "->", "==", "+"]  # a := b, a + b
//! tight = ["^", "_", "#"]                # a^b
//! spacedPrefix = ["<<"]                  # << x, but -x and #L
//! ```
//!
//! A list left unset is its default minus what the other list names, so
//! `tight = ["+"]` alone takes `+` out of the default `spaced` list (see
//! [`EXCLUSIVE_LISTS`]). An operator both lists name is spaced.
//!
//! Operators spelled as words (`or`, `and`, `xor`, `not`, `SPACE`, and their
//! qualified `Core$or` forms) are always spaced, as `1 or 2` and `not x`;
//! listing one changes nothing. A postfix operator is tight, as `n!`.

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

/// `formatting.spacing.operators.*`: the binary operators written spaced
/// and those written tight, and the prefix operators written spaced.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperatorSpacingSettings {
    /// `a + b`.
    pub spaced: HashSet<OperatorSpelling>,
    /// `a^b`.
    pub tight: HashSet<OperatorSpelling>,
    /// `<< x`; every other prefix operator is tight, `-x`.
    pub spaced_prefix: HashSet<OperatorSpelling>,
}

impl OperatorSpacingSettings {
    /// How the binary `operator` is spaced; `None` keeps it as written. A
    /// word operator is always spaced: written tight it runs into its
    /// operands (`notx` is a name, and nobody means `1or 2`).
    pub fn binary(&self, operator: &str) -> Option<Spacing> {
        if operator.starts_with(char::is_alphabetic) || self.spaced.contains(operator) {
            Some(Spacing::Spaced)
        } else {
            self.tight.contains(operator).then_some(Spacing::Tight)
        }
    }

    /// How a prefix `operator` is spaced from its operand.
    pub fn prefix(&self, operator: &str) -> Spacing {
        if operator.starts_with(char::is_alphabetic) || self.spaced_prefix.contains(operator) {
            Spacing::Spaced
        } else {
            Spacing::Tight
        }
    }
}
