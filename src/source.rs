//! Indexed source text and the repository's single position-conversion API.

use std::collections::HashMap;

use tower_lsp::lsp_types::Position;
use tower_lsp::lsp_types::Range as TextRange;
/// A zero-based line and UTF-8 byte column in source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourcePoint {
    /// Zero-based source line.
    pub row: usize,
    /// UTF-8 byte column within the line.
    pub column: usize,
}

impl SourcePoint {
    /// Creates a zero-based source point with a UTF-8 byte column.
    pub const fn new(row: usize, column: usize) -> Self {
        Self { row, column }
    }
}
use SourcePoint as Point;

use crate::node_metadata::SyntaxNode;
use crate::node_metadata::SyntaxSnapshot;
use crate::util::TextRangeExt;

/// A half-open range of byte offsets into source text.
pub type ByteRange = std::ops::Range<usize>;

/// Immutable coordinates for a source span in both parser-byte and LSP UTF-16
/// space.
///
/// The fields stay private so the two representations can only be constructed
/// together by [`SourceNavigation::span_for_bytes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSpan {
    /// Inclusive start and exclusive end offsets into the document UTF-8 text.
    bytes: ByteRange,
    /// Source extent used to locate this item.
    range: TextRange,
}

impl DocumentSpan {
    /// The parser-oriented byte range of this span.
    pub fn bytes(&self) -> ByteRange {
        self.bytes.clone()
    }

    /// The protocol-oriented UTF-16 range of this span.
    pub fn range(&self) -> TextRange {
        self.range
    }
}

/// A symbol named inside text that is not code: a link of a documentation
/// comment, or a name string listed by `export`. It refers to the same object
/// the name would refer to were it written as code at that position, so
/// navigation, highlights, references, and renames treat it as a use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolMention {
    /// Source coordinates associated with this item.
    span: DocumentSpan,
}

impl SymbolMention {
    /// The mention covering exactly the name at `span`.
    pub fn new(span: DocumentSpan) -> Self {
        Self { span }
    }

    /// The mentioned name, out of `text`, the file the mention is in.
    pub fn name<'a>(&self, text: &'a str) -> &'a str {
        &text[self.span.bytes()]
    }

    /// Where the mentioned name is written.
    pub fn span(&self) -> &DocumentSpan {
        &self.span
    }

    /// LSP UTF-16 extent of the represented source item.
    pub fn range(&self) -> TextRange {
        self.span.range()
    }

    /// Inclusive start and exclusive end byte offsets of the source extent.
    pub fn byte_span(&self) -> (usize, usize) {
        let bytes = self.span.bytes();
        (bytes.start, bytes.end)
    }

    /// Tests membership in the represented selection or source extent.
    pub fn contains(&self, position: Position) -> bool {
        self.span.range().contains_position(position)
    }
}
/// Cached byte ranges for every line's visible content.
#[derive(Debug, Clone)]
struct LineIndex {
    /// Byte ranges of each line's content, excluding its line ending.
    content_ranges: Vec<ByteRange>,
}

/// Original source text paired with its cached line-content byte ranges.
///
/// Each cached range excludes the line ending. The source text itself is stored
/// only once; the ranges are an index into it.
#[derive(Debug, Clone)]
pub struct DocumentSource {
    /// Original or extracted text represented by this item.
    text: String,
    /// Cached line boundaries used for byte and UTF-16 conversions.
    line_index: LineIndex,
    /// Parser source identities mapped to their current document offsets.
    syntax_offsets: HashMap<m2_syn::File, usize>,
}

impl DocumentSource {
    /// Index one owned source snapshot.
    pub fn new(text: String) -> Self {
        let line_index = LineIndex::new(&text);
        Self {
            text,
            line_index,
            syntax_offsets: Default::default(),
        }
    }

    /// Associates cell-local typed spans with their location in this revision.
    pub fn register_syntax(&mut self, syntax: &SyntaxSnapshot) {
        use m2_syn::Spanned;
        self.syntax_offsets.clear();
        for cell in syntax.cells() {
            if let Some(node) = cell.syntax() {
                self.syntax_offsets
                    .insert(node.span().file(), cell.document_range().start);
            }
        }
    }

    /// Replace one byte range and rebuild the line index exactly once.
    pub fn replace_range(&mut self, bytes: ByteRange, replacement: &str) {
        self.text.replace_range(bytes, replacement);
        self.line_index = LineIndex::new(&self.text);
    }
}

/// Navigation over source text using one cached line index.
///
/// Implementers expose their [`DocumentSource`]; every byte, source point,
/// and LSP UTF-16 conversion is supplied here so capabilities cannot grow
/// independent conversion formulas.
pub trait SourceNavigation {
    /// The indexed source snapshot backing this navigator.
    fn source(&self) -> &DocumentSource;

    /// Maps a retained cell's typed range into this document revision.
    fn syntax_bytes(&self, syntax: &(impl m2_syn::Spanned + ?Sized)) -> Option<ByteRange> {
        let span = syntax.try_span()?;
        let range = span.range()?;
        let offset = self
            .source()
            .syntax_offsets
            .get(&span.file())
            .copied()
            .unwrap_or(0);
        Some(offset + range.start..offset + range.end)
    }

    /// The original source text.
    fn text(&self) -> &str {
        &self.source().text
    }

    /// Convert a source byte index to an LSP UTF-16 position.
    fn position_for_byte(&self, byte_index: usize) -> Position {
        let source = self.source();
        let byte_index = floor_char_boundary(&source.text, byte_index);
        let (line_index, line) = source.line_index.line_for_byte(byte_index);
        let byte_index = byte_index.min(line.end);
        pos!(
            line_index as u32,
            utf16_len(&source.text[line.start..byte_index])
        )
    }

    /// Convert an LSP UTF-16 position to a source byte index.
    fn byte_for_position(&self, position: Position) -> Option<usize> {
        let source = self.source();
        let line = source.line_index.content_range(position.line)?;
        let content = &source.text[line.clone()];
        Some(line.start + utf16_column_to_byte(content, position.character))
    }

    /// Convert an LSP UTF-16 range to its source byte range.
    fn bytes_for_range(&self, range: TextRange) -> Option<ByteRange> {
        let start = self.byte_for_position(range.start)?;
        let end = self.byte_for_position(range.end)?;
        (start <= end).then_some(start..end)
    }

    /// Convert a source byte index to a UTF-8 byte-column point.
    fn point_for_byte(&self, byte_index: usize) -> Point {
        let source = self.source();
        let byte_index = floor_char_boundary(&source.text, byte_index);
        let (line_index, line) = source.line_index.line_for_byte(byte_index);
        Point::new(line_index, byte_index - line.start)
    }

    /// Convert an LSP UTF-16 position to a UTF-8 byte-column point.
    fn point_for_position(&self, position: Position) -> Option<Point> {
        let byte_index = self.byte_for_position(position)?;
        Some(self.point_for_byte(byte_index))
    }

    /// Convert a byte range once and retain both coordinate representations.
    fn span_for_bytes(&self, bytes: ByteRange) -> DocumentSpan {
        DocumentSpan {
            range: TextRange::new(
                self.position_for_byte(bytes.start),
                self.position_for_byte(bytes.end),
            ),
            bytes,
        }
    }

    /// Convert one parser node to a dual-coordinate source span.
    fn span_for_node(&self, node: SyntaxNode<'_>) -> DocumentSpan {
        self.span_for_bytes(node.start_byte()..node.end_byte())
    }

    /// Convert one byte range to its LSP UTF-16 range.
    fn range_for_bytes(&self, bytes: ByteRange) -> TextRange {
        self.span_for_bytes(bytes).range()
    }

    /// Convert one parser node to its LSP UTF-16 range.
    fn range_for_node(&self, node: SyntaxNode<'_>) -> TextRange {
        self.span_for_node(node).range()
    }

    /// Convert one parser node's start to an LSP UTF-16 position.
    fn position_for_node(&self, node: SyntaxNode<'_>) -> Position {
        self.position_for_byte(node.start_byte())
    }

    /// The full LSP range of the indexed source snapshot.
    fn full_range(&self) -> TextRange {
        self.range_for_bytes(0..self.text().len())
    }

    /// The range from a byte position through the end of its visible line.
    fn remainder_of_line_range(&self, start_byte: usize) -> TextRange {
        let source = self.source();
        let start = self.position_for_byte(start_byte);
        let line = source
            .line_index
            .content_range(start.line)
            .expect("a byte within source text must belong to an indexed line");
        TextRange::new(start, self.position_for_byte(line.end))
    }

    /// Split a possibly multiline span into legal single-line semantic-token
    /// ranges, excluding line endings and empty lines.
    fn visible_ranges(&self, span: &DocumentSpan) -> Vec<TextRange> {
        let source = self.source();
        let bytes = span.bytes();
        let range = span.range();
        let first_line = range.start.line as usize;
        let last_line = range.end.line as usize;
        let mut ranges = Vec::new();

        for line_index in first_line..=last_line {
            let line = &source.line_index.content_ranges[line_index];
            let start_byte = if line_index == first_line {
                bytes.start
            } else {
                line.start
            }
            .min(line.end);
            let end_byte = if line_index == last_line {
                bytes.end
            } else {
                line.end
            }
            .min(line.end);
            if start_byte >= end_byte {
                continue;
            }

            ranges.push(self.range_for_bytes(start_byte..end_byte));
        }
        ranges
    }
}

impl LineIndex {
    /// Discover line boundaries once for a source snapshot.
    fn new(text: &str) -> Self {
        let mut content_ranges = Vec::new();
        let mut start_byte = 0;
        for segment in text.split_inclusive('\n') {
            let content = segment.strip_suffix('\n').unwrap_or(segment);
            let content = content.strip_suffix('\r').unwrap_or(content);
            content_ranges.push(start_byte..start_byte + content.len());
            start_byte += segment.len();
        }
        if text.is_empty() || text.ends_with('\n') {
            content_ranges.push(text.len()..text.len());
        }
        Self { content_ranges }
    }

    /// The indexed line containing `byte_index`, with line endings attributed to
    /// the preceding line.
    fn line_for_byte(&self, byte_index: usize) -> (usize, &ByteRange) {
        let line_index = self
            .content_ranges
            .partition_point(|range| range.start <= byte_index)
            .saturating_sub(1);
        (line_index, &self.content_ranges[line_index])
    }

    /// The visible byte range for an LSP line number.
    fn content_range(&self, line: u32) -> Option<&ByteRange> {
        self.content_ranges.get(line as usize)
    }
}

impl SourceNavigation for DocumentSource {
    fn source(&self) -> &DocumentSource {
        self
    }
}

/// Clamp a byte offset down to a valid UTF-8 character boundary.
fn floor_char_boundary(text: &str, byte_index: usize) -> usize {
    let mut byte_index = byte_index.min(text.len());
    while byte_index > 0 && !text.is_char_boundary(byte_index) {
        byte_index -= 1;
    }
    byte_index
}

/// Convert a UTF-16 column within one line to its byte offset.
fn utf16_column_to_byte(line: &str, utf16_column: u32) -> usize {
    let mut current_column = 0;

    for (byte_index, character) in line.char_indices() {
        let next_column = current_column + character.len_utf16() as u32;
        if next_column > utf16_column {
            return byte_index;
        }
        current_column = next_column;
    }

    line.len()
}

/// Count the UTF-16 code units in one valid source slice.
fn utf16_len(text: &str) -> u32 {
    text.encode_utf16().count() as u32
}

/// Source coordinates paired with the native snapshot supplying typed spans.
pub struct SyntaxNavigation<'a, S: ?Sized> {
    /// Original source navigator supplying unchanged text coordinates.
    original: &'a S,
    /// Cell-local source identities mapped to revision-specific offsets.
    offsets: HashMap<m2_syn::File, usize>,
}

impl<'a, S: SourceNavigation + ?Sized> SyntaxNavigation<'a, S> {
    /// Associates a source navigator with the cell offsets of this syntax revision.
    pub fn new(original: &'a S, snapshot: &SyntaxSnapshot) -> Self {
        use m2_syn::Spanned;
        let offsets = snapshot
            .cells()
            .iter()
            .filter_map(|cell| Some((cell.syntax()?.span().file(), cell.document_range().start)))
            .collect();
        Self { original, offsets }
    }
}

impl<S: SourceNavigation + ?Sized> SourceNavigation for SyntaxNavigation<'_, S> {
    fn source(&self) -> &DocumentSource {
        self.original.source()
    }
    fn syntax_bytes(&self, syntax: &(impl m2_syn::Spanned + ?Sized)) -> Option<ByteRange> {
        let span = syntax.try_span()?;
        let range = span.range()?;
        let offset = self.offsets.get(&span.file()).copied().unwrap_or(0);
        Some(offset + range.start..offset + range.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_converts_utf8_bytes_and_utf16_positions_both_ways() {
        let source = DocumentSource::new("x\n😀 ideal\r\n".to_string());
        let ideal_start = source.text().find("ideal").expect("fixture contains ideal");

        assert_eq!(source.position_for_byte(ideal_start), pos!(1, 3));
        assert_eq!(source.byte_for_position(pos!(1, 3)), Some(ideal_start));
        assert_eq!(source.full_range(), TextRange::new(pos!(), pos!(2, 0)));
    }

    #[test]
    fn visible_ranges_split_multiline_spans_and_exclude_line_endings() {
        let source = DocumentSource::new("aa\r\n😀\nbb".to_string());
        let span = source.span_for_bytes(1..source.text().len() - 1);

        assert_eq!(
            source.visible_ranges(&span),
            vec![
                TextRange::new(pos!(0, 1), pos!(0, 2)),
                TextRange::new(pos!(1, 0), pos!(1, 2)),
                TextRange::new(pos!(2, 0), pos!(2, 1)),
            ]
        );
    }
}
