//! `formatting.operators.*` and `formatting.brackets.*`: what the formatter
//! writes with spaces, what it writes compact, and that everything listed in
//! neither keeps its spacing as written.
//!
//! ```toml
//! [formatting.operators.spacesAround]
//! binary = ["+", "-", "==", ":="]   # a + b
//! unary = ["<<"]                    # << x
//!
//! [formatting.operators.compact]
//! binary = ["^", "_", "#"]          # a^b
//! unary = ["-", "#", "!"]           # -x, n!
//!
//! [formatting.brackets]
//! spacesInside = ["{}"]             # { x }
//! compact = ["()", "[]"]            # (x), [x]
//! ```
//!
//! A list left unset is its default minus what the other list names, so
//! `compact.binary = ["+"]` alone takes `+` out of the default spaced
//! operators (see [`OPPOSED_LISTS`]). An item both lists name is spaced.
//!
//! Operators spelled as words (`or`, `and`, `xor`, `not`, `SPACE`, and their
//! qualified `Core$or` forms) are always spaced, as `1 or 2` and `not x`;
//! listing one changes nothing.

use std::borrow::Borrow;
use std::collections::HashSet;
use std::fmt;
use std::hash::Hash;
use std::marker::PhantomData;

use m2_syn::nodes::{TokenParseRule, TOKEN_PARSE_RULES};
use m2_syn::{lex_str, DelimiterKind, TokenTree};
use serde::de::Error;
use serde::{Deserialize, Deserializer};

/// The package qualifier a word operator may be written with: `Core$or`.
const CORE_QUALIFIER: &str = "Core$";

/// Whether the formatter writes something with spaces or without.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Spacing {
    /// `a + b`, `{ x }`.
    Spaced,
    /// `a^b`, `{x}`.
    Compact,
}

impl Spacing {
    /// The whitespace written on each padded side.
    pub fn padding(self) -> &'static str {
        match self {
            Self::Spaced => " ",
            Self::Compact => "",
        }
    }
}

impl Spacing {
    /// The spacing `operator` has whatever the settings say: a word (`or`,
    /// `and`, `xor`, `not`, `SPACE`, or one qualified as `Core$or`) is always
    /// spaced, since written compact it runs into its operands (`notx` is a
    /// name, and nobody means `1or 2`).
    fn fixed(operator: &str) -> Option<Self> {
        operator
            .starts_with(char::is_alphabetic)
            .then_some(Self::Spaced)
    }

    /// How `item` is written given the `spaced` and `compact` lists; `None`
    /// keeps its spacing as written.
    fn of<T, Q>(item: &Q, spaced: &HashSet<T>, compact: &HashSet<T>) -> Option<Self>
    where
        T: Eq + Hash + Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        if spaced.contains(item) {
            Some(Self::Spaced)
        } else if compact.contains(item) {
            Some(Self::Compact)
        } else {
            None
        }
    }
}

/// Pairs of lists naming the same kind of item written two ways, as JSON
/// pointers into the settings section. An item a user lists in one leaves
/// the other's default.
pub const OPPOSED_LISTS: [[&str; 2]; 3] = [
    [
        "/formatting/operators/spacesAround/binary",
        "/formatting/operators/compact/binary",
    ],
    [
        "/formatting/operators/spacesAround/unary",
        "/formatting/operators/compact/unary",
    ],
    [
        "/formatting/brackets/spacesInside",
        "/formatting/brackets/compact",
    ],
];

/// The operand count an operator list is for, deciding which grammar roles
/// its spellings must have.
pub trait OperatorArity: fmt::Debug + Clone + Copy + Eq + Hash {
    /// How a rejection names the arity, e.g. `binary`.
    const NAME: &str;
    /// Whether a token parsed by `rule` can be used with this arity.
    fn admits(rule: TokenParseRule) -> bool;
}

/// Operators between two operands: `a + b`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Binary {}

/// Operators on one operand, before it or after it: `-x`, `n!`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unary {}

impl OperatorArity for Binary {
    const NAME: &str = "binary";
    fn admits(rule: TokenParseRule) -> bool {
        matches!(
            rule,
            TokenParseRule::Binary(..) | TokenParseRule::PrefixBinary(..)
        )
    }
}

impl OperatorArity for Unary {
    const NAME: &str = "unary";
    fn admits(rule: TokenParseRule) -> bool {
        matches!(
            rule,
            TokenParseRule::Prefix(..)
                | TokenParseRule::PrefixBinary(..)
                | TokenParseRule::Postfix(_)
        )
    }
}

/// An operator the grammar declares with arity `A`, as written in source:
/// `"+"`, `"^**"` or `"(*)"`. Its hash is its spelling's, so a set of them is
/// looked up by `&str`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OperatorSpelling<A> {
    /// The spelling, borrowed from the grammar's token table.
    spelling: &'static str,
    /// The arity the spelling was checked for.
    arity: PhantomData<A>,
}

impl<A: OperatorArity> TryFrom<&str> for OperatorSpelling<A> {
    type Error = String;
    fn try_from(spelling: &str) -> Result<Self, String> {
        let bare = spelling.strip_prefix(CORE_QUALIFIER).unwrap_or(spelling);
        let index = TOKEN_PARSE_RULES
            .binary_search_by_key(&bare, |(declared, _)| declared)
            .map_err(|_| format!("`{spelling}` is not an operator"))?;
        let (spelling, rule) = TOKEN_PARSE_RULES[index];
        A::admits(rule)
            .then_some(Self {
                spelling,
                arity: PhantomData,
            })
            .ok_or_else(|| format!("`{spelling}` is not a {} operator", A::NAME))
    }
}

impl<'de, A: OperatorArity> Deserialize<'de> for OperatorSpelling<A> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?.as_str()).map_err(D::Error::custom)
    }
}

impl<A> Borrow<str> for OperatorSpelling<A> {
    fn borrow(&self) -> &str {
        self.spelling
    }
}

impl<A> fmt::Display for OperatorSpelling<A> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.spelling)
    }
}

/// A bracket pair, written as its opener and closer: `"{}"`, `"()"`, `"[]"`
/// or `"<||>"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BracketPair(pub DelimiterKind);

impl TryFrom<&str> for BracketPair {
    type Error = String;
    fn try_from(spelling: &str) -> Result<Self, String> {
        match lex_str(spelling).as_deref() {
            Ok([TokenTree::Group(group)]) if group.stream().is_empty() => {
                Ok(Self(group.delim_kind()))
            }
            _ => Err(format!(
                "`{spelling}` is not an empty bracket pair such as `{{}}`"
            )),
        }
    }
}

impl<'de> Deserialize<'de> for BracketPair {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?.as_str()).map_err(D::Error::custom)
    }
}

impl fmt::Display for BracketPair {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}{}", self.0.opening(), self.0.closing())
    }
}

/// `formatting.operators.*`: the operators written with spaces around them
/// and those written compact, by arity. A unary operator is spaced or
/// compacted on its operand's side: `<< x`, `-x`, `n!`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperatorSpacingSettings {
    /// `spacesAround`: `a + b`, `<< x`.
    pub spaces_around: OperatorLists,
    /// `compact`: `a^b`, `-x`.
    pub compact: OperatorLists,
}

/// One spacing's operators by arity, e.g. `binary = ["+"]`, `unary = ["<<"]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorLists {
    /// Binary operators.
    pub binary: HashSet<OperatorSpelling<Binary>>,
    /// Prefix and postfix operators.
    pub unary: HashSet<OperatorSpelling<Unary>>,
}

impl OperatorSpacingSettings {
    /// How the binary `operator` is written; `None` keeps it as written.
    pub fn binary(&self, operator: &str) -> Option<Spacing> {
        Spacing::fixed(operator)
            .or_else(|| Spacing::of(operator, &self.spaces_around.binary, &self.compact.binary))
    }

    /// How the prefix or postfix `operator` is written; `None` keeps it as
    /// written.
    pub fn unary(&self, operator: &str) -> Option<Spacing> {
        Spacing::fixed(operator)
            .or_else(|| Spacing::of(operator, &self.spaces_around.unary, &self.compact.unary))
    }
}

/// `formatting.brackets.*`: the bracket pairs padded inside and those written
/// compact; an empty pair is always closed up, as `{}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BracketSpacingSettings {
    /// `spacesInside`: `{ x }`.
    pub spaces_inside: HashSet<BracketPair>,
    /// `compact`: `{x}`.
    pub compact: HashSet<BracketPair>,
}

impl BracketSpacingSettings {
    /// How the inside of `pair` is written; `None` keeps it as written.
    pub fn spacing(&self, pair: BracketPair) -> Option<Spacing> {
        Spacing::of(&pair, &self.spaces_inside, &self.compact)
    }
}
