//! Configured parser and owned syntax-tree lifecycle.

use m2_syn::nodes::SourceFile;
use tree_sitter::{InputEdit, Parser, Tree};

use super::M2Node;

/// A Macaulay2 parser with its language configured.
///
/// Parser construction and grammar selection are centralized here so syntax
/// consumers receive typed [`M2Node`] roots instead of configuring Tree-sitter
/// themselves.
pub struct M2Parser {
    parser: Parser,
    tree: Option<M2Tree>,
}

impl M2Parser {
    /// Construct a parser configured for the pinned Macaulay2 grammar.
    pub fn new() -> Option<Self> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_macaulay2::language())
            .ok()?;
        Some(Self { parser, tree: None })
    }

    /// Parse source and return its typed root.
    ///
    /// The parser retains the backing syntax tree. Consequently Rust prevents a
    /// second parse through this parser while the returned root is still used.
    pub fn parse<'tree>(&'tree mut self, source: &'tree str) -> Option<M2Node<'tree>> {
        let tree = self.parser.parse(source, None).map(M2Tree::new)?;
        self.tree = Some(tree);
        self.tree.as_ref().map(|tree| tree.root(source))
    }

    /// Parse source into an opaque tree retained by long-lived document state.
    pub fn parse_tree(&mut self, source: &str, old_tree: Option<&M2Tree>) -> Option<M2Tree> {
        self.parser
            .parse(source, old_tree.map(|tree| &tree.tree))
            .map(M2Tree::new)
    }
}

/// An owned Macaulay2 syntax tree.
///
/// The raw Tree-sitter tree remains private; consumers obtain a typed root with
/// [`M2Tree::root`]. Document snapshots retain this wrapper to support
/// incremental reparsing.
#[derive(Debug, Clone)]
pub struct M2Tree {
    tree: Tree,
}

impl M2Tree {
    fn new(tree: Tree) -> Self {
        Self { tree }
    }

    /// Return the typed root paired with the source represented by this tree.
    pub fn root<'tree>(&'tree self, source: &'tree str) -> M2Node<'tree> {
        M2Node::new(self.tree.root_node(), source)
    }

    /// The typed syntax for this tree's source.
    ///
    /// An editing session spends most of its time mid-keystroke, where the
    /// strict entry point rejects the whole document over one incomplete
    /// expression. Recovery keeps the usable syntax instead, so analysis stays
    /// on the typed tree rather than falling back to untyped traversal.
    /// `None` means the source could not be lexed at all.
    pub fn typed_source_file(&self, source: &str) -> Option<SourceFile> {
        m2_syn::parse_file_recovering(source)
            .ok()
            .map(|recovered| recovered.value)
    }

    /// Apply an incremental source edit before reparsing.
    pub fn edit(&mut self, edit: &InputEdit) {
        self.tree.edit(edit);
    }
}
