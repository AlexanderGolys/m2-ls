//! Mutable whitespace coordinates over one immutable, already parsed tree.

use m2_syn::nodes::{Expr, Literal};
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
    /// Original byte intervals of strings, comments, and malformed syntax
    /// that edits must not alter, disjoint and in order.
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
        ranges.sort_by_key(|range| range.start);
        let mut protected: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
        for range in ranges {
            match protected.last_mut() {
                Some(last) if range.start < last.end => last.end = last.end.max(range.end),
                _ => protected.push(range),
            }
        }
        Self {
            original: source.clone(),
            source,
            starts: (0..=text.len()).collect(),
            ends: (0..=text.len()).collect(),
            protected,
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

    /// Whether `byte` lies in a string, a comment or an unparsable region.
    pub(super) fn is_protected_at(&self, byte: usize) -> bool {
        self.is_protected_within(byte..byte + 1)
    }

    /// Whether any byte of `bytes` lies in a string, a comment or an
    /// unparsable region.
    pub(super) fn is_protected_within(&self, bytes: Range<usize>) -> bool {
        let mapped = |span: &Range<usize>| {
            let start = self.starts[span.start];
            start..self.ends[span.end].max(start)
        };
        let first = self
            .protected
            .partition_point(|span| mapped(span).end <= bytes.start);
        self.protected
            .get(first)
            .is_some_and(|span| mapped(span).start < bytes.end)
    }

    /// Whether a line comment starts at `byte`, as `--` outside a string.
    pub(super) fn starts_line_comment(&self, byte: usize) -> bool {
        self.is_protected_at(byte) && self.text()[byte..].starts_with(Self::LINE_COMMENT)
    }

    /// The width in bytes of the spaces and tabs filling `bytes`; `None`
    /// when anything else, such as a line break, is there.
    pub(super) fn horizontal_gap(&self, bytes: Range<usize>) -> Option<usize> {
        let gap = self.text().get(bytes)?;
        gap.bytes()
            .all(|byte| matches!(byte, b' ' | b'\t'))
            .then_some(gap.len())
    }

    /// Apply the edits that are allowed, dropping the others: an edit must
    /// replace whitespace outside protected regions with whitespace, or a
    /// whole line comment with another one, and must not overlap an edit
    /// listed before it. Returns whether the text changed.
    pub(super) fn apply(&mut self, edits: Vec<FormatEdit>) -> bool {
        let mut accepted: Vec<FormatEdit> = Vec::new();
        for edit in edits {
            let overlaps = accepted.iter().any(|kept| {
                kept.start_byte < edit.end_byte && edit.start_byte < kept.end_byte
                    || (kept.start_byte == edit.start_byte && kept.end_byte == edit.end_byte)
            });
            if !overlaps && self.allows(&edit) {
                accepted.push(edit);
            }
        }
        accepted.retain(|edit| self.text()[edit.start_byte..edit.end_byte] != edit.replacement);
        if accepted.is_empty() {
            return false;
        }
        accepted.sort_by_key(|edit| (edit.start_byte, edit.end_byte));
        self.remap(&accepted);
        let mut text = self.text().to_string();
        for edit in accepted.into_iter().rev() {
            text.replace_range(edit.start_byte..edit.end_byte, &edit.replacement);
        }
        self.source = DocumentSource::new(text);
        true
    }

    /// Whether `edit` may be applied: see [`Self::apply`].
    fn allows(&self, edit: &FormatEdit) -> bool {
        let bytes = edit.start_byte..edit.end_byte;
        let replaced = &self.text()[bytes.clone()];
        let whitespace = |text: &str| text.bytes().all(|byte| byte.is_ascii_whitespace());
        if whitespace(replaced) && whitespace(&edit.replacement) {
            return if bytes.is_empty() {
                bytes.start == 0
                    || !(self.is_protected_at(bytes.start - 1) && self.is_protected_at(bytes.start))
            } else {
                !self.is_protected_within(bytes)
            };
        }
        [replaced, edit.replacement.as_str()]
            .iter()
            .all(|text| text.starts_with(Self::LINE_COMMENT) && !text.contains('\n'))
            && self.protected_ranges().any(|span| span == bytes)
    }

    /// The marker opening a line comment.
    const LINE_COMMENT: &str = "--";

    /// Move every original boundary past the sorted, disjoint `edits`. A
    /// token starting where text is inserted starts after the insertion; one
    /// starting where an edit replaces text, as a rewritten comment does,
    /// starts where the replacement does.
    fn remap(&mut self, edits: &[FormatEdit]) {
        let mut shifts = vec![0isize];
        for edit in edits {
            shifts.push(
                shifts.last().unwrap() + edit.replacement.len() as isize
                    - (edit.end_byte - edit.start_byte) as isize,
            );
        }
        let map = |position: usize, is_start: bool| {
            let count = edits.partition_point(|edit| {
                edit.start_byte < position
                    || (is_start && edit.start_byte == position && edit.start_byte == edit.end_byte)
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
            Expr::Literal(Literal::String(_) | Literal::RawString(_))
        ) {
            self.ranges.extend(self.source.syntax_bytes(expression));
        }
        visit::visit_expr(self, expression);
    }
}
