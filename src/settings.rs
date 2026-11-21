//! The settings a client configures m2-ls with.
//!
//! Each group below is a plain struct whose fields are the settings, under
//! their camelCase JSON names. Their values default to the compiled-in
//! [defaults file](ServerSettings::DEFAULTS_FILE), the bottom layer under the
//! global, project and editor settings. The client sends them under the `m2-ls` section (`macaulay2` is accepted too):
//!
//! ```json
//! { "m2-ls": { "formatting": { "indent": 4 }, "sources": { "packagePaths": ["~/packages"] } } }
//! ```
//!
//! Settings are read leniently: a value that does not deserialize, an unknown
//! key in a group, or an unknown entry in a list is reported as a
//! [`SettingProblem`] and dropped, its default showing through, and
//! everything else still applies. A
//! deprecated key is read as the settings that replaced it, and reported too.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};
use std::{fmt, mem};

use semver::VersionReq;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use serde_path_to_error::{Path, Segment};
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity};

use crate::diagnostics::{catalog, DiagnosticCode};
use crate::init_file::InitFileChoice;
use crate::object_registry::ObjectName;

mod formatting;
mod project;
pub use formatting::{
    CloserIndent, CloserPlacement, ConditionBreaks, ElsePlacement, FormattingSettings,
    FunctionBodyLayout, IndentWhitespace, Keepable, LineBreaks, SingleLineIf, StatementLayout,
    ThenPlacement, TrailingComment,
};
pub use project::ProjectConfiguration;
pub mod spacing;
use spacing::EXCLUSIVE_LISTS;

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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSettings {
    /// Whether builtin metadata and documentation are loaded.
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
    /// Operational log verbosity; independent of source diagnostic severity.
    pub logging: LoggingSettings,
    /// `workspace.*`: how much of the workspace is indexed unasked.
    pub workspace: WorkspaceSettings,
}

/// `workspace.*`: how much of the workspace the server indexes on its own,
/// beyond the open documents and the files they load. For example,
/// `[workspace] indexFileLimit = 100` keeps the walk from each opened file to
/// its 100 nearest sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceSettings {
    /// The most `.m2` files opening a document indexes from its workspace
    /// folder, nearest to the document first. The default, `500`, holds
    /// Core's sources (121 files) or `Macaulay2Doc` (386) whole; `0`
    /// indexes only what the documents load.
    pub index_file_limit: usize,
    /// The most directory entries, files and directories alike, one walk
    /// visits before it stops, however few sources it found: a folder holding
    /// more is a home directory or a filesystem rather than a project.
    pub scan_entry_limit: usize,
}

/// Operational logging configuration, for example `[logging] level = "debug"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoggingSettings {
    /// Maximum verbosity recorded in the operational log.
    pub level: LogLevel,
}

/// Increasing operational verbosity. For example, `error` keeps failed
/// operations, while `debug` also records fast operation timings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum LogLevel {
    /// Disable operational log output.
    Off,
    /// Record failed operations and error events.
    Error,
    /// Record errors and warnings.
    Warn,
    /// Also record ordinary events, slow operations, and summaries.
    Info,
    /// Also record fast operation timings.
    Debug,
    /// Also record the start of each operation.
    Trace,
}

/// Controls optional builtin metadata and documentation.
/// The server reads this group during initialization; changes require a restart.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogSettings {
    /// Read and automatically download the catalog.
    /// Disabling skips catalog reads, downloads, and background updates.
    pub enabled: bool,
    /// Cargo-style M2Book release bound; `0.1.*` excludes breaking releases.
    pub version: VersionReq,
}

/// `diagnostics.*`: which diagnostics are published, and how prominently.
/// Rules are named by code or by name, e.g. `T02` or `unused-binding`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticSettings {
    /// Whether any diagnostic is shown.
    pub enabled: bool,
    /// Rules that are never shown.
    pub disabled: HashSet<DiagnosticCode>,
    /// Rules whose warnings are shown as hints.
    pub warnings_as_hint: HashSet<DiagnosticCode>,
    /// Rules whose warnings are shown as information.
    pub warnings_as_info: HashSet<DiagnosticCode>,
    /// Explicit publication categories for selected rules.
    pub emit_as: DiagnosticEmissionSettings,
    /// Whether rewrites may turn an error into a fallback value, such as
    /// `h.k ?? v` for `if h.k =!= null then h.k else v`, which yields `v`
    /// where the conditional fails because `h` lacks the key `k`.
    pub error_catching_rewrites: bool,
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
    /// `f(x, y)`, but not `f x`: the type it returns.
    pub after_call: bool,
    /// At the end of each line: the type of the last expression complete on
    /// it, as `2` after `2+` and `2+2` after the `2` below it.
    pub after_line_end: bool,
    /// After each top-level expression, as Macaulay2's `oN : T` line.
    ///
    pub after_cell: bool,
    /// After a name being assigned, where its type is first known or
    /// changes.
    pub during_assignment: bool,
    /// Before each argument of a call, the name of the parameter it is
    /// passed to, as `f(x: 2)`.
    pub arg_names: bool,
    /// Before each argument of a call, the type the parameter it is passed
    /// to is declared for, as `f(Number 2)`, or with argument names `f(Number x:
    /// 2)`.
    pub arg_types: bool,
    /// After each parenthesized value, as `(2)` or `(a + b)`: the type of
    /// the value it evaluates to. Not after collections such as `(2,)`,
    /// `()`, `[2]` or `{2}`, nor after a call's arguments.
    pub after_brackets: bool,
    /// After the value a function returns.
    pub after_return: bool,
    /// After every expression.
    pub everywhere: bool,
    /// Also the types a literal states, as in `x = 3`.
    pub show_literal: bool,
    /// Marks a type standing also for its subtypes with `↑`, as `↑Ring`.
    ///
    pub show_upper_closure: bool,
    /// Also the hints that say nothing, of values only known to be a
    /// `Thing`.
    pub show_thing: bool,
    /// Also the hints of values known to be `null`, of type `Nothing`.
    ///
    pub show_nothing: bool,
    /// Also the types a nearby hint already shows: a binding's unchanged
    /// type, and an assignment's beside its binding's.
    pub show_repeated: bool,
    /// Types never shown, e.g. `["ZZ", "String"]`.
    pub hide_types: HashSet<ObjectName>,
    /// Drops the hints of values a `;` ends, whose value is discarded.
    ///
    pub hide_after_semicolon: bool,
    /// The longest label shown in full; a longer one is cut, its whole type
    /// in the tooltip. `0` or `null` never cuts.
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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SemanticTokenSettings {
    /// Only LSP-standard classifications: custom escapes become strings,
    /// delimiters and punctuation become operators, algebraic modifiers become
    /// abstract, and documentation styles become documentation. Remaining custom
    /// modifiers are dropped.
    pub standard_only: bool,
    /// Which values receive `defaultLibrary`: a named policy or a list of
    /// policies and defining packages.
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSettings {
    /// Whether the library sources are discovered at all: navigation enters
    /// them, the discovered Core sources are analyzed as primary code, and
    /// the discovered `init.m2` is the init file.
    pub enabled: bool,
    /// Searched before the discovered installation: each an `M2` executable
    /// or an installation prefix (a build tree's `usr-dist` included),
    /// standing for that installation, or else a directory added to
    /// Macaulay2's `path`. A leading `~` is the home directory.
    #[serde(rename = "packagePaths")]
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
    /// `enabled` when a path. Unset leaves the init file to
    /// `initialization.files`.
    pub init_file: Option<InitFileChoice>,
    /// Core source discovery: `auto`, `false`, or an explicit directory.
    pub core_path: CorePathSetting,
}

/// Grouped hint configuration accepted from editor settings and configuration files.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypeHintSettings {
    /// Enable type annotations.
    pub enabled: bool,
    /// Expression positions after which annotations are displayed.
    pub after: TypeHintPositions,
}

/// Independent positions at which the type of an expression is displayed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

/// Filters shared by type hint positions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArgumentNameSettings {
    /// Whether parameter names are shown.
    pub enabled: bool,
    /// Show names only for calls with bracketed arguments.
    pub require_bracket: bool,
    /// Omit names for variadic parameters.
    pub ignore_variadic: bool,
    /// Maximum parameter-name length, or no limit with `0` or `null`.
    pub max_length: Option<u32>,
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

/// Files supplying the initial globals and packages of document sessions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InitializationSettings {
    /// Initialization files in execution order; an empty list disables initialization.
    pub files: Vec<PathBuf>,
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
    /// Its key no longer means anything, for the reason given; it is ignored.
    Removed(&'static str),
}

impl SettingProblem {
    /// Whether the key is only out of date, read as its replacement or
    /// retired, rather than holding a value that could not be read.
    pub fn is_deprecated(&self) -> bool {
        matches!(
            self.issue,
            SettingIssue::Deprecated(_) | SettingIssue::Removed(_)
        )
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
            SettingIssue::Removed(reason) => {
                write!(formatter, "`{}`: removed, ignored; {reason}", self.path)
            }
        }
    }
}

impl ServerSettings {
    /// Selects the current wrapper or the legacy wrapper, while keeping the
    /// native `macaulay2` installation group in an unwrapped settings object.
    pub fn section(value: &Value) -> &Value {
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
        value.get("m2-ls").or(legacy_section).unwrap_or(value)
    }

    /// The defaults file, compiled in: every setting's value where no
    /// global, project or editor layer sets it.
    pub const DEFAULTS_FILE: &str = include_str!("settings/defaults.toml");

    /// The settings [`Self::DEFAULTS_FILE`] gives on its own.
    pub fn defaults() -> &'static Self {
        static DEFAULTS: LazyLock<ServerSettings> = LazyLock::new(|| {
            let section = ServerSettings::over_defaults(&Value::Object(Map::new()));
            serde_path_to_error::deserialize(&section)
                .unwrap_or_else(|error| panic!("the defaults file: {error}"))
        });
        &DEFAULTS
    }

    /// [`Self::DEFAULTS_FILE`] as a settings section.
    fn defaults_section() -> &'static Value {
        static SECTION: LazyLock<Value> = LazyLock::new(|| {
            toml::from_str(ServerSettings::DEFAULTS_FILE)
                .unwrap_or_else(|error| panic!("the defaults file: {error}"))
        });
        &SECTION
    }

    /// `section` laid over the defaults file, with the formatting preset
    /// `section` names laid in between: tables merge, other values replace.
    /// An item `section`
    /// lists in one of a group of exclusive lists leaves the other lists'
    /// defaults.
    fn over_defaults(section: &Value) -> Value {
        let mut merged = Self::defaults_section().clone();
        if let Some(formatting) = merged.get_mut(FormattingSettings::SECTION_KEY) {
            FormattingSettings::lay_profile(
                formatting,
                section.get(FormattingSettings::SECTION_KEY),
            );
        }
        for group in EXCLUSIVE_LISTS {
            for pointer in group {
                let Some(Value::Array(listed)) = section.pointer(pointer) else {
                    continue;
                };
                for other in group.iter().filter(|other| *other != pointer) {
                    if let Some(Value::Array(defaults)) = merged.pointer_mut(other) {
                        defaults.retain(|item| !listed.contains(item));
                    }
                }
            }
        }
        ProjectConfiguration::merge(&mut merged, section);
        merged
    }

    /// Read the settings in `value`, either the section itself or an object
    /// holding it, over the defaults file. Each value that fails to
    /// deserialize is reported and dropped, so its default shows through.
    pub fn from_value(value: &Value) -> ParsedSettings {
        let mut problems = Vec::new();
        let mut section = match Self::section(value) {
            Value::Object(fields) => Value::Object(fields.clone()),
            Value::Null => Value::Object(Map::new()),
            other => {
                problems.push(SettingProblem {
                    path: "settings".to_string(),
                    issue: SettingIssue::Invalid(format!("expected a table, found {other}")),
                });
                Value::Object(Map::new())
            }
        };
        problems.extend(InlayHintSettings::upgrade_deprecated_keys(&mut section));
        problems.extend(FormattingSettings::upgrade_deprecated_keys(&mut section));
        problems.extend(SourceSettings::upgrade_deprecated_keys(&mut section));
        Self::upgrade_hint_layout(&mut section);
        loop {
            match serde_path_to_error::deserialize::<_, Self>(&Self::over_defaults(&section)) {
                Ok(settings) => return ParsedSettings { settings, problems },
                Err(error) => {
                    assert!(
                        Self::remove_value(&mut section, error.path()),
                        "the defaults file is rejected at `{}`",
                        error.path()
                    );
                    problems.push(SettingProblem {
                        path: error.path().to_string(),
                        issue: SettingIssue::Invalid(error.into_inner().to_string()),
                    });
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

/// A group of settings under one key of the section, e.g. `formatting`,
/// whose deprecated keys are read as the settings replacing them.
trait SettingsGroup {
    /// The group's key in the settings section.
    const SECTION_KEY: &str;

    /// Rewrite the deprecated keys of the group in `section` into the
    /// settings replacing them, reporting each as deprecated. A replacing
    /// setting the client sets itself keeps its value.
    fn upgrade_deprecated_keys(section: &mut Value) -> Vec<SettingProblem> {
        section
            .get_mut(Self::SECTION_KEY)
            .and_then(Value::as_object_mut)
            .map(Self::upgrade_group)
            .unwrap_or_default()
    }

    /// [`Self::upgrade_deprecated_keys`] within the group's own fields.
    fn upgrade_group(group: &mut Map<String, Value>) -> Vec<SettingProblem>;

    /// Remove the deprecated `key` from `group`, setting the table
    /// `replacing` translates its value into wherever the client has not,
    /// nested tables included, and report it as read as `replacement`.
    fn upgrade_deprecated_key(
        group: &mut Map<String, Value>,
        key: &str,
        replacement: &'static str,
        replacing: impl FnOnce(&Value) -> Value,
    ) -> Option<SettingProblem> {
        let value = group.remove(key)?;
        if let Value::Object(replacing) = replacing(&value) {
            Self::insert_missing(group, replacing);
        }
        Some(SettingProblem {
            path: format!("{}.{key}", Self::SECTION_KEY),
            issue: SettingIssue::Deprecated(replacement),
        })
    }

    /// Add to `fields` every value of `added` it lacks, merging tables both
    /// hold; a value `fields` already has wins.
    fn insert_missing(fields: &mut Map<String, Value>, added: Map<String, Value>) {
        for (key, value) in added {
            match (fields.get_mut(&key), value) {
                (Some(Value::Object(present)), Value::Object(added)) => {
                    Self::insert_missing(present, added);
                }
                (Some(_), _) => {}
                (None, value) => {
                    fields.insert(key, value);
                }
            }
        }
    }
}

impl SettingsGroup for SourceSettings {
    const SECTION_KEY: &str = "sources";

    /// `paths` is read as `packagePaths`.
    fn upgrade_group(group: &mut Map<String, Value>) -> Vec<SettingProblem> {
        Self::upgrade_deprecated_key(
            group,
            "paths",
            "`packagePaths`",
            |value| serde_json::json!({ "packagePaths": value }),
        )
        .into_iter()
        .collect()
    }
}

impl SettingsGroup for InlayHintSettings {
    const SECTION_KEY: &str = "inlayHints";

    /// The keys 2.0.0 replaced.
    fn upgrade_group(group: &mut Map<String, Value>) -> Vec<SettingProblem> {
        use serde_json::json;
        let when_enabled = |value: &Value, set: Value| {
            if *value == Value::Bool(true) {
                set
            } else {
                json!({})
            }
        };
        [
            Self::upgrade_deprecated_key(group, "expressionTypes", "`everywhere`", |value| {
                when_enabled(value, json!({ "everywhere": true }))
            }),
            Self::upgrade_deprecated_key(
                group,
                "allKnownTypes",
                "`everywhere` with `showLiteral` and `showRepeated`",
                |value| {
                    when_enabled(
                        value,
                        json!({ "everywhere": true, "showLiteral": true, "showRepeated": true }),
                    )
                },
            ),
            Self::upgrade_deprecated_key(
                group,
                "calls",
                "`afterLineEnd` or `afterCall`",
                |value| match value.as_str() {
                    Some("off") => json!({ "afterLineEnd": false }),
                    Some("lineEnds") => json!({ "afterLineEnd": true }),
                    Some("every") => json!({ "afterCall": true }),
                    _ => json!({}),
                },
            ),
            Self::upgrade_deprecated_key(group, "unknownTypes", "`showThing`", |value| {
                if value.is_boolean() {
                    json!({ "showThing": value })
                } else {
                    json!({})
                }
            }),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

impl DiagnosticSettings {
    /// Whether findings filed under `code` are shown: diagnostics are enabled,
    /// the user has not disabled this one, and it changes no error into a value
    /// unless the user opted into such rewrites.
    pub fn allows(&self, code: DiagnosticCode) -> bool {
        self.enabled
            && !self.disabled.contains(&code)
            && (self.error_catching_rewrites || code != catalog::ERROR_CATCHING_REWRITE.code)
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

#[cfg(test)]
mod tests {
    use super::formatting::FormattingProfile;
    use super::spacing::Spacing;
    use super::*;
    use std::num::NonZeroUsize;

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
        assert_eq!(
            settings.formatting,
            ServerSettings::defaults().formatting.clone()
        );
        assert_eq!(
            settings.inlay_hints,
            ServerSettings::defaults().inlay_hints.clone()
        );
        assert!(!settings.semantic_tokens.standard_only);
        assert_eq!(
            settings.semantic_tokens.default_library,
            ServerSettings::defaults()
                .semantic_tokens
                .default_library
                .clone()
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
                    "indent": {"width": 2, "whitespace": "spaces"},
                    "lines": {"breaks": "wrap", "softWidth": 88, "hardWidth": 88},
                    "layout": {"singleLineIf": "never", "statements": "keep"}
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
        let formatting = &settings.formatting;
        assert_eq!(formatting.indent.width, NonZeroUsize::new(2).unwrap());
        assert_eq!(formatting.indent.whitespace, IndentWhitespace::Spaces);
        assert_eq!(formatting.line_widths(), (Some(88), Some(88)));
        assert_eq!(formatting.layout.single_line_if, SingleLineIf::Never);
        assert_eq!(formatting.layout.statements, StatementLayout::Keep);
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
                ..ServerSettings::defaults().inlay_hints.clone()
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
                ..ServerSettings::defaults().inlay_hints.clone()
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
            ServerSettings::defaults()
                .semantic_tokens
                .default_library
                .clone()
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
                    "indent": {"width": "two", "tabWidth": 4}
                },
                "inlayHints": "on"
            }
        }));

        let settings = &parsed.settings;
        assert!(!settings.diagnostics.allows(catalog::UNUSED_BINDING.code));
        assert!(settings.diagnostics.allows(catalog::SYNTAX_ERROR.code));
        assert_eq!(
            settings.formatting.indent.width,
            ServerSettings::defaults().formatting.indent.width
        );
        assert_eq!(settings.formatting.indent.tab_width.get(), 4);
        assert_eq!(
            settings.inlay_hints,
            ServerSettings::defaults().inlay_hints.clone()
        );

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
            problems[2].starts_with("`formatting.indent.width`: "),
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

        assert_eq!(parsed.settings, ServerSettings::defaults().clone());
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
                    "lines": {"breaks": "wrap", "softWidth": width, "hardWidth": width}
                }
            }));

            assert_eq!(settings.formatting.line_widths(), (None, None));
        }
    }

    #[test]
    fn line_widths_clamp_the_soft_target_and_need_breaks() {
        let widths = |breaks: &str, soft: usize, hard: usize| {
            valid_settings(serde_json::json!({
                "formatting": {
                    "lines": {"breaks": breaks, "softWidth": soft, "hardWidth": hard}
                }
            }))
            .formatting
            .line_widths()
        };
        assert_eq!(widths("wrap", 90, 110), (Some(90), Some(110)));
        assert_eq!(widths("normalize", 140, 120), (Some(120), Some(120)));
        assert_eq!(widths("keep", 90, 110), (None, None));
    }

    /// A profile sets every key it names, and a key set explicitly wins.
    #[test]
    fn profiles_lay_a_preset_under_explicit_keys() {
        let modern = valid_settings(serde_json::json!({
            "formatting": {"profile": "modern", "indent": {"width": 2}}
        }))
        .formatting;
        assert_eq!(modern.profile, FormattingProfile::Modern);
        assert_eq!(modern.indent.width, NonZeroUsize::new(2).unwrap());
        assert_eq!(modern.indent.whitespace, IndentWhitespace::Spaces);
        assert_eq!(modern.lines.breaks, LineBreaks::Wrap);
        assert_eq!(modern.spacing.operators.binary("+"), Some(Spacing::Spaced));
        assert_eq!(modern.spacing.lambda_param_comma(), Spacing::Spaced);
        let core = &ServerSettings::defaults().formatting;
        assert_eq!(core.profile, FormattingProfile::Core);
        assert_eq!(core.spacing.lambda_param_comma(), Spacing::Tight);
    }

    /// Naming an operator in one list takes it out of the other's default.
    #[test]
    fn operator_lists_exclude_each_other() {
        let operators = valid_settings(serde_json::json!({
            "formatting": {"spacing": {"operators": {"tight": ["+", "^"]}}}
        }))
        .formatting
        .spacing
        .operators;
        assert_eq!(operators.binary("+"), Some(Spacing::Tight));
        assert_eq!(operators.binary("-"), Some(Spacing::Spaced));
        assert_eq!(operators.binary("or"), Some(Spacing::Spaced));
        assert_eq!(operators.binary("^**"), None);
    }

    /// Every key of the flat 2.x schema still loads, as its replacement and
    /// with a deprecation notice; `compactFactorOperators` is reported removed.
    #[test]
    fn flat_formatting_keys_upgrade_with_notices() {
        let parsed = ServerSettings::from_value(&serde_json::json!({
            "formatting": {
                "indent": 2,
                "tabWidth": 4,
                "softWrapWidth": 88,
                "breakAfterSemicolon": false,
                "controlFlowLayout": "multilineCompactElse",
                "operators": {"spacesAround": {"binary": ["^"]}, "compact": {"binary": ["+"]}},
                "brackets": {"spacesInside": ["{}"], "compact": ["[]"]},
                "compactFactorOperators": true
            }
        }));
        let problems = parsed
            .problems
            .iter()
            .map(SettingProblem::to_string)
            .collect::<Vec<_>>();
        assert!(
            parsed.problems.iter().all(SettingProblem::is_deprecated),
            "{problems:?}"
        );
        assert!(
            problems
                .iter()
                .any(|problem| problem.starts_with("`formatting.compactFactorOperators`: removed")),
            "{problems:?}"
        );
        let formatting = parsed.settings.formatting;
        assert_eq!(formatting.indent.width, NonZeroUsize::new(2).unwrap());
        assert_eq!(formatting.indent.whitespace, IndentWhitespace::Spaces);
        assert_eq!(formatting.indent.tab_width.get(), 4);
        assert_eq!(formatting.line_widths(), (Some(88), None));
        assert_eq!(formatting.layout.statements, StatementLayout::Keep);
        assert_eq!(formatting.layout.single_line_if, SingleLineIf::Never);
        assert_eq!(formatting.layout.else_placement, ElsePlacement::AfterClose);
        assert_eq!(
            formatting.spacing.operators.binary("^"),
            Some(Spacing::Spaced)
        );
        assert_eq!(
            formatting.spacing.operators.binary("+"),
            Some(Spacing::Tight)
        );
        let brackets = formatting.spacing.brackets_inside;
        assert_eq!(brackets.braces, Keepable::Fixed(Spacing::Spaced));
        assert_eq!(brackets.brackets, Keepable::Fixed(Spacing::Tight));
        assert_eq!(brackets.parentheses, Keepable::Fixed(Spacing::Tight));
    }

    /// `bracketsInside` is one value for every pair or a table by pair.
    #[test]
    fn brackets_inside_is_one_value_or_one_per_pair() {
        let brackets = |value: Value| {
            valid_settings(serde_json::json!({
                "formatting": {"spacing": {"bracketsInside": value}}
            }))
            .formatting
            .spacing
            .brackets_inside
        };
        let all = brackets(serde_json::json!("keep"));
        assert_eq!(
            (all.parentheses, all.angle_bars),
            (Keepable::Keep, Keepable::Keep)
        );
        let lists = brackets(serde_json::json!({"{}": "space"}));
        assert_eq!(lists.braces, Keepable::Fixed(Spacing::Spaced));
        assert_eq!(lists.parentheses, Keepable::Fixed(Spacing::Tight));
    }

    #[test]
    fn unknown_formatting_keys_are_reported() {
        for formatting in [
            serde_json::json!({"lines": {"brakes": "keep"}}),
            serde_json::json!({"profiles": {"mine": {"indent": {"width": 3}}}}),
        ] {
            let parsed =
                ServerSettings::from_value(&serde_json::json!({ "formatting": formatting }));
            let problems = parsed
                .problems
                .iter()
                .map(SettingProblem::to_string)
                .collect::<Vec<_>>();
            assert_eq!(problems.len(), 1, "{problems:?}");
            assert!(problems[0].contains("unknown field"), "{problems:?}");
        }
    }

    #[test]
    fn the_init_file_is_discovered_disabled_or_a_path() {
        assert_eq!(ServerSettings::defaults().sources.init_file, None);
        let settings = valid_settings(serde_json::json!({"sources": {"initFile": false}}));
        assert_eq!(settings.sources.init_file, Some(InitFileChoice::Disabled));
        let settings =
            valid_settings(serde_json::json!({"sources": {"initFile": "~/project/init.m2"}}));
        assert_eq!(
            settings.sources.init_file,
            Some(InitFileChoice::Path(PathBuf::from("~/project/init.m2")))
        );
        let parsed = ServerSettings::from_value(&serde_json::json!({
            "sources": {"initFile": "", "primaryCodePaths": ["~/M2"]}
        }));
        assert_eq!(parsed.settings.sources.init_file, None);
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
    /// The compiled-in defaults file reads with no problem, as a user's file too.
    #[test]
    fn the_defaults_file_reads_cleanly() {
        let value = toml::from_str(ServerSettings::DEFAULTS_FILE).unwrap();
        let parsed = ServerSettings::from_value(&value);
        assert!(parsed.problems.is_empty(), "{:?}", parsed.problems);
        assert_eq!(parsed.settings, ServerSettings::defaults().clone());
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
        assert_eq!(store.snapshot(), ServerSettings::defaults().clone());
    }
}
