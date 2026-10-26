//! Mutable whitespace coordinates over one immutable, already parsed tree.

use std::ops::{Deref, Range};

use crate::source::SourcePoint as Point;

use crate::node_metadata::SyntaxNode;
use crate::source::{DocumentSource, SourceNavigation};

use super::FormatEdit;

pub(super) struct LayoutSource {
    source: DocumentSource,
    starts: Vec<usize>,
    ends: Vec<usize>,
    protected: Vec<Range<usize>>,
}

impl SourceNavigation for LayoutSource {
    fn source(&self) -> &DocumentSource {
        &self.source
    }
}

impl LayoutSource {
    pub(super) fn new(root: SyntaxNode<'_>) -> Self {
        let text = root.source_text();
        Self {
            source: DocumentSource::new(text.to_string()),
            starts: (0..=text.len()).collect(),
            ends: (0..=text.len()).collect(),
            protected: root
                .descendants()
                .filter(|node| {
                    node.is_string_literal()
                        || node.is_comment()
                        || node.is_error() && node.syntax().is_none()
                })
                .map(|node| node.start_byte()..node.end_byte())
                .collect(),
        }
    }

    pub(super) fn into_text(self) -> String {
        self.text().to_string()
    }

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

/// Syntax identity and relationships always come from the supplied tree.
/// Only byte and line coordinates follow the formatter's whitespace edits.
#[derive(Clone, Copy)]
pub(super) struct LayoutNode<'a> {
    syntax: SyntaxNode<'a>,
    layout: &'a LayoutSource,
}

impl<'a> Deref for LayoutNode<'a> {
    type Target = SyntaxNode<'a>;

    fn deref(&self) -> &Self::Target {
        &self.syntax
    }
}

impl<'a> LayoutNode<'a> {
    pub(super) fn new(syntax: SyntaxNode<'a>, layout: &'a LayoutSource) -> Self {
        Self { syntax, layout }
    }

    fn wrap(&self, syntax: SyntaxNode<'a>) -> Self {
        Self::new(syntax, self.layout)
    }

    pub(super) fn source_text(&self) -> &'a str {
        self.layout.text()
    }

    pub(super) fn start_byte(&self) -> usize {
        self.layout.starts[self.syntax.start_byte()]
    }

    pub(super) fn end_byte(&self) -> usize {
        self.layout.ends[self.syntax.end_byte()].max(self.start_byte())
    }

    pub(super) fn text(&self) -> &'a str {
        &self.source_text()[self.start_byte()..self.end_byte()]
    }

    pub(super) fn start_position(&self) -> Point {
        self.layout.point_for_byte(self.start_byte())
    }

    pub(super) fn end_position(&self) -> Point {
        self.layout.point_for_byte(self.end_byte())
    }

    pub(super) fn parent(&self) -> Option<Self> {
        self.syntax.parent().map(|node| self.wrap(node))
    }

    pub(super) fn child(&self, index: u32) -> Option<Self> {
        self.syntax.child(index).map(|node| self.wrap(node))
    }

    pub(super) fn named_child(&self, index: u32) -> Option<Self> {
        self.syntax.named_child(index).map(|node| self.wrap(node))
    }

    pub(super) fn operator_node(&self) -> Option<Self> {
        self.syntax.operator_node().map(|node| self.wrap(node))
    }
    pub(super) fn lambda_body(&self) -> Option<Self> {
        self.syntax.lambda_body().map(|node| self.wrap(node))
    }
    pub(super) fn clause_value(&self) -> Option<Self> {
        self.syntax.clause_value().map(|node| self.wrap(node))
    }
    pub(super) fn listed_value(&self) -> Option<Self> {
        self.syntax.listed_value().map(|node| self.wrap(node))
    }
    pub(super) fn ignored_value(&self) -> Option<Self> {
        self.syntax.ignored_value().map(|node| self.wrap(node))
    }
    pub(super) fn method_installation_head(&self) -> Option<LayoutBinaryParts<'a>> {
        let parts = self.syntax.method_installation_head()?;
        Some(LayoutBinaryParts {
            left: self.wrap(parts.left),
            right: self.wrap(parts.right),
        })
    }
    pub(super) fn binary_parts(&self) -> Option<LayoutBinaryParts<'a>> {
        let parts = self.syntax.binary_parts()?;
        Some(LayoutBinaryParts {
            left: self.wrap(parts.left),
            right: self.wrap(parts.right),
        })
    }

    pub(super) fn children(&self) -> impl Iterator<Item = Self> + '_ {
        self.syntax.children().map(|node| self.wrap(node))
    }

    pub(super) fn named_children(&self) -> impl Iterator<Item = Self> + '_ {
        self.syntax.named_children().map(|node| self.wrap(node))
    }

    pub(super) fn descendants(&self) -> impl Iterator<Item = Self> + '_ {
        self.syntax.descendants().map(|node| self.wrap(node))
    }

    pub(super) fn collection_elements(&self) -> impl Iterator<Item = Self> + '_ {
        self.syntax
            .collection_elements()
            .map(|node| self.wrap(node))
    }

    pub(super) fn is_delimited_expression(&self) -> bool {
        self.syntax.is_delimited_expression()
    }

    pub(super) fn is_option_assignment(&self) -> bool {
        self.syntax.is_option_assignment()
    }
}

pub(super) struct LayoutBinaryParts<'a> {
    pub left: LayoutNode<'a>,
    pub right: LayoutNode<'a>,
}
