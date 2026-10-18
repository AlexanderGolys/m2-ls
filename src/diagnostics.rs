//! Diagnostics: what m2-ls reports, how it finds it, and how it publishes it.
//!
//! Every diagnostic is one [`DiagnosticRule`] value in the [`catalog`]: the
//! code, name, severity, and tags it is published under. [`collect`] inspects a
//! completed analysis on three occasions — each syntax node, each method
//! installation, and the whole document — and the checks run on each occasion
//! spawn [`M2Diagnostic`]s: a rule together with a [`Finding`] the check worded.
//! Settings filter those by code and [`publish_diagnostics`] sends the rest to
//! the client. Code actions recognize a diagnostic the client sends back by its
//! [`DiagnosticCode`] alone.
//!
//! # Codes
//!
//! A code is a category letter followed by two digits:
//!
//! | Letter | Category |
//! | --- | --- |
//! | `X` | malformed syntax or assignment structure |
//! | `S` | style and simplification hints |
//! | `E` | code Macaulay2 rejects or silently ignores when it runs |
//! | `T` | type errors found by inference |
//!
//! Users write codes and names in their settings, so neither ever changes, and
//! a removed diagnostic's code is never reused: `E07` belonged to the
//! `missing-output-cell` warning, removed in 1.1.0, and `E05` to the
//! `protect-assigned-symbol` hint, removed after 1.2.0 because protecting a
//! name's current value is often intended.

pub mod catalog;
mod checks;
pub mod rewrites;

pub use checks::collect;

use tower_lsp::lsp_types::{
    Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, DiagnosticTag, Location,
    NumberOrString, Range as TextRange, Url,
};
use tower_lsp::Client;

use crate::document::DocumentSnapshot;
use crate::settings::DiagnosticSettings;

/// The stable code a diagnostic is published under, such as `S02`. Only the
/// [`catalog`] creates codes, so every code names a registered diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DiagnosticCode(&'static str);

/// The stable kebab-case name of a diagnostic, such as `"unused-binding"`.
type DiagnosticName = &'static str;

/// One diagnostic m2-ls can report: the code, name, severity, and tags it is
/// published under. Every rule is a value in the [`catalog`].
#[derive(Debug)]
pub struct DiagnosticRule {
    /// The code this diagnostic is published under.
    pub code: DiagnosticCode,
    /// The name users may write instead of the code.
    name: DiagnosticName,
    /// How prominently clients display this diagnostic.
    severity: DiagnosticSeverity,
    /// How clients render the flagged source, such as fading unnecessary code.
    tags: &'static [DiagnosticTag],
}

impl DiagnosticCode {
    /// The diagnostic a user names in their settings, by code or by name.
    ///
    /// ```ignore
    /// assert_eq!(DiagnosticCode::from_selector("S02"), Some(UNUSED_BINDING.code));
    /// assert_eq!(DiagnosticCode::from_selector("unused-binding"), Some(UNUSED_BINDING.code));
    /// assert_eq!(DiagnosticCode::from_selector("E07"), None);
    /// ```
    pub fn from_selector(selector: &str) -> Option<Self> {
        catalog::RULES
            .iter()
            .find(|rule| selector == rule.code.0 || selector == rule.name)
            .map(|rule| rule.code)
    }
}

/// A settings entry naming a diagnostic, by code or by name.
impl<'de> serde::Deserialize<'de> for DiagnosticCode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let selector = String::deserialize(deserializer)?;
        Self::from_selector(&selector)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown diagnostic `{selector}`")))
    }
}

impl DiagnosticCode {
    /// The code of a diagnostic a client sends back with a code-action
    /// request; `None` for diagnostics another server published.
    pub fn from_lsp(diagnostic: &Diagnostic) -> Option<Self> {
        let NumberOrString::String(code) = diagnostic.code.as_ref()? else {
            return None;
        };
        Self::from_selector(code)
    }

    /// Every registered code, in registration order.
    #[cfg(test)]
    pub fn registered() -> impl Iterator<Item = Self> {
        catalog::RULES.iter().map(|rule| rule.code)
    }
}

/// A location elsewhere in the same document that explains a finding, such as
/// the definition the finding is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedNote {
    /// The span the note points at.
    pub range: TextRange,
    /// What that span contributes to the finding.
    pub message: String,
}

/// What a check reports: the span it flags, the explanation, and the related
/// locations behind it. The check's diagnostic supplies everything else.
///
/// ```ignore
/// let finding = Finding::new(range, "`protect x` evaluates the current value of `x`")
///     .with_note(binding.range, "`x` is bound here");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The source span the client underlines.
    pub range: TextRange,
    /// The explanation shown to the reader, ideally stating the fix.
    pub message: String,
    /// Locations that explain the finding.
    pub notes: Vec<RelatedNote>,
}

impl Finding {
    /// A finding covering `range`, worded by `message`, with no related notes.
    pub fn new(range: TextRange, message: impl Into<String>) -> Self {
        Self {
            range,
            message: message.into(),
            notes: Vec::new(),
        }
    }

    /// The finding, additionally pointing at `range` for the reason `message`.
    pub fn with_note(mut self, range: TextRange, message: impl Into<String>) -> Self {
        self.notes.push(RelatedNote {
            range,
            message: message.into(),
        });
        self
    }
}

/// A finding spawned under a rule, ready to be filtered by settings and
/// published.
#[derive(Debug, Clone)]
pub struct M2Diagnostic {
    /// The rule this finding belongs to.
    pub rule: &'static DiagnosticRule,
    /// What the spawning check found.
    pub finding: Finding,
}

impl M2Diagnostic {
    /// The finding as published to clients for the document at `uri`, under
    /// its rule's code, name, severity, and tags.
    ///
    /// ```ignore
    /// let finding = Finding::new(range, "Unused variable total");
    /// let published = M2Diagnostic { rule: &UNUSED_BINDING, finding }.to_lsp(&uri);
    /// assert_eq!(published.code, Some(NumberOrString::String("S02".to_string())));
    /// assert_eq!(published.source.as_deref(), Some("unused-binding"));
    /// ```
    pub fn to_lsp(&self, uri: &Url) -> Diagnostic {
        let rule = self.rule;
        Diagnostic {
            range: self.finding.range,
            severity: Some(rule.severity),
            code: Some(NumberOrString::String(rule.code.0.to_string())),
            source: Some(rule.name.to_string()),
            message: self.finding.message.clone(),
            related_information: (!self.finding.notes.is_empty()).then(|| {
                self.finding
                    .notes
                    .iter()
                    .map(|note| DiagnosticRelatedInformation {
                        location: Location::new(uri.clone(), note.range),
                        message: note.message.clone(),
                    })
                    .collect()
            }),
            tags: (!rule.tags.is_empty()).then(|| rule.tags.to_vec()),
            ..Default::default()
        }
    }
}

/// The findings of the document at `uri` that the user's settings allow,
/// converted for publication.
pub fn visible_diagnostics(
    document: &DocumentSnapshot,
    uri: &Url,
    settings: &DiagnosticSettings,
) -> Vec<Diagnostic> {
    document
        .diagnostics()
        .iter()
        .filter(|diagnostic| settings.allows(diagnostic.rule.code))
        .map(|diagnostic| {
            let mut published = diagnostic.to_lsp(uri);
            published.severity = published
                .severity
                .map(|severity| settings.published_severity(diagnostic.rule.code, severity));
            published
        })
        .collect()
}

/// Publishes the document's visible findings to the client, replacing any it
/// published for `uri` before.
pub async fn publish_diagnostics(
    client: &Client,
    uri: Url,
    document: &DocumentSnapshot,
    settings: &DiagnosticSettings,
) {
    let diagnostics = visible_diagnostics(document, &uri, settings);
    client.publish_diagnostics(uri, diagnostics, None).await;
}

#[cfg(test)]
mod tests {
    use super::catalog::{INSTALL_CODOMAIN_MISMATCH, INSTALL_NO_EFFECT, UNUSED_BINDING};
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn codes_follow_the_category_scheme_and_codes_and_names_are_unique() {
        let mut codes = HashSet::new();
        let mut names = HashSet::new();
        for rule in catalog::RULES {
            let code = rule.code.0;
            assert!(
                matches!(
                    code.as_bytes(),
                    [b'X' | b'S' | b'E' | b'T', b'0'..=b'9', b'0'..=b'9']
                ),
                "invalid diagnostic code `{code}`"
            );
            assert!(codes.insert(code), "duplicate diagnostic code");
            assert!(names.insert(rule.name), "duplicate diagnostic name");
        }
    }

    #[test]
    fn selectors_accept_only_registered_codes_and_names() {
        assert_eq!(
            DiagnosticCode::from_selector("E01"),
            Some(INSTALL_NO_EFFECT.code)
        );
        assert_eq!(
            DiagnosticCode::from_selector("install-codomain-mismatch"),
            Some(INSTALL_CODOMAIN_MISMATCH.code)
        );
        assert_eq!(DiagnosticCode::from_selector("E00"), None);
        assert_eq!(DiagnosticCode::from_selector("E07"), None);
        assert_eq!(DiagnosticCode::from_selector("if-condition-type"), None);
    }

    #[test]
    fn publication_carries_the_rules_tags_and_the_findings_notes() {
        let uri = Url::parse("file:///notes.m2").unwrap();
        let range = TextRange::default();
        let published = M2Diagnostic {
            rule: &UNUSED_BINDING,
            finding: Finding::new(range, "Unused variable total").with_note(range, "bound here"),
        }
        .to_lsp(&uri);

        assert_eq!(published.tags, Some(vec![DiagnosticTag::UNNECESSARY]));
        let related = published
            .related_information
            .expect("the note is published");
        assert_eq!(related[0].location.uri, uri);
        assert_eq!(related[0].message, "bound here");
    }
}
