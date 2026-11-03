//! Mutable whitespace coordinates over one immutable, already parsed tree.

use m2_syn::nodes::{Expr, ExprLiteral};
use m2_syn::visit::{self, Visit};
use std::ops::Range;

use crate::node_metadata::SyntaxSnapshot;
use crate::source::{DocumentSource, SourceNavigation};

use super::FormatEdit;

/// Mutable whitespace coordinates mapped back to one immutable parse snapshot.
pub(super) struct LayoutSource {
    /// Source data supplying text and coordinate conversions.
    source: DocumentSource,
    /// Original source and syntax provenance, unaffected by whitespace edits.
    original: DocumentSource,
    /// Original byte boundaries mapped to current layout start coordinates.
    starts: Vec<usize>,
    /// Original byte boundaries mapped to current layout end coordinates.
    ends: Vec<usize>,
    /// Original byte intervals of strings, comments, and malformed syntax that edits must not alter.
    protected: Vec<Range<usize>>,
}

impl SourceNavigation for LayoutSource {
    fn source(&self) -> &DocumentSource {
        &self.source
    }
}

impl LayoutSource {
    /// Constructs this value from the supplied dependencies and initial state.
    pub(super) fn new(syntax: &SyntaxSnapshot) -> Self {
        let text = syntax.source();
        let mut source = DocumentSource::new(text.to_string());
        source.register_syntax(syntax);
        let mut protected = ProtectedSyntax {
            source: &source,
            ranges: Vec::new(),
        };
        syntax.visit(&mut protected);
        let mut ranges = protected.ranges;
        for cell in syntax.cells() {
            ranges.extend(cell.trivia().iter().filter_map(|comment| {
                let mut bytes = source.syntax_bytes(comment)?;
                let text = source.text().get(bytes.clone())?;
                bytes.start += text.len() - text.trim_start_matches([' ', '\t']).len();
                bytes.end -= text.len() - text.trim_end_matches(['\r', '\n']).len();
                (bytes.start <= bytes.end).then_some(bytes)
            }));
            if cell.syntax().is_none()
                && (cell.parse_error().is_some() || !cell.lexer_diagnostics().is_empty())
            {
                ranges.push(cell.document_range());
            }
        }
        Self {
            original: source.clone(),
            source,
            starts: (0..=text.len()).collect(),
            ends: (0..=text.len()).collect(),
            protected: ranges,
        }
    }

    /// Maps a typed syntax span into the current whitespace layout.
    pub(super) fn syntax_range(
        &self,
        syntax: &(impl m2_syn::Spanned + ?Sized),
    ) -> Option<Range<usize>> {
        let bytes = self.original.syntax_bytes(syntax)?;
        Some(self.starts[bytes.start]..self.ends[bytes.end].max(self.starts[bytes.start]))
    }

    /// Protected original intervals mapped to the current layout.
    pub(super) fn protected_ranges(&self) -> impl Iterator<Item = Range<usize>> + '_ {
        self.protected.iter().map(|span| {
            let start = self.starts[span.start];
            start..self.ends[span.end].max(start)
        })
    }

    /// Consumes the layout and returns the resulting formatted source.
    pub(super) fn into_text(self) -> String {
        self.text().to_string()
    }

    /// Text at an original syntax interval after earlier whitespace edits.
    pub(super) fn text_for_bytes(&self, bytes: Range<usize>) -> &str {
        let start = self.starts[bytes.start];
        let end = self.ends[bytes.end].max(start);
        &self.text()[start..end]
    }

    /// Apply only whitespace edits, updating coordinates rather than syntax.
    pub(super) fn apply(&mut self, mut edits: Vec<FormatEdit>) -> bool {
        edits.retain(|edit| self.text()[edit.start_byte..edit.end_byte] != edit.replacement);
        if edits.is_empty() {
            return false;
        }
        edits.sort_by_key(|edit| (edit.start_byte, edit.end_byte));
        if edits
            .windows(2)
            .any(|pair| pair[0].end_byte > pair[1].start_byte)
            || edits.iter().any(|edit| {
                !self.text()[edit.start_byte..edit.end_byte]
                    .bytes()
                    .all(|byte| byte.is_ascii_whitespace())
                    || !edit
                        .replacement
                        .bytes()
                        .all(|byte| byte.is_ascii_whitespace())
                    || self.protected.iter().any(|span| {
                        let start = self.starts[span.start];
                        let end = self.ends[span.end];
                        (edit.start_byte < end && start < edit.end_byte)
                            || (edit.start_byte == edit.end_byte
                                && start < edit.start_byte
                                && edit.start_byte < end)
                    })
            })
        {
            return false;
        }

        let mut shifts = vec![0isize];
        for edit in &edits {
            shifts.push(
                shifts.last().unwrap() + edit.replacement.len() as isize
                    - (edit.end_byte - edit.start_byte) as isize,
            );
        }
        let map = |position: usize, is_start: bool| {
            let count = edits.partition_point(|edit| {
                edit.start_byte < position || (is_start && edit.start_byte == position)
            });
            if count == 0 {
                return position;
            }
            let edit = &edits[count - 1];
            if position >= edit.end_byte {
                position.checked_add_signed(shifts[count]).unwrap()
            } else {
                let edge = edit.start_byte + if is_start { edit.replacement.len() } else { 0 };
                edge.checked_add_signed(shifts[count - 1]).unwrap()
            }
        };
        for position in &mut self.starts {
            *position = map(*position, true);
        }
        for position in &mut self.ends {
            *position = map(*position, false);
        }
        let mut text = self.text().to_string();
        for edit in edits.into_iter().rev() {
            text.replace_range(edit.start_byte..edit.end_byte, &edit.replacement);
        }
        self.source = DocumentSource::new(text);
        true
    }
}

/// Typed literal regions whose contents whitespace edits must preserve.
struct ProtectedSyntax<'a> {
    /// Coordinates of the original syntax revision.
    source: &'a DocumentSource,
    /// Original byte intervals occupied by strings.
    ranges: Vec<Range<usize>>,
}

impl<'ast> Visit<'ast> for ProtectedSyntax<'_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        if matches!(
            expression,
            Expr::Literal(ExprLiteral::String(_) | ExprLiteral::RawString(_))
        ) {
            self.ranges.extend(self.source.syntax_bytes(expression));
        }
        visit::visit_expr(self, expression);
    }
}
