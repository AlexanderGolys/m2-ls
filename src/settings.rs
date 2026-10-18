//! The settings a client configures m2-ls with.
//!
//! Each group below is a plain struct whose fields are the settings, under
//! their camelCase JSON names; its `Default` impl holds the defaults. The
//! client sends them under the `m2-ls` section (`macaulay2` is accepted too):
//!
//! ```json
//! { "m2-ls": { "formatting": { "indentWidth": 4 }, "sources": { "paths": ["~/M2"] } } }
//! ```
//!
//! Settings are read leniently: a value that does not deserialize, an unknown
//! key in a group, or an unknown entry in a list is reported as a
//! [`SettingProblem`] and dropped, and everything else still applies.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::RwLock;
use std::{fmt, mem};

use serde::Deserialize;
use serde_json::Value;
use serde_path_to_error::{Path, Segment};
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity};

use crate::capabilities::formatting::ControlFlowLayout;
use crate::diagnostics::DiagnosticCode;

/// The current value of a setting that requests read while the client
/// replaces it.
#[derive(Debug, Default)]
pub struct SettingsStore<T> {
    current: RwLock<T>,
}

impl<T> SettingsStore<T> {
    pub fn new(value: T) -> Self {
        Self {
            current: RwLock::new(value),
        }
    }

    pub fn replace(&self, value: T) -> T {
        mem::replace(
            &mut self
                .current
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            value,
        )
    }
}

impl<T: Clone> SettingsStore<T> {
    pub fn snapshot(&self) -> T {
        self.current
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// Every setting m2-ls reads. Other top-level keys of the section belong to
/// the client (e.g. an extension's `server.path`) and are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerSettings {
    pub diagnostics: DiagnosticSettings,
    pub formatting: FormattingSettings,
    pub inlay_hints: InlayHintSettings,
    pub sources: SourceSettings,
}

/// `diagnostics.*`: which diagnostics are published, and how prominently.
/// Rules are named by code or by name, e.g. `T02` or `unused-binding`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticSettings {
    /// Whether any diagnostic is shown. Default `true`.
    pub enabled: bool,
    /// Rules that are never shown.
    pub disabled: HashSet<DiagnosticCode>,
    /// Rules whose warnings are shown as hints.
    pub warnings_as_hint: HashSet<DiagnosticCode>,
    /// Rules whose warnings are shown as information.
    pub warnings_as_info: HashSet<DiagnosticCode>,
}

/// `formatting.*`: how the formatter lays out a document.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct FormattingSettings {
    /// Spaces per indentation level; the editor's tab size when unset.
    pub indent_width: Option<u32>,
    /// Whether to indent with tabs; the editor's choice when unset.
    pub use_tabs: Option<bool>,
    /// The width lines are preferably wrapped at. Default `100`; `0` or
    /// `null` disables it. It never exceeds `hardLineWidth`.
    pub soft_line_width: Option<u32>,
    /// The width lines are always wrapped at. Default `100`; `0` or `null`
    /// disables it.
    pub hard_line_width: Option<u32>,
    /// `compact`, `multiline`, or `multilineCompactElse` (the default).
    pub control_flow_layout: ControlFlowLayout,
    /// `2*x` instead of `2 * x`. Default `false`.
    pub compact_factor_operators: bool,
    /// Whether the next expression after `;` starts on a new line. Default
    /// `true`.
    pub break_after_semicolon: bool,
}

/// `inlayHints.*`: which inferred types are shown inline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct InlayHintSettings {
    /// Argument, subexpression, and call-result types. Default `false`.
    pub expression_types: bool,
    /// Also the types that are obvious from literals. Default `false`.
    pub all_known_types: bool,
}

/// `sources.*`: where navigation finds the Macaulay2 library sources.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSettings {
    /// Whether navigation enters the library sources at all. Default `true`.
    pub enabled: bool,
    /// Searched before the discovered installation: each an `M2` executable
    /// or an installation prefix (a build tree's `usr-dist` included),
    /// standing for that installation, or else a directory added to
    /// Macaulay2's `path`. A leading `~` is the home directory.
    pub paths: Vec<PathBuf>,
}

impl Default for DiagnosticSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            disabled: HashSet::new(),
            warnings_as_hint: HashSet::new(),
            warnings_as_info: HashSet::new(),
        }
    }
}

impl Default for FormattingSettings {
    fn default() -> Self {
        Self {
            indent_width: None,
            use_tabs: None,
            soft_line_width: Some(100),
            hard_line_width: Some(100),
            control_flow_layout: ControlFlowLayout::MultilineCompactElse,
            compact_factor_operators: false,
            break_after_semicolon: true,
        }
    }
}

impl Default for SourceSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            paths: Vec::new(),
        }
    }
}

/// Settings read from a client, plus every value that could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSettings {
    pub settings: ServerSettings,
    pub problems: Vec<SettingProblem>,
}

/// One setting the server ignored, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingProblem {
    /// Where the value sits in the section, e.g. `diagnostics.disabled[1]`.
    path: String,
    reason: String,
}

impl fmt::Display for SettingProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "`{}`: {}", self.path, self.reason)
    }
}

impl ServerSettings {
    /// The names the client may send the settings section under.
    const SECTIONS: [&str; 2] = ["m2-ls", "macaulay2"];

    /// Read the settings in `value`, either the section itself or an object
    /// holding it. Each value that fails to deserialize is reported, removed,
    /// and the rest read again, until what remains reads cleanly.
    pub fn from_value(value: &Value) -> ParsedSettings {
        let mut section = Self::SECTIONS
            .iter()
            .find_map(|name| value.get(name))
            .unwrap_or(value)
            .clone();
        if section.is_null() {
            section = Value::Object(Default::default());
        }
        let mut problems = Vec::new();
        loop {
            match serde_path_to_error::deserialize::<_, Self>(&section) {
                Ok(settings) => return ParsedSettings { settings, problems },
                Err(error) => {
                    let removed = Self::remove_value(&mut section, error.path());
                    problems.push(SettingProblem {
                        path: if error.path().iter().next().is_some() {
                            error.path().to_string()
                        } else {
                            "settings".to_string()
                        },
                        reason: error.into_inner().to_string(),
                    });
                    if !removed {
                        return ParsedSettings {
                            settings: Self::default(),
                            problems,
                        };
                    }
                }
            }
        }
    }

    /// Remove the value at `path` from `section`; `false` when the path does
    /// not lead to a removable value, such as the section itself.
    fn remove_value(section: &mut Value, path: &Path) -> bool {
        let segments = path.iter().collect::<Vec<_>>();
        let Some((last, parents)) = segments.split_last() else {
            return false;
        };
        let mut parent = section;
        for segment in parents {
            let child = match (segment, parent) {
                (Segment::Map { key }, Value::Object(fields)) => fields.get_mut(key),
                (Segment::Seq { index }, Value::Array(items)) => items.get_mut(*index),
                _ => None,
            };
            let Some(child) = child else {
                return false;
            };
            parent = child;
        }
        match (last, parent) {
            (Segment::Map { key }, Value::Object(fields)) => fields.remove(key).is_some(),
            (Segment::Seq { index }, Value::Array(items)) if *index < items.len() => {
                items.remove(*index);
                true
            }
            _ => false,
        }
    }
}

impl DiagnosticSettings {
    /// Whether findings filed under `code` are shown: diagnostics are enabled
    /// and the user has not disabled this one.
    pub fn allows(&self, code: DiagnosticCode) -> bool {
        self.enabled && !self.disabled.contains(&code)
    }

    /// The severity a finding filed under `code` is published with: a warning
    /// the user listed in `warningsAsHint` or `warningsAsInfo` is lowered.
    pub fn published_severity(
        &self,
        code: DiagnosticCode,
        severity: DiagnosticSeverity,
    ) -> DiagnosticSeverity {
        if severity != DiagnosticSeverity::WARNING {
            severity
        } else if self.warnings_as_hint.contains(&code) {
            DiagnosticSeverity::HINT
        } else if self.warnings_as_info.contains(&code) {
            DiagnosticSeverity::INFORMATION
        } else {
            severity
        }
    }

    /// Whether a diagnostic a client sent back is shown. Diagnostics another
    /// server published have no code here and are always kept.
    pub fn allows_lsp_diagnostic(&self, diagnostic: &Diagnostic) -> bool {
        DiagnosticCode::from_lsp(diagnostic).is_none_or(|code| self.allows(code))
    }
}

impl FormattingSettings {
    /// The soft and hard wrapping widths in effect: a zero width is none, and
    /// the soft width never exceeds the hard one.
    pub fn line_widths(&self) -> (Option<u32>, Option<u32>) {
        let hard = self.hard_line_width.filter(|width| *width > 0);
        let soft = self
            .soft_line_width
            .filter(|width| *width > 0)
            .map(|width| hard.map_or(width, |hard| width.min(hard)));
        (soft, hard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::catalog;

    /// Settings parsed from `value`, which must contain no invalid setting.
    fn valid_settings(value: Value) -> ServerSettings {
        let parsed = ServerSettings::from_value(&value);
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        parsed.settings
    }

    #[test]
    fn defaults_enable_diagnostics_and_canonical_factor_spacing() {
        let settings = valid_settings(serde_json::json!({}));

        assert!(settings.diagnostics.allows(catalog::UNUSED_BINDING.code));
        assert_eq!(settings.formatting.line_widths(), (Some(100), Some(100)));
        assert_eq!(
            settings.formatting.control_flow_layout,
            ControlFlowLayout::MultilineCompactElse
        );
        assert!(!settings.formatting.compact_factor_operators);
        assert!(settings.formatting.break_after_semicolon);
        assert_eq!(settings.inlay_hints, InlayHintSettings::default());
        assert_eq!(valid_settings(Value::Null), settings);
    }

    #[test]
    fn parses_nested_editor_settings_and_diagnostic_selectors() {
        let settings = valid_settings(serde_json::json!({
            "m2-ls": {
                "diagnostics": {
                    "disabled": ["unused-binding", "S01"]
                },
                "formatting": {
                    "indentWidth": 2,
                    "useTabs": false,
                    "hardLineWidth": 88,
                    "controlFlowLayout": "multiline",
                    "compactFactorOperators": true,
                    "breakAfterSemicolon": false
                },
                "inlayHints": {
                    "expressionTypes": true,
                    "allKnownTypes": true
                }
            }
        }));

        assert!(!settings.diagnostics.allows(catalog::UNUSED_BINDING.code));
        assert!(!settings
            .diagnostics
            .allows(catalog::OPTION_KEY_CONVENTION.code));
        assert!(settings.diagnostics.allows(catalog::SYNTAX_ERROR.code));
        assert_eq!(settings.formatting.indent_width, Some(2));
        assert_eq!(settings.formatting.use_tabs, Some(false));
        assert_eq!(settings.formatting.line_widths(), (Some(88), Some(88)));
        assert_eq!(
            settings.formatting.control_flow_layout,
            ControlFlowLayout::Multiline
        );
        assert!(settings.formatting.compact_factor_operators);
        assert!(!settings.formatting.break_after_semicolon);
        assert_eq!(
            settings.inlay_hints,
            InlayHintSettings {
                expression_types: true,
                all_known_types: true,
            }
        );
    }

    #[test]
    fn one_invalid_value_keeps_every_other_setting() {
        let parsed = ServerSettings::from_value(&serde_json::json!({
            "macaulay2": {
                "server": { "path": "/opt/m2-ls" },
                "diagnostics": {
                    "disabled": ["not-a-rule", "unused-binding"],
                    "enabeld": false
                },
                "formatting": {
                    "indentWidth": "two",
                    "useTabs": true
                },
                "inlayHints": "on"
            }
        }));

        let settings = &parsed.settings;
        assert!(!settings.diagnostics.allows(catalog::UNUSED_BINDING.code));
        assert!(settings.diagnostics.allows(catalog::SYNTAX_ERROR.code));
        assert_eq!(settings.formatting.indent_width, None);
        assert_eq!(settings.formatting.use_tabs, Some(true));
        assert_eq!(settings.inlay_hints, InlayHintSettings::default());

        let problems = parsed
            .problems
            .iter()
            .map(SettingProblem::to_string)
            .collect::<Vec<_>>();
        assert_eq!(problems.len(), 4, "{problems:?}");
        assert!(
            problems[0].starts_with("`diagnostics.disabled[0]`: unknown diagnostic `not-a-rule`"),
            "{problems:?}"
        );
        assert!(
            problems[1].starts_with("`diagnostics.enabeld`: unknown field"),
            "{problems:?}"
        );
        assert!(
            problems[2].starts_with("`formatting.indentWidth`: invalid type"),
            "{problems:?}"
        );
        assert!(
            problems[3].starts_with("`inlayHints`: invalid type"),
            "{problems:?}"
        );
    }

    #[test]
    fn a_section_that_is_not_an_object_keeps_the_defaults() {
        let parsed = ServerSettings::from_value(&serde_json::json!("on"));

        assert_eq!(parsed.settings, ServerSettings::default());
        assert_eq!(parsed.problems.len(), 1);
        assert!(parsed.problems[0].to_string().starts_with("`settings`:"));
    }

    #[test]
    fn listed_warnings_are_published_as_hints_or_information() {
        let settings = valid_settings(serde_json::json!({
            "diagnostics": {
                "warningsAsHint": ["unused-binding"],
                "warningsAsInfo": ["S01"]
            }
        }));
        let diagnostics = &settings.diagnostics;

        assert_eq!(
            diagnostics
                .published_severity(catalog::UNUSED_BINDING.code, DiagnosticSeverity::WARNING),
            DiagnosticSeverity::HINT
        );
        assert_eq!(
            diagnostics.published_severity(
                catalog::OPTION_KEY_CONVENTION.code,
                DiagnosticSeverity::WARNING
            ),
            DiagnosticSeverity::INFORMATION
        );
        assert_eq!(
            diagnostics.published_severity(catalog::UNUSED_BINDING.code, DiagnosticSeverity::ERROR),
            DiagnosticSeverity::ERROR,
            "only warnings are lowered"
        );
    }

    #[test]
    fn can_disable_every_diagnostic() {
        let settings = valid_settings(serde_json::json!({
            "diagnostics": {
                "enabled": false
            }
        }));

        assert!(DiagnosticCode::registered().all(|code| !settings.diagnostics.allows(code)));
    }

    #[test]
    fn null_or_zero_line_widths_disable_wrapping() {
        for width in [Value::Null, serde_json::json!(0)] {
            let settings = valid_settings(serde_json::json!({
                "formatting": {
                    "softLineWidth": width,
                    "hardLineWidth": width
                }
            }));

            assert_eq!(settings.formatting.line_widths(), (None, None));
        }
    }

    #[test]
    fn parses_soft_and_hard_line_widths_and_clamps_the_soft_target() {
        let settings = valid_settings(serde_json::json!({
            "formatting": {
                "softLineWidth": 90,
                "hardLineWidth": 110
            }
        }));
        assert_eq!(settings.formatting.line_widths(), (Some(90), Some(110)));

        let clamped = valid_settings(serde_json::json!({
            "formatting": {
                "softLineWidth": 140,
                "hardLineWidth": 120
            }
        }));
        assert_eq!(clamped.formatting.line_widths(), (Some(120), Some(120)));
    }

    #[test]
    fn parses_every_control_flow_layout_variant() {
        for (name, expected) in [
            ("compact", ControlFlowLayout::Compact),
            ("multiline", ControlFlowLayout::Multiline),
            (
                "multilineCompactElse",
                ControlFlowLayout::MultilineCompactElse,
            ),
        ] {
            let settings = valid_settings(serde_json::json!({
                "formatting": {
                    "controlFlowLayout": name
                }
            }));

            assert_eq!(settings.formatting.control_flow_layout, expected);
        }
    }

    #[test]
    fn generic_store_replaces_complete_snapshots() {
        let store = SettingsStore::<Vec<u8>>::default();

        assert!(store.replace(vec![1, 2, 3]).is_empty());

        assert_eq!(store.snapshot(), vec![1, 2, 3]);
    }
}
