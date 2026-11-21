//! `formatting.*`: how the formatter lays out a document.
//!
//! Every key is one layout rule, grouped by what it governs, and its default
//! reproduces the Macaulay2 Core library. A rule the Core library itself is
//! split on accepts `"detect"`: the document being formatted is measured and
//! its own majority followed, with a fallback when it says too little.
//!
//! ```toml
//! [formatting]
//! profile = "modern"          # start from the 4-column, spaces-only preset
//!
//! [formatting.indent]
//! width = "detect"            # but follow each file's own indent step
//!
//! [formatting.spacing.operators]
//! tight = ["^", "_", "*"]     # 2*x, x^2
//! ```
//!
//! `profile` names a preset in the defaults file's `[formatting.profiles]`;
//! keys set explicitly override it. Keys this schema replaced are still read,
//! as the settings replacing them, and reported as deprecated.

use std::num::NonZeroUsize;

use m2_syn::DelimiterKind;
use serde::de::Error;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

use super::spacing::OperatorSpacingSettings;
use super::{SettingIssue, SettingProblem, SettingsGroup};
use crate::capabilities::formatting::RedundantParentheses;
use crate::node_metadata::CallParentheses;

/// `formatting.*`: how the formatter lays out a document.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FormattingSettings {
    /// The preset every other key starts from.
    pub profile: FormattingProfile,
    /// `detect.*`: when a document says enough for `"detect"` to follow it.
    pub detect: DetectionSettings,
    /// `indent.*`: how far each line is indented, and with what.
    pub indent: IndentSettings,
    /// `lines.*`: which line breaks the formatter may move, and blank lines.
    pub lines: LineSettings,
    /// `layout.*`: where multi-line constructs break, when breaks may move.
    pub layout: LayoutSettings,
    /// `spacing.*`: the whitespace between tokens on one line.
    pub spacing: SpacingSettings,
    /// `comments.*`: what may change in comment lines.
    pub comments: CommentSettings,
    /// `off`, `action` (a source action removes them) or `format` (also
    /// removed by formatting): parentheses the syntax does not need.
    pub redundant_parentheses: RedundantParentheses,
    /// `remove` or `keep`: whether redundant parentheses around a call's
    /// argument go too, `f(x)` as `f x`.
    pub call_parentheses: CallParentheses,
}

/// A preset for every formatting key, chosen with `profile`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum FormattingProfile {
    /// The Core library as it is: each file's own indent width and comma
    /// style, Emacs tabification, line breaks kept.
    Core,
    /// The newer Core files and the current Emacs `M2-mode`: four columns,
    /// spaces only, commas and arithmetic spaced, long lines wrapped.
    Modern,
}

/// A setting fixed to a value, or `"detect"`: measured on each document and
/// its majority followed, as `width = "detect"` follows a file indented by 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detectable<T> {
    /// Measured on the document, with a fallback when it says too little.
    Detect,
    /// This value, whatever the document does.
    Fixed(T),
}

/// A setting fixed to a value, or `"keep"`: left as each document writes it,
/// as `bracketsInside = "keep"` leaves both `( x )` and `(x)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keepable<T> {
    /// Left as written.
    Keep,
    /// This value.
    Fixed(T),
}

/// Implements `Deserialize` for a word-or-value setting: the word as its own
/// variant, anything else read as the value with the value's own errors.
macro_rules! word_or_value {
    ($setting:ident :: $variant:ident = $word:literal) => {
        impl<'de, T: Deserialize<'de>> Deserialize<'de> for $setting<T> {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                match Value::deserialize(deserializer)? {
                    Value::String(word) if word == $word => Ok(Self::$variant),
                    value => T::deserialize(value)
                        .map(Self::Fixed)
                        .map_err(|error| D::Error::custom(format!("{error}, or \"{}\"", $word))),
                }
            }
        }
    };
}

word_or_value!(Detectable::Detect = "detect");
word_or_value!(Keepable::Keep = "keep");

/// `spacing.bracketsInside`: the gap just inside each kind of bracket pair,
/// written as one value for every pair or a table by pair, so that lists can
/// be padded while parentheses stay tight:
///
/// ```toml
/// [formatting.spacing]
/// bracketsInside = { "{}" = "space" }   # { 1, 2 }, but f(x) and [a]
/// ```
///
/// An empty pair always closes up, as `{}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BracketsInside {
    /// Parentheses, call arguments, parameter lists and parallel assignments.
    #[serde(rename = "()")]
    pub parentheses: Keepable<Spacing>,
    /// Lists, `{1, 2}`.
    #[serde(rename = "{}")]
    pub braces: Keepable<Spacing>,
    /// Arrays, `[1, 2]`.
    #[serde(rename = "[]")]
    pub brackets: Keepable<Spacing>,
    /// Angle-bar lists, `<|1, 2|>`.
    #[serde(rename = "<||>")]
    pub angle_bars: Keepable<Spacing>,
}

impl BracketsInside {
    /// The spacing inside pairs of `kind`.
    pub fn of(&self, kind: DelimiterKind) -> Keepable<Spacing> {
        match kind {
            DelimiterKind::Parenthesis => self.parentheses,
            DelimiterKind::Brace => self.braces,
            DelimiterKind::Bracket => self.brackets,
            DelimiterKind::AngleBar => self.angle_bars,
        }
    }

    /// Read one value for every pair, `"tight"`, or a table by pair.
    fn one_or_each<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Value::deserialize(deserializer)? {
            table @ Value::Object(_) => Self::deserialize(table).map_err(D::Error::custom),
            one => {
                let spacing = Keepable::deserialize(one).map_err(D::Error::custom)?;
                Ok(Self {
                    parentheses: spacing,
                    braces: spacing,
                    brackets: spacing,
                    angle_bars: spacing,
                })
            }
        }
    }
}

/// `formatting.detect.*`: how much a document must say before `"detect"`
/// follows it, e.g. 3 indent steps of which 60 % are 4 columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DetectionSettings {
    /// The fewest occurrences a detected rule is decided on; fewer use the
    /// rule's fallback.
    pub min_samples: usize,
    /// The share of the occurrences the document's majority must reach.
    pub threshold: Share,
}

/// A share of a whole, between `0` and `1`, as `0.6` for 60 %.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Share(f64);

/// Shares are never NaN, which deserialization rules out.
impl Eq for Share {}

impl Share {
    /// Whether `part` of `whole` reaches this share.
    pub fn reached_by(self, part: usize, whole: usize) -> bool {
        part as f64 >= self.0 * whole as f64
    }
}

impl<'de> Deserialize<'de> for Share {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let share = f64::deserialize(deserializer)?;
        (0.0..=1.0)
            .contains(&share)
            .then_some(Self(share))
            .ok_or_else(|| D::Error::custom(format!("{share}: expected a share between 0 and 1")))
    }
}

/// `formatting.indent.*`: how far each line is indented, and with what.
/// A line is indented by its predecessor's indent plus `width` for every
/// bracket the predecessor leaves open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IndentSettings {
    /// Columns per open bracket, or `"detect"`: the file's dominant step
    /// among 2, 4 and 5.
    pub width: Detectable<NonZeroUsize>,
    /// The width `"detect"` uses when the file shows no majority.
    pub fallback_width: NonZeroUsize,
    /// The characters an indent is written with.
    pub whitespace: IndentWhitespace,
    /// Columns per tab, for `emacsTabs` and for measuring line widths.
    pub tab_width: NonZeroUsize,
    /// Where a line starting with a closing bracket is indented.
    pub closers: CloserIndent,
}

/// The characters an indent is written with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum IndentWhitespace {
    /// One tab per `tabWidth` columns, then spaces, as Emacs writes 13
    /// columns: a tab and 5 spaces.
    EmacsTabs,
    /// Spaces only.
    Spaces,
    /// One tab per indent level.
    Tabs,
}

/// Where a line starting with `)`, `}` or `]` is indented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum CloserIndent {
    /// At the body's indent, as the Core library does; the next line drops.
    Body,
    /// Dedented to the indent of the line its bracket opened on.
    Opener,
}

/// `formatting.lines.*`: which line breaks the formatter may move, how wide
/// lines may grow, and the blank lines and line ends it always tidies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LineSettings {
    /// Which line breaks may move.
    pub breaks: LineBreaks,
    /// The width a long line is broken toward; `0` or `null` for none.
    pub soft_width: Option<usize>,
    /// The width no line is wrapped past; `0` or `null` for none.
    pub hard_width: Option<usize>,
    /// The longest run of blank lines kept; longer runs collapse to it.
    pub max_blank_lines: usize,
    /// Drop blank lines just after a line-ending `(` or `{` and just before
    /// a line-starting `)` or `}`.
    pub trim_blank_lines_in_brackets: bool,
    /// End the document with exactly one line break.
    pub final_newline: bool,
    /// Remove spaces and tabs at line ends.
    pub trim_trailing_whitespace: bool,
}

/// Which line breaks the formatter may move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum LineBreaks {
    /// None: only indentation and spacing change.
    Keep,
    /// Also break lines longer than `softWidth`.
    Wrap,
    /// Also apply every `layout.*` rule to every construct.
    Normalize,
}

/// `formatting.layout.*`: where multi-line constructs break. Read only when
/// `lines.breaks` is not `keep`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LayoutSettings {
    /// The `)` of a multi-line `(` group.
    pub close_paren: CloserPlacement,
    /// The `}` of a multi-line `{` list.
    pub close_brace: CloserPlacement,
    /// When a parenthesized function body starts on the line after `-> (`.
    pub function_body: FunctionBodyLayout,
    /// Whether a `;` ends its line.
    pub statements: StatementLayout,
    /// When an `if` is written on one line.
    pub single_line_if: SingleLineIf,
    /// Where the `else` of a multi-line `if` goes.
    pub else_placement: ElsePlacement,
    /// Where `then` goes when the condition spans lines.
    pub then_placement: ThenPlacement,
    /// Which side of a line break a condition's `or` and `and` go.
    pub condition_breaks: ConditionBreaks,
}

/// Where the closing bracket of a multi-line group goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum CloserPlacement {
    /// On the last element's line: `result)`.
    Hug,
    /// On a line of its own, with any `;`, `)` or `,` after it: `},`.
    OwnLine,
}

/// When a parenthesized function body starts on the line after `-> (`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum FunctionBodyLayout {
    /// Always.
    Always,
    /// When it holds more than one statement, or does not fit on one line.
    BreakWhenMultiStatement,
    /// As written.
    Keep,
}

/// Whether a `;` ends its line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum StatementLayout {
    /// Each statement of a block on its own line.
    OnePerLine,
    /// As written.
    Keep,
}

/// When an `if` is written on one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum SingleLineIf {
    /// When `if C then X else Y` fits within `softWidth`.
    WhenFits,
    /// Never: every branch starts its own line.
    Never,
    /// As written.
    Keep,
}

/// Where the `else` of a multi-line `if` goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ElsePlacement {
    /// Starting a line, at the indent of the `if` line.
    OwnLine,
    /// After the then-branch's closing parenthesis: `) else (`.
    AfterClose,
}

/// Where `then` goes when the condition spans lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ThenPlacement {
    /// Ending the condition's last line.
    WithCondition,
    /// Starting a line of its own.
    OwnLine,
}

/// Which side of a line break a condition's `or` and `and` go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ConditionBreaks {
    /// Starting the continuation lines.
    BeforeOperator,
    /// Ending the broken lines.
    AfterOperator,
}

/// `formatting.spacing.*`: the whitespace between tokens on one line. A gap
/// beside a line break is indentation and never spacing.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpacingSettings {
    /// After a comma; `"detect"` follows the document's commas inside `( )`,
    /// falling back to a space.
    pub comma: Detectable<Spacing>,
    /// After a comma between lambda parameters, `(x,y) ->`; `"detect"`
    /// follows the document's parameter lists, falling back to tight.
    pub lambda_param_comma: Detectable<LambdaParamComma>,
    /// After a `;` followed by more code on the same line.
    pub semicolon: Spacing,
    /// Just inside each kind of bracket pair: `(x)` or `( x )`.
    #[serde(deserialize_with = "BracketsInside::one_or_each")]
    pub brackets_inside: BracketsInside,
    /// Between a function and the parenthesis of its call: `f(x)`.
    pub call_paren: Keepable<Spacing>,
    /// Between a keyword and a following parenthesis: `then (`.
    pub keyword_paren: Keepable<Spacing>,
    /// `+ - * /` tight inside a bracket opened right after `_`, `#`, `^` or
    /// `.`, as `x#(i+1)`.
    pub index_context_tight: bool,
    /// The gap before a comment ending a code line.
    pub trailing_comment: TrailingComment,
    /// Keep padding that lines a token up with the same token on the
    /// neighbouring line.
    pub preserve_alignment: bool,
    /// `operators.*`: the binary operators written spaced, tight, or as
    /// each document does.
    pub operators: OperatorSpacingSettings,
}

/// Whether something is written with a space or without.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[non_exhaustive]
pub enum Spacing {
    /// `a + b`, `f(a, b)`.
    #[serde(rename = "space")]
    Spaced,
    /// `a^b`, `f(a,b)`.
    #[serde(rename = "tight")]
    Tight,
}

impl Spacing {
    /// The whitespace written on each padded side.
    pub fn padding(self) -> &'static str {
        match self {
            Self::Spaced => " ",
            Self::Tight => "",
        }
    }
}

/// The comma between lambda parameters, `(x,y) ->`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum LambdaParamComma {
    /// `(x, y) ->`.
    Space,
    /// `(x,y) ->`.
    Tight,
    /// As `comma` decides.
    SameAsComma,
}

/// The gap before a comment ending a code line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum TrailingComment {
    /// One space, unless the comment lines up with one on a neighbouring
    /// line and `preserveAlignment` is set.
    OneSpace,
    /// As written.
    Keep,
}

/// `formatting.comments.*`: what may change in comment lines. Comment text
/// is never rewritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommentSettings {
    /// The dash count rule-only comment lines of 60 to 90 dashes are
    /// written with; `0` or `null` leaves them.
    pub banner_width: Option<usize>,
}

impl FormattingSettings {
    /// The key presets sit under in the defaults file.
    pub const PROFILES_KEY: &str = "profiles";
    /// The key naming the preset in effect.
    pub const PROFILE_KEY: &str = "profile";

    /// The soft and hard wrapping widths in effect: none while line breaks
    /// are kept, a zero width is none, and the soft width never exceeds the
    /// hard one.
    pub fn line_widths(&self) -> (Option<usize>, Option<usize>) {
        if self.lines.breaks == LineBreaks::Keep {
            return (None, None);
        }
        let positive = |width: Option<usize>| width.filter(|width| *width > 0);
        let hard = positive(self.lines.hard_width);
        let soft =
            positive(self.lines.soft_width).map(|soft| hard.map_or(soft, |hard| soft.min(hard)));
        (soft, hard)
    }

    /// Lay the preset the client's `formatting` table names (else the
    /// defaults') over the defaults-file `formatting` table, taking the
    /// presets out of both; presets the client defines join the defaults'.
    /// An unknown profile adds nothing: deserialization reports it.
    ///
    /// ```toml
    /// [formatting]
    /// profile = "mine"
    /// [formatting.profiles.mine.indent]
    /// width = 3
    /// ```
    pub fn resolve_profile(defaults: &mut Value, client: Option<&mut Value>) {
        let mut presets = Self::take_presets(defaults);
        let client = client.map(|client| {
            super::ProjectConfiguration::merge(&mut presets, &Self::take_presets(client));
            &*client
        });
        let profile = client
            .and_then(|client| client.get(Self::PROFILE_KEY))
            .or_else(|| defaults.get(Self::PROFILE_KEY))
            .and_then(Value::as_str);
        if let Some(preset) = profile.and_then(|name| presets.get(name)).cloned() {
            super::ProjectConfiguration::merge(defaults, &preset);
        }
    }

    /// Remove the presets from a `formatting` table; an empty table when it
    /// has none.
    fn take_presets(formatting: &mut Value) -> Value {
        formatting
            .as_object_mut()
            .and_then(|formatting| formatting.remove(Self::PROFILES_KEY))
            .unwrap_or_else(|| Value::Object(Map::new()))
    }
}

impl SettingsGroup for FormattingSettings {
    const SECTION_KEY: &str = "formatting";

    /// The flat keys of 2.9.0 and earlier, read into the grouped schema:
    ///
    /// | Deprecated | Read as |
    /// |---|---|
    /// | `indent = N` / `"tab"` / `"editor"` | `indent.width = N`, `whitespace = "spaces"` / `whitespace = "tabs"` / the defaults |
    /// | `tabWidth` | `indent.tabWidth` |
    /// | `keepLineBreaks` | `lines.breaks = "keep"`, or `"normalize"` when false |
    /// | `softWrapWidth`, `hardLineWidth` | `lines.softWidth`, `lines.hardWidth`, with `lines.breaks = "wrap"` |
    /// | `breakAfterSemicolon` | `layout.statements = "onePerLine"` or `"keep"` |
    /// | `controlFlowLayout` | `layout.singleLineIf`, `elsePlacement`, `thenPlacement` |
    /// | `operators.spacesAround`, `operators.compact` | `spacing.operators.spaced`, `tight` |
    /// | `brackets.spacesInside`, `brackets.compact` | `spacing.bracketsInside`, pair by pair |
    /// | `compactFactorOperators` | removed: `spacing.indexContextTight` covers its one measured case |
    ///
    /// `useTabs`, `softLineWidth` and `spaceInBraces`, deprecated in 2.7.0,
    /// are read into the 2.9.0 keys first.
    fn upgrade_group(group: &mut Map<String, Value>) -> Vec<SettingProblem> {
        let mut problems = Self::upgrade_2_7_keys(group);
        problems.extend(Self::upgrade_flat_keys(group));
        problems
    }
}

impl FormattingSettings {
    /// The `indent` 2.7.0 assumed when unset, as a tab width or a count of
    /// spaces depending on `useTabs`.
    const LEGACY_INDENT_COLUMNS: u64 = 4;

    /// `softLineWidth` is read as `softWrapWidth`. `spaceInBraces` (2.7.0) is
    /// read as `{}` listed under `brackets.spacesInside` or
    /// `brackets.compact`. `useTabs` (2.7.0) is read into `indent`: its old
    /// companion `indent = N`, `4` when unset, was the width of a tab or the
    /// count of spaces.
    fn upgrade_2_7_keys(group: &mut Map<String, Value>) -> Vec<SettingProblem> {
        let columns = group
            .get("indent")
            .and_then(Value::as_u64)
            .unwrap_or(Self::LEGACY_INDENT_COLUMNS);
        let tabs = group.get("useTabs").and_then(Value::as_bool);
        if tabs == Some(true) {
            group.remove("indent");
        }
        let use_tabs = Self::upgrade_deprecated_key(
            group,
            "useTabs",
            "`indent.whitespace = \"tabs\"` with `indent.tabWidth`, or `indent.width`",
            |_| match tabs {
                Some(true) => serde_json::json!({ "indent": "tab", "tabWidth": columns }),
                Some(false) => serde_json::json!({ "indent": columns }),
                None => serde_json::json!({}),
            },
        );
        let soft_line_width = Self::upgrade_deprecated_key(
            group,
            "softLineWidth",
            "`lines.softWidth`",
            |value| serde_json::json!({ "softWrapWidth": value }),
        );
        let space_in_braces = Self::upgrade_deprecated_key(
            group,
            "spaceInBraces",
            "`spacing.bracketsInside`",
            |value| match value.as_bool() {
                Some(true) => serde_json::json!({ "brackets": { "spacesInside": ["{}"] } }),
                Some(false) => serde_json::json!({ "brackets": { "compact": ["{}"] } }),
                None => serde_json::json!({}),
            },
        );
        [use_tabs, soft_line_width, space_in_braces]
            .into_iter()
            .flatten()
            .collect()
    }

    /// The 2.9.0 flat keys, read into the grouped schema; see
    /// [`SettingsGroup::upgrade_group`].
    fn upgrade_flat_keys(group: &mut Map<String, Value>) -> Vec<SettingProblem> {
        use serde_json::json;
        let flag = |value: &Value, yes: Value, no: Value| match value.as_bool() {
            Some(true) => yes,
            Some(false) => no,
            None => json!({}),
        };
        let indent = match group.get("indent") {
            Some(Value::Object(_)) | None => None,
            Some(_) => Self::upgrade_deprecated_key(
                group,
                "indent",
                "`indent.width` with `indent.whitespace`",
                |value| match value {
                    Value::Number(width) => {
                        json!({ "indent": { "width": width, "whitespace": "spaces" } })
                    }
                    Value::String(word) if word == "tab" => {
                        json!({ "indent": { "whitespace": "tabs" } })
                    }
                    Value::String(word) if word == "editor" => json!({}),
                    unreadable => json!({ "indent": { "width": unreadable } }),
                },
            ),
        };
        let upgrades = [
            indent,
            Self::upgrade_deprecated_key(
                group,
                "tabWidth",
                "`indent.tabWidth`",
                |value| json!({ "indent": { "tabWidth": value } }),
            ),
            Self::upgrade_deprecated_key(group, "keepLineBreaks", "`lines.breaks`", |value| {
                flag(
                    value,
                    json!({ "lines": { "breaks": "keep" } }),
                    json!({ "lines": { "breaks": "normalize" } }),
                )
            }),
            Self::upgrade_deprecated_key(
                group,
                "softWrapWidth",
                "`lines.softWidth`",
                |value| json!({ "lines": { "softWidth": value, "breaks": "wrap" } }),
            ),
            Self::upgrade_deprecated_key(
                group,
                "hardLineWidth",
                "`lines.hardWidth`",
                |value| json!({ "lines": { "hardWidth": value, "breaks": "wrap" } }),
            ),
            Self::upgrade_deprecated_key(
                group,
                "breakAfterSemicolon",
                "`layout.statements`",
                |value| {
                    flag(
                        value,
                        json!({ "layout": { "statements": "onePerLine" } }),
                        json!({ "layout": { "statements": "keep" } }),
                    )
                },
            ),
            Self::upgrade_deprecated_key(
                group,
                "controlFlowLayout",
                "`layout.singleLineIf`, `layout.elsePlacement` and `layout.thenPlacement`",
                |value| {
                    let (single_line, placement, then) = match value.as_str() {
                        Some("compact") => ("keep", "ownLine", "withCondition"),
                        Some("multiline") => ("never", "ownLine", "withCondition"),
                        Some("multilineCompactElse") => ("never", "afterClose", "withCondition"),
                        Some("leadingKeywords") => ("never", "ownLine", "ownLine"),
                        _ => return json!({}),
                    };
                    json!({ "layout": {
                        "singleLineIf": single_line,
                        "elsePlacement": placement,
                        "thenPlacement": then,
                    } })
                },
            ),
            Self::upgrade_deprecated_key(
                group,
                "operators",
                "`spacing.operators.spaced` and `spacing.operators.tight`",
                |value| {
                    let listed = |path: &str| value.pointer(path).cloned().unwrap_or(json!([]));
                    let mut spaced = listed("/spacesAround/binary");
                    if let (Some(spaced), Value::Array(unary)) =
                        (spaced.as_array_mut(), listed("/spacesAround/unary"))
                    {
                        spaced.extend(unary);
                    }
                    json!({ "spacing": { "operators": {
                        "spaced": spaced,
                        "tight": listed("/compact/binary"),
                    } } })
                },
            ),
            Self::upgrade_deprecated_key(group, "brackets", "`spacing.bracketsInside`", |value| {
                let mut pairs = Map::new();
                for (list, spacing) in [("compact", "tight"), ("spacesInside", "space")] {
                    let listed = value
                        .get(list)
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten();
                    for pair in listed.filter_map(Value::as_str) {
                        pairs.insert(pair.to_string(), json!(spacing));
                    }
                }
                json!({ "spacing": { "bracketsInside": pairs } })
            }),
            group
                .remove("compactFactorOperators")
                .map(|_| SettingProblem {
                    path: format!("{}.compactFactorOperators", Self::SECTION_KEY),
                    issue: SettingIssue::Removed(
                        "`spacing.indexContextTight` covers its measured case",
                    ),
                }),
        ];
        upgrades.into_iter().flatten().collect()
    }
}
