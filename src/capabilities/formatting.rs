//! Syntax-guided whitespace formatting and folding for Macaulay2 source.
//!
//! The formatter reproduces the layout of the Macaulay2 Core library. It reads
//! one immutable parse and edits a coordinate-mapped [`LayoutSource`], so no
//! pass reparses the source; only the finished text is parsed once more, to
//! prove it means what the input meant ([`ChangedSyntax`]). Strings, comment
//! text and unparsable regions are never edited, and every edit is whitespace,
//! except that a rule-only comment line may change its count of dashes.
//!
//! The passes, in order:
//!
//! 1. indentation by the column model of [`indent`], first, so that the
//!    next pass sees the columns tokens end up in;
//! 2. horizontal spacing within each line ([`spacing`]), driven by the style
//!    [`detect`] resolves for the document;
//! 3. under `lines.breaks = "normalize"`, the line structure of multi-line
//!    constructs ([`lines`]);
//! 4. blank lines and the end of the file;
//! 5. unless `lines.breaks = "keep"`, indentation again and one break in
//!    each line longer than the width.
//!
//! The passes repeat until a round changes nothing, so formatting the result
//! again changes nothing either: each pass decides on the layout the others
//! left, as padding kept for alignment does on its neighbour's columns.
//!
//! Folding ([`folding`]) reuses the indentation model on the unformatted text.

use std::fmt;
use std::ops::Range;

use m2_syn::{Delimiter, DelimiterKind};
use tower_lsp::lsp_types::{DocumentFormattingOptions, OneOf, TextEdit};

use crate::document::DocumentSnapshot;
use crate::node_metadata::SyntaxSnapshot;
use crate::settings::{FormattingSettings, LineBreaks, LogLevel};
use crate::source::{SourceNavigation, SyntaxSource};
use detect::FormatStyle;
use layout::LayoutSource;

mod detect;
mod folding;
mod indent;
mod layout;
mod lines;
mod spacing;

pub use folding::{folding_range_provider_capability, folding_ranges};

/// What becomes of parentheses the syntax does not need, as in `(a*b) + c`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum RedundantParentheses {
    /// Left as written, and no action offers to remove them.
    Off,
    /// Removed by the "Remove redundant parentheses in file" source action.
    Action,
    /// Removed by the source action and whenever the document is formatted.
    Format,
}

impl RedundantParentheses {
    /// Whether the source action removing them is offered.
    pub fn offers_action(self) -> bool {
        self != Self::Off
    }
}

/// The formatter's output would parse to different syntax than its input,
/// so it is withheld: a layout rule broke the invariant that formatting only
/// moves whitespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangedSyntax;

impl fmt::Display for ChangedSyntax {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("formatting would change what the source parses to; left as it is")
    }
}

impl std::error::Error for ChangedSyntax {}

/// Advertises whole-document formatting support in the initialization response.
pub fn document_formatting_provider_capability() -> Option<OneOf<bool, DocumentFormattingOptions>> {
    Some(OneOf::Left(true))
}

/// The edit replacing the document with its formatted text; none when it is
/// already formatted, or when formatting would change its syntax, which is
/// logged.
pub fn document_formatting_text_edits(
    source: &DocumentSnapshot,
    settings: &FormattingSettings,
) -> Vec<TextEdit> {
    match format_document(source.syntax(), settings) {
        Ok(formatted) if formatted != source.text() => vec![TextEdit {
            range: source.full_range(),
            new_text: formatted,
        }],
        Ok(_) => Vec::new(),
        Err(error) => {
            crate::telemetry::event_at(
                LogLevel::Error,
                serde_json::json!({"operation": "formatting", "error": error.to_string()}),
            );
            Vec::new()
        }
    }
}

/// Format the whole document, after removing redundant parentheses when
/// configured. The result is checked to parse to the same syntax as
/// `syntax`, its redundant parentheses aside.
pub fn format_document(
    syntax: &SyntaxSnapshot,
    settings: &FormattingSettings,
) -> Result<String, ChangedSyntax> {
    let ungrouped;
    let syntax = if settings.redundant_parentheses == RedundantParentheses::Format {
        ungrouped = syntax.without_redundant_parentheses(settings.call_parentheses);
        &ungrouped
    } else {
        syntax
    };
    let formatted = formatted_layout(syntax, settings).into_text();
    syntax
        .same_structure(&formatted)
        .then_some(formatted)
        .ok_or(ChangedSyntax)
}

/// Format a replacement in its surrounding syntax, returning only the changed
/// span so a code action does not reformat unrelated source. Insertions retain
/// their boundary whitespace (for example, a codomain's trailing separator).
pub(super) fn format_action_edit(
    source: &DocumentSnapshot,
    edit: &mut TextEdit,
    settings: &FormattingSettings,
) {
    if edit.range.start == edit.range.end {
        return;
    }
    let Some(bytes) = source.bytes_for_range(edit.range) else {
        return;
    };
    let end = bytes.start + edit.new_text.len();
    let mut text = source.text().to_string();
    text.replace_range(bytes.clone(), &edit.new_text);
    let syntax = SyntaxSnapshot::parse(&text, Some(source.syntax()));
    let layout = formatted_layout(&syntax, settings);
    edit.new_text = layout.text_for_bytes(bytes.start..end).to_string();
}

/// Runs the passes of the module documentation against one syntax snapshot.
fn formatted_layout(syntax: &SyntaxSnapshot, settings: &FormattingSettings) -> LayoutSource {
    /// Rounds of every pass before the result is taken as it is. A round
    /// breaks each long line once, so this bounds how many pieces one
    /// written line is cut into.
    const MAX_LAYOUT_ROUNDS: usize = 64;

    let mut layout = LayoutSource::new(syntax);
    let style = FormatStyle::for_document(settings, syntax, &layout);
    let newline = Whitespace::newline(syntax.source());
    let breaks = settings.lines.breaks;
    for _ in 0..MAX_LAYOUT_ROUNDS {
        let mut changed = layout.apply(indent::ColumnIndent::edits(syntax, &layout, &style));
        changed |= layout.apply(spacing::HorizontalSpacing::edits(syntax, &layout, &style));
        if breaks == LineBreaks::Normalize {
            changed |= lines::LineStructure::apply(syntax, &mut layout, &style, newline);
        }
        changed |= layout.apply(lines::BlankLines::edits(syntax, &layout, settings, newline));
        if breaks != LineBreaks::Keep {
            changed |= layout.apply(indent::ColumnIndent::edits(syntax, &layout, &style));
            changed |= layout.apply(lines::Wrapping::edits(syntax, &layout, &style, newline));
        }
        if !changed {
            break;
        }
    }
    layout
}

/// A whitespace replacement expressed in the current layout's byte offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FormatEdit {
    /// Inclusive UTF-8 byte offset of the replaced interval.
    start_byte: usize,
    /// Exclusive UTF-8 byte offset of the replaced interval.
    end_byte: usize,
    /// Text replacing the interval.
    replacement: String,
}

impl FormatEdit {
    /// Replace `bytes` with `replacement`.
    fn new(bytes: Range<usize>, replacement: impl Into<String>) -> Self {
        Self {
            start_byte: bytes.start,
            end_byte: bytes.end,
            replacement: replacement.into(),
        }
    }
}

/// A source line's byte extent, without its line terminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TextLine {
    /// Offset of the line's first byte.
    start_byte: usize,
    /// Offset just past its last byte, before `\r\n` or `\n`.
    end_byte: usize,
}

impl TextLine {
    /// Every line of `text`, the last one included even when empty.
    fn all(text: &str) -> Vec<Self> {
        let mut start_byte = 0;
        text.split('\n')
            .map(|line| {
                let content = line.strip_suffix('\r').unwrap_or(line);
                let span = Self {
                    start_byte,
                    end_byte: start_byte + content.len(),
                };
                start_byte += line.len() + 1;
                span
            })
            .collect()
    }

    /// The line's text in `text`.
    fn text(self, text: &str) -> &str {
        &text[self.start_byte..self.end_byte]
    }

    /// The spaces and tabs the line starts with.
    fn indentation(self, text: &str) -> &str {
        let line = self.text(text);
        &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
    }

    /// Offset of the line's first byte that is not a space or tab.
    fn content_start(self, text: &str) -> usize {
        self.start_byte + self.indentation(text).len()
    }

    /// Offset just past the line's last byte that is not a space or tab.
    fn content_end(self, text: &str) -> usize {
        self.start_byte + self.text(text).trim_end_matches([' ', '\t']).len()
    }

    /// Whether the line holds nothing but spaces and tabs.
    fn is_blank(self, text: &str) -> bool {
        self.content_start(text) == self.end_byte
    }

    /// The index of the line holding `byte` among `lines`.
    fn row_of(lines: &[Self], byte: usize) -> usize {
        lines.partition_point(|line| line.end_byte < byte)
    }
}

/// Measuring and finding horizontal whitespace.
struct Whitespace;

impl Whitespace {
    /// The line terminator the document already uses, so the formatter keeps
    /// it: a document with any `\r\n` is treated as CRLF.
    fn newline(text: &str) -> &'static str {
        if text.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        }
    }

    /// Characters that run together into one operator token: `- -` closed
    /// up is the comment `--`, `< -` the arrow `<-`.
    const OPERATOR_CHARACTERS: &str = "+-*/^<>=!&|#@\\~?:.%$";

    /// Whether removing the gap between `left` and `right` would make the
    /// text read as different tokens: two words or numbers run together
    /// (`x y`), a number swallows a dot (`0 .?a` closed up is the real `0.`
    /// followed by `?a`), or two operators fuse (`- -`).
    fn fuses(left: &str, right: &str) -> bool {
        let (Some(last), Some(first)) = (left.chars().next_back(), right.chars().next()) else {
            return false;
        };
        let word =
            |character: char| character.is_alphanumeric() || matches!(character, '_' | '\'' | '$');
        let operator = |character: char| Self::OPERATOR_CHARACTERS.contains(character);
        (word(last) && word(first))
            || (last.is_ascii_digit() && first == '.')
            || (operator(last) && operator(first))
    }

    /// Display width of `text` with a tab stop every `tab_width` columns.
    fn width(text: &str, tab_width: usize) -> usize {
        text.chars().fold(0, |column, character| {
            if character == '\t' {
                column + tab_width - column % tab_width
            } else {
                column + 1
            }
        })
    }

    /// Where the spaces and tabs ending just before `byte` start, unless
    /// they start a line.
    fn start_before(text: &str, byte: usize) -> Option<usize> {
        let start = text[..byte].trim_end_matches([' ', '\t']).len();
        (start > 0 && !matches!(text.as_bytes()[start - 1], b'\n' | b'\r')).then_some(start)
    }

    /// Where the spaces and tabs starting at `byte` end, unless they end a
    /// line or the text.
    fn end_after(text: &str, byte: usize) -> Option<usize> {
        let end = text.len() - text[byte..].trim_start_matches([' ', '\t']).len();
        (end < text.len() && !matches!(text.as_bytes()[end], b'\n' | b'\r')).then_some(end)
    }
}

/// A bracket pair at its place in the current layout: `(…)`, `{…}`, `[…]` or
/// `<|…|>`, including call arguments and lambda parameter lists.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Bracket {
    /// Which pair.
    kind: DelimiterKind,
    /// The opener's bytes.
    open: Range<usize>,
    /// The closer's bytes; absent when the source never closes it.
    close: Option<Range<usize>>,
}

impl Bracket {
    /// Every bracket pair of `syntax`, outer before inner, where `layout`
    /// now has them.
    fn all(syntax: &SyntaxSnapshot, layout: &LayoutSource) -> Vec<Self> {
        syntax
            .delimiters()
            .into_iter()
            .filter_map(|delimiter: Delimiter| {
                Some(Self {
                    kind: delimiter.kind,
                    open: layout.syntax_range(&delimiter.opening_span())?,
                    close: delimiter
                        .closing_span()
                        .and_then(|span| layout.syntax_range(&span)),
                })
            })
            .collect()
    }

    /// Whether the pair is a brace list, `{…}`, rather than a parenthesis-like
    /// group.
    fn is_brace(&self) -> bool {
        self.kind == DelimiterKind::Brace
    }
}
