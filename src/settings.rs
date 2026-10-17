use std::collections::HashSet;
use std::fmt;
use std::sync::RwLock;

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity};

use crate::capabilities::formatting::{ControlFlowLayout, FormattingConfiguration};
use crate::capabilities::inlay_hints::InlayHintOptions;
use crate::diagnostics::DiagnosticCode;

#[derive(Debug)]
pub struct SettingsStore<T> {
    current: RwLock<T>,
}

impl<T: Default> Default for SettingsStore<T> {
    fn default() -> Self {
        Self {
            current: RwLock::new(T::default()),
        }
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

impl<T> SettingsStore<T> {
    pub fn replace(&self, value: T) -> T {
        std::mem::replace(
            &mut self
                .current
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            value,
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerSettings {
    diagnostics: DiagnosticSettings,
    formatting: FormattingSettings,
    inlay_hints: InlayHintSettings,
}

/// Settings read from a client, plus every value that could not be used.
/// Each setting is read on its own, so one bad value never discards the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSettings {
    pub settings: ServerSettings,
    pub problems: Vec<SettingProblem>,
}

/// One setting the server ignored, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingProblem {
    path: String,
    reason: String,
}

impl fmt::Display for SettingProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "`{}`: {}", self.path, self.reason)
    }
}

/// The settings groups this server owns. Other top-level keys in the shared
/// section belong to the client (e.g. an extension's `server.path`).
const DIAGNOSTICS_GROUP: &str = "diagnostics";
const FORMATTING_GROUP: &str = "formatting";
const INLAY_HINTS_GROUP: &str = "inlayHints";

impl ServerSettings {
    pub fn from_value(value: &Value) -> ParsedSettings {
        let section = value
            .get("m2-ls")
            .or_else(|| value.get("macaulay2"))
            .unwrap_or(value);
        let mut problems = Vec::new();
        let settings = match section {
            Value::Object(section) => Self {
                diagnostics: read_group(
                    section,
                    DIAGNOSTICS_GROUP,
                    &mut problems,
                    DiagnosticSettings::read,
                ),
                formatting: read_group(
                    section,
                    FORMATTING_GROUP,
                    &mut problems,
                    FormattingSettings::read,
                ),
                inlay_hints: read_group(
                    section,
                    INLAY_HINTS_GROUP,
                    &mut problems,
                    InlayHintSettings::read,
                ),
            },
            Value::Null => Self::default(),
            other => {
                problems.push(SettingProblem {
                    path: "settings".to_string(),
                    reason: format!("expected an object, got {other}"),
                });
                Self::default()
            }
        };
        ParsedSettings { settings, problems }
    }

    pub fn diagnostics(&self) -> &DiagnosticSettings {
        &self.diagnostics
    }

    pub fn formatting(&self) -> &FormattingSettings {
        &self.formatting
    }

    pub fn inlay_hints(&self) -> &InlayHintSettings {
        &self.inlay_hints
    }

    pub fn inlay_hint_options(&self) -> InlayHintOptions {
        InlayHintOptions {
            expression_types: self.inlay_hints.expression_types,
            all_known_types: self.inlay_hints.all_known_types,
        }
    }
}

/// Read the group `name` of `section` with `read`, then report every key in it
/// that `read` never asked for: an unknown key is most likely a typo.
fn read_group<Settings>(
    section: &Map<String, Value>,
    name: &'static str,
    problems: &mut Vec<SettingProblem>,
    read: impl FnOnce(&mut SettingsGroup<'_>) -> Settings,
) -> Settings {
    let mut group = SettingsGroup::open(section, name, problems);
    let settings = read(&mut group);
    group.report_unknown_keys();
    settings
}

/// One settings group (`diagnostics`, `formatting`, …) read key by key: an
/// invalid or unknown key is recorded as a problem and the rest still apply.
struct SettingsGroup<'settings> {
    name: &'static str,
    fields: Option<&'settings Map<String, Value>>,
    read_keys: Vec<&'static str>,
    problems: &'settings mut Vec<SettingProblem>,
}

impl<'settings> SettingsGroup<'settings> {
    fn open(
        section: &'settings Map<String, Value>,
        name: &'static str,
        problems: &'settings mut Vec<SettingProblem>,
    ) -> Self {
        let fields = match section.get(name) {
            None | Some(Value::Null) => None,
            Some(Value::Object(fields)) => Some(fields),
            Some(other) => {
                problems.push(SettingProblem {
                    path: name.to_string(),
                    reason: format!("expected an object, got {other}"),
                });
                None
            }
        };
        Self {
            name,
            fields,
            read_keys: Vec::new(),
            problems,
        }
    }

    /// The value configured for `key`, or `None` when it is absent or invalid.
    fn read<T: DeserializeOwned>(&mut self, key: &'static str) -> Option<T> {
        self.read_keys.push(key);
        let value = self.fields?.get(key)?;
        T::deserialize(value)
            .map_err(|error| self.report(key, error.to_string()))
            .ok()
    }

    fn report(&mut self, key: &str, reason: String) {
        self.problems.push(SettingProblem {
            path: format!("{}.{key}", self.name),
            reason,
        });
    }

    fn report_unknown_keys(&mut self) {
        let Some(fields) = self.fields else {
            return;
        };
        for key in fields.keys() {
            if !self.read_keys.contains(&key.as_str()) {
                self.report(key, "unknown setting".to_string());
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticSettings {
    enabled: bool,
    disabled: HashSet<DiagnosticCode>,
    warnings_as_hint: HashSet<DiagnosticCode>,
    warnings_as_info: HashSet<DiagnosticCode>,
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

impl DiagnosticSettings {
    fn read(group: &mut SettingsGroup<'_>) -> Self {
        let defaults = Self::default();
        Self {
            enabled: group.read("enabled").unwrap_or(defaults.enabled),
            disabled: read_diagnostic_codes(group, "disabled"),
            warnings_as_hint: read_diagnostic_codes(group, "warningsAsHint"),
            warnings_as_info: read_diagnostic_codes(group, "warningsAsInfo"),
        }
    }

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

/// The diagnostics named in the list setting `key`, by code or name. Unknown
/// names are reported; the recognized ones still apply.
fn read_diagnostic_codes(
    group: &mut SettingsGroup<'_>,
    key: &'static str,
) -> HashSet<DiagnosticCode> {
    group
        .read::<Vec<String>>(key)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|selector| {
            let code = DiagnosticCode::from_selector(&selector);
            if code.is_none() {
                group.report(key, format!("unknown diagnostic `{selector}`"));
            }
            code
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormattingSettings {
    indent_width: Option<u32>,
    use_tabs: Option<bool>,
    soft_line_width: Option<u32>,
    hard_line_width: Option<u32>,
    max_line_width: LegacyLineWidth,
    control_flow_layout: ControlFlowLayout,
    compact_factor_operators: bool,
    break_after_semicolon: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum LegacyLineWidth {
    #[default]
    Unset,
    Configured(Option<u32>),
}

impl Default for FormattingSettings {
    fn default() -> Self {
        Self {
            indent_width: None,
            use_tabs: None,
            soft_line_width: Some(100),
            hard_line_width: Some(100),
            max_line_width: LegacyLineWidth::Unset,
            control_flow_layout: ControlFlowLayout::MultilineCompactElse,
            compact_factor_operators: false,
            break_after_semicolon: true,
        }
    }
}

impl FormattingSettings {
    fn read(group: &mut SettingsGroup<'_>) -> Self {
        let defaults = Self::default();
        Self {
            indent_width: group.read("indentWidth").unwrap_or(defaults.indent_width),
            use_tabs: group.read("useTabs").unwrap_or(defaults.use_tabs),
            soft_line_width: group
                .read("softLineWidth")
                .unwrap_or(defaults.soft_line_width),
            hard_line_width: group
                .read("hardLineWidth")
                .unwrap_or(defaults.hard_line_width),
            max_line_width: group
                .read("maxLineWidth")
                .map_or(defaults.max_line_width, LegacyLineWidth::Configured),
            control_flow_layout: group
                .read("controlFlowLayout")
                .unwrap_or(defaults.control_flow_layout),
            compact_factor_operators: group
                .read("compactFactorOperators")
                .unwrap_or(defaults.compact_factor_operators),
            break_after_semicolon: group
                .read("breakAfterSemicolon")
                .unwrap_or(defaults.break_after_semicolon),
        }
    }
}

impl FormattingConfiguration for FormattingSettings {
    fn indent_width(&self) -> Option<u32> {
        self.indent_width
    }

    fn use_tabs(&self) -> Option<bool> {
        self.use_tabs
    }

    fn line_widths(&self) -> (Option<u32>, Option<u32>) {
        if let LegacyLineWidth::Configured(width) = self.max_line_width {
            let width = width.filter(|width| *width > 0);
            return (width, width);
        }

        let hard = self.hard_line_width.filter(|width| *width > 0);
        let soft = self
            .soft_line_width
            .filter(|width| *width > 0)
            .map(|width| hard.map_or(width, |hard| width.min(hard)));
        (soft, hard)
    }

    fn control_flow_layout(&self) -> ControlFlowLayout {
        self.control_flow_layout
    }

    fn compact_factor_operators(&self) -> bool {
        self.compact_factor_operators
    }

    fn break_after_semicolon(&self) -> bool {
        self.break_after_semicolon
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InlayHintSettings {
    expression_types: bool,
    all_known_types: bool,
}

impl InlayHintSettings {
    fn read(group: &mut SettingsGroup<'_>) -> Self {
        let defaults = Self::default();
        Self {
            expression_types: group
                .read("expressionTypes")
                .unwrap_or(defaults.expression_types),
            all_known_types: group
                .read("allKnownTypes")
                .unwrap_or(defaults.all_known_types),
        }
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

        assert!(settings.diagnostics().allows(catalog::UNUSED_BINDING.code));
        assert_eq!(settings.formatting().line_widths(), (Some(100), Some(100)));
        assert_eq!(
            settings.formatting().control_flow_layout(),
            ControlFlowLayout::MultilineCompactElse
        );
        assert!(!settings.formatting().compact_factor_operators());
        assert!(settings.formatting().break_after_semicolon());
        assert_eq!(settings.inlay_hint_options(), InlayHintOptions::default());
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
                    "maxLineWidth": 88,
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

        assert!(!settings.diagnostics().allows(catalog::UNUSED_BINDING.code));
        assert!(!settings
            .diagnostics()
            .allows(catalog::OPTION_KEY_CONVENTION.code));
        assert!(settings.diagnostics().allows(catalog::SYNTAX_ERROR.code));
        assert_eq!(settings.formatting().indent_width(), Some(2));
        assert_eq!(settings.formatting().use_tabs(), Some(false));
        assert_eq!(settings.formatting().line_widths(), (Some(88), Some(88)));
        assert_eq!(
            settings.formatting().control_flow_layout(),
            ControlFlowLayout::Multiline
        );
        assert!(settings.formatting().compact_factor_operators());
        assert!(!settings.formatting().break_after_semicolon());
        assert_eq!(
            settings.inlay_hint_options(),
            InlayHintOptions {
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
        assert!(!settings.diagnostics().allows(catalog::UNUSED_BINDING.code));
        assert!(settings.diagnostics().allows(catalog::SYNTAX_ERROR.code));
        assert_eq!(settings.formatting().indent_width(), None);
        assert_eq!(settings.formatting().use_tabs(), Some(true));
        assert_eq!(settings.inlay_hint_options(), InlayHintOptions::default());

        let problems = parsed
            .problems
            .iter()
            .map(SettingProblem::to_string)
            .collect::<Vec<_>>();
        assert_eq!(problems.len(), 4, "{problems:?}");
        assert!(problems[0].contains("`diagnostics.disabled`: unknown diagnostic `not-a-rule`"));
        assert!(problems[1].contains("`diagnostics.enabeld`: unknown setting"));
        assert!(problems[2].starts_with("`formatting.indentWidth`: invalid type"));
        assert!(problems[3].starts_with("`inlayHints`: expected an object"));
    }

    #[test]
    fn listed_warnings_are_published_as_hints_or_information() {
        let settings = valid_settings(serde_json::json!({
            "diagnostics": {
                "warningsAsHint": ["unused-binding"],
                "warningsAsInfo": ["S01"]
            }
        }));
        let diagnostics = settings.diagnostics();

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

        assert!(DiagnosticCode::registered().all(|code| !settings.diagnostics().allows(code)));
    }

    #[test]
    fn null_or_zero_line_width_disables_wrapping() {
        for max_line_width in [serde_json::Value::Null, serde_json::json!(0)] {
            let settings = valid_settings(serde_json::json!({
                "formatting": {
                    "maxLineWidth": max_line_width
                }
            }));

            assert_eq!(settings.formatting().line_widths(), (None, None));
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
        assert_eq!(settings.formatting().line_widths(), (Some(90), Some(110)));

        let clamped = valid_settings(serde_json::json!({
            "formatting": {
                "softLineWidth": 140,
                "hardLineWidth": 120
            }
        }));
        assert_eq!(clamped.formatting().line_widths(), (Some(120), Some(120)));
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

            assert_eq!(settings.formatting().control_flow_layout(), expected);
        }
    }

    #[test]
    fn generic_store_replaces_complete_snapshots() {
        let store = SettingsStore::<Vec<u8>>::default();

        assert!(store.replace(vec![1, 2, 3]).is_empty());

        assert_eq!(store.snapshot(), vec![1, 2, 3]);
    }
}
