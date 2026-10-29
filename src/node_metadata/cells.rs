//! Document-owned cell parses and immutable syntax revisions.

use m2_syn::{
    frame_recovering, lex_str_recovering,
    nodes::{Cell, SourceFile},
    CellStream, CellTokenStream, CommentTrivia, LexError, NativeParseError, NativeParser, Parser,
    Spanned, Token, Trivia,
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::{iter, ops::Range};

use crate::telemetry::Timing;

/// Stable identity shared by revisions retaining the same cell parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellId(u64);
/// Process-wide counter allocating identities for newly parsed cells.
static NEXT_CELL_ID: AtomicU64 = AtomicU64::new(1);

/// A revision-specific location of an independently retained parse result.
#[derive(Debug, Clone)]
pub struct ParsedCell {
    /// Source extent used to locate this item.
    range: Range<usize>,
    /// Parse allocation shared by revisions retaining the same cell.
    data: Arc<CellData>,
}

/// Shared ownership of one cell's source, parse result, trivia, and diagnostics.
#[derive(Debug)]
struct CellData {
    /// Stable identity used to refer to this retained item.
    id: CellId,
    /// Original UTF-8 source bytes retained for this cell or document snapshot.
    source: Arc<str>,
    /// Source-file parse result for this independently framed cell.
    syntax: Option<SourceFile>,
    /// Comments retained alongside the cell parse.
    trivia: Vec<Trivia>,
    /// Recoverable and fatal lexical errors for this cell.
    lexer_diagnostics: Vec<LexError>,
    /// Fatal grammatical error, absent when parsing succeeded.
    parse_error: Option<NativeParseError>,
}

impl ParsedCell {
    /// Identity of this retained item, stable while its owning cell is reused.
    pub fn id(&self) -> CellId {
        self.data.id
    }
    /// Original source bytes owned by this retained parse.
    pub fn source(&self) -> &str {
        &self.data.source
    }
    /// Cell extent in this document revision; sharing the parse does not share its location.
    pub fn document_range(&self) -> Range<usize> {
        self.range.clone()
    }
    /// First generated cell in the retained parse result, absent for comment-only or invalid source.
    pub fn syntax(&self) -> Option<&Cell> {
        self.data.syntax.as_ref()?.cells.first()
    }
    /// Comments retained alongside the cell, including when its code fails to parse.
    pub fn trivia(&self) -> &[Trivia] {
        &self.data.trivia
    }
    /// Recoverable and fatal lexical diagnostics recorded for this cell.
    pub fn lexer_diagnostics(&self) -> &[LexError] {
        &self.data.lexer_diagnostics
    }
    /// Fatal grammatical error for the owning cell, when one was recorded.
    pub fn parse_error(&self) -> Option<&NativeParseError> {
        self.data.parse_error.as_ref()
    }

    /// Parses this framed source interval and retains lexical trivia even if grammar parsing fails.
    fn parse(source: &str, range: Range<usize>) -> Self {
        let _timing = Timing::start(
            "syntax.native_cell_parse",
            json!({"source_bytes": source.len(), "byte_start": range.start, "byte_end": range.end}),
        );
        let mut data = CellData {
            id: CellId(NEXT_CELL_ID.fetch_add(1, Ordering::Relaxed)),
            source: Arc::from(source),
            syntax: None,
            trivia: Vec::new(),
            lexer_diagnostics: Vec::new(),
            parse_error: None,
        };
        match lex_str_recovering(source) {
            Ok(recovered) => {
                let comments = CommentTrivia::of(&recovered.value);
                data.trivia = comments
                    .outer()
                    .iter()
                    .chain(comments.inner())
                    .cloned()
                    .collect();
                data.lexer_diagnostics = recovered.diagnostics;
                let framed = frame_recovering(recovered.value);
                let cells =
                    CellStream::from_cells(framed.cells.into_iter().chain(framed.incomplete));
                match NativeParser.parse_cells(cells) {
                    Ok(syntax) => data.syntax = Some(syntax),
                    Err(error) => data.parse_error = Some(error),
                }
            }
            Err(error) => data.lexer_diagnostics.push(error),
        }
        Self {
            range,
            data: Arc::new(data),
        }
    }
}

/// Syntax shared by all capabilities for one document revision.
#[derive(Debug, Clone)]
pub struct SyntaxSnapshot {
    /// Original UTF-8 source bytes retained for this cell or document snapshot.
    source: Arc<str>,
    /// Independently retained cells in document order.
    cells: Arc<[ParsedCell]>,
    /// Current revision ranges that required reparsing.
    changed: Arc<[Range<usize>]>,
}

impl SyntaxSnapshot {
    /// Original source bytes owned by this retained parse.
    pub fn source(&self) -> &str {
        &self.source
    }
    /// Independently retained cell parses in document order.
    pub fn cells(&self) -> &[ParsedCell] {
        &self.cells
    }
    /// Source ranges reparsed in the current document revision.
    pub fn changed_ranges(&self) -> &[Range<usize>] {
        &self.changed
    }

    /// Parses this framed source interval and retains lexical trivia even if grammar parsing fails.
    pub fn parse(source: &str, previous: Option<&Self>) -> Self {
        let mut prefix = 0;
        let mut old_end = 0;
        let mut new_end = source.len();
        if let Some(old) = previous {
            if source == old.source() {
                return Self {
                    changed: Arc::from([]),
                    ..old.clone()
                };
            }
            prefix = source
                .bytes()
                .zip(old.source.bytes())
                .take_while(|(a, b)| a == b)
                .count();
            while !source.is_char_boundary(prefix) || !old.source.is_char_boundary(prefix) {
                prefix -= 1;
            }
            let mut suffix = source[prefix..]
                .bytes()
                .rev()
                .zip(old.source[prefix..].bytes().rev())
                .take_while(|(a, b)| a == b)
                .count();
            while !source.is_char_boundary(source.len() - suffix)
                || !old.source.is_char_boundary(old.source.len() - suffix)
            {
                suffix -= 1;
            }
            old_end = old.source.len() - suffix;
            new_end = source.len() - suffix;
        }
        // Include the preceding cell at a boundary: deleting whitespace can join cells.
        let first = previous.map_or(0, |old| {
            old.cells.partition_point(|cell| cell.range.end < prefix)
        });
        let start = previous
            .and_then(|old| old.cells.get(first))
            .map_or(0, |cell| cell.range.start);
        let mut cells = previous.map_or_else(Vec::new, |old| old.cells[..first].to_vec());
        // Stop framing once the changed region reaches a confirmed newline
        // boundary aligned with a retained cell in the unchanged suffix.
        let resumed = previous.and_then(|old| {
            old.cells
                .iter()
                .enumerate()
                .skip(first)
                .filter(|(_, cell)| cell.range.start >= old_end)
                .find_map(|(index, cell)| {
                    let end = new_end + cell.range.start - old_end;
                    let region = source.get(start..end)?;
                    if !region.ends_with('\n') {
                        return None;
                    }
                    let recovered = lex_str_recovering(region).ok()?;
                    if !recovered.diagnostics.is_empty()
                        || frame_recovering(recovered.value).incomplete.is_some()
                    {
                        return None;
                    }
                    Some((index, end))
                })
        });
        let rebuild_end = resumed.map_or(source.len(), |(_, end)| end);
        let mut changed = Vec::new();
        for range in frame_ranges(&source[start..rebuild_end]) {
            let range = start + range.start..start + range.end;
            let retained = previous.and_then(|old| {
                let old_start = if range.start >= new_end {
                    old_end.checked_add(range.start - new_end)?
                } else if range.end <= prefix {
                    range.start
                } else {
                    return None;
                };
                let index = old
                    .cells
                    .partition_point(|cell| cell.range.start < old_start);
                let cell = old.cells.get(index)?;
                (cell.range.start == old_start && cell.source() == &source[range.clone()]).then(
                    || ParsedCell {
                        range: range.clone(),
                        data: Arc::clone(&cell.data),
                    },
                )
            });
            cells.push(retained.unwrap_or_else(|| {
                changed.push(range.clone());
                ParsedCell::parse(&source[range.clone()], range)
            }));
        }
        if let Some((index, _)) = resumed {
            let old = previous.expect("a retained suffix belongs to a previous revision");
            cells.extend(old.cells[index..].iter().map(|cell| {
                let start = new_end + cell.range.start - old_end;
                ParsedCell {
                    range: start..start + cell.source().len(),
                    data: Arc::clone(&cell.data),
                }
            }));
        }
        Self {
            source: Arc::from(source),
            cells: cells.into(),
            changed: changed.into(),
        }
    }
}

/// Frames independently parseable source cells without guessing boundaries inside lexical constructs.
fn frame_ranges(source: &str) -> Vec<Range<usize>> {
    if source.is_empty() {
        return Vec::new();
    }
    let (tokens, failed_at) = match lex_str_recovering(source) {
        Ok(recovered) => (recovered.value, None),
        Err(error) => {
            // Retain completed cells before a fatal lexical error without guessing
            // a restart point inside a string, comment, or delimiter group.
            let end = error.span().start().unwrap_or(0);
            match lex_str_recovering(&source[..end]) {
                Ok(prefix) => (prefix.value, Some(end)),
                Err(_) => return iter::once(0..source.len()).collect(),
            }
        }
    };
    let framed = frame_recovering(tokens);
    let has_tail = framed.incomplete.is_some() || failed_at.is_some();
    let count = framed.cells.len();
    let mut ranges = Vec::new();
    let mut start = 0;
    for (i, tokens) in framed.cells.into_iter().enumerate() {
        let mut end = cell_end(&tokens, source);
        if i + 1 == count && !has_tail {
            end = source.len();
        }
        if start < end {
            ranges.push(start..end);
            start = end;
        }
    }
    if start < source.len() {
        ranges.push(start..source.len());
    }
    ranges
}

/// End of a framed cell including its terminating newline when appropriate.
fn cell_end(tokens: &CellTokenStream, source: &str) -> usize {
    let end = tokens.try_span().and_then(|span| span.end()).unwrap_or(0);
    let semicolon =
        tokens.last().and_then(|token| token.spelling()) == Some(<m2_syn::Token![;]>::SPELLING);
    if semicolon {
        end
    } else {
        source[end..]
            .find('\n')
            .map_or(end, |newline| end + newline + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_cells_keep_allocations_and_revision_specific_locations() {
        let before = SyntaxSnapshot::parse("x = 1\ny = 2\nz = 3\n", None);
        let after = SyntaxSnapshot::parse("x = 12345\ny = 2\nz = 3\n", Some(&before));
        assert_eq!(before.cells().len(), 3);
        assert_eq!(after.cells().len(), 3);
        assert!(Arc::ptr_eq(&before.cells()[1].data, &after.cells()[1].data));
        assert_eq!(before.cells()[1].document_range(), 6..12);
        assert_eq!(after.cells()[1].document_range(), 10..16);
        assert_eq!(after.changed_ranges().len(), 1);
        assert_eq!(after.changed_ranges()[0], 0..10);
        assert_eq!(before.source(), "x = 1\ny = 2\nz = 3\n");
    }

    #[test]
    fn editing_a_boundary_reframes_joined_cells_and_retains_the_suffix() {
        let before = SyntaxSnapshot::parse("a\nb\nc\n", None);
        let after = SyntaxSnapshot::parse("a+\nb\nc\n", Some(&before));
        assert_eq!(after.cells().len(), 2);
        assert_eq!(after.cells()[0].source(), "a+\nb\n");
        assert!(after.cells()[0].syntax().is_some());
        assert_eq!(after.cells()[1].id(), before.cells()[2].id());
    }

    #[test]
    fn no_op_revisions_do_not_reparse() {
        let before = SyntaxSnapshot::parse("α = 1\nβ = 2\n", None);
        let after = SyntaxSnapshot::parse(before.source(), Some(&before));
        assert!(after.changed_ranges().is_empty());
        assert!(Arc::ptr_eq(&before.cells, &after.cells));
    }

    #[test]
    fn malformed_cell_preserves_valid_neighbors() {
        let snapshot = SyntaxSnapshot::parse("a = 1\nf = x -> (x + )\nb = 2\n", None);
        assert_eq!(snapshot.cells().len(), 3);
        assert!(snapshot.cells()[0].syntax().is_some());
        assert!(snapshot.cells()[1].parse_error().is_some());
        assert!(snapshot.cells()[2].syntax().is_some());
    }

    #[test]
    fn comment_only_source_keeps_its_trivia() {
        let snapshot = SyntaxSnapshot::parse("-- comment\n", None);
        assert_eq!(snapshot.cells().len(), 1);
        assert_eq!(snapshot.cells()[0].trivia().len(), 1);
    }

    #[test]
    fn fatal_lexical_error_keeps_confirmed_prefix_cells() {
        let snapshot = SyntaxSnapshot::parse("a = 1\nb = 2\ns = \"bad\\q\"\n", None);
        assert_eq!(snapshot.cells().len(), 3);
        assert!(snapshot.cells()[0].syntax().is_some());
        assert!(snapshot.cells()[1].syntax().is_some());
        assert_eq!(snapshot.cells()[2].lexer_diagnostics().len(), 1);
    }
}
