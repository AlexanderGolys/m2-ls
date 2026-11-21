//! Indentation by bracket balance, as Emacs `M2-mode` computes it and as
//! the Core library is written:
//!
//! ```text
//! indent(L) = max(0, indent(P) + W × Δ(P))
//! ```
//!
//! where `P` is the nearest line before `L` that is neither blank nor starts
//! inside a string or block comment, `W` the indent width, and `Δ(P)` the
//! brackets opened minus those closed from the start of `P` to the start of
//! `L`, counted on syntax tokens only. So a closer keeps the body's indent and
//! the line after it drops, a line opening two groups indents the next one
//! twice, and a line ending in an operator does not indent at all:
//!
//! ```m2
//! f = (a,b) -> (
//!      if a then (
//!           b)
//!      else a)
//! ```
//!
//! With `indent.closers = "opener"`, a line starting with closers is moved
//! back one width per closer, to the indent of the line that opened them.

use std::ops::Range;

use super::layout::LayoutSource;
use super::{Bracket, FormatEdit, TextLine, Whitespace};
use crate::node_metadata::SyntaxSnapshot;
use crate::settings::{CloserIndent, FormattingSettings, IndentWhitespace};
use crate::source::SourceNavigation;

/// How one line is indented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineIndent {
    /// A line of spaces and tabs only, cleared.
    Blank,
    /// A line left as it is at this column: it starts inside a string or
    /// comment, or its indentation borders unparsable source.
    Kept(usize),
    /// A line indented to this column.
    Column(usize),
}

/// The indent column of every line of a layout.
#[derive(Debug, Clone)]
pub(super) struct ColumnIndent {
    /// One entry per line.
    lines: Vec<LineIndent>,
    /// How many closing brackets each line starts with.
    leading_closers: Vec<usize>,
}

/// How many brackets each line of a layout opens minus how many it closes,
/// counted on syntax tokens only, so brackets in strings and comments never
/// count: `g(y -> (` opens 2.
#[derive(Debug, Clone)]
pub(super) struct BracketBalance {
    /// The balance of each line.
    rows: Vec<isize>,
    /// Every closer's bytes, in order.
    closers: Vec<Range<usize>>,
}

impl BracketBalance {
    /// The balance of every line of `layout`, split as `lines`.
    pub(super) fn new(syntax: &SyntaxSnapshot, layout: &LayoutSource, lines: &[TextLine]) -> Self {
        let mut rows = vec![0isize; lines.len()];
        let mut closers = Vec::new();
        for bracket in Bracket::all(syntax, layout) {
            rows[TextLine::row_of(lines, bracket.open.start)] += 1;
            if let Some(close) = bracket.close {
                rows[TextLine::row_of(lines, close.start)] -= 1;
                closers.push(close);
            }
        }
        closers.sort_by_key(|close| close.start);
        Self { rows, closers }
    }

    /// The lines the model indents from, each with `Δ` of the module
    /// documentation: the balance of the lines from the previous such line
    /// up to it. A line is skipped when blank or when it starts inside a
    /// string or comment.
    pub(super) fn code_rows<'a>(
        &'a self,
        layout: &'a LayoutSource,
        lines: &'a [TextLine],
    ) -> impl Iterator<Item = (usize, isize)> + 'a {
        let text = layout.text();
        let mut pending = 0;
        lines.iter().enumerate().filter_map(move |(row, line)| {
            let skipped = line.is_blank(text) || Self::starts_inside(layout, *line);
            let delta = pending;
            pending = if skipped {
                pending + self.rows[row]
            } else {
                self.rows[row]
            };
            (!skipped).then_some((row, delta))
        })
    }

    /// Whether `line` starts inside a string or block comment.
    fn starts_inside(layout: &LayoutSource, line: TextLine) -> bool {
        line.start_byte > 0 && layout.is_protected_at(line.start_byte - 1)
    }
}

impl ColumnIndent {
    /// The indentation of `layout` with `width` columns per bracket.
    pub(super) fn compute(
        syntax: &SyntaxSnapshot,
        layout: &LayoutSource,
        width: usize,
        closers: CloserIndent,
        tab_width: usize,
    ) -> Self {
        let text = layout.text();
        let lines = TextLine::all(text);
        let balance = BracketBalance::new(syntax, layout, &lines);
        let leading_closers = lines
            .iter()
            .map(|line| Self::leading_closers(text, line.content_start(text), &balance.closers))
            .collect::<Vec<_>>();
        let written = |line: TextLine| Whitespace::width(line.indentation(text), tab_width);
        let mut indents = lines
            .iter()
            .map(|line| {
                if line.is_blank(text) {
                    LineIndent::Blank
                } else {
                    LineIndent::Kept(written(*line))
                }
            })
            .collect::<Vec<_>>();
        let width = width as isize;
        let mut body = 0isize;
        for (row, delta) in balance.code_rows(layout, &lines) {
            let line = lines[row];
            if layout.is_protected_within(line.start_byte..line.content_start(text)) {
                body = written(line) as isize;
                continue;
            }
            body = (body + width * delta).max(0);
            let dedent = match closers {
                CloserIndent::Body => 0,
                CloserIndent::Opener => width * leading_closers[row] as isize,
            };
            indents[row] = LineIndent::Column((body - dedent).max(0) as usize);
        }
        Self {
            lines: indents,
            leading_closers,
        }
    }

    /// The edits indenting every line of `layout` as `settings` say, and
    /// removing trailing whitespace when configured.
    pub(super) fn edits(
        syntax: &SyntaxSnapshot,
        layout: &LayoutSource,
        settings: &FormattingSettings,
    ) -> Vec<FormatEdit> {
        let configured = &settings.indent;
        let tab_width = configured.tab_width.get();
        let columns = Self::compute(
            syntax,
            layout,
            configured.width.get(),
            configured.closers,
            tab_width,
        );
        let text = layout.text();
        let mut edits = Vec::new();
        for (line, indent) in TextLine::all(text).into_iter().zip(&columns.lines) {
            let leading = line.start_byte..line.content_start(text);
            match *indent {
                LineIndent::Blank => {
                    edits.push(FormatEdit::new(line.start_byte..line.end_byte, ""))
                }
                LineIndent::Column(column) => edits.push(FormatEdit::new(
                    leading,
                    Self::written(column, configured.whitespace, tab_width),
                )),
                LineIndent::Kept(_) => {}
            }
            let trailing = line.content_end(text).max(line.content_start(text))..line.end_byte;
            if settings.lines.trim_trailing_whitespace
                && *indent != LineIndent::Blank
                && !trailing.is_empty()
            {
                edits.push(FormatEdit::new(trailing, ""));
            }
        }
        edits
    }

    /// The column the model indents line `row` to; absent for a blank line
    /// and for one left as written.
    pub(super) fn column(&self, row: usize) -> Option<usize> {
        match self.lines[row] {
            LineIndent::Column(column) => Some(column),
            LineIndent::Blank | LineIndent::Kept(_) => None,
        }
    }

    /// Whether line `row` starts with a closing bracket.
    pub(super) fn starts_with_closer(&self, row: usize) -> bool {
        self.leading_closers
            .get(row)
            .is_some_and(|count| *count > 0)
    }

    /// How many closers, separated by spaces at most, start at `start`.
    fn leading_closers(text: &str, start: usize, closers: &[Range<usize>]) -> usize {
        let mut cursor = start;
        let mut count = 0;
        while let Ok(index) = closers.binary_search_by_key(&cursor, |close| close.start) {
            count += 1;
            cursor = closers[index].end;
            cursor += text[cursor..].len() - text[cursor..].trim_start_matches(' ').len();
        }
        count
    }

    /// An indent of `column` columns written with `whitespace`.
    fn written(column: usize, whitespace: IndentWhitespace, tab_width: usize) -> String {
        let (tabs, spaces) = match whitespace {
            IndentWhitespace::Tabs => (column / tab_width, column % tab_width),
            IndentWhitespace::Spaces => (0, column),
        };
        "\t".repeat(tabs) + &" ".repeat(spaces)
    }
}
