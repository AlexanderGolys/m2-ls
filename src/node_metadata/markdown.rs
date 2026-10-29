//! Typed access to the Markdown syntax used by documentation blocks.

use std::ops::Range;

use tree_sitter::Node;
use tree_sitter_md::{MarkdownParser, MarkdownTree};

/// Parser for Markdown documentation syntax.
#[derive(Default)]
pub struct MarkdownSyntaxParser {
    /// Parser instance reused for successive Markdown inputs.
    parser: MarkdownParser,
}

impl MarkdownSyntaxParser {
    /// Parses the supplied source or name into this representation.
    pub fn parse(&mut self, source: &str) -> Option<MarkdownSyntaxTree> {
        self.parser
            .parse(source.as_bytes(), None)
            .map(|tree| MarkdownSyntaxTree { tree })
    }
}

/// Owned syntax tree for one Markdown document.
pub struct MarkdownSyntaxTree {
    /// Parsed syntax or the retained tree owning borrowed nodes.
    tree: MarkdownTree,
}

impl MarkdownSyntaxTree {
    /// Walks the retained syntax with the supplied generated visitor.
    pub fn visit(&self, mut visit: impl FnMut(MarkdownNode<'_>)) {
        let mut cursor = self.tree.walk();
        loop {
            visit(MarkdownNode {
                node: cursor.node(),
            });
            if cursor.goto_first_child() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    return;
                }
            }
        }
    }
}

/// One node in a parsed Markdown document.
#[derive(Clone, Copy)]
pub struct MarkdownNode<'tree> {
    /// Syntax supplying this item's source and structure.
    node: Node<'tree>,
}

impl<'tree> MarkdownNode<'tree> {
    /// Target and display-label byte ranges of a recognized documentation wikilink.
    pub fn wiki_link_ranges(self, source: &str) -> Option<(Range<usize>, Range<usize>)> {
        if self.node.kind() != "shortcut_link" {
            return None;
        }
        let range = self.node.byte_range();
        if range.start == 0
            || source.as_bytes().get(range.start - 1) != Some(&b'[')
            || source.as_bytes().get(range.end) != Some(&b']')
        {
            return None;
        }
        let link_text = self.named_child("link_text")?;
        Some((range.start - 1..range.end + 1, link_text.byte_range()))
    }

    /// Source interval containing Macaulay2 code inside this Markdown node.
    pub fn embedded_m2_range(self, source: &str) -> Option<Range<usize>> {
        match self.node.kind() {
            "code_span" => {
                let children = self.named_children();
                let opening = children.first()?;
                let closing = children.last()?;
                Some(opening.end_byte()..closing.start_byte())
            }
            "code_fence_content" if self.is_m2_fence(source) => Some(self.node.byte_range()),
            _ => None,
        }
    }

    /// Direct named Markdown children, excluding anonymous punctuation tokens.
    fn named_children(self) -> Vec<Node<'tree>> {
        let mut cursor = self.node.walk();
        self.node.named_children(&mut cursor).collect()
    }

    /// Direct named Markdown child with the requested grammar category.
    fn named_child(self, kind: &str) -> Option<Node<'tree>> {
        self.named_children()
            .into_iter()
            .find(|child| child.kind() == kind)
    }

    /// Whether a Markdown fence declares Macaulay2 code.
    fn is_m2_fence(self, source: &str) -> bool {
        self.node
            .parent()
            .filter(|parent| parent.kind() == "fenced_code_block")
            .and_then(|parent| MarkdownNode { node: parent }.named_child("info_string"))
            .is_some_and(|info| {
                matches!(
                    source[info.byte_range()].trim(),
                    "m2" | "macaulay2" | "Macaulay2"
                )
            })
    }
}
