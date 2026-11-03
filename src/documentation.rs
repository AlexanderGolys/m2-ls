//! Structurally attached Markdown documentation and references embedded in it.

use std::ops::Range;

use m2_syn::nodes::{Expr, ExprCollection, ExprLiteral, PackedContent, Statement};
use m2_syn::visit::{self, Visit};
use m2_syn::{Trivia, TriviaKind};
use tower_lsp::lsp_types::Range as TextRange;

use crate::analysis::{Analysis, AssignmentFactKind, BindingId, BindingView, MethodInstallationId};
use crate::macaulay2::Macaulay2Conventions;
use crate::meta::BindingRole;
use crate::node_metadata::markdown::MarkdownSyntaxParser;
use crate::node_metadata::SyntaxSnapshot;
use crate::object_registry::ObjectName;
use crate::source::{ByteRange, DocumentSource, SourceNavigation, SymbolMention};
use crate::syntax::{
    AssignmentSyntax, BindingReferences, ExpressionSyntax, LambdaSyntax, PackedArguments,
};

/// The semantic owner of one accepted documentation block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentationTarget {
    /// Documentation attached to a package declaration.
    Package,
    /// Documentation attached to a particular source binding state.
    Binding {
        /// Stable identity of the scoped source binding.
        binding_id: BindingId,
        /// Source extent identifying the selected binding state.
        state_span: TextRange,
        /// Name used to resolve and present this item.
        name: ObjectName,
    },
    /// Documentation attached to a method installation.
    Installation {
        /// Identity of the documented method installation.
        installation_id: MethodInstallationId,
        /// Name used to resolve and present this item.
        name: ObjectName,
        /// Source extent used to associate documentation with its owner.
        anchor: TextRange,
    },
    /// Documentation attached to an assignment's source extent.
    Assignment {
        /// Text presented to the editor or user.
        label: String,
        /// Source extent used to associate documentation with its owner.
        anchor: TextRange,
    },
}

impl DocumentationTarget {
    /// Whether this value owns binding.
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

    /// Whether this value owns installation.
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
    /// Rendered Markdown shown when hovering the documented item.
    hover_markdown: String,
    /// Binding targets or documentation owners associated with this item.
    targets: Vec<DocumentationTarget>,
}

impl DocumentationBlock {
    /// A block showing `hover_markdown` for each of `targets`.
    pub fn new(hover_markdown: String, targets: Vec<DocumentationTarget>) -> Self {
        Self {
            hover_markdown,
            targets,
        }
    }

    /// Rendered Markdown associated with this documentation block.
    pub fn hover_markdown(&self) -> &str {
        &self.hover_markdown
    }

    #[cfg(test)]
    pub fn targets(&self) -> &[DocumentationTarget] {
        &self.targets
    }
}

/// One Markdown code fragment highlighted inside an opaque comment or legacy
/// documentation string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentationSnippet {
    /// Original source interval in UTF-8 byte offsets.
    pub bytes: ByteRange,
}

impl DocumentationSnippet {
    /// Original UTF-8 byte extent of this documentation snippet.
    pub fn span(&self) -> ByteRange {
        self.bytes.clone()
    }
}

/// All documentation projections computed for one source snapshot.
#[derive(Debug, Default)]
pub struct DocumentationIndex {
    /// Documentation blocks associated with source declarations.
    blocks: Vec<DocumentationBlock>,
    /// Code fragments extracted from documentation comments.
    snippets: Vec<DocumentationSnippet>,
    /// Symbol mentions or typed references retained for later queries.
    references: Vec<SymbolMention>,
    /// Original source intervals occupied by documentation comments.
    comment_ranges: Vec<ByteRange>,
    /// Source intervals treated as executable documentation examples.
    example_ranges: Vec<ByteRange>,
}

impl DocumentationIndex {
    #[cfg(test)]
    pub fn blocks(&self) -> &[DocumentationBlock] {
        &self.blocks
    }

    /// Source intervals occupied by documentation comments.
    pub fn comment_ranges(&self) -> &[ByteRange] {
        &self.comment_ranges
    }

    /// Source intervals treated as executable documentation examples.
    pub fn example_ranges(&self) -> &[ByteRange] {
        &self.example_ranges
    }

    /// Code fragments extracted from the document's documentation comments.
    pub fn snippets(&self) -> &[DocumentationSnippet] {
        &self.snippets
    }

    /// Add `blocks` documented elsewhere, such as package documentation
    /// pages, after the comment documentation, which answers first.
    pub fn extend_blocks(&mut self, blocks: impl IntoIterator<Item = DocumentationBlock>) {
        self.blocks.extend(blocks);
    }

    /// Symbol mentions extracted from documentation links and code snippets.
    pub fn references(&self) -> &[SymbolMention] {
        &self.references
    }

    /// Documentation associated with the selected binding state.
    pub fn for_binding(&self, binding: BindingView<'_>) -> Option<&DocumentationBlock> {
        self.blocks.iter().find(|block| {
            block
                .targets
                .iter()
                .any(|target| target.owns_binding(binding))
        })
    }

    /// Documentation associated with the selected method-installation identity.
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

/// Classify documentation by its position among typed statement lists, parse its
/// Markdown, and index its explicit references.
pub fn collect_documentation(
    source: &(impl SourceNavigation + ?Sized),
    syntax: &SyntaxSnapshot,
    analysis: &Analysis,
) -> DocumentationIndex {
    let mut index = DocumentationIndex::default();
    let mut markdown_parser = MarkdownSyntaxParser::default();
    let lists = ItemLists::collect(source, syntax);
    let sites = lists.comment_sites(source, syntax);

    collect_line_documentation(
        &sites,
        &lists,
        source,
        analysis,
        &mut markdown_parser,
        &mut index,
    );
    collect_inner_documentation(
        &sites,
        &lists,
        source,
        analysis,
        &mut markdown_parser,
        &mut index,
    );

    let mut snippets = LegacySnippets {
        source,
        index: &mut index,
    };
    syntax.visit(&mut snippets);

    index.references.sort_by_key(SymbolMention::byte_span);
    index
        .references
        .dedup_by_key(|reference| reference.byte_span());
    index
        .snippets
        .sort_by_key(|snippet| (snippet.bytes.start, snippet.bytes.end));
    index.snippets.dedup_by_key(|snippet| snippet.span());
    index
}

/// Extracted Markdown with mappings back to the original comment bytes.
#[derive(Debug)]
struct DocumentationText {
    /// Rendered documentation or its extracted Markdown coordinates.
    markdown: String,
    /// Mappings between extracted Markdown and original comment bytes.
    mappings: Vec<DocumentationMapping>,
}

impl DocumentationText {
    /// Constructs this representation from line comments.
    fn from_line_comments(comments: &[Comment<'_>]) -> Self {
        let mut markdown = String::new();
        let mut mappings = Vec::new();
        for comment in comments {
            let mut offset = comment.bytes.start;
            for line in comment.text.split_inclusive('\n') {
                if !markdown.is_empty() {
                    markdown.push('\n');
                }
                let line_text = line.trim_end_matches(['\n', '\r']);
                let raw = line_text
                    .trim_start_matches([' ', '\t'])
                    .trim_start_matches('-');
                let content = raw.strip_prefix(' ').unwrap_or(raw);
                let source_end = offset + line_text.len();
                let markdown_start = markdown.len();
                markdown.push_str(content);
                mappings.push(DocumentationMapping {
                    markdown: markdown_start..markdown.len(),
                    source: source_end - content.len()..source_end,
                });
                offset += line.len();
            }
        }
        Self { markdown, mappings }
    }

    /// Constructs this representation from block comment.
    fn from_block_comment(comment: &Comment<'_>) -> Self {
        // `-*` opens a block comment and `-**` a doc block comment, closing with
        // the mirrored `*-` / `**-`. Measuring the opening marker and mirroring
        // its width keeps a `*` that belongs to the text (`-* x**-`) while still
        // removing the whole doc marker.
        let text = comment.text;
        let opening = 1 + text[1..]
            .chars()
            .take_while(|marker| *marker == '*')
            .count();
        let closing = opening.min(text.len().saturating_sub(opening));
        let content_start = comment.bytes.start + opening;
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

    /// Maps an extracted Markdown interval to one original source interval.
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

    /// Maps an extracted Markdown interval to all contributing original source intervals.
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

/// A Markdown byte interval paired with the original source interval.
#[derive(Debug)]
struct DocumentationMapping {
    /// Rendered documentation or its extracted Markdown coordinates.
    markdown: ByteRange,
    /// Original document byte interval corresponding to the extracted Markdown text.
    source: ByteRange,
}

/// A borrowed documentation line retaining its original byte coordinates.
#[derive(Debug)]
struct SourceLine<'a> {
    /// Original or extracted text represented by this item.
    text: &'a str,
    /// Original document byte interval corresponding to the extracted Markdown text.
    source: ByteRange,
}

/// Splits documentation text into lines retaining original source offsets.
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

/// Collects line documentation from the supplied syntax and semantic context.
fn collect_line_documentation(
    sites: &[CommentSite<'_>],
    lists: &ItemLists<'_>,
    source: &(impl SourceNavigation + ?Sized),
    analysis: &Analysis,
    markdown_parser: &mut MarkdownSyntaxParser,
    index: &mut DocumentationIndex,
) {
    let full_line_comment = |site: &CommentSite<'_>| {
        site.comment.kind == TriviaKind::LineComment && site.comment.takes_full_line(source.text())
    };
    let row = |byte: usize| source.point_for_byte(byte).row;
    let mut cursor = 0;
    while cursor < sites.len() {
        if !full_line_comment(&sites[cursor]) {
            cursor += 1;
            continue;
        }
        let mut end = cursor + 1;
        while end < sites.len()
            && full_line_comment(&sites[end])
            && sites[end].list == sites[end - 1].list
            && row(sites[end].comment.bytes.start) == row(sites[end - 1].comment.bytes.start) + 1
        {
            end += 1;
        }
        let block = &sites[cursor..end];
        cursor = end;
        let last = &block[block.len() - 1];
        let Some(item) = lists.following_item(last) else {
            continue;
        };
        if row(item.bytes.start) != row(last.comment.bytes.start) + 1 {
            continue;
        }
        let targets = targets_for_item(item, source, analysis);
        if targets.is_empty() {
            continue;
        }
        let comments = block
            .iter()
            .map(|site| site.comment.clone())
            .collect::<Vec<_>>();
        index
            .comment_ranges
            .extend(comments.iter().map(|comment| comment.bytes.clone()));
        collect_markdown_block(
            DocumentationText::from_line_comments(&comments),
            targets,
            source,
            markdown_parser,
            index,
        );
    }
}

/// Collects inner documentation from the supplied syntax and semantic context.
fn collect_inner_documentation(
    sites: &[CommentSite<'_>],
    lists: &ItemLists<'_>,
    source: &(impl SourceNavigation + ?Sized),
    analysis: &Analysis,
    markdown_parser: &mut MarkdownSyntaxParser,
    index: &mut DocumentationIndex,
) {
    for site in sites
        .iter()
        .filter(|site| site.comment.kind == TriviaKind::BlockComment)
    {
        let list = &lists.lists[site.list];
        let Some(following) = lists.following_item(site) else {
            continue;
        };
        if list
            .items
            .iter()
            .any(|item| item.bytes.start < site.comment.bytes.start)
        {
            continue;
        }
        let target = match &list.owner {
            ListOwner::Document => is_new_package_declaration(following.expression)
                .then_some(DocumentationTarget::Package),
            ListOwner::Delimited { .. } => enclosing_scope_target(&list.owner, source, analysis),
        };
        let Some(target) = target else {
            continue;
        };
        index.comment_ranges.push(site.comment.bytes.clone());
        collect_markdown_block(
            DocumentationText::from_block_comment(&site.comment),
            vec![target],
            source,
            markdown_parser,
            index,
        );
    }
}

/// Collects markdown block from the supplied syntax and semantic context.
fn collect_markdown_block(
    text: DocumentationText,
    targets: Vec<DocumentationTarget>,
    source: &(impl SourceNavigation + ?Sized),
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
        index
            .example_ranges
            .extend(text.source_ranges(range.clone()));
        index.snippets.extend(
            text.source_ranges(range)
                .into_iter()
                .map(|bytes| DocumentationSnippet { bytes }),
        );
    }

    {
        for (_, target_range) in &wiki_links {
            collect_mapped_m2_references(&text, target_range.clone(), source, index);
            index.snippets.extend(
                text.source_ranges(target_range.clone())
                    .into_iter()
                    .map(|bytes| DocumentationSnippet { bytes }),
            );
        }
    }

    let mut hover_markdown = text.markdown.clone();
    for (whole, target) in wiki_links.iter().rev() {
        hover_markdown.replace_range(
            whole.clone(),
            &format!("`{}`", &text.markdown[target.clone()]),
        );
    }
    index.blocks.push(DocumentationBlock {
        hover_markdown,
        targets,
    });
}

/// Collects mapped m2 references from the supplied syntax and semantic context.
fn collect_mapped_m2_references(
    text: &DocumentationText,
    target: ByteRange,
    source: &(impl SourceNavigation + ?Sized),
    index: &mut DocumentationIndex,
) {
    let candidate = &text.markdown[target.clone()];
    for symbol in snippet_binding_ranges(candidate) {
        let markdown_range = target.start + symbol.start..target.start + symbol.end;
        if let Some(bytes) = text.source_range(markdown_range) {
            index
                .references
                .push(SymbolMention::new(source.span_for_bytes(bytes)));
        }
    }
}

/// Visits raw strings that may contain legacy documentation snippets.
struct LegacySnippets<'a, Source: ?Sized> {
    /// Source coordinates of the current revision.
    source: &'a Source,
    /// Documentation being collected.
    index: &'a mut DocumentationIndex,
}

impl<'ast, Source: SourceNavigation + ?Sized> Visit<'ast> for LegacySnippets<'_, Source> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        if matches!(expression, Expr::Literal(ExprLiteral::RawString(_))) {
            if let Some(bytes) = self.source.syntax_bytes(expression) {
                collect_legacy_backtick_snippets(bytes, self.source, self.index);
            }
        }
        visit::visit_expr(self, expression);
    }
}

/// Collects legacy backtick snippets from one raw string's source range.
fn collect_legacy_backtick_snippets(
    range: ByteRange,
    source: &(impl SourceNavigation + ?Sized),
    index: &mut DocumentationIndex,
) {
    let Some(container) = source.text().get(range.clone()) else {
        return;
    };
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
        let source_start = range.start + content_start;
        index
            .references
            .extend(snippet_binding_ranges(candidate).into_iter().map(|symbol| {
                SymbolMention::new(
                    source.span_for_bytes(source_start + symbol.start..source_start + symbol.end),
                )
            }));
        index.snippets.push(DocumentationSnippet {
            bytes: source_start..range.start + close,
        });
    }
}

/// Binding-reference ranges in a standalone documentation code fragment.
fn snippet_binding_ranges(text: &str) -> Vec<ByteRange> {
    let syntax = crate::node_metadata::SyntaxSnapshot::parse(text, None);
    let mut source = DocumentSource::new(text.to_owned());
    source.register_syntax(&syntax);
    let mut references = BindingReferences::default();
    syntax.visit(&mut references);
    references
        .0
        .into_iter()
        .filter_map(|symbol| source.syntax_bytes(symbol))
        .collect()
}

/// Bindings, assignments, or installations to which this documented source item contributes.
fn targets_for_item(
    item: &ListItem<'_>,
    source: &(impl SourceNavigation + ?Sized),
    analysis: &Analysis,
) -> Vec<DocumentationTarget> {
    let Expr::Assign(assignment) = item.expression else {
        return Vec::new();
    };
    let Some(target) = assignment.parts().target.bytes(source) else {
        return Vec::new();
    };
    let assignment_range = source.range_for_bytes(item.bytes.clone());
    let target_range = source.range_for_bytes(target);
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

/// Whether this expression is a new package declaration.
fn is_new_package_declaration(item: &Expr) -> bool {
    matches!(item,
        Expr::Op(m2_syn::nodes::ExprOp::Bin(m2_syn::nodes::ExprOpBin::Adj(call)))
            if call.left_operand.symbol_name() == Some(Macaulay2Conventions::PACKAGE_CONSTRUCTOR))
}

/// Tests whether the outer source range covers the complete inner range.
fn range_contains(outer: TextRange, inner: TextRange) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// The binding whose function body or scoped value is the list a block comment opens.
fn enclosing_scope_target(
    owner: &ListOwner,
    source: &(impl SourceNavigation + ?Sized),
    analysis: &Analysis,
) -> Option<DocumentationTarget> {
    let ListOwner::Delimited { expression, lambda } = owner else {
        return None;
    };
    let own_range = source.range_for_bytes(expression.clone());
    let (owner_range, scope_idx) = match analysis.scope_with_range(own_range) {
        Some(scope_idx) => (own_range, scope_idx),
        None => {
            let lambda_range = source.range_for_bytes(lambda.clone()?);
            (lambda_range, analysis.scope_with_range(lambda_range)?)
        }
    };
    analysis
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
        })
}

/// A comment retained as trivia, in document coordinates.
#[derive(Debug, Clone)]
struct Comment<'a> {
    /// Whether this is a line or a block comment.
    kind: TriviaKind,
    /// Extent of the comment in the document.
    bytes: ByteRange,
    /// Comment source text, including its markers.
    text: &'a str,
}

impl Comment<'_> {
    /// Whether the comment occupies the whole visible content of its source line.
    fn takes_full_line(&self, source: &str) -> bool {
        let line_start = source[..self.bytes.start]
            .rfind('\n')
            .map_or(0, |end| end + 1);
        source[line_start..self.bytes.start].trim().is_empty()
    }
}

/// A comment together with the statement list it sits directly in.
struct CommentSite<'a> {
    /// The comment.
    comment: Comment<'a>,
    /// Index of the innermost statement list containing the comment.
    list: usize,
}

/// One expression in a statement list.
struct ListItem<'ast> {
    /// Extent of the expression in the document.
    bytes: ByteRange,
    /// The generated expression.
    expression: &'ast Expr,
}

/// What a statement list belongs to.
enum ListOwner {
    /// The sequence of top-level cells.
    Document,
    /// A parenthesized or bracketed expression.
    Delimited {
        /// Extent of the delimited expression.
        expression: ByteRange,
        /// Extent of the function literal this expression is the body of.
        lambda: Option<ByteRange>,
    },
}

/// Ordered expressions that comments can document.
struct ItemList<'ast> {
    /// Extent enclosed by the list.
    bytes: ByteRange,
    /// Expressions in source order.
    items: Vec<ListItem<'ast>>,
    /// Construct the list belongs to.
    owner: ListOwner,
}

/// Every statement list of a document; the document-level list comes first.
struct ItemLists<'ast> {
    /// The document list followed by delimited lists in visiting order.
    lists: Vec<ItemList<'ast>>,
}

impl<'ast> ItemLists<'ast> {
    /// Gathers the document list and every delimited list in the snapshot.
    fn collect(source: &(impl SourceNavigation + ?Sized), syntax: &'ast SyntaxSnapshot) -> Self {
        let document_items = syntax
            .cells()
            .iter()
            .filter_map(|cell| cell.syntax())
            .flat_map(|cell| cell.content.arguments())
            .flatten()
            .filter_map(|expression| ListItem::of(expression, source))
            .collect();
        let mut collector = ListCollector {
            source,
            ancestors: Vec::new(),
            lists: vec![ItemList {
                bytes: 0..source.text().len(),
                items: document_items,
                owner: ListOwner::Document,
            }],
        };
        syntax.visit(&mut collector);
        Self {
            lists: collector.lists,
        }
    }

    /// Comments of the snapshot that sit between the items of their innermost list.
    fn comment_sites<'text>(
        &self,
        source: &'text (impl SourceNavigation + ?Sized),
        syntax: &SyntaxSnapshot,
    ) -> Vec<CommentSite<'text>> {
        let mut sites = syntax
            .cells()
            .iter()
            .flat_map(|cell| cell.trivia())
            .flat_map(|trivia| self.sites_of(trivia, source))
            .collect::<Vec<_>>();
        sites.sort_by_key(|site| site.comment.bytes.start);
        sites
    }

    /// Places each comment line of one trivia in its innermost list, skipping
    /// those that sit inside an item.
    ///
    /// Lexing merges adjacent line comments into one trivia; they are documented
    /// and attached line by line.
    fn sites_of<'text>(
        &self,
        trivia: &Trivia,
        source: &'text (impl SourceNavigation + ?Sized),
    ) -> Vec<CommentSite<'text>> {
        let Some(bytes) = source.syntax_bytes(trivia) else {
            return Vec::new();
        };
        let Some((list, innermost)) = self
            .lists
            .iter()
            .enumerate()
            .filter(|(_, list)| list.bytes.start <= bytes.start && bytes.end <= list.bytes.end)
            .min_by_key(|(_, list)| list.bytes.len())
        else {
            return Vec::new();
        };
        if innermost
            .items
            .iter()
            .any(|item| item.bytes.start <= bytes.start && bytes.end <= item.bytes.end)
        {
            return Vec::new();
        }
        let text = &source.text()[bytes.clone()];
        let comment = |kind, bytes: ByteRange| CommentSite {
            comment: Comment {
                kind,
                text: &source.text()[bytes.clone()],
                bytes,
            },
            list,
        };
        match trivia.kind() {
            TriviaKind::BlockComment => vec![comment(TriviaKind::BlockComment, bytes)],
            _ => {
                let mut offset = bytes.start;
                text.split_inclusive('\n')
                    .map(|line| {
                        let line_bytes = offset..offset + line.trim_end_matches(['\n', '\r']).len();
                        offset += line.len();
                        comment(TriviaKind::LineComment, line_bytes)
                    })
                    .collect()
            }
        }
    }

    /// The first item starting after the comment, in the comment's own list.
    fn following_item(&self, site: &CommentSite<'_>) -> Option<&ListItem<'ast>> {
        self.lists[site.list]
            .items
            .iter()
            .find(|item| item.bytes.start >= site.comment.bytes.end)
    }
}

impl<'ast> ListItem<'ast> {
    /// The item for an expression, absent when its extent is not registered.
    fn of(expression: &'ast Expr, source: &(impl SourceNavigation + ?Sized)) -> Option<Self> {
        Some(Self {
            bytes: source.syntax_bytes(expression)?,
            expression,
        })
    }
}

/// Collects the statement list of every delimited expression.
struct ListCollector<'a, 'ast, Source: ?Sized> {
    /// Source coordinates of the current revision.
    source: &'a Source,
    /// Expressions enclosing the one being visited, outermost first.
    ancestors: Vec<&'ast Expr>,
    /// Lists found so far.
    lists: Vec<ItemList<'ast>>,
}

impl<'ast, Source: SourceNavigation + ?Sized> Visit<'ast> for ListCollector<'_, 'ast, Source> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        if let (Some(items), Some(bytes)) = (
            delimited_items(expression),
            self.source.syntax_bytes(expression),
        ) {
            let lambda = match self.ancestors.last() {
                Some(Expr::Lambda(lambda)) if std::ptr::eq(lambda.body(), expression) => self
                    .ancestors
                    .last()
                    .and_then(|parent| self.source.syntax_bytes(*parent)),
                _ => None,
            };
            self.lists.push(ItemList {
                items: items
                    .into_iter()
                    .filter_map(|item| ListItem::of(item, self.source))
                    .collect(),
                owner: ListOwner::Delimited {
                    expression: bytes.clone(),
                    lambda,
                },
                bytes,
            });
        }
        self.ancestors.push(expression);
        visit::visit_expr(self, expression);
        self.ancestors.pop();
    }
}

/// Statement and trailing expressions of a delimited expression, in source order.
fn delimited_items(expression: &Expr) -> Option<Vec<&Expr>> {
    fn items<'ast>(
        statements: &'ast [Statement],
        elements: &'ast Option<PackedContent>,
    ) -> Vec<&'ast Expr> {
        statements
            .iter()
            .map(|statement| &statement.content)
            .chain(elements)
            .flat_map(|content| content.arguments())
            .flatten()
            .collect()
    }
    match expression {
        Expr::Holder(holder) => Some(
            holder
                .muted
                .iter()
                .flat_map(|statement| statement.content.arguments())
                .flatten()
                .chain(holder.value.as_deref())
                .collect(),
        ),
        Expr::Collection(ExprCollection::Array(node)) => {
            Some(items(&node.statements, &node.elements))
        }
        Expr::Collection(ExprCollection::List(node)) => {
            Some(items(&node.statements, &node.elements))
        }
        Expr::Collection(ExprCollection::Sequence(node)) => {
            Some(items(&node.statements, &node.elements))
        }
        Expr::Collection(ExprCollection::AngleBar(node)) => {
            Some(items(&node.statements, &node.elements))
        }
        _ => None,
    }
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
            document.documentation_blocks()[0].hover_markdown(),
            "docs for x and `ideal`"
        );
        assert_eq!(
            document
                .symbol_mentions()
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
            document("--- Triple dash docs.\nx = 1\n").documentation_blocks()[0].hover_markdown(),
            "Triple dash docs."
        );
        assert_eq!(
            document("-- Plain docs.\nx = 1\n").documentation_blocks()[0].hover_markdown(),
            "Plain docs."
        );
        assert_eq!(
            document("-** Doc block. **-\nnewPackage(\"P\")\n").documentation_blocks()[0]
                .hover_markdown(),
            "Doc block."
        );
        // The mirrored width leaves a `*` that belongs to the prose alone.
        assert_eq!(
            document("-* Emphasis **here**. *-\nnewPackage(\"P\")\n").documentation_blocks()[0]
                .hover_markdown(),
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
                .hover_markdown(),
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
        assert_eq!(document.symbol_mentions()[0].name(text), "ideal");
        assert!(!document.documentation_snippets().is_empty());
    }

    #[test]
    fn legacy_backticks_are_limited_to_raw_documentation_strings() {
        let text = "-- ordinary `x` comment\ndoc ///Use `x`.///\nx = 1\n";
        let document = document(text);

        assert_eq!(
            document
                .symbol_mentions()
                .iter()
                .map(|reference| reference.name(text))
                .collect::<Vec<_>>(),
            ["x"]
        );
    }
}
