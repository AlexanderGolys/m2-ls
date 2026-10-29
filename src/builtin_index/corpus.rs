//! JSONL deserialization boundary for the generated builtin corpus.

use std::collections::HashMap;

use serde::Deserialize;

use super::OptionPassing;
use these_macros_should_be_illegal::forall;

/// Deserialize every non-empty physical line in a generated JSONL corpus.
///
/// Markdown newlines are escaped inside each JSON object, so one physical line
/// always contains one complete record.
/// The corpus is trusted once loaded: a malformed record is a broken invariant
/// and panics. Untrusted input is vetted first with [`parse_records`].
pub fn deserialize_records(corpus: &str) -> impl Iterator<Item = RawRecord> + '_ {
    parse_records(corpus).map(|record| record.unwrap_or_else(|error| panic!("{error}")))
}

/// Parse every non-empty physical line, reporting malformed records as errors.
pub fn parse_records(corpus: &str) -> impl Iterator<Item = Result<RawRecord, String>> + '_ {
    corpus.lines().filter_map(|line| {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let record = match serde_json::from_str::<RawRecord>(line) {
            Ok(record) => record,
            Err(error) => return Some(Err(format!("malformed corpus line: {error}\n{line}"))),
        };
        if record.kind != "meta" && record.name.is_empty() {
            return Some(Err(format!(
                "corpus record of kind '{}' has no name: {line}",
                record.kind
            )));
        }
        Some(Ok(record))
    })
}

/// Wire representation of one generated corpus record.
#[forall(#[serde(default)])]
#[derive(Debug, Deserialize)]
pub struct RawRecord {
    /// What the record describes: `type`, `function`, `operator`, `symbol`,
    /// `object`, `table`, `package`, or `meta` for the corpus-global record.
    pub kind: String,
    /// The name the record is known by in Macaulay2; empty only on the `meta`
    /// record.
    pub name: String,
    /// The canonical spelling of the name when it differs from [`Self::name`],
    /// used for the record's identity; empty when the name is already
    /// canonical.
    pub normalized_name: String,
    /// Other names bound to the same object, each resolving to this record.
    pub aliases: Vec<String>,
    /// Further lookup keys resolving to this record that are not Macaulay2
    /// names of it.
    pub extra_keys: Vec<String>,
    /// The package defining the object, as a corpus reference; absent for
    /// objects of no package.
    pub package: Option<String>,
    /// The class of the object, as a corpus reference; absent means `Thing`, or
    /// `Package` for a package record.
    pub class: Option<String>,
    /// The parent of a type record, as a corpus reference.
    pub parent: Option<String>,
    /// The documented value type of a callable as a whole, beside the per-
    /// method ones in [`Self::methods`].
    pub typical_value: Option<String>,
    /// The options a callable accepts as a whole.
    pub options: Vec<RawOptionSpec>,
    /// How a callable receives its options, when the corpus knows.
    pub option_passing: Option<OptionPassing>,
    /// The methods installed for a callable, one per domain.
    pub methods: Vec<RawMethod>,
    /// The forms of an operator record; required for `operator` records.
    pub operator: Option<RawOperator>,
    /// On the `meta` record: the packages loaded in a fresh Macaulay2 session,
    /// by bare name.
    pub default_loaded: Vec<String>,
    /// Whether the name is protected against assignment; absent means
    /// protected.
    pub protected: Option<bool>,
    /// The object's documentation, rendered as Markdown.
    pub markdown: Option<String>,
}

/// Wire representation of one callable method signature.
#[forall(#[serde(default)])]
#[derive(Debug, Deserialize)]
pub struct RawMethod {
    /// The types the method is installed for, as corpus references, one per
    /// argument.
    pub domain: Vec<String>,
    /// The documented value type of the method.
    #[serde(rename = "typicalValue")]
    pub typical_value: Option<String>,
    /// The options this method accepts.
    pub options: Vec<RawOptionSpec>,
    /// How this method receives its options, when the corpus knows.
    pub option_passing: Option<OptionPassing>,
}

/// Wire representation of one callable option specification.
#[forall(#[serde(default)])]
#[derive(Debug, Deserialize)]
pub struct RawOptionSpec {
    /// The option's name.
    #[tmsbi(skip)]
    pub key: String,
    /// The values the documentation lists for the option.
    #[serde(rename = "possibleValues")]
    pub possible_values: Vec<String>,
    /// The types the documentation accepts for the option's value.
    #[serde(rename = "acceptedTypes")]
    pub accepted_types: Vec<String>,
}

/// Wire representation of an operator's forms and form attributes.
#[forall(#[serde(default)])]
#[derive(Debug, Deserialize)]
pub struct RawOperator {
    /// The syntactic forms the operator is used in, in lowercase: `binary`,
    /// `prefix`, `postfix` or `assignment`.
    pub forms: Vec<String>,
    /// Attributes of each form, such as `Flexible` for a form whose methods can
    /// be installed at runtime.
    pub attributes: HashMap<RawOperatorForm, Vec<String>>,
}

/// Lowercase operator form used by the generated corpus.
#[derive(Debug, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum RawOperatorForm {
    /// The operator between two operands, `x op y`.
    Binary,
    /// The operator before its operand, `op x`.
    Prefix,
    /// The operator after its operand, `x op`.
    Postfix,
    /// The operator's assignment form, `x op= y`.
    Assignment,
}
