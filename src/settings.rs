//! The settings a client configures m2-ls with.
//!
//! Each group below is a plain struct whose fields are the settings, under
//! their camelCase JSON names; its `Default` impl holds the defaults. The
//! client sends them under the `m2-ls` section (`macaulay2` is accepted too):
//!
//! ```json
//! { "m2-ls": { "formatting": { "indent": 4 }, "sources": { "paths": ["~/M2"] } } }
//! ```
//!
//! Settings are read leniently: a value that does not deserialize, an unknown
//! key in a group, or an unknown entry in a list is reported as a
//! [`SettingProblem`] and dropped, and everything else still applies. A key
//! that newer settings replaced is read as those, and reported too.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::RwLock;
use std::{fmt, mem};

use serde::Deserialize;
use serde_json::{json, Value};
use serde_path_to_error::{Path, Segment};
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity};

use crate::capabilities::formatting::ControlFlowLayout;
use crate::capabilities::inlay_hints::{CallForm, HintAnchor};
use crate::diagnostics::DiagnosticCode;
use crate::object_registry::ObjectName;

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
    pub semantic_tokens: SemanticTokenSettings,
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
    pub indent: Option<u32>,
    /// Whether to indent with tabs; the editor's choice when unset.
    pub use_tabs: Option<bool>,
    /// The width lines are preferably wrapped at; `0` or `null` disables it.
    /// It never exceeds `hardLineWidth`.
    pub soft_line_width: Option<u32>,
    /// The width lines are always wrapped at; `0` or `null` disables it.
    pub hard_line_width: Option<u32>,
    /// `compact`, `multiline`, or `multilineCompactElse`.
    pub control_flow_layout: ControlFlowLayout,
    /// `2*x` instead of `2 * x`.
    pub compact_factor_operators: bool,
    /// Whether the next expression after `;` starts on a new line.
    pub break_after_semicolon: bool,
}

/// `inlayHints.*`: which inferred types are shown inline, and where.
///
/// `show` picks the places hints are attached to; the other settings drop
/// the hints that would only repeat what the source says.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct InlayHintSettings {
    /// The places types are shown at. Default
    /// `["bindings", "returns", "cells", "lineEnds"]`.
    pub show: HashSet<HintAnchor>,
    /// Which calls `calls` annotates: `bracketed` (as in `f(x)`) or `all`
    /// (also `f x`). Default `bracketed`.
    pub call_form: CallForm,
    /// Also the types a literal states, as in `x = 3`. Default `false`.
    pub literal_types: bool,
    /// Also the types a nearby hint already shows: a binding's unchanged
    /// type, and an assignment's beside its binding's. Default `false`.
    pub repeated_types: bool,
    /// Also the hints that say nothing, of values only known to be a
    /// `Thing`. Default `false`.
    pub unknown_types: bool,
    /// The longest label shown in full; a longer one is cut, its whole type
    /// in the tooltip. `0` or `null` never cuts. Default `40`.
    pub max_label_length: Option<u32>,
    /// Types never shown, e.g. `["ZZ", "String"]`.
    pub hidden_types: HashSet<ObjectName>,
    /// Functions whose calls and arguments get no hints, e.g. `["print"]`.
    pub excluded_calls: HashSet<ObjectName>,
}

/// `semanticTokens.*`: the vocabulary semantic highlighting is sent in.
///
/// The legend offers standard token types and both standard and custom
/// modifiers. Changes take effect through a semantic-token refresh.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct SemanticTokenSettings {
    /// Only LSP-standard modifiers: the Macaulay2-specific ones, such as
    /// `option` or `algebraic`, are dropped. Token types are always
    /// standard. Default `false`.
    pub standard_only: bool,
    /// Which values receive `defaultLibrary`: a named policy or a list of
    /// defining packages. Default `"CompiledFunction"`.
    pub default_library: DefaultLibrarySetting,
}

/// Selects `defaultLibrary` by runtime implementation or defining package.
/// JSON accepts a policy string or an array such as `["Core", "Graphs"]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum DefaultLibrarySetting {
    Mode(DefaultLibraryMode),
    Packages(Vec<String>),
}

impl Default for DefaultLibrarySetting {
    fn default() -> Self {
        Self::Mode(DefaultLibraryMode::CompiledFunction)
    }
}

/// Named policies for `semanticTokens.defaultLibrary`. Package policies apply
/// to package globals; local aliases keep their own binding's package origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum DefaultLibraryMode {
    /// `CompiledFunction` values, including aliases, from any package.
    CompiledFunction,
    /// `CompiledFunction` and `CompiledFunctionClosure`, including subtypes.
    Compiled,
    /// Bindings defined in `Core`.
    Core,
    /// Bindings from the library index's preloaded packages.
    Prelude,
    /// Globals defined in any indexed or source package.
    AllPackages,
    /// Disable the modifier.
    None,
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
            indent: None,
            use_tabs: None,
            soft_line_width: Some(78),
            hard_line_width: Some(90),
            control_flow_layout: ControlFlowLayout::default(),
            compact_factor_operators: true,
            break_after_semicolon: false,
        }
    }
}

impl Default for InlayHintSettings {
    fn default() -> Self {
        Self {
            show: HintAnchor::DEFAULT_SHOWN.into_iter().collect(),
            call_form: CallForm::default(),
            literal_types: false,
            repeated_types: false,
            unknown_types: false,
            max_label_length: Some(40),
            hidden_types: HashSet::new(),
            excluded_calls: HashSet::new(),
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

/// One setting the server could not read as given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingProblem {
    /// Where the value sits in the section, e.g. `diagnostics.disabled[1]`.
    path: String,
    issue: SettingIssue,
}

/// What is wrong with a [`SettingProblem`]'s value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
enum SettingIssue {
    /// It does not deserialize, for this reason; the default applies.
    Invalid(String),
    /// Its key was replaced; it is read as the settings described.
    Replaced(&'static str),
}

impl fmt::Display for SettingProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.issue {
            SettingIssue::Invalid(reason) => {
                write!(formatter, "`{}`: {reason}; default kept", self.path)
            }
            SettingIssue::Replaced(replacement) => {
                write!(
                    formatter,
                    "`{}`: deprecated, read as {replacement}",
                    self.path
                )
            }
        }
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
        let mut problems = InlayHintSettings::upgrade_replaced_keys(&mut section);
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
                        issue: SettingIssue::Invalid(error.into_inner().to_string()),
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

impl InlayHintSettings {
    /// The group's key in the settings section.
    const SECTION_KEY: &str = "inlayHints";

    /// Rewrite the keys of the `inlayHints` group in `section` that `show`
    /// and the type filters replaced into those, reporting each.
    ///
    /// `expressionTypes` became the `subexpressions` anchor; `allKnownTypes`
    /// became it with `literalTypes` and `repeatedTypes`; `calls` became the
    /// `lineEnds` and `calls` anchors with `callForm`.
    fn upgrade_replaced_keys(section: &mut Value) -> Vec<SettingProblem> {
        let Some(group) = section
            .get_mut(Self::SECTION_KEY)
            .and_then(Value::as_object_mut)
        else {
            return Vec::new();
        };
        let mut shown = match group.get("show") {
            None => Some(
                HintAnchor::DEFAULT_SHOWN
                    .iter()
                    .map(|anchor| json!(anchor))
                    .collect::<Vec<_>>(),
            ),
            Some(Value::Array(anchors)) => Some(anchors.clone()),
            // Left for deserialization to report.
            Some(_) => None,
        };
        let mut show = |anchor: HintAnchor, shown_or_hidden: bool| {
            if let Some(shown) = shown.as_mut() {
                let anchor = json!(anchor);
                shown.retain(|listed| *listed != anchor);
                if shown_or_hidden {
                    shown.push(anchor);
                }
            }
        };
        let mut problems = Vec::new();
        let mut replaced = |key: &str, replacement: &'static str| {
            problems.push(SettingProblem {
                path: format!("{}.{key}", Self::SECTION_KEY),
                issue: SettingIssue::Replaced(replacement),
            });
        };

        if let Some(expression_types) = group.remove("expressionTypes") {
            if expression_types == Value::Bool(true) {
                show(HintAnchor::Subexpressions, true);
            }
            replaced("expressionTypes", "`show` listing `subexpressions`");
        }
        if let Some(all_known_types) = group.remove("allKnownTypes") {
            if all_known_types == Value::Bool(true) {
                show(HintAnchor::Subexpressions, true);
                for filter in ["literalTypes", "repeatedTypes"] {
                    group.entry(filter).or_insert(Value::Bool(true));
                }
            }
            replaced(
                "allKnownTypes",
                "`show` listing `subexpressions`, with `literalTypes` and `repeatedTypes`",
            );
        }
        if let Some(calls) = group.remove("calls") {
            match calls.as_str() {
                Some("off") => show(HintAnchor::LineEnds, false),
                Some("every") => {
                    show(HintAnchor::Calls, true);
                    group
                        .entry("callForm")
                        .or_insert_with(|| json!(CallForm::All));
                }
                _ => {}
            }
            replaced(
                "calls",
                "`show` listing `lineEnds` or `calls`, with `callForm`",
            );
        }
        if let Some(shown) = shown.filter(|_| !problems.is_empty()) {
            group.insert("show".to_string(), Value::Array(shown));
        }
        problems
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
        assert_eq!(settings.formatting, FormattingSettings::default());
        assert_eq!(settings.inlay_hints, InlayHintSettings::default());
        assert!(!settings.semantic_tokens.standard_only);
        assert_eq!(
            settings.semantic_tokens.default_library,
            DefaultLibrarySetting::Mode(DefaultLibraryMode::CompiledFunction)
        );
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
                    "indent": 2,
                    "useTabs": false,
                    "softLineWidth": 88,
                    "hardLineWidth": 88,
                    "controlFlowLayout": "multiline",
                    "compactFactorOperators": true,
                    "breakAfterSemicolon": false
                },
                "inlayHints": {
                    "show": ["calls", "arguments"],
                    "callForm": "all",
                    "literalTypes": true,
                    "maxLabelLength": 0,
                    "hiddenTypes": ["ZZ"],
                    "excludedCalls": ["print"]
                },
                "semanticTokens": {
                    "standardOnly": true
                }
            }
        }));

        assert!(!settings.diagnostics.allows(catalog::UNUSED_BINDING.code));
        assert!(!settings
            .diagnostics
            .allows(catalog::OPTION_KEY_CONVENTION.code));
        assert!(settings.diagnostics.allows(catalog::SYNTAX_ERROR.code));
        assert_eq!(settings.formatting.indent, Some(2));
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
                show: HashSet::from([HintAnchor::Calls, HintAnchor::Arguments]),
                call_form: CallForm::All,
                literal_types: true,
                max_label_length: Some(0),
                hidden_types: HashSet::from([ObjectName::new("ZZ")]),
                excluded_calls: HashSet::from([ObjectName::new("print")]),
                ..InlayHintSettings::default()
            }
        );
        assert!(settings.semantic_tokens.standard_only);
    }

    #[test]
    fn replaced_inlay_hint_keys_are_read_as_their_replacements() {
        let parsed = ServerSettings::from_value(&serde_json::json!({
            "inlayHints": {
                "expressionTypes": true,
                "allKnownTypes": false,
                "calls": "every",
                "unknownTypes": true
            }
        }));

        let mut show = HashSet::from(HintAnchor::DEFAULT_SHOWN);
        show.extend([HintAnchor::Subexpressions, HintAnchor::Calls]);
        assert_eq!(
            parsed.settings.inlay_hints,
            InlayHintSettings {
                show,
                call_form: CallForm::All,
                unknown_types: true,
                ..InlayHintSettings::default()
            }
        );
        let problems = parsed
            .problems
            .iter()
            .map(SettingProblem::to_string)
            .collect::<Vec<_>>();
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(
            problems
                .iter()
                .all(|problem| problem.contains("deprecated, read as")),
            "{problems:?}"
        );

        // `allKnownTypes` turns on both filters it was split into, and
        // `calls: off` hides line ends, which once showed only calls.
        let parsed = ServerSettings::from_value(&serde_json::json!({
            "inlayHints": { "allKnownTypes": true, "calls": "off", "show": ["bindings", "lineEnds"] }
        }));
        let hints = &parsed.settings.inlay_hints;
        assert_eq!(
            hints.show,
            HashSet::from([HintAnchor::Bindings, HintAnchor::Subexpressions])
        );
        assert!(hints.literal_types && hints.repeated_types);
    }

    #[test]
    fn default_library_accepts_named_policies_or_package_lists() {
        for (name, mode) in [
            ("CompiledFunction", DefaultLibraryMode::CompiledFunction),
            ("Compiled", DefaultLibraryMode::Compiled),
            ("Core", DefaultLibraryMode::Core),
            ("Prelude", DefaultLibraryMode::Prelude),
            ("AllPackages", DefaultLibraryMode::AllPackages),
            ("None", DefaultLibraryMode::None),
        ] {
            let settings =
                valid_settings(serde_json::json!({"semanticTokens": {"defaultLibrary": name}}));
            assert_eq!(
                settings.semantic_tokens.default_library,
                DefaultLibrarySetting::Mode(mode)
            );
        }
        for packages in [vec!["Core", "Graphs"], vec![]] {
            let settings =
                valid_settings(serde_json::json!({"semanticTokens": {"defaultLibrary": packages}}));
            assert_eq!(
                settings.semantic_tokens.default_library,
                DefaultLibrarySetting::Packages(packages.into_iter().map(str::to_string).collect())
            );
        }
        let parsed = ServerSettings::from_value(&serde_json::json!({
            "semanticTokens": {"defaultLibrary": "unknown", "standardOnly": true}
        }));
        assert_eq!(
            parsed.settings.semantic_tokens.default_library,
            DefaultLibrarySetting::default()
        );
        assert!(parsed.settings.semantic_tokens.standard_only);
        assert_eq!(parsed.problems.len(), 1);
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
                    "indent": "two",
                    "useTabs": true
                },
                "inlayHints": "on"
            }
        }));

        let settings = &parsed.settings;
        assert!(!settings.diagnostics.allows(catalog::UNUSED_BINDING.code));
        assert!(settings.diagnostics.allows(catalog::SYNTAX_ERROR.code));
        assert_eq!(settings.formatting.indent, None);
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
            problems[2].starts_with("`formatting.indent`: invalid type"),
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
