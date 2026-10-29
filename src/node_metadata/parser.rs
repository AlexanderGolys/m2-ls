//! Native parsing and retained syntax ownership.

use serde_json::json;

use crate::source::SourcePoint;
use crate::telemetry::Timing;

use super::cells::SyntaxSnapshot;
use super::native::{indexes, CellIndex};
use super::{SyntaxNode, TypedSource};
use std::iter;
use std::ops::Range;
use std::sync::Arc;

/// Native Macaulay2 parser, retaining the syntax backing borrowed results.
pub struct SyntaxParser {
    /// Parsed syntax or the retained tree owning borrowed nodes.
    tree: Option<SyntaxTree>,
}

impl SyntaxParser {
    /// Creates an empty parser owner ready to retain its next syntax snapshot.
    pub fn new() -> Option<Self> {
        Some(Self { tree: None })
    }
    /// Parses source and returns a root borrowed from the tree retained by this parser.
    pub fn parse<'a>(&'a mut self, source: &'a str) -> Option<SyntaxNode<'a>> {
        self.tree = self.parse_tree(source, None);
        self.tree.as_ref().map(|tree| tree.root(source))
    }
    /// Builds a retained syntax revision, reusing unchanged cells from the previous tree.
    pub fn parse_tree(
        &mut self,
        source: &str,
        old_tree: Option<&SyntaxTree>,
    ) -> Option<SyntaxTree> {
        let mut timing = Timing::start(
            "syntax.parse",
            json!({"source_bytes": source.len(), "incremental": old_tree.is_some()}),
        );
        let framing = Timing::start("syntax.cells", json!({"source_bytes": source.len()}));
        let snapshot = SyntaxSnapshot::parse(source, old_tree.map(|tree| tree.snapshot()));
        drop(framing);
        let indexing = Timing::start(
            "syntax.navigation_index",
            json!({"source_bytes": source.len()}),
        );
        let indexes = indexes(
            &snapshot,
            old_tree.map(|old| (&old.syntax.snapshot, old.indexes.as_slice())),
        );
        drop(indexing);
        let reparsed = snapshot.changed_ranges().len();
        timing.record("reparsed_cells", reparsed);
        timing.record(
            "reused_cells",
            snapshot.cells().len().saturating_sub(reparsed),
        );
        let line_starts = iter::once(0)
            .chain(source.match_indices('\n').map(|(i, _)| i + 1))
            .collect::<Vec<_>>()
            .into();
        Some(SyntaxTree {
            syntax: TypedSource { snapshot },
            indexes,
            line_starts,
        })
    }
}

/// One authoritative native syntax snapshot and its navigation metadata.
#[derive(Debug, Clone)]
pub struct SyntaxTree {
    /// Parsed syntax backing this item, when available.
    pub(super) syntax: TypedSource,
    /// Navigation index shared with each retained cell.
    pub(super) indexes: Vec<Arc<CellIndex>>,
    /// UTF-8 byte offsets at which source lines begin.
    line_starts: Arc<[usize]>,
}

impl SyntaxTree {
    /// Synthetic root spanning the complete source document.
    pub fn root<'a>(&'a self, source: &'a str) -> SyntaxNode<'a> {
        debug_assert_eq!(source, self.syntax.snapshot.source());
        SyntaxNode::root_for(self)
    }
    /// Immutable cell parses and original source for this revision.
    pub fn snapshot(&self) -> &SyntaxSnapshot {
        &self.syntax.snapshot
    }
    /// Generated typed source view retained by this syntax tree.
    pub fn syntax(&self) -> &TypedSource {
        &self.syntax
    }
    /// Converts a document byte offset to its row and UTF-8 byte column.
    pub(super) fn point_for_byte(&self, byte: usize) -> SourcePoint {
        let row = self
            .line_starts
            .partition_point(|&start| start <= byte)
            .saturating_sub(1);
        SourcePoint::new(row, byte - self.line_starts[row])
    }
    /// Resolves a row and byte column to a document offset, when in bounds.
    pub(super) fn byte_for_point(&self, point: SourcePoint) -> Option<usize> {
        let byte = self.line_starts.get(point.row)?.checked_add(point.column)?;
        (byte <= self.snapshot().source().len()).then_some(byte)
    }
    /// Source ranges reparsed in the current document revision.
    pub fn changed_ranges(&self) -> Vec<Range<usize>> {
        self.snapshot().changed_ranges().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_typed_lookups_do_not_rebuild_syntax_children() {
        let source = format!(
            "values = {{{}}}\n",
            (0..256)
                .map(|value| format!("symbol value{value}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let tree = SyntaxParser::new()
            .unwrap()
            .parse_tree(&source, None)
            .unwrap();
        let enumerations = crate::node_metadata::syntax_ref::child_enumerations();
        for _ in 0..3 {
            for node in tree.root(&source).descendants() {
                let _ = node.syntax();
                let _ = node.get::<m2_syn::nodes::Symbol>();
            }
        }
        assert_eq!(
            crate::node_metadata::syntax_ref::child_enumerations(),
            enumerations
        );
    }

    #[test]
    fn cached_typed_references_survive_original_revision_drop_and_cell_relocation() {
        let mut parser = SyntaxParser::new().unwrap();
        let before = parser.parse_tree("a\nb\nc\n", None).unwrap();
        let after = parser.parse_tree("a -\nb\nc\n", Some(&before)).unwrap();
        assert!(Arc::ptr_eq(&before.indexes[2], &after.indexes[1]));
        drop(before);
        drop(parser);
        let root = after.root(after.snapshot().source());
        let c = root.symbols().find(|symbol| symbol.text() == "c").unwrap();
        assert!(c.get::<m2_syn::nodes::Symbol>().is_some());
        assert_eq!(c.start_position().row, 2);
    }

    #[test]
    fn changed_revisions_reuse_unaffected_navigation_indexes() {
        let mut parser = SyntaxParser::new().unwrap();
        let before = parser.parse_tree("x = 1\ny = 2\n", None).unwrap();
        let after = parser
            .parse_tree("x = 123\ny = 2\n", Some(&before))
            .unwrap();
        assert!(Arc::ptr_eq(&before.indexes[1], &after.indexes[1]));
        let y = after
            .root(after.snapshot().source())
            .symbols()
            .find(|symbol| symbol.text() == "y")
            .unwrap();
        assert_eq!(y.start_byte(), 8);
        assert_eq!(y.start_position().row, 1);
    }
}
