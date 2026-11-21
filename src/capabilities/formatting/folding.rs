//! Folding ranges: the blocks the indentation model nests, runs of whole-line
//! comments, multi-line block comments and strings, and SimpleDoc sections.
//!
//! Code folds come from the columns [`ColumnIndent`] assigns under the
//! default settings, not from how the document happens to be indented, so a
//! badly indented file still folds by its brackets:
//!
//! ```m2
//! f = x -> (       -- fold 0..2
//!      y := x;
//!      y + 1)
//! ```

use m2_syn::nodes::{Expr, Literal};
use m2_syn::visit::{self, Visit};
use m2_syn::CommentKind;
use tower_lsp::lsp_types::{FoldingRange, FoldingRangeKind, FoldingRangeProviderCapability};

use super::indent::ColumnIndent;
use super::layout::LayoutSource;
use super::TextLine;
use crate::node_metadata::SyntaxSnapshot;
use crate::package_documentation::PackageDocumentation;
use crate::settings::{CloserIndent, ServerSettings};
use crate::source::SourceNavigation;

/// Advertises source-code and comment folding support in the initialization response.
pub fn folding_range_provider_capability() -> Option<FoldingRangeProviderCapability> {
    Some(FoldingRangeProviderCapability::Simple(true))
}

/// Foldable code, literals, comments, and nested SimpleDoc sections.
/// Documentation folds retain their keyword line and exclude trailing blanks.
pub fn folding_ranges(
    syntax: &SyntaxSnapshot,
    documentation: &PackageDocumentation,
) -> Vec<FoldingRange> {
    let layout = LayoutSource::new(syntax);
    let mut folds = Folds {
        layout: &layout,
        lines: TextLine::all(layout.text()),
        ranges: Vec::new(),
    };
    folds.code(syntax);
    folds.comments(syntax);
    syntax.visit(&mut folds);
    for section in documentation.section_ranges() {
        folds.push(
            section.start.line as usize,
            section.end.line as usize,
            Some(FoldingRangeKind::Region),
        );
    }
    let mut ranges = folds.ranges;
    ranges.sort_by_key(|range| (range.start_line, range.end_line));
    ranges.dedup_by_key(|range| (range.start_line, range.end_line));
    ranges
}

/// The folds of one document, collected by row.
struct Folds<'a> {
    /// The document as written.
    layout: &'a LayoutSource,
    /// Its lines.
    lines: Vec<TextLine>,
    /// Folds collected so far.
    ranges: Vec<FoldingRange>,
}

impl Folds<'_> {
    /// Fold rows `start..=end`, unless that is a single row.
    fn push(&mut self, start: usize, end: usize, kind: Option<FoldingRangeKind>) {
        if start < end {
            self.ranges.push(FoldingRange {
                start_line: start as u32,
                start_character: None,
                end_line: end as u32,
                end_character: None,
                kind,
                collapsed_text: None,
            });
        }
    }

    /// Fold the rows `bytes` spans.
    fn push_bytes(&mut self, bytes: std::ops::Range<usize>, kind: Option<FoldingRangeKind>) {
        let start = TextLine::row_of(&self.lines, bytes.start);
        let end = TextLine::row_of(&self.lines, bytes.end);
        self.push(start, end, kind);
    }

    /// A fold from each line that the next code line is indented past, to
    /// the last line before code comes back out to its column. Under
    /// `indent.closers = "opener"` a closer starting the returning line
    /// belongs to the fold; under `"body"` it already sat on the fold's last
    /// line, at the body's column.
    fn code(&mut self, syntax: &SyntaxSnapshot) {
        let settings = &ServerSettings::defaults().formatting;
        let columns = ColumnIndent::compute(
            syntax,
            self.layout,
            settings.indent.width.get(),
            settings.indent.closers,
            settings.indent.tab_width.get(),
        );
        let mut open: Vec<(usize, usize)> = Vec::new();
        let mut previous: Option<(usize, usize)> = None;
        for row in 0..self.lines.len() {
            let Some(column) = columns.column(row) else {
                continue;
            };
            if let Some((previous_row, previous_column)) = previous {
                if column > previous_column {
                    open.push((previous_row, previous_column));
                }
                let end = if settings.indent.closers == CloserIndent::Opener
                    && columns.starts_with_closer(row)
                {
                    row
                } else {
                    previous_row
                };
                while let Some(&(start, _)) = open.last().filter(|(_, opened)| *opened >= column) {
                    self.push(start, end, None);
                    open.pop();
                }
            }
            previous = Some((row, column));
        }
        if let Some((last, _)) = previous {
            for (start, _) in open {
                self.push(start, last, None);
            }
        }
    }

    /// Runs of rows holding only a line comment, and each multi-line block
    /// comment.
    fn comments(&mut self, syntax: &SyntaxSnapshot) {
        let text = self.layout.text();
        let mut comment_rows = Vec::new();
        for comment in syntax.cells().iter().flat_map(|cell| cell.trivia()) {
            let Some(bytes) = self.layout.syntax_range(comment) else {
                continue;
            };
            let written = &text[bytes.clone()];
            let bytes = bytes.start + written.len() - written.trim_start().len()
                ..bytes.start + written.trim_end().len();
            match comment.kind() {
                CommentKind::Line => {
                    let row = TextLine::row_of(&self.lines, bytes.start);
                    if self.lines[row].content_start(text) == bytes.start {
                        comment_rows.push(row);
                    }
                }
                CommentKind::Block => self.push_bytes(bytes, Some(FoldingRangeKind::Comment)),
            }
        }
        comment_rows.sort_unstable();
        comment_rows.dedup();
        for run in comment_rows.chunk_by(|row, next| row + 1 == *next) {
            self.push(run[0], run[run.len() - 1], Some(FoldingRangeKind::Comment));
        }
    }
}

impl<'ast> Visit<'ast> for Folds<'_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        if let Expr::Literal(Literal::String(_) | Literal::RawString(_)) = expression {
            if let Some(bytes) = self.layout.syntax_range(expression) {
                self.push_bytes(bytes, None);
            }
        }
        visit::visit_expr(self, expression);
    }
}
