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
/// Each `after…`, `during…` and `of…` setting places hints at one kind of
/// place, and each `show…` and `hide…` setting keeps or drops a kind of hint
/// wherever it is placed; all of them are independent. Two places giving the
/// same hint at the same position show it once.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct InlayHintSettings {
    /// After each call whose arguments are bracketed, as `f(x)`, `f()` or
    /// `f(x, y)`, but not `f x`: the type it returns. Default `false`.
    pub after_call: bool,
    /// At the end of each line: the type of the last expression complete on
    /// it, as `2` after `2+` and `2+2` after the `2` below it. Default `true`.
    pub after_line_end: bool,
    /// After each top-level expression, as Macaulay2's `oN : T` line.
    /// Default `true`.
    pub after_cell: bool,
    /// After a name being assigned, where its type is first known or
    /// changes. Default `true`.
    pub during_assignment: bool,
    /// Before each argument of a call, the name of the parameter it is
    /// passed to, as `f(x: 2)`. Default `false`.
    pub arg_names: bool,
    /// Before each argument of a call, the type the parameter it is passed
    /// to is declared for, as `f(Number 2)`, or with `argNames` `f(x: Number
    /// 2)`. Default `false`.
    pub arg_types: bool,
    /// After each parenthesized value, as `(2)` or `(a + b)`: the type of
    /// the value it evaluates to. Not after collections such as `(2,)`,
    /// `()`, `[2]` or `{2}`, nor after a call's arguments. Default `false`.
    pub after_brackets: bool,
    /// After the value a function returns. Default `true`.
    pub after_return: bool,
    /// After every expression. Default `false`.
    pub everywhere: bool,
    /// Also the types a literal states, as in `x = 3`. Default `false`.
    pub show_literal: bool,
    /// Marks a type standing also for its subtypes with `↑`, as `↑Ring`.
    /// Default `true`.
    pub show_upper_closure: bool,
    /// Also the hints that say nothing, of values only known to be a
    /// `Thing`. Default `false`.
    pub show_thing: bool,
    /// Also the hints of values known to be `null`, of type `Nothing`.
    /// Default `false`.
    pub show_nothing: bool,
    /// Also the types a nearby hint already shows: a binding's unchanged
    /// type, and an assignment's beside its binding's. Default `false`.
    pub show_repeated: bool,
    /// Types never shown, e.g. `["ZZ", "String"]`.
    pub hide_types: HashSet<ObjectName>,
    /// Drops the hints of values a `;` ends, whose value is discarded.
    /// Default `false`.
    pub hide_after_semicolon: bool,
    /// The longest label shown in full; a longer one is cut, its whole type
    /// in the tooltip. `0` or `null` never cuts. Default `40`.
    pub max_label_length: Option<u32>,
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
            after_call: false,
            after_line_end: true,
            after_cell: true,
            during_assignment: true,
            arg_names: false,
            arg_types: false,
            after_brackets: false,
            after_return: true,
            everywhere: false,
            show_literal: false,
            show_upper_closure: true,
            show_thing: false,
            show_nothing: false,
            show_repeated: false,
            hide_types: HashSet::new(),
            hide_after_semicolon: false,
            max_label_length: Some(40),
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
    /// Why the value does not deserialize; the default applies instead.
    reason: String,
}

impl fmt::Display for SettingProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "`{}`: {}; default kept", self.path, self.reason)
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
                    "afterCall": true,
                    "afterCell": false,
                    "argNames": true,
                    "argTypes": true,
                    "showLiteral": true,
                    "showUpperClosure": false,
                    "showNothing": true,
                    "hideAfterSemicolon": true,
                    "maxLabelLength": 0,
                    "hideTypes": ["ZZ"],
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
                after_call: true,
                after_cell: false,
                arg_names: true,
                arg_types: true,
                show_literal: true,
                show_upper_closure: false,
                show_nothing: true,
                hide_after_semicolon: true,
                max_label_length: Some(0),
                hide_types: HashSet::from([ObjectName::new("ZZ")]),
                excluded_calls: HashSet::from([ObjectName::new("print")]),
                ..InlayHintSettings::default()
            }
        );
        assert!(settings.semantic_tokens.standard_only);
    }

    #[test]
    fn inlay_hint_settings_are_read_only_under_their_own_names() {
        let parsed = ServerSettings::from_value(&serde_json::json!({
            "inlayHints": { "show": ["calls"], "unknownTypes": true, "afterCall": true }
        }));

        assert_eq!(
            parsed.settings.inlay_hints,
            InlayHintSettings {
                after_call: true,
                ..InlayHintSettings::default()
            }
        );
        let problems = parsed
            .problems
            .iter()
            .map(SettingProblem::to_string)
            .collect::<Vec<_>>();
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(
            problems
                .iter()
                .all(|problem| problem.contains("unknown field")),
            "{problems:?}"
        );
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
