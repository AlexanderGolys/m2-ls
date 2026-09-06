//! Structurally attached Markdown documentation and references embedded in it.

use std::ops::Range;

use m2_syn::nodes::{RawStringLiteral, Symbol};
use tower_lsp::lsp_types::{Position, Range as TextRange};

use crate::analysis::{Analysis, AssignmentFactKind, BindingId, BindingView, MethodInstallationId};
use crate::meta::BindingRole;
use crate::node_metadata::markdown::MarkdownSyntaxParser;
use crate::node_metadata::{M2Node, M2Parser};
use crate::object_registry::ObjectName;
use crate::source::{ByteRange, DocumentSpan, SourceNavigation};
use crate::util::TextRangeExt;

/// The semantic owner of one accepted documentation block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentationTarget {
    Package,
    Binding {
        binding_id: BindingId,
        state_span: TextRange,
        name: ObjectName,
    },
    Installation {
        installation_id: MethodInstallationId,
        name: ObjectName,
        anchor: TextRange,
    },
    Assignment {
        label: String,
        anchor: TextRange,
    },
}

impl DocumentationTarget {
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Package => None,
            Self::Binding { name, .. } | Self::Installation { name, .. } => Some(name.name()),
            Self::Assignment { label, .. } => Some(label),
        }
    }

    pub fn owns_binding(&self, binding: BindingView<'_>) -> bool {
        matches!(
            self,
            Self::Binding {
                binding_id,
                state_span,
                ..
            } if *binding_id == binding.binding_id && *state_span == binding.state.span
        )
    }

    pub fn owns_installation(&self, installation_id: MethodInstallationId) -> bool {
        matches!(
            self,
            Self::Installation {
                installation_id: target_id,
                ..
            } if *target_id == installation_id
        )
    }
}

/// One Markdown block whose placement resolves to at least one meaningful owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentationBlock {
    source: DocumentSpan,
    markdown: String,
    hover_markdown: String,
    wiki_links: Vec<(ByteRange, ByteRange)>,
    targets: Vec<DocumentationTarget>,
}

impl DocumentationBlock {
    pub fn source_span(&self) -> &DocumentSpan {
        &self.source
    }

    pub fn hover_markdown(&self) -> &str {
        &self.hover_markdown
    }

    pub fn targets(&self) -> &[DocumentationTarget] {
        &self.targets
    }

    pub fn render_markdown(&self, mut resolve: impl FnMut(&str) -> Option<String>) -> String {
        render_wiki_links(&self.markdown, &self.wiki_links, |target| {
            resolve(target)
                .map(|destination| format!("[{target}]({destination})"))
                .unwrap_or_else(|| format!("`{target}`"))
        })
    }
}

/// One Markdown code fragment highlighted inside an opaque comment or legacy
/// documentation string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentationSnippet {
    bytes: ByteRange,
}

impl DocumentationSnippet {
    pub fn byte_span(&self) -> (usize, usize) {
        (self.bytes.start, self.bytes.end)
    }
}

/// One symbol mention extracted from an otherwise opaque documentation region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentationReference {
    span: DocumentSpan,
}

impl DocumentationReference {
    pub fn name<'a>(&self, text: &'a str) -> &'a str {
        &text[self.span.bytes()]
    }

    pub fn range(&self) -> TextRange {
        self.span.range()
    }

    pub fn byte_span(&self) -> (usize, usize) {
        let bytes = self.span.bytes();
        (bytes.start, bytes.end)
    }

    pub fn contains(&self, position: Position) -> bool {
        self.span.range().contains_position(position)
    }
}

/// All documentation projections computed for one source snapshot.
#[derive(Debug, Default)]
pub struct DocumentationIndex {
    blocks: Vec<DocumentationBlock>,
    snippets: Vec<DocumentationSnippet>,
    references: Vec<DocumentationReference>,
}

impl DocumentationIndex {
    pub fn blocks(&self) -> &[DocumentationBlock] {
        &self.blocks
    }

    pub fn snippets(&self) -> &[DocumentationSnippet] {
        &self.snippets
    }

    pub fn references(&self) -> &[DocumentationReference] {
        &self.references
    }

    pub fn for_binding(&self, binding: BindingView<'_>) -> Option<&DocumentationBlock> {
        self.blocks.iter().find(|block| {
            block
                .targets
                .iter()
                .any(|target| target.owns_binding(binding))
        })
    }

    pub fn for_installation(
        &self,
        installation_id: MethodInstallationId,
    ) -> Option<&DocumentationBlock> {
        self.blocks.iter().find(|block| {
            block
                .targets
                .iter()
                .any(|target| target.owns_installation(installation_id))
        })
    }
}

/// Classify documentation by CST position, parse its Markdown, and index its
/// explicit references.
pub fn collect_documentation(
    source: &(impl SourceNavigation + ?Sized),
    root: M2Node<'_>,
    analysis: &Analysis,
) -> DocumentationIndex {
    let mut index = DocumentationIndex::default();
    let mut m2_parser = M2Parser::new();
    let mut markdown_parser = MarkdownSyntaxParser::default();
    let comments = root
        .descendants()
        .filter(M2Node::is_comment)
        .collect::<Vec<_>>();

    collect_line_documentation(
        &comments,
        source,
        analysis,
        m2_parser.as_mut(),
        &mut markdown_parser,
        &mut index,
    );
    collect_inner_documentation(
        &comments,
        source,
        root,
        analysis,
        m2_parser.as_mut(),
        &mut markdown_parser,
        &mut index,
    );

    if let Some(parser) = m2_parser.as_mut() {
        for node in root
            .descendants()
            .filter(|node| node.is::<RawStringLiteral>())
        {
            collect_legacy_backtick_snippets(node, source, parser, &mut index);
        }
    }

    index
        .references
        .sort_by_key(DocumentationReference::byte_span);
    index
        .references
        .dedup_by_key(|reference| reference.byte_span());
    index.snippets.sort_by_key(DocumentationSnippet::byte_span);
    index.snippets.dedup_by_key(|snippet| snippet.byte_span());
    index
}

#[derive(Debug)]
struct DocumentationText {
    markdown: String,
    mappings: Vec<DocumentationMapping>,
}

impl DocumentationText {
    fn from_line_comments(comments: &[M2Node<'_>]) -> Self {
        let mut markdown = String::new();
        let mut mappings = Vec::new();
        for (index, comment) in comments.iter().enumerate() {
            if index > 0 {
                markdown.push('\n');
            }
            // A line comment opens with at least `--`, and a doc comment adds
            // further dashes (`---`). Stripping a fixed `--` would leave the
            // extra dash in the Markdown, where it reads as a list bullet.
            let raw = comment.text().trim_start_matches('-');
            let content = raw.strip_prefix(' ').unwrap_or(raw);
            let source_start = comment.end_byte() - content.len();
            let markdown_start = markdown.len();
            markdown.push_str(content);
            mappings.push(DocumentationMapping {
                markdown: markdown_start..markdown.len(),
                source: source_start..comment.end_byte(),
            });
        }
        Self { markdown, mappings }
    }

    fn from_block_comment(comment: M2Node<'_>) -> Self {
        // `-*` opens a block comment and `-**` a doc block comment, closing with
        // the mirrored `*-` / `**-`. Measuring the opening marker and mirroring
        // its width keeps a `*` that belongs to the text (`-* x**-`) while still
        // removing the whole doc marker.
        let text = comment.text();
        let opening = 1 + text[1..]
            .chars()
            .take_while(|marker| *marker == '*')
            .count();
        let closing = opening.min(text.len().saturating_sub(opening));
        let content_start = comment.start_byte() + opening;
        let content = &text[opening..text.len() - closing];
        let lines = source_lines(content, content_start);
        let first = lines
            .iter()
            .position(|line| !line.text.trim().is_empty())
            .unwrap_or(lines.len());
        let last = lines
            .iter()
            .rposition(|line| !line.text.trim().is_empty())
            .map_or(first, |last| last + 1);
        let lines = &lines[first..last];
        let indentation = lines
            .iter()
            .filter(|line| !line.text.trim().is_empty())
            .map(|line| line.text.len() - line.text.trim_start().len())
            .min()
            .unwrap_or(0);
        let mut markdown = String::new();
        let mut mappings = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                markdown.push('\n');
            }
            let strip = indentation.min(line.text.len());
            let visible_end = if index + 1 == lines.len() {
                line.text.trim_end().len()
            } else {
                line.text.len()
            };
            let visible_end = visible_end.max(strip);
            let text = &line.text[strip..visible_end];
            let markdown_start = markdown.len();
            markdown.push_str(text);
            mappings.push(DocumentationMapping {
                markdown: markdown_start..markdown.len(),
                source: line.source.start + strip..line.source.start + visible_end,
            });
        }
        Self { markdown, mappings }
    }

    fn source_range(&self, markdown: Range<usize>) -> Option<ByteRange> {
        self.mappings.iter().find_map(|mapping| {
            (mapping.markdown.start <= markdown.start && markdown.end <= mapping.markdown.end).then(
                || {
                    let start = mapping.source.start + markdown.start - mapping.markdown.start;
                    let end = mapping.source.start + markdown.end - mapping.markdown.start;
                    start..end
                },
            )
        })
    }

    fn source_ranges(&self, markdown: Range<usize>) -> Vec<ByteRange> {
        self.mappings
            .iter()
            .filter_map(|mapping| {
                let start = markdown.start.max(mapping.markdown.start);
                let end = markdown.end.min(mapping.markdown.end);
                (start < end).then(|| {
                    mapping.source.start + start - mapping.markdown.start
                        ..mapping.source.start + end - mapping.markdown.start
                })
            })
            .collect()
    }
}

#[derive(Debug)]
struct DocumentationMapping {
    markdown: ByteRange,
    source: ByteRange,
}

#[derive(Debug)]
struct SourceLine<'a> {
    text: &'a str,
    source: ByteRange,
}

fn source_lines(text: &str, source_start: usize) -> Vec<SourceLine<'_>> {
    let mut lines = Vec::new();
    let mut offset = 0;
    for segment in text.split_inclusive('\n') {
        let visible = segment.strip_suffix('\n').unwrap_or(segment);
        let visible = visible.strip_suffix('\r').unwrap_or(visible);
        lines.push(SourceLine {
            text: visible,
            source: source_start + offset..source_start + offset + visible.len(),
        });
        offset += segment.len();
    }
    if text.is_empty() || text.ends_with('\n') {
        lines.push(SourceLine {
            text: "",
            source: source_start + text.len()..source_start + text.len(),
        });
    }
    lines
}

fn collect_line_documentation(
    comments: &[M2Node<'_>],
    source: &(impl SourceNavigation + ?Sized),
    analysis: &Analysis,
    m2_parser: Option<&mut M2Parser>,
    markdown_parser: &mut MarkdownSyntaxParser,
    index: &mut DocumentationIndex,
) {
    let mut m2_parser = m2_parser;
    let mut cursor = 0;
    while cursor < comments.len() {
        let comment = comments[cursor];
        if !comment.is_line_comment() || !takes_full_line(comment, source.text()) {
            cursor += 1;
            continue;
        }
        let mut end = cursor + 1;
        while end < comments.len()
            && comments[end].is_line_comment()
            && takes_full_line(comments[end], source.text())
            && same_parent(comments[end - 1], comments[end])
            && comments[end].start_position().row == comments[end - 1].end_position().row + 1
        {
            end += 1;
        }
        let block_comments = &comments[cursor..end];
        cursor = end;
        let last = *block_comments.last().expect("nonempty comment block");
        let Some(item) = following_item(last) else {
            continue;
        };
        if item.start_position().row != last.end_position().row + 1 {
            continue;
        }
        let targets = targets_for_item(item, source, analysis);
        if targets.is_empty() {
            continue;
        }
        let text = DocumentationText::from_line_comments(block_comments);
        let source_span = source.span_for_bytes(
            block_comments[0].start_byte()
                ..block_comments
                    .last()
                    .expect("nonempty comment block")
                    .end_byte(),
        );
        collect_markdown_block(
            text,
            source_span,
            targets,
            source,
            m2_parser.as_deref_mut(),
            markdown_parser,
            index,
        );
    }
}

fn collect_inner_documentation(
    comments: &[M2Node<'_>],
    source: &(impl SourceNavigation + ?Sized),
    root: M2Node<'_>,
    analysis: &Analysis,
    m2_parser: Option<&mut M2Parser>,
    markdown_parser: &mut MarkdownSyntaxParser,
    index: &mut DocumentationIndex,
) {
    let mut m2_parser = m2_parser;
    for comment in comments.iter().copied().filter(M2Node::is_block_comment) {
        let Some(parent) = comment.parent() else {
            continue;
        };
        if !is_first_content(comment, parent) || following_item(comment).is_none() {
            continue;
        }
        let target = if parent.id() == root.id() {
            following_item(comment)
                .is_some_and(is_new_package_declaration)
                .then_some(DocumentationTarget::Package)
        } else {
            enclosing_scope_target(comment, parent, source, analysis)
        };
        let Some(target) = target else {
            continue;
        };
        collect_markdown_block(
            DocumentationText::from_block_comment(comment),
            source.span_for_node(comment),
            vec![target],
            source,
            m2_parser.as_deref_mut(),
            markdown_parser,
            index,
        );
    }
}

fn collect_markdown_block(
    text: DocumentationText,
    source_span: DocumentSpan,
    targets: Vec<DocumentationTarget>,
    source: &(impl SourceNavigation + ?Sized),
    m2_parser: Option<&mut M2Parser>,
    markdown_parser: &mut MarkdownSyntaxParser,
    index: &mut DocumentationIndex,
) {
    let Some(tree) = markdown_parser.parse(&text.markdown) else {
        return;
    };
    let mut wiki_links = Vec::new();
    let mut code_ranges = Vec::new();
    tree.visit(|node| {
        if let Some(ranges) = node.wiki_link_ranges(&text.markdown) {
            wiki_links.push(ranges);
        }
        if let Some(range) = node.embedded_m2_range(&text.markdown) {
            code_ranges.push(range);
        }
    });

    for range in code_ranges {
        index.snippets.extend(
            text.source_ranges(range)
                .into_iter()
                .map(|bytes| DocumentationSnippet { bytes }),
        );
    }

    if let Some(parser) = m2_parser {
        for (_, target_range) in &wiki_links {
            collect_mapped_m2_references(&text, target_range.clone(), source, parser, index);
            index.snippets.extend(
                text.source_ranges(target_range.clone())
                    .into_iter()
                    .map(|bytes| DocumentationSnippet { bytes }),
            );
        }
    }

    let hover_markdown =
        render_wiki_links(&text.markdown, &wiki_links, |target| format!("`{target}`"));
    index.blocks.push(DocumentationBlock {
        source: source_span,
        markdown: text.markdown,
        hover_markdown,
        wiki_links,
        targets,
    });
}

fn render_wiki_links(
    markdown: &str,
    wiki_links: &[(ByteRange, ByteRange)],
    mut render: impl FnMut(&str) -> String,
) -> String {
    let mut rendered = markdown.to_string();
    for (whole, target) in wiki_links.iter().rev() {
        rendered.replace_range(whole.clone(), &render(&markdown[target.clone()]));
    }
    rendered
}

fn collect_mapped_m2_references(
    text: &DocumentationText,
    target: ByteRange,
    source: &(impl SourceNavigation + ?Sized),
    parser: &mut M2Parser,
    index: &mut DocumentationIndex,
) {
    let candidate = &text.markdown[target.clone()];
    let Some(root) = parser.parse(candidate) else {
        return;
    };
    for symbol in root.symbols() {
        let markdown_range = target.start + symbol.start_byte()..target.start + symbol.end_byte();
        if let Some(bytes) = text.source_range(markdown_range) {
            index.references.push(DocumentationReference {
                span: source.span_for_bytes(bytes),
            });
        }
    }
}

fn collect_legacy_backtick_snippets(
    node: M2Node<'_>,
    source: &(impl SourceNavigation + ?Sized),
    parser: &mut M2Parser,
    index: &mut DocumentationIndex,
) {
    let container = node.text();
    let bytes = container.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let Some(relative_open) = container[cursor..].find('`') else {
            break;
        };
        let open = cursor + relative_open;
        if bytes.get(open.wrapping_sub(1)) == Some(&b'`') || bytes.get(open + 1) == Some(&b'`') {
            cursor = open + 1;
            continue;
        }
        let content_start = open + 1;
        let Some(relative_close) = container[content_start..].find('`') else {
            break;
        };
        let close = content_start + relative_close;
        cursor = close + 1;
        if bytes.get(close + 1) == Some(&b'`') {
            continue;
        }
        let candidate = &container[content_start..close];
        if candidate.trim().is_empty() || candidate.contains(['`', '\n', '\r']) {
            continue;
        }
        let source_start = node.start_byte() + content_start;
        let Some(root) = parser.parse(candidate) else {
            continue;
        };
        index
            .references
            .extend(root.symbols().map(|symbol| DocumentationReference {
                span: source.span_for_bytes(
                    source_start + symbol.start_byte()..source_start + symbol.end_byte(),
                ),
            }));
        index.snippets.push(DocumentationSnippet {
            bytes: source_start..node.start_byte() + close,
        });
    }
}

fn targets_for_item(
    item: M2Node<'_>,
    source: &(impl SourceNavigation + ?Sized),
    analysis: &Analysis,
) -> Vec<DocumentationTarget> {
    let Some(assignment) = first_item_expression(item).filter(M2Node::is_assignment) else {
        return Vec::new();
    };
    let Some(target) = assignment.child_by_field_name("left") else {
        return Vec::new();
    };
    let assignment_range = source.range_for_node(assignment);
    let target_range = source.range_for_node(target);
    let mut targets = Vec::new();
    for fact in analysis
        .assignment_facts()
        .iter()
        .filter(|fact| fact.span == assignment_range)
    {
        match fact.kind {
            AssignmentFactKind::MethodInstallation(id) => {
                if let Some(installation) = analysis.method_installation(id) {
                    targets.push(DocumentationTarget::Installation {
                        installation_id: id,
                        name: installation.method.head.name().clone(),
                        anchor: fact.target_span,
                    });
                }
            }
            AssignmentFactKind::IndexedVariable | AssignmentFactKind::ScopedCallable => {
                targets.push(DocumentationTarget::Assignment {
                    label: fact.label.clone(),
                    anchor: fact.target_span,
                });
            }
        }
    }
    targets.extend(
        analysis
            .binding_states()
            .filter(|binding| {
                binding.role == BindingRole::Ordinary
                    && range_contains(target_range, binding.state.span)
            })
            .map(|binding| DocumentationTarget::Binding {
                binding_id: binding.binding_id,
                state_span: binding.state.span,
                name: binding.name.clone(),
            }),
    );
    targets
}

fn first_item_expression(mut item: M2Node<'_>) -> Option<M2Node<'_>> {
    while item.is_source_cell() {
        let next = {
            let mut children = item.named_children();
            children.find(|child| !child.is_comment())
        }?;
        item = next;
    }
    Some(item)
}

fn is_new_package_declaration(item: M2Node<'_>) -> bool {
    let Some(application) = first_item_expression(item) else {
        return false;
    };
    if !application.is_space_application() {
        return false;
    }
    application
        .child_by_field_name("left")
        .is_some_and(|callee| callee.is::<Symbol>() && callee.text() == "newPackage")
}

fn range_contains(outer: TextRange, inner: TextRange) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

fn following_item(comment: M2Node<'_>) -> Option<M2Node<'_>> {
    let parent = comment.parent()?;
    let mut found = false;
    for sibling in parent.named_children() {
        if found && !sibling.is_comment() {
            return Some(sibling);
        }
        found |= sibling.id() == comment.id();
    }
    None
}

fn is_first_content(comment: M2Node<'_>, parent: M2Node<'_>) -> bool {
    parent
        .named_children()
        .take_while(|child| child.id() != comment.id())
        .all(|child| child.is_comment())
}

fn enclosing_scope_target(
    comment: M2Node<'_>,
    parent: M2Node<'_>,
    source: &(impl SourceNavigation + ?Sized),
    analysis: &Analysis,
) -> Option<DocumentationTarget> {
    for owner in comment.ancestors() {
        let owner_range = source.range_for_node(owner);
        let Some(scope_idx) = analysis.scope_with_range(owner_range) else {
            continue;
        };
        let owns_parent = owner.id() == parent.id()
            || owner
                .child_by_field_name("body")
                .is_some_and(|body| body.id() == parent.id());
        if owns_parent {
            return analysis
                .binding_states()
                .find(|binding| {
                    binding.role == BindingRole::Ordinary
                        && binding.state.value_range == Some(owner_range)
                        && binding.state.scope_idx != scope_idx
                })
                .map(|binding| DocumentationTarget::Binding {
                    binding_id: binding.binding_id,
                    state_span: binding.state.span,
                    name: binding.name.clone(),
                });
        }
        return None;
    }
    None
}

fn takes_full_line(comment: M2Node<'_>, source: &str) -> bool {
    let column = comment.start_position().column;
    comment
        .start_byte()
        .checked_sub(column)
        .is_some_and(|line_start| source[line_start..comment.start_byte()].trim().is_empty())
}

fn same_parent(left: M2Node<'_>, right: M2Node<'_>) -> bool {
    left.parent()
        .zip(right.parent())
        .is_some_and(|(left, right)| left.id() == right.id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::DocumentSnapshot;
    use crate::object_registry::ObjectRegistry;

    fn document(text: &str) -> DocumentSnapshot {
        DocumentSnapshot::from_text(text.to_string(), &ObjectRegistry::default())
            .expect("fixture should parse")
    }

    #[test]
    fn only_attached_line_blocks_become_documentation() {
        let text = concat!(
            "-- detached [[x]]\n",
            "\n",
            "-- docs for x and [[ideal]]\n",
            "x = 1\n",
            "-- ordinary expression\n",
            "x\n",
            "y = 2 -- trailing\n",
        );
        let document = document(text);

        assert_eq!(document.documentation_blocks().len(), 1);
        assert_eq!(
            document.documentation_blocks()[0].markdown.as_str(),
            "docs for x and [[ideal]]"
        );
        assert_eq!(
            document
                .documentation_references()
                .iter()
                .map(|reference| reference.name(text))
                .collect::<Vec<_>>(),
            ["ideal"]
        );
    }

    #[test]
    fn doc_comment_markers_are_stripped_whole() {
        // `---` and `-**` are the doc-comment markers. Removing only the `--` /
        // `-*` that a plain comment opens with would leave a stray `-` or `*` at
        // the start of the Markdown, which renders as a list bullet.
        assert_eq!(
            document("--- Triple dash docs.\nx = 1\n").documentation_blocks()[0]
                .markdown
                .as_str(),
            "Triple dash docs."
        );
        assert_eq!(
            document("-- Plain docs.\nx = 1\n").documentation_blocks()[0]
                .markdown
                .as_str(),
            "Plain docs."
        );
        assert_eq!(
            document("-** Doc block. **-\nnewPackage(\"P\")\n").documentation_blocks()[0]
                .markdown
                .as_str(),
            "Doc block."
        );
        // The mirrored width leaves a `*` that belongs to the prose alone.
        assert_eq!(
            document("-* Emphasis **here**. *-\nnewPackage(\"P\")\n").documentation_blocks()[0]
                .markdown
                .as_str(),
            "Emphasis **here**."
        );
    }

    #[test]
    fn leading_block_docs_require_a_package_declaration() {
        let package = document("-* Package **docs**. *-\nnewPackage(\"P\")\n");
        assert!(matches!(
            package.documentation_blocks()[0].targets()[0],
            DocumentationTarget::Package
        ));

        let script = document("-* Just a comment. *-\nx = 1\n");
        assert!(script.documentation_blocks().is_empty());
    }

    #[test]
    fn inner_block_docs_require_an_owning_binding() {
        let function = document("f := x -> (\n    -* Function docs. *-\n    x)\nf\n");
        let binding = function
            .source_binding_at("f", pos!(3, 0))
            .expect("function binding should resolve");
        assert_eq!(
            function
                .documentation_for_binding(binding)
                .expect("inner docs should attach to the function")
                .markdown
                .as_str(),
            "Function docs."
        );

        let expression = document("f := x -> ({-* Not scope docs. *- x}; x)\n");
        assert!(expression.documentation_blocks().is_empty());
    }

    #[test]
    fn markdown_wikilinks_and_code_are_parsed_from_attached_docs() {
        let text = concat!(
            "-- Use [[ideal]] and `x + 1`.\n",
            "--\n",
            "-- ```m2\n",
            "-- x = ideal 1\n",
            "-- ```\n",
            "x = 1\n",
        );
        let document = document(text);
        let block = &document.documentation_blocks()[0];

        assert_eq!(
            block.hover_markdown(),
            "Use `ideal` and `x + 1`.\n\n```m2\nx = ideal 1\n```"
        );
        assert_eq!(document.documentation_references()[0].name(text), "ideal");
        assert!(!document.documentation_snippets().is_empty());
    }

    #[test]
    fn legacy_backticks_are_limited_to_raw_documentation_strings() {
        let text = "-- ordinary `x` comment\ndoc ///Use `x`.///\nx = 1\n";
        let document = document(text);

        assert_eq!(
            document
                .documentation_references()
                .iter()
                .map(|reference| reference.name(text))
                .collect::<Vec<_>>(),
            ["x"]
        );
    }
}
