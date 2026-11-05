//! The settings a client configures m2-ls with.
//!
//! Each group below is a plain struct whose fields are the settings, under
//! their camelCase JSON names; its `Default` impl holds the defaults. The
//! client sends them under the `m2-ls` section (`macaulay2` is accepted too):
//!
//! ```json
//! { "m2-ls": { "formatting": { "indent": 4 }, "sources": { "packagePaths": ["~/packages"] } } }
//! ```
//!
//! Settings are read leniently: a value that does not deserialize, an unknown
//! key in a group, or an unknown entry in a list is reported as a
//! [`SettingProblem`] and dropped, and everything else still applies. A
//! deprecated key is read as the settings that replaced it, and reported too.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::RwLock;
use std::{fmt, mem};

use semver::VersionReq;
use serde::Deserialize;
use serde_json::{Map, Value};
use serde_path_to_error::{Path, Segment};
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity};

use crate::capabilities::formatting::ControlFlowLayout;
use crate::diagnostics::DiagnosticCode;
use crate::init_file::InitFileChoice;
use crate::object_registry::ObjectName;

/// The current value of a setting that requests read while the client
/// replaces it.
#[derive(Debug, Default)]
pub struct SettingsStore<T> {
    /// The value, replaced whole so a reader never sees a partial update.
    current: RwLock<T>,
}

impl<T> SettingsStore<T> {
    /// A store holding `value`.
    pub fn new(value: T) -> Self {
        Self {
            current: RwLock::new(value),
        }
    }

    /// Replace the value with `value`, returning the one it replaced.
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
    /// A copy of the current value, unaffected by later replacements.
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
    /// Whether builtin metadata and documentation are loaded. Default `true`.
    pub catalog: CatalogSettings,
    /// `diagnostics.*`: which diagnostics are published, and how prominently.
    pub diagnostics: DiagnosticSettings,
    /// `formatting.*`: how the formatter lays out a document.
    pub formatting: FormattingSettings,
    /// `inlayHints.*`: which inferred types are shown inline.
    pub inlay_hints: InlayHintSettings,
    /// `semanticTokens.*`: the vocabulary of semantic highlighting.
    pub semantic_tokens: SemanticTokenSettings,
    /// `sources.*`: where the library sources and the init file are.
    pub sources: SourceSettings,
    /// Installation overrides, independent of source navigation.
    pub macaulay2: Macaulay2Settings,
    /// Files analyzed before each ordinary document, in order.
    pub initialization: InitializationSettings,
}

/// Controls optional builtin metadata and documentation.
/// The server reads this group during initialization; changes require a restart.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogSettings {
    /// Read and automatically download the catalog. Default `true`.
    /// Disabling skips catalog reads, downloads, and background updates.
    pub enabled: bool,
    /// Cargo-style M2Book release bound. Default `0.1.*` excludes breaking releases.
    pub version: VersionReq,
}

impl CatalogSettings {
    /// Catalog release line understood by this server version.
    pub const DEFAULT_VERSION: &str = "0.1.*";
}

impl Default for CatalogSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            version: VersionReq::parse(Self::DEFAULT_VERSION).expect("valid catalog release bound"),
        }
    }
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
    /// Explicit publication categories for selected rules.
    pub emit_as: DiagnosticEmissionSettings,
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
    #[serde(rename = "softWrapWidth", alias = "softLineWidth")]
    pub soft_line_width: Option<u32>,
    /// The width lines are always wrapped at; `0` or `null` disables it.
    pub hard_line_width: Option<u32>,
    /// `compact`, `multiline`, `multilineCompactElse`, or `leadingKeywords`.
    pub control_flow_layout: ControlFlowLayout,
    /// `2*x` instead of `2 * x`.
    pub compact_factor_operators: bool,
    /// Whether the next expression after `;` starts on a new line.
    pub break_after_semicolon: bool,
    /// Keep every line break as written: only spacing within lines and
    /// indentation change, so line widths and `controlFlowLayout` have no
    /// effect.
    pub keep_line_breaks: bool,
    /// `{ b }` when `true`, `{b}` when `false`; as written when unset.
    pub space_in_braces: Option<bool>,
}

/// `inlayHints.*`: which inferred types are shown inline, and where.
///
/// Runtime controls lowered from [`HintConfiguration`]. The grouped configuration
/// separates argument names, type placement, and filters; these normalized fields
/// let every hint source share the same presentation rules.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "HintConfiguration")]
pub struct InlayHintSettings {
    /// After each call whose arguments are bracketed, as `f(x)`, `f()` or
    /// `f(x, y)`, but not `f x`: the type it returns. Default `true`.
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
    /// passed to, as `f(x: 2)`. Default `true`.
    pub arg_names: bool,
    /// Before each argument of a call, the type the parameter it is passed
    /// to is declared for, as `f(Number 2)`, or with argument names `f(Number x:
    /// 2)`. Default `false`.
    pub arg_types: bool,
    /// After each parenthesized value, as `(2)` or `(a + b)`: the type of
    /// the value it evaluates to. Not after collections such as `(2,)`,
    /// `()`, `[2]` or `{2}`, nor after a call's arguments. Default `false`.
    pub after_brackets: bool,
    /// After the value a function returns. Default `false`.
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
    /// in the tooltip. `0` or `null` never cuts. Unlimited by default.
    pub max_label_length: Option<u32>,
    /// Functions whose calls and arguments get no hints, e.g. `["print"]`.
    pub excluded_calls: HashSet<ObjectName>,
    /// Controls argument-name labels independently of argument type labels.
    pub argument_names: ArgumentNameSettings,
    /// Enables all type hints without changing argument-name hints.
    pub types_enabled: bool,
    /// Collection delimiters receiving type hints.
    pub bracket_kinds: HashSet<HintBracket>,
    /// Hides values whose type is apparent from their producer.
    pub hide_trivial: bool,
    /// Keeps only the final type label at a position with adjacent type hints.
    pub hide_sequential: bool,
    /// Hides parameter-name and domain labels on operator operands.
    pub hide_operator_calls: bool,
}

/// `semanticTokens.*`: the vocabulary semantic highlighting is sent in.
///
/// The legend offers standard token types, custom escape sequences, and both
/// standard and custom modifiers. Changes take effect through a semantic-token
/// refresh.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct SemanticTokenSettings {
    /// Only LSP-standard classifications: custom escapes become strings,
    /// delimiters and punctuation become operators, algebraic modifiers become
    /// abstract, and documentation styles become documentation. Remaining custom
    /// modifiers are dropped. Default `false`.
    pub standard_only: bool,
    /// Which values receive `defaultLibrary`: a named policy or a list of
    /// policies and defining packages. Default `["CompiledFunction"]`.
    pub default_library: DefaultLibrarySetting,
}

/// Selects `defaultLibrary` by runtime implementation or defining package.
/// JSON accepts a policy string or an array such as `["Core", "Graphs"]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum DefaultLibrarySetting {
    /// A named policy, written as a string.
    Mode(DefaultLibraryMode),
    /// Policy and package selectors combined by union, written as an array.
    Selectors(Vec<String>),
}

impl Default for DefaultLibrarySetting {
    fn default() -> Self {
        Self::Selectors(vec!["CompiledFunction".to_string()])
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

/// `sources.*`: where the Macaulay2 library sources are, for navigation and
/// for analyzing the files implementing the library.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSettings {
    /// Whether the library sources are discovered at all: navigation enters
    /// them, the discovered Core sources are analyzed as primary code, and
    /// the discovered `init.m2` is the init file. Default `true`.
    pub enabled: bool,
    /// Searched before the discovered installation: each an `M2` executable
    /// or an installation prefix (a build tree's `usr-dist` included),
    /// standing for that installation, or else a directory added to
    /// Macaulay2's `path`. A leading `~` is the home directory.
    #[serde(rename = "packagePaths", alias = "paths")]
    pub paths: Vec<PathBuf>,
    /// Directories whose files implement the library itself, analyzed as if
    /// each declared `-- m2-ls: primaryCode`, in addition to the discovered
    /// Core sources; independent of `enabled`. A leading `~` is the home
    /// directory.
    pub primary_code_paths: Vec<PathBuf>,
    /// The init file every document starts with, unless its own
    /// `-- m2-ls: initFile` comment selects another: `true` for the
    /// discovered `init.m2`, `false` for none, or a path, relative to the
    /// workspace folder, a leading `~` the home directory. Independent of
    /// `enabled` when a path. Default `true`.
    pub init_file: InitFileChoice,
    /// Core source discovery: `auto`, `false`, or an explicit directory.
    pub core_path: CorePathSetting,
}

impl Default for DiagnosticSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            disabled: HashSet::new(),
            warnings_as_hint: HashSet::new(),
            warnings_as_info: HashSet::new(),
            emit_as: DiagnosticEmissionSettings::default(),
        }
    }
}

impl Default for FormattingSettings {
    fn default() -> Self {
        Self {
            indent: Some(4),
            use_tabs: Some(true),
            soft_line_width: Some(78),
            hard_line_width: Some(90),
            control_flow_layout: ControlFlowLayout::default(),
            compact_factor_operators: true,
            break_after_semicolon: false,
            keep_line_breaks: false,
            space_in_braces: None,
        }
    }
}

impl Default for InlayHintSettings {
    fn default() -> Self {
        HintConfiguration::default().into()
    }
}

impl Default for SourceSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            paths: Vec::new(),
            primary_code_paths: Vec::new(),
            init_file: InitFileChoice::Discovered,
            core_path: CorePathSetting::default(),
        }
    }
}

/// Grouped hint configuration accepted from editor settings and configuration files.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct HintConfiguration {
    /// Parameter-name hint controls.
    pub arg_names: ArgumentNameSettings,
    /// Include parameter domain types before arguments.
    pub arg_types: bool,
    /// Annotate every expression, subject to the hide filters.
    pub everywhere: bool,
    /// Enables type annotations and chooses their positions.
    pub types: TypeHintSettings,
    /// Filters applied to displayed hints.
    pub hide: HiddenHintSettings,
    /// Compatibility switch for repeated hints from the previous configuration.
    pub show_repeated: Option<bool>,
    /// Compatibility limit for labels; omitted labels are unlimited.
    pub max_label_length: Option<u32>,
}

/// Type annotations, independently enabled from parameter-name annotations.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct TypeHintSettings {
    /// Enable type annotations. Default `true`.
    pub enabled: bool,
    /// Expression positions after which annotations are displayed.
    pub after: TypeHintPositions,
}

impl Default for TypeHintSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            after: TypeHintPositions::default(),
        }
    }
}

/// Independent positions at which the type of an expression is displayed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct TypeHintPositions {
    /// After the result of a function call.
    pub call: bool,
    /// After the last expression completed on a line.
    pub end_of_line: bool,
    /// After a top-level cell's final value.
    pub end_of_cell: bool,
    /// After the name bound by an assignment.
    pub assignment: bool,
    /// After values enclosed by the selected delimiters.
    pub brackets: HashSet<HintBracket>,
    /// After the value returned by a function.
    #[serde(rename = "return")]
    pub return_value: bool,
}

impl Default for TypeHintPositions {
    fn default() -> Self {
        Self {
            call: true,
            end_of_line: true,
            end_of_cell: true,
            assignment: true,
            brackets: HashSet::from([HintBracket::Parentheses]),
            return_value: false,
        }
    }
}

/// Filters shared by type hint positions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct HiddenHintSettings {
    /// Type names whose hints are omitted.
    pub types: HashSet<ObjectName>,
    /// Function names whose calls and arguments are not annotated.
    pub calls: HashSet<ObjectName>,
    /// Hide the non-exact type marker without hiding the type.
    pub non_exact_mark: bool,
    /// Hide types explicitly stated by literal syntax.
    pub literal: bool,
    /// Hide hints whose type is evident from the surrounding source.
    pub trivial: bool,
    /// Hide preceding type labels when several appear consecutively at one position.
    pub sequential: bool,
    /// Hide result hints before a value-discarding semicolon.
    pub before_semicolon: bool,
    /// Hide operator operand parameter labels, preserving expression type hints.
    pub operator_calls: bool,
}

impl Default for HiddenHintSettings {
    fn default() -> Self {
        Self {
            types: HashSet::from([ObjectName::new("Thing"), ObjectName::new("Nothing")]),
            calls: ["toString", "net", "toList"]
                .into_iter()
                .map(ObjectName::new)
                .collect(),
            non_exact_mark: false,
            literal: true,
            trivial: true,
            sequential: true,
            before_semicolon: false,
            operator_calls: true,
        }
    }
}

impl From<HintConfiguration> for InlayHintSettings {
    fn from(configuration: HintConfiguration) -> Self {
        let after = configuration.types.after;
        let mut hidden_types = configuration.hide.types;
        let show_thing = !hidden_types.remove(&ObjectName::new("Thing"));
        let show_nothing = !hidden_types.remove(&ObjectName::new("Nothing"));
        Self {
            after_call: after.call,
            after_line_end: after.end_of_line,
            after_cell: after.end_of_cell,
            during_assignment: after.assignment,
            arg_names: configuration.arg_names.enabled,
            arg_types: configuration.arg_types,
            after_brackets: false,
            after_return: after.return_value,
            everywhere: configuration.everywhere,
            show_literal: !configuration.hide.literal,
            show_upper_closure: !configuration.hide.non_exact_mark,
            show_thing,
            show_nothing,
            show_repeated: configuration.show_repeated.unwrap_or(false),
            hide_types: hidden_types,
            hide_after_semicolon: configuration.hide.before_semicolon,
            max_label_length: configuration.max_label_length,
            excluded_calls: configuration.hide.calls,
            argument_names: configuration.arg_names,
            types_enabled: configuration.types.enabled,
            bracket_kinds: after.brackets,
            hide_trivial: configuration.hide.trivial,
            hide_sequential: configuration.hide.sequential,
            hide_operator_calls: configuration.hide.operator_calls,
        }
    }
}

/// Rules explicitly assigned to each diagnostic publication category.
/// If a rule is listed more than once, the first category in field order wins.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticEmissionSettings {
    /// Rules emitted as errors, regardless of their original severity.
    pub error: HashSet<DiagnosticCode>,
    /// Rules emitted as warnings.
    pub warning: HashSet<DiagnosticCode>,
    /// Rules emitted as information.
    pub info: HashSet<DiagnosticCode>,
    /// Rules emitted as hints.
    pub hint: HashSet<DiagnosticCode>,
    /// Rules available to quick fixes without publishing diagnostics.
    pub fix_action_only: HashSet<DiagnosticCode>,
}

/// Controls the presentation of argument names without truncating type labels.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ArgumentNameSettings {
    /// Whether parameter names are shown. Default `true`.
    pub enabled: bool,
    /// Show names only for calls with bracketed arguments. Default `true`.
    pub require_bracket: bool,
    /// Omit names for variadic parameters. Default `true`.
    pub ignore_variadic: bool,
    /// Maximum parameter-name length, or no limit with `0` or `null`.
    pub max_length: Option<u32>,
}

impl Default for ArgumentNameSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            require_bracket: true,
            ignore_variadic: true,
            max_length: Some(15),
        }
    }
}

impl ArgumentNameSettings {
    /// Ellipsis used when a parameter name exceeds its display limit.
    pub const TRUNCATION_MARK: char = '…';

    /// Truncate this individual name by Unicode scalar count, excluding its colon.
    /// A zero or absent limit preserves the complete name. The ellipsis counts
    /// toward the limit, so a one-character limit displays only the ellipsis.
    pub fn display_name(&self, name: &str) -> String {
        match self
            .max_length
            .filter(|limit| *limit > 0)
            .map(|limit| limit as usize)
        {
            Some(limit) if name.chars().count() > limit => name
                .chars()
                .take(limit - 1)
                .chain(std::iter::once(Self::TRUNCATION_MARK))
                .collect(),
            _ => name.to_string(),
        }
    }
}

/// Delimiters after which an expression's type may be displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[non_exhaustive]
pub enum HintBracket {
    /// Parenthesized sequences such as `(1, 2)`.
    #[serde(rename = "parentheses")]
    Parentheses,
    /// Array literals written with square brackets.
    #[serde(rename = "array")]
    Array,
    /// List literals written with braces.
    #[serde(rename = "list")]
    List,
    /// Expressions delimited by angle bars.
    #[serde(rename = "angle bar")]
    AngleBar,
    /// A single held value, such as `(1)` or `((1, 2))`.
    #[serde(rename = "holder")]
    Holder,
}

/// Explicit Macaulay2 installation paths; omitted paths are discovered.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Macaulay2Settings {
    /// The Macaulay2 executable, overriding discovery through PATH.
    pub executable: Option<PathBuf>,
    /// Installation prefix, overriding the prefix inferred from the executable.
    pub prefix: Option<PathBuf>,
    /// User application directory containing packages and init.m2.
    pub application_directory: Option<PathBuf>,
    /// Whether discovering an installation may invoke its executable.
    pub runtime_discovery: bool,
}

impl Default for Macaulay2Settings {
    fn default() -> Self {
        Self {
            executable: None,
            prefix: None,
            application_directory: None,
            runtime_discovery: true,
        }
    }
}

/// Files supplying the initial globals and packages of document sessions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct InitializationSettings {
    /// Initialization files in execution order; an empty list disables initialization.
    pub files: Vec<PathBuf>,
}

impl Default for InitializationSettings {
    fn default() -> Self {
        Self {
            files: vec![PathBuf::from("${applicationDirectory}/init.m2")],
        }
    }
}

/// Whether Core sources are discovered, disabled, or selected explicitly.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum CorePathSetting {
    /// `auto` selects discovered Core sources; other strings are directory paths.
    Path(PathBuf),
    /// `false` disables Core sources; `true` enables automatic discovery.
    Enabled(bool),
}

impl Default for CorePathSetting {
    fn default() -> Self {
        Self::Path(PathBuf::from("auto"))
    }
}

/// Settings read from a client, plus every value that could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSettings {
    /// The settings, with defaults in place of the values that could not be
    /// used.
    pub settings: ServerSettings,
    /// Every value not read as given, in the order found.
    pub problems: Vec<SettingProblem>,
}

/// One setting the server could not read as given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingProblem {
    /// Where the value sits in the section, e.g. `diagnostics.disabled[1]`.
    path: String,
    /// What is wrong with the value.
    issue: SettingIssue,
}

/// What is wrong with a [`SettingProblem`]'s value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
enum SettingIssue {
    /// It does not deserialize, for this reason; the default applies.
    Invalid(String),
    /// Its key is deprecated; it is read as the settings described.
    Deprecated(&'static str),
}

impl SettingProblem {
    /// Whether the value was read, under a deprecated key, rather than
    /// dropped.
    pub fn is_deprecated(&self) -> bool {
        matches!(self.issue, SettingIssue::Deprecated(_))
    }
}

impl fmt::Display for SettingProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.issue {
            SettingIssue::Invalid(reason) => {
                write!(formatter, "`{}`: {reason}; default kept", self.path)
            }
            SettingIssue::Deprecated(replacement) => {
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
    /// Read the settings in `value`, either the section itself or an object
    /// holding it. Each value that fails to deserialize is reported, removed,
    /// and the rest read again, until what remains reads cleanly.
    pub fn from_value(value: &Value) -> ParsedSettings {
        let legacy_section = value.get("macaulay2").filter(|section| {
            [
                "diagnostics",
                "formatting",
                "inlayHints",
                "semanticTokens",
                "sources",
            ]
            .iter()
            .any(|group| section.get(group).is_some())
        });
        let mut section = value
            .get("m2-ls")
            .or(legacy_section)
            .unwrap_or(value)
            .clone();
        if section.is_null() {
            section = Value::Object(Default::default());
        }
        let mut problems = InlayHintSettings::upgrade_deprecated_keys(&mut section);
        Self::upgrade_hint_layout(&mut section);
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

    /// Translate the previous flat hint keys; explicitly supplied grouped values win.
    fn upgrade_hint_layout(section: &mut Value) {
        let Some(hints) = section.get_mut("inlayHints").and_then(Value::as_object_mut) else {
            return;
        };
        if let Some(enabled) = hints
            .get("argNames")
            .filter(|value| value.is_boolean())
            .cloned()
        {
            hints.insert("argNames".into(), serde_json::json!({
                "enabled": enabled, "requireBracket": false, "ignoreVariadic": false, "maxLength": null
            }));
        }
        for (old, group, key, inverted) in [
            ("afterCall", "after", "call", false),
            ("afterLineEnd", "after", "endOfLine", false),
            ("afterCell", "after", "endOfCell", false),
            ("duringAssignment", "after", "assignment", false),
            ("afterReturn", "after", "return", false),
            ("showLiteral", "hide", "literal", true),
            ("showUpperClosure", "hide", "nonExactMark", true),
            ("hideAfterSemicolon", "hide", "beforeSemicolon", false),
            ("excludedCalls", "hide", "calls", false),
        ] {
            let Some(mut value) = hints.remove(old) else {
                continue;
            };
            if inverted {
                if let Some(flag) = value.as_bool() {
                    value = Value::Bool(!flag);
                }
            }
            let path = if group == "after" {
                vec!["types", "after", key]
            } else {
                vec![group, key]
            };
            Self::insert_hint_value(hints, &path, value);
        }
        if let Some(value) = hints.remove("afterBrackets") {
            let value = match value.as_bool() {
                Some(true) => serde_json::json!(["holder"]),
                Some(false) => serde_json::json!([]),
                None => value,
            };
            Self::insert_hint_value(hints, &["types", "after", "brackets"], value);
        }
        let legacy_types = hints.remove("hideTypes");
        let thing = hints.remove("showThing");
        let nothing = hints.remove("showNothing");
        if legacy_types.is_some() || thing.is_some() || nothing.is_some() {
            let mut types = legacy_types.unwrap_or_else(|| serde_json::json!([]));
            if let Some(types) = types.as_array_mut() {
                for (name, show) in [("Thing", thing), ("Nothing", nothing)] {
                    if show.as_ref().and_then(Value::as_bool) != Some(true) {
                        types.push(Value::String(name.into()));
                    }
                }
            }
            Self::insert_hint_value(hints, &["hide", "types"], types);
        }
    }

    /// Insert a compatibility setting only if its grouped replacement is absent.
    fn insert_hint_value(fields: &mut Map<String, Value>, path: &[&str], value: Value) {
        let Some((key, parents)) = path.split_last() else {
            return;
        };
        let mut fields = fields;
        for parent in parents {
            let group = fields
                .entry((*parent).to_string())
                .or_insert_with(|| serde_json::json!({}));
            let Some(group) = group.as_object_mut() else {
                return;
            };
            fields = group;
        }
        fields.entry((*key).to_string()).or_insert(value);
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

impl InlayHintSettings {
    /// The group's key in the settings section.
    const SECTION_KEY: &str = "inlayHints";

    /// Rewrite the keys of the `inlayHints` group in `section` that 2.0.0
    /// read into the settings replacing them, reporting each as deprecated.
    /// A replacing setting the client sets itself keeps its value.
    fn upgrade_deprecated_keys(section: &mut Value) -> Vec<SettingProblem> {
        let Some(group) = section
            .get_mut(Self::SECTION_KEY)
            .and_then(Value::as_object_mut)
        else {
            return Vec::new();
        };
        let enabled = |value: &Value| *value == Value::Bool(true);
        [
            Self::upgrade_deprecated_key(group, "expressionTypes", "`everywhere`", |value| {
                [("everywhere", Value::Bool(true))]
                    .into_iter()
                    .filter(|_| enabled(value))
                    .collect()
            }),
            Self::upgrade_deprecated_key(
                group,
                "allKnownTypes",
                "`everywhere` with `showLiteral` and `showRepeated`",
                |value| {
                    ["everywhere", "showLiteral", "showRepeated"]
                        .into_iter()
                        .filter(|_| enabled(value))
                        .map(|key| (key, Value::Bool(true)))
                        .collect()
                },
            ),
            Self::upgrade_deprecated_key(
                group,
                "calls",
                "`afterLineEnd` or `afterCall`",
                |value| match value.as_str() {
                    Some("off") => vec![("afterLineEnd", Value::Bool(false))],
                    Some("lineEnds") => vec![("afterLineEnd", Value::Bool(true))],
                    Some("every") => vec![("afterCall", Value::Bool(true))],
                    _ => Vec::new(),
                },
            ),
            Self::upgrade_deprecated_key(group, "unknownTypes", "`showThing`", |value| {
                value
                    .is_boolean()
                    .then(|| ("showThing", value.clone()))
                    .into_iter()
                    .collect()
            }),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// Remove the deprecated `key` from `group`, setting what `replacing`
    /// translates its value into where the client has not, and report it
    /// as read as `replacement`.
    fn upgrade_deprecated_key(
        group: &mut Map<String, Value>,
        key: &str,
        replacement: &'static str,
        replacing: impl FnOnce(&Value) -> Vec<(&'static str, Value)>,
    ) -> Option<SettingProblem> {
        let value = group.remove(key)?;
        for (replacing_key, replacing_value) in replacing(&value) {
            group.entry(replacing_key).or_insert(replacing_value);
        }
        Some(SettingProblem {
            path: format!("{}.{key}", Self::SECTION_KEY),
            issue: SettingIssue::Deprecated(replacement),
        })
    }
}

impl DiagnosticSettings {
    /// Whether findings filed under `code` are shown: diagnostics are enabled
    /// and the user has not disabled this one.
    pub fn allows(&self, code: DiagnosticCode) -> bool {
        self.enabled && !self.disabled.contains(&code)
    }

    /// Whether the rule is published, rather than available only as a quick fix.
    pub fn publishes(&self, code: DiagnosticCode) -> bool {
        self.allows(code) && !self.emit_as.fix_action_only.contains(&code)
    }

    /// The severity a finding filed under `code` is published with: a warning
    /// the user listed in `warningsAsHint` or `warningsAsInfo` is lowered.
    pub fn published_severity(
        &self,
        code: DiagnosticCode,
        severity: DiagnosticSeverity,
    ) -> DiagnosticSeverity {
        if self.emit_as.error.contains(&code) {
            DiagnosticSeverity::ERROR
        } else if self.emit_as.warning.contains(&code) {
            DiagnosticSeverity::WARNING
        } else if self.emit_as.info.contains(&code) {
            DiagnosticSeverity::INFORMATION
        } else if self.emit_as.hint.contains(&code) {
            DiagnosticSeverity::HINT
        } else if severity != DiagnosticSeverity::WARNING {
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
            DefaultLibrarySetting::default()
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
                argument_names: ArgumentNameSettings {
                    enabled: true,
                    require_bracket: false,
                    ignore_variadic: false,
                    max_length: None
                },
                ..InlayHintSettings::default()
            }
        );
        assert!(settings.semantic_tokens.standard_only);
    }

    #[test]
    fn deprecated_inlay_hint_keys_are_read_as_their_replacements() {
        let parsed = ServerSettings::from_value(&serde_json::json!({
            "inlayHints": {
                "allKnownTypes": true,
                "calls": "off",
                "unknownTypes": true,
                "showRepeated": false,
                "show": ["calls"]
            }
        }));

        assert_eq!(
            parsed.settings.inlay_hints,
            InlayHintSettings {
                everywhere: true,
                show_literal: true,
                after_line_end: false,
                show_thing: true,
                show_repeated: false,
                ..InlayHintSettings::default()
            }
        );
        let problems = parsed
            .problems
            .iter()
            .map(SettingProblem::to_string)
            .collect::<Vec<_>>();
        assert_eq!(problems.len(), 4, "{problems:?}");
        assert_eq!(
            problems
                .iter()
                .filter(|problem| problem.contains("deprecated, read as"))
                .count(),
            3,
            "{problems:?}"
        );
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("unknown field")),
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
                DefaultLibrarySetting::Selectors(
                    packages.into_iter().map(str::to_string).collect()
                )
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
        assert_eq!(settings.formatting.indent, Some(4));
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
    fn the_init_file_is_discovered_disabled_or_a_path() {
        assert_eq!(
            ServerSettings::default().sources.init_file,
            InitFileChoice::Discovered
        );
        let settings = valid_settings(serde_json::json!({"sources": {"initFile": false}}));
        assert_eq!(settings.sources.init_file, InitFileChoice::Disabled);
        let settings =
            valid_settings(serde_json::json!({"sources": {"initFile": "~/project/init.m2"}}));
        assert_eq!(
            settings.sources.init_file,
            InitFileChoice::Path(PathBuf::from("~/project/init.m2"))
        );
        let parsed = ServerSettings::from_value(&serde_json::json!({
            "sources": {"initFile": "", "primaryCodePaths": ["~/M2"]}
        }));
        assert_eq!(
            parsed.settings.sources.init_file,
            InitFileChoice::Discovered
        );
        assert_eq!(
            parsed.settings.sources.primary_code_paths,
            [PathBuf::from("~/M2")]
        );
        assert_eq!(parsed.problems.len(), 1, "{:?}", parsed.problems);
    }

    #[test]
    fn generic_store_replaces_complete_snapshots() {
        let store = SettingsStore::<Vec<u8>>::default();

        assert!(store.replace(vec![1, 2, 3]).is_empty());

        assert_eq!(store.snapshot(), vec![1, 2, 3]);
    }
    /// The complete documented TOML example is the server's actual default configuration.
    #[test]
    fn grouped_example_matches_every_default() {
        let value = toml::from_str(include_str!("../tests/fixtures/configuration.toml")).unwrap();
        let parsed = ServerSettings::from_value(&value);
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        assert_eq!(parsed.settings, ServerSettings::default());
    }

    /// Invalid nested settings retain valid siblings and defaults for rejected values.
    #[test]
    fn nested_validation_preserves_valid_siblings() {
        let parsed = ServerSettings::from_value(&serde_json::json!({
            "m2-ls": {
                "inlayHints": {
                    "argNames": {"enabled": false, "maxLength": "short"},
                    "types": {"enabled": false, "after": {"endOfLine": false}},
                    "hide": {"literal": false, "types": ["String", 123]}
                },
                "macaulay2": {"runtimeDiscovery": false},
                "initialization": {"files": []}
            }
        }));
        assert!(!parsed.settings.inlay_hints.arg_names);
        assert!(!parsed.settings.inlay_hints.types_enabled);
        assert!(!parsed.settings.inlay_hints.after_line_end);
        assert!(parsed.settings.inlay_hints.show_literal);
        assert!(!parsed.settings.macaulay2.runtime_discovery);
        assert!(parsed.settings.initialization.files.is_empty());
        assert_eq!(parsed.problems.len(), 2, "{:?}", parsed.problems);
        let store = SettingsStore::new(parsed.settings);
        store.replace(ServerSettings::from_value(&serde_json::json!({})).settings);
        assert_eq!(store.snapshot(), ServerSettings::default());
    }
}
