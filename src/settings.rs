use std::collections::HashSet;
use std::sync::RwLock;

use serde::{de::Error as _, Deserialize, Deserializer};
use serde_json::Value;
use tower_lsp::lsp_types::Diagnostic;

use crate::capabilities::formatting::{ControlFlowLayout, FormattingConfiguration};
use crate::capabilities::inlay_hints::InlayHintOptions;
use crate::diagnostic_registry::DiagnosticKind;

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

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerSettings {
    diagnostics: DiagnosticSettings,
    formatting: FormattingSettings,
    inlay_hints: InlayHintSettings,
}

impl ServerSettings {
    pub fn from_value(value: &Value) -> serde_json::Result<Self> {
        let settings = value
            .get("m2-ls")
            .or_else(|| value.get("macaulay2"))
            .unwrap_or(value);
        serde_json::from_value(settings.clone())
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DiagnosticSettings {
    enabled: bool,
    #[serde(deserialize_with = "deserialize_diagnostic_set")]
    disabled: HashSet<DiagnosticKind>,
}

impl Default for DiagnosticSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            disabled: HashSet::new(),
        }
    }
}

impl DiagnosticSettings {
    /// Whether findings of `kind` are shown: diagnostics are enabled and the
    /// user has not disabled this one.
    pub fn allows(&self, kind: DiagnosticKind) -> bool {
        self.enabled && !self.disabled.contains(&kind)
    }

    /// Whether a diagnostic a client sent back is shown. Diagnostics another
    /// server published have no kind here and are always kept.
    pub fn allows_lsp_diagnostic(&self, diagnostic: &Diagnostic) -> bool {
        DiagnosticKind::from_lsp(diagnostic).is_none_or(|kind| self.allows(kind))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
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

impl<'de> Deserialize<'de> for LegacyLineWidth {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<u32>::deserialize(deserializer).map(Self::Configured)
    }
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct InlayHintSettings {
    expression_types: bool,
    all_known_types: bool,
}

fn deserialize_diagnostic_set<'de, D>(deserializer: D) -> Result<HashSet<DiagnosticKind>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<String>::deserialize(deserializer)?
        .into_iter()
        .map(|selector| {
            DiagnosticKind::from_selector(&selector)
                .ok_or_else(|| D::Error::custom(format!("unknown diagnostic `{selector}`")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_enable_diagnostics_and_canonical_factor_spacing() {
        let settings = ServerSettings::from_value(&serde_json::json!({})).unwrap();

        assert!(settings.diagnostics().allows(DiagnosticKind::UnusedBinding));
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
        let settings = ServerSettings::from_value(&serde_json::json!({
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
        }))
        .unwrap();

        assert!(!settings.diagnostics().allows(DiagnosticKind::UnusedBinding));
        assert!(!settings
            .diagnostics()
            .allows(DiagnosticKind::OptionKeyConvention));
        assert!(settings.diagnostics().allows(DiagnosticKind::SyntaxError));
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
    fn rejects_unknown_diagnostic_selectors() {
        let error = ServerSettings::from_value(&serde_json::json!({
            "diagnostics": {
                "disabled": ["not-a-rule"]
            }
        }))
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("unknown diagnostic `not-a-rule`"));
    }

    #[test]
    fn can_disable_every_diagnostic() {
        let settings = ServerSettings::from_value(&serde_json::json!({
            "diagnostics": {
                "enabled": false
            }
        }))
        .unwrap();

        assert!(DiagnosticKind::ALL
            .iter()
            .all(|diagnostic| !settings.diagnostics().allows(*diagnostic)));
    }

    #[test]
    fn null_or_zero_line_width_disables_wrapping() {
        for max_line_width in [serde_json::Value::Null, serde_json::json!(0)] {
            let settings = ServerSettings::from_value(&serde_json::json!({
                "formatting": {
                    "maxLineWidth": max_line_width
                }
            }))
            .unwrap();

            assert_eq!(settings.formatting().line_widths(), (None, None));
        }
    }

    #[test]
    fn parses_soft_and_hard_line_widths_and_clamps_the_soft_target() {
        let settings = ServerSettings::from_value(&serde_json::json!({
            "formatting": {
                "softLineWidth": 90,
                "hardLineWidth": 110
            }
        }))
        .unwrap();
        assert_eq!(settings.formatting().line_widths(), (Some(90), Some(110)));

        let clamped = ServerSettings::from_value(&serde_json::json!({
            "formatting": {
                "softLineWidth": 140,
                "hardLineWidth": 120
            }
        }))
        .unwrap();
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
            let settings = ServerSettings::from_value(&serde_json::json!({
                "formatting": {
                    "controlFlowLayout": name
                }
            }))
            .unwrap();

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
