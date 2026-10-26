//! Tree-sitter-guided formatting and folding ranges for Macaulay2 source.

use std::collections::VecDeque;

use m2_syn::nodes::{
    ElseClause, ExprFor as ForLoop, ExprIf as IfStatement, ExprLambda as LambdaExpression,
    ExprTry as TryStatement, ExprWhile as WhileLoop, FloatLiteral, IntegerLiteral, List, LoopBody,
    RawStringLiteral, Sequence, StringLiteral, Symbol, ThenClause,
};
use m2_syn::Token;
use tower_lsp::lsp_types::{
    DocumentFormattingOptions, FoldingRange, FoldingRangeKind, FoldingRangeProviderCapability,
    OneOf, TextEdit,
};

use crate::document::DocumentSnapshot;
use crate::node_metadata::{matches_token, SyntaxNode, SyntaxParser};

mod layout;
use crate::settings::FormattingSettings;
use crate::source::SourceNavigation;
use layout::{LayoutNode, LayoutSource};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ControlFlowLayout {
    Compact,
    Multiline,
    #[default]
    MultilineCompactElse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatOptions {
    indent: String,
    tab_width: usize,
    soft_line_width: Option<usize>,
    max_line_width: Option<usize>,
    control_flow_layout: ControlFlowLayout,
    compact_factor_operators: bool,
    break_after_semicolon: bool,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self::from(&FormattingSettings::default())
    }
}

impl From<&FormattingSettings> for FormatOptions {
    /// Resolve settings without an editor, using four-space indentation as
    /// the fallback for both the CLI and `Default`.
    fn from(settings: &FormattingSettings) -> Self {
        Self::from_settings(4, true, settings)
    }
}

impl FormatOptions {
    /// The options for `settings`, with the editor's `tab_size` and
    /// `insert_spaces` where the settings leave indentation to the editor.
    pub fn from_settings(
        tab_size: u32,
        insert_spaces: bool,
        settings: &FormattingSettings,
    ) -> Self {
        let tab_width = settings.indent.unwrap_or(tab_size).max(1) as usize;
        let use_tabs = settings.use_tabs.unwrap_or(!insert_spaces);
        let (soft_line_width, hard_line_width) = settings.line_widths();
        Self {
            indent: if use_tabs {
                "\t".to_string()
            } else {
                " ".repeat(tab_width)
            },
            tab_width,
            soft_line_width: soft_line_width.map(|width| width as usize),
            max_line_width: hard_line_width.map(|width| width as usize),
            control_flow_layout: settings.control_flow_layout,
            compact_factor_operators: settings.compact_factor_operators,
            break_after_semicolon: settings.break_after_semicolon,
        }
    }
}

pub fn document_formatting_provider_capability() -> Option<OneOf<bool, DocumentFormattingOptions>> {
    Some(OneOf::Left(true))
}

pub fn folding_range_provider_capability() -> Option<FoldingRangeProviderCapability> {
    Some(FoldingRangeProviderCapability::Simple(true))
}

pub fn document_formatting_text_edits(
    source: &DocumentSnapshot,
    tab_size: u32,
    insert_spaces: bool,
    settings: &FormattingSettings,
) -> Vec<TextEdit> {
    let text = source.text();
    let options = FormatOptions::from_settings(tab_size, insert_spaces, settings);
    let formatted = format_document_with_options(source.root_node(), &options);
    if formatted == text {
        return Vec::new();
    }

    vec![TextEdit {
        range: source.full_range(),
        new_text: formatted,
    }]
}

pub fn folding_ranges(root: SyntaxNode<'_>) -> Vec<FoldingRange> {
    folding_ranges_for_syntax(root)
        .into_iter()
        .map(|range| FoldingRange {
            start_line: range.start_line,
            start_character: None,
            end_line: range.end_line,
            end_character: None,
            kind: (range.kind == FormatFoldKind::Comment).then_some(FoldingRangeKind::Comment),
            collapsed_text: None,
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FormatEdit {
    start_byte: usize,
    end_byte: usize,
    replacement: String,
}

/// Format the whole document: whitespace normalization around operators and
/// punctuation, tree-derived indentation, and width-based wrapping at parsed
/// syntax boundaries.
pub fn format_document_with_options(root: SyntaxNode<'_>, options: &FormatOptions) -> String {
    formatted_layout(root, options).into_text()
}

/// Format a replacement in its surrounding syntax, returning only the changed
/// span so a code action does not reformat unrelated source. Insertions retain
/// their boundary whitespace (for example, a codomain's trailing separator).
pub(super) fn format_action_edit(
    source: &DocumentSnapshot,
    edit: &mut TextEdit,
    options: &FormatOptions,
) {
    if edit.range.start == edit.range.end {
        return;
    }
    let Some(bytes) = source.bytes_for_range(edit.range) else {
        return;
    };
    let end = bytes.start + edit.new_text.len();
    let mut text = source.text().to_string();
    text.replace_range(bytes.clone(), &edit.new_text);
    let Some(mut parser) = SyntaxParser::new() else {
        return;
    };
    let Some(tree) = parser.parse_tree(&text, Some(source.syntax_tree())) else {
        return;
    };
    let layout = formatted_layout(tree.root(&text), options);
    edit.new_text = layout.text_for_bytes(bytes.start..end).to_string();
}

fn formatted_layout(root: SyntaxNode<'_>, options: &FormatOptions) -> LayoutSource {
    let root = root.root();
    let newline = detect_line_ending(root.source_text());
    let mut layout = LayoutSource::new(root);
    let mut passes = VecDeque::from([
        FormatPass::Spacing,
        FormatPass::LeadingOperators,
        FormatPass::Options,
        FormatPass::EmptyBodies,
        FormatPass::ControlFlow,
        FormatPass::Indent,
        FormatPass::Wrap,
        FormatPass::Closers,
        FormatPass::Indent,
    ]);
    while let Some(pass) = passes.pop_front() {
        let node = LayoutNode::new(root, &layout);
        let edits = match pass {
            FormatPass::Spacing => normalize_whitespace(node, options),
            FormatPass::LeadingOperators => lead_rows_with_operators(node, options, newline),
            FormatPass::Options => break_option_tables(node, newline),
            FormatPass::EmptyBodies => inline_empty_bodies(node),
            FormatPass::ControlFlow => format_control_flow(node, options, newline),
            FormatPass::Indent => reindent_from_tree(node, options),
            FormatPass::Wrap => wrap_long_lines(node, options, newline),
            FormatPass::Closers => separate_line_end_bracket_closers(node, newline),
        };
        if layout.apply(edits) && matches!(pass, FormatPass::Wrap) {
            passes.push_front(FormatPass::Wrap);
            passes.push_front(FormatPass::Indent);
        }
    }
    layout
}

#[derive(Clone, Copy)]
enum FormatPass {
    Spacing,
    LeadingOperators,
    Options,
    EmptyBodies,
    ControlFlow,
    Indent,
    Wrap,
    Closers,
}

fn separate_line_end_bracket_closers(
    root: LayoutNode<'_>,
    newline: &'static str,
) -> Vec<FormatEdit> {
    let text = root.source_text();
    let mut edits = Vec::new();
    for node in root
        .descendants()
        .filter(LayoutNode::is_delimited_expression)
    {
        let children = node.children().collect::<Vec<_>>();
        let (Some(opener), Some(closer)) = (children.first(), children.last()) else {
            continue;
        };
        if opener.is_missing()
            || closer.is_missing()
            || !opener.is_opening_delimiter()
            || !closer.is_closing_delimiter()
            || opener.start_position().row >= closer.start_position().row
            || !text[opener.end_byte()..]
                .split(['\n', '\r'])
                .next()
                .is_some_and(|suffix| suffix.bytes().all(|byte| matches!(byte, b' ' | b'\t')))
        {
            continue;
        }
        let Some(start_byte) = same_line_horizontal_whitespace_start(text, closer.start_byte())
        else {
            continue;
        };
        edits.push(FormatEdit {
            start_byte,
            end_byte: closer.start_byte(),
            replacement: newline.to_string(),
        });
    }
    edits
}

/// Lays out every hash-table construction, a `{…}` list of at least two
/// options and nothing else, one option per line; re-indentation then indents
/// the options and puts the closing brace back at the opener's level:
///
/// ```m2
/// {
///     x => 1,
///     y => 2
/// }
/// ```
fn break_option_tables(root: LayoutNode<'_>, newline: &'static str) -> Vec<FormatEdit> {
    let text = root.source_text();
    let mut edits = Vec::new();
    for table in root.descendants().filter(|node| is_option_table(*node)) {
        let children = table.children().collect::<Vec<_>>();
        for pair in children.windows(2) {
            let [before, after] = pair else {
                continue;
            };
            if before.is_opening_delimiter() || before.is_comma() || after.is_closing_delimiter() {
                push_line_break_between(
                    text,
                    before.end_byte(),
                    after.start_byte(),
                    newline,
                    &mut edits,
                );
            }
        }
    }
    edits
}

/// Whether `node` is a `{…}` list of at least two elements, all options.
fn is_option_table(node: LayoutNode<'_>) -> bool {
    if !node.is::<List>() {
        return false;
    }
    let elements = node.collection_elements().collect::<Vec<_>>();
    elements.len() >= 2 && elements.iter().all(LayoutNode::is_option_assignment)
}

/// Replaces the horizontal whitespace between two tokens on one line, possibly
/// none, with a line break. Tokens already on separate lines, or with anything
/// but spaces between them, are left as they are.
fn push_line_break_between(
    text: &str,
    start_byte: usize,
    end_byte: usize,
    newline: &'static str,
    edits: &mut Vec<FormatEdit>,
) {
    if start_byte > end_byte
        || !text[start_byte..end_byte]
            .bytes()
            .all(|byte| matches!(byte, b' ' | b'\t'))
    {
        return;
    }
    edits.push(FormatEdit {
        start_byte,
        end_byte,
        replacement: newline.to_string(),
    });
}

/// The line terminator the document already uses, so re-indentation preserves it
/// instead of silently rewriting every CRLF line ending to LF (the trap behind
/// `str::lines()` + `join("\n")`). A document with any `\r\n` is treated as CRLF.
fn detect_line_ending(text: &str) -> &'static str {
    if text.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// Moves every binary operator that dangles at a line end onto the start of
/// the next row, when `breaks_before_operator` allows it:
///
/// ```m2
/// total := (first
///     + second
///     + third)
/// ```
fn lead_rows_with_operators(
    root: LayoutNode<'_>,
    options: &FormatOptions,
    newline: &'static str,
) -> Vec<FormatEdit> {
    let text = root.source_text();
    let mut edits = Vec::new();
    for expression in root.descendants() {
        let Some((parts, operator)) = expression.binary_parts().zip(expression.operator_node())
        else {
            continue;
        };
        if !breaks_before_operator(operator, options.compact_factor_operators) {
            continue;
        }
        let before = &text[parts.left.end_byte()..operator.start_byte()];
        let after = &text[operator.end_byte()..parts.right.start_byte()];
        let dangles = before.bytes().all(|byte| matches!(byte, b' ' | b'\t'))
            && after.bytes().all(|byte| byte.is_ascii_whitespace())
            && after.contains('\n');
        if dangles {
            edits.push(FormatEdit {
                start_byte: parts.left.end_byte(),
                end_byte: operator.start_byte(),
                replacement: newline.to_string(),
            });
            edits.push(FormatEdit {
                start_byte: operator.end_byte(),
                end_byte: parts.right.start_byte(),
                replacement: " ".to_string(),
            });
        }
    }
    edits
}

fn inline_empty_bodies(root: LayoutNode<'_>) -> Vec<FormatEdit> {
    let text = root.source_text();
    let mut edits = Vec::new();
    for body in root.descendants().filter(|node| empty_body_role(*node)) {
        let children = body.children().collect::<Vec<_>>();
        let (Some(opener), Some(closer)) = (children.first(), children.last()) else {
            continue;
        };
        if opener.is_opening_delimiter()
            && closer.is_closing_delimiter()
            && text[opener.end_byte()..closer.start_byte()]
                .bytes()
                .all(|byte| byte.is_ascii_whitespace())
        {
            edits.push(FormatEdit {
                start_byte: opener.end_byte(),
                end_byte: closer.start_byte(),
                replacement: String::new(),
            });
        }
    }
    edits
}

fn empty_body_role(node: LayoutNode<'_>) -> bool {
    if !node.is_delimited_expression() {
        return false;
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.is::<LambdaExpression>() {
        return parent
            .lambda_body()
            .is_some_and(|body| body.id() == node.id());
    }
    if parent.is_keyword_clause() {
        return parent
            .named_child(0)
            .is_some_and(|body| body.id() == node.id());
    }
    if parent.is::<LoopBody>() {
        return [parent.listed_value(), parent.ignored_value()]
            .into_iter()
            .flatten()
            .any(|body| body.id() == node.id());
    }
    parent.is::<TryStatement>()
}

fn format_control_flow(
    root: LayoutNode<'_>,
    options: &FormatOptions,
    newline: &'static str,
) -> Vec<FormatEdit> {
    let text = root.source_text();
    if options.control_flow_layout == ControlFlowLayout::Compact {
        return Vec::new();
    }

    let mut edits = Vec::new();
    for clause in root.descendants().filter(|node| {
        node.is::<ThenClause>()
            || node.is::<ElseClause>()
            || node.is_except_clause()
            || node.is::<LoopBody>()
    }) {
        if !control_clause_breaks_are_parseable(clause, text) {
            continue;
        }
        if clause.is::<LoopBody>() {
            for (keyword, body) in [
                (
                    direct_keyword::<Token![list]>(clause),
                    clause.listed_value(),
                ),
                (direct_keyword::<Token![do]>(clause), clause.ignored_value()),
            ] {
                let Some((keyword, body)) = keyword.zip(body) else {
                    continue;
                };
                push_control_clause_body_break(keyword, body, text, newline, &mut edits);
            }
            continue;
        }
        if clause.is_except_clause() {
            push_control_flow_break(
                text,
                same_line_horizontal_whitespace_start(text, clause.start_byte()),
                clause.start_byte(),
                newline,
                &mut edits,
            );
            if let Some((keyword, body)) =
                direct_keyword::<Token![do]>(clause).zip(clause.clause_value())
            {
                push_control_clause_body_break(keyword, body, text, newline, &mut edits);
            }
            continue;
        }
        let Some(keyword) = clause.child(0) else {
            continue;
        };
        let Some(body) = clause.named_child(0) else {
            continue;
        };

        if clause.is::<ElseClause>() || clause.is_except_clause() {
            if !clause.is::<ElseClause>() || !keep_else_after_closer(clause, text, &mut edits) {
                push_control_flow_break(
                    text,
                    same_line_horizontal_whitespace_start(text, clause.start_byte()),
                    clause.start_byte(),
                    newline,
                    &mut edits,
                );
            }
            if clause.is_except_clause() {
                continue;
            }
            if body.is::<IfStatement>()
                || options.control_flow_layout == ControlFlowLayout::MultilineCompactElse
            {
                continue;
            }
        }

        push_control_clause_body_break(keyword, body, text, newline, &mut edits);
    }

    edits
}

fn direct_keyword<'tree, Keyword: m2_syn::Token>(
    node: LayoutNode<'tree>,
) -> Option<LayoutNode<'tree>> {
    node.children()
        .find(|child| child.is_keyword_token() && matches_token::<Keyword>(child.text()))
}

fn push_control_clause_body_break(
    keyword: LayoutNode<'_>,
    body: LayoutNode<'_>,
    text: &str,
    newline: &'static str,
    edits: &mut Vec<FormatEdit>,
) {
    if body
        .child(0)
        .is_some_and(|child| child.is_opening_delimiter())
    {
        return;
    }
    push_control_flow_break(
        text,
        Some(keyword.end_byte()),
        body.start_byte(),
        newline,
        edits,
    );
}

fn keep_else_after_closer(clause: LayoutNode<'_>, text: &str, edits: &mut Vec<FormatEdit>) -> bool {
    let Some(body) = clause.parent().and_then(|owner| {
        owner
            .children()
            .filter(|candidate| {
                candidate.is::<ThenClause>() && candidate.end_byte() <= clause.start_byte()
            })
            .max_by_key(LayoutNode::end_byte)
            .and_then(|then_clause| then_clause.named_child(0))
    }) else {
        return false;
    };
    let children = body.children().collect::<Vec<_>>();
    let (Some(opener), Some(closer)) = (children.first(), children.last()) else {
        return false;
    };
    if !opener.is_opening_delimiter()
        || !closer.is_closing_delimiter()
        || !text[closer.end_byte()..clause.start_byte()]
            .bytes()
            .all(|byte| byte.is_ascii_whitespace())
    {
        return false;
    }
    if closer.end_position().row < clause.start_position().row {
        edits.push(FormatEdit {
            start_byte: closer.end_byte(),
            end_byte: clause.start_byte(),
            replacement: " ".to_string(),
        });
    }
    true
}

fn control_clause_breaks_are_parseable(clause: LayoutNode<'_>, text: &str) -> bool {
    let mut current = clause;
    let control = loop {
        let Some(parent) = current.parent() else {
            return true;
        };
        if parent.is::<IfStatement>()
            || parent.is::<TryStatement>()
            || parent.is::<ForLoop>()
            || parent.is::<WhileLoop>()
        {
            break parent;
        }
        current = parent;
    };

    let mut layout_control = control;
    while layout_control.is::<IfStatement>() && if_statement_is_else_if(layout_control) {
        let Some(outer_if) = layout_control
            .parent()
            .and_then(|else_clause| else_clause.parent())
            .filter(|parent| parent.is::<IfStatement>())
        else {
            break;
        };
        layout_control = outer_if;
    }

    if std::iter::successors(layout_control.parent(), |node| node.parent())
        .any(|node| node.is_delimited_expression())
    {
        return true;
    }

    let line_start = text[..layout_control.start_byte()]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    if !text[line_start..layout_control.start_byte()]
        .bytes()
        .all(|byte| matches!(byte, b' ' | b'\t'))
    {
        return false;
    }

    let has_alternative = layout_control
        .children()
        .any(|child| child.is::<ElseClause>() || child.is_except_clause());
    if !has_alternative {
        return true;
    }

    let mut ancestor = layout_control.parent();
    while let Some(node) = ancestor {
        if node.is_delimited_expression() {
            return true;
        }
        ancestor = node.parent();
    }
    false
}

fn push_control_flow_break(
    text: &str,
    start_byte: Option<usize>,
    end_byte: usize,
    newline: &'static str,
    edits: &mut Vec<FormatEdit>,
) {
    let Some(start_byte) = start_byte else {
        return;
    };
    if start_byte >= end_byte
        || !text[start_byte..end_byte]
            .bytes()
            .all(|byte| matches!(byte, b' ' | b'\t'))
    {
        return;
    }
    edits.push(FormatEdit {
        start_byte,
        end_byte,
        replacement: newline.to_string(),
    });
}

/// Edit indentation and trailing whitespace outside syntax-owned strings and
/// comments. Copy all other bytes, including literal contents and line endings.
fn reindent_from_tree(root: LayoutNode<'_>, options: &FormatOptions) -> Vec<FormatEdit> {
    let text = root.source_text();
    let layout = TreeIndentLayout::from_root(root, options.compact_factor_operators);
    let protected = root
        .descendants()
        .filter(|node| node.is_string_literal() || node.is_comment())
        .map(|node| node.start_byte()..node.end_byte())
        .collect::<Vec<_>>();
    let mut edits = Vec::new();
    for (row, line) in text_lines(text).into_iter().enumerate() {
        let content = &text[line.start_byte..line.end_byte];
        let leading = content.len() - content.trim_start_matches([' ', '\t']).len();
        let trailing = content.trim_end_matches([' ', '\t']).len();
        let leading_end = line.start_byte + leading;
        let intersects = |start, end| {
            protected.iter().any(|span| {
                // An insertion inside a token must also be protected.
                (start < span.end && span.start < end)
                    || (start == end && span.start < start && start < span.end)
            })
        };
        if !intersects(line.start_byte, leading_end) {
            edits.push(FormatEdit {
                start_byte: line.start_byte,
                end_byte: leading_end,
                replacement: if leading == content.len() {
                    String::new()
                } else {
                    options.indent.repeat(layout.depth(row))
                },
            });
        }
        let trailing_start = line.start_byte + trailing.max(leading);
        if trailing_start < line.end_byte && !intersects(trailing_start, line.end_byte) {
            edits.push(FormatEdit {
                start_byte: trailing_start,
                end_byte: line.end_byte,
                replacement: String::new(),
            });
        }
    }
    edits
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineBreakCandidate {
    start_byte: usize,
    end_byte: usize,
}

trait LineBreakRule {
    fn collect(
        &self,
        node: LayoutNode<'_>,
        text: &str,
        options: &FormatOptions,
        candidates: &mut Vec<LineBreakCandidate>,
    );
}

impl<A, B> LineBreakRule for (A, B)
where
    A: LineBreakRule,
    B: LineBreakRule,
{
    fn collect(
        &self,
        node: LayoutNode<'_>,
        text: &str,
        options: &FormatOptions,
        candidates: &mut Vec<LineBreakCandidate>,
    ) {
        self.0.collect(node, text, options, candidates);
        self.1.collect(node, text, options, candidates);
    }
}

#[derive(Debug, Clone, Copy)]
struct DelimiterBreakRule;

impl LineBreakRule for DelimiterBreakRule {
    fn collect(
        &self,
        node: LayoutNode<'_>,
        text: &str,
        _options: &FormatOptions,
        candidates: &mut Vec<LineBreakCandidate>,
    ) {
        if !node.is_opening_delimiter() {
            return;
        }
        let has_contents = node
            .parent()
            .is_some_and(|parent| parent.named_children().next().is_some());
        if has_contents {
            push_line_break_candidate_after(node, text, candidates);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CommaBreakRule;

impl LineBreakRule for CommaBreakRule {
    fn collect(
        &self,
        node: LayoutNode<'_>,
        text: &str,
        _options: &FormatOptions,
        candidates: &mut Vec<LineBreakCandidate>,
    ) {
        if node.is_comma() && !node.comma_borders_empty_slot() {
            push_line_break_candidate_after(node, text, candidates);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct OperatorBreakRule;

impl LineBreakRule for OperatorBreakRule {
    fn collect(
        &self,
        node: LayoutNode<'_>,
        text: &str,
        options: &FormatOptions,
        candidates: &mut Vec<LineBreakCandidate>,
    ) {
        let Some(parent) = node.parent() else {
            return;
        };
        if !node.is_operator() {
            return;
        }
        if breaks_before_operator(node, options.compact_factor_operators) {
            push_line_break_candidate_before(node, text, candidates);
        } else if (parent.binary_operator().is_some() || parent.is::<LambdaExpression>())
            && is_spaced_line_final_operator(node.text(), options.compact_factor_operators)
        {
            push_line_break_candidate_after(node, text, candidates);
        }
    }
}

fn push_line_break_candidate_before(
    node: LayoutNode<'_>,
    text: &str,
    candidates: &mut Vec<LineBreakCandidate>,
) {
    let end_byte = node.start_byte();
    let Some(start_byte) = same_line_horizontal_whitespace_start(text, end_byte) else {
        return;
    };
    candidates.push(LineBreakCandidate {
        start_byte,
        end_byte,
    });
}

fn push_line_break_candidate_after(
    node: LayoutNode<'_>,
    text: &str,
    candidates: &mut Vec<LineBreakCandidate>,
) {
    let start_byte = node.end_byte();
    let Some(end_byte) = same_line_horizontal_whitespace_end(text, start_byte) else {
        return;
    };
    candidates.push(LineBreakCandidate {
        start_byte,
        end_byte,
    });
}

#[derive(Debug, Clone, Copy)]
struct TextLine {
    start_byte: usize,
    end_byte: usize,
}

fn wrap_long_lines(
    root: LayoutNode<'_>,
    options: &FormatOptions,
    newline: &'static str,
) -> Vec<FormatEdit> {
    let text = root.source_text();
    let Some(max_line_width) = options.max_line_width else {
        return Vec::new();
    };
    let soft_line_width = options
        .soft_line_width
        .unwrap_or(max_line_width)
        .min(max_line_width);
    let rules = (DelimiterBreakRule, (CommaBreakRule, OperatorBreakRule));
    let candidates = collect_line_break_candidates(root, options, &rules);
    select_line_breaks(
        text,
        &candidates,
        soft_line_width,
        max_line_width,
        options.tab_width,
        newline,
    )
}

fn collect_line_break_candidates(
    root: LayoutNode<'_>,
    options: &FormatOptions,
    rules: &(impl LineBreakRule + ?Sized),
) -> Vec<LineBreakCandidate> {
    let text = root.source_text();
    let mut candidates = Vec::new();
    for node in root.descendants() {
        rules.collect(node, text, options, &mut candidates);
    }
    candidates.sort_by_key(|candidate| (candidate.start_byte, candidate.end_byte));
    candidates.dedup();
    candidates
}

fn select_line_breaks(
    text: &str,
    candidates: &[LineBreakCandidate],
    soft_line_width: usize,
    max_line_width: usize,
    tab_width: usize,
    newline: &'static str,
) -> Vec<FormatEdit> {
    text_lines(text)
        .into_iter()
        .filter(|line| {
            visual_width(&text[line.start_byte..line.end_byte], tab_width) > max_line_width
        })
        .filter_map(|line| {
            let candidates_on_line = candidates
                .iter()
                .filter(|candidate| {
                    line.start_byte < candidate.start_byte && candidate.end_byte < line.end_byte
                })
                .collect::<Vec<_>>();
            let candidate = candidates_on_line
                .iter()
                .copied()
                .rfind(|candidate| {
                    visual_width(&text[line.start_byte..candidate.start_byte], tab_width)
                        <= soft_line_width
                })
                .or_else(|| {
                    candidates_on_line.iter().copied().rfind(|candidate| {
                        visual_width(&text[line.start_byte..candidate.start_byte], tab_width)
                            <= max_line_width
                    })
                })
                .or_else(|| candidates_on_line.first().copied())?;

            Some(FormatEdit {
                start_byte: candidate.start_byte,
                end_byte: candidate.end_byte,
                replacement: newline.to_string(),
            })
        })
        .collect()
}

fn text_lines(text: &str) -> Vec<TextLine> {
    let mut offset = 0;
    text.split_inclusive('\n')
        .map(|line| {
            let content = line.strip_suffix('\n').unwrap_or(line);
            let content = content.strip_suffix('\r').unwrap_or(content);
            let span = TextLine {
                start_byte: offset,
                end_byte: offset + content.len(),
            };
            offset += line.len();
            span
        })
        .collect()
}

fn visual_width(text: &str, tab_width: usize) -> usize {
    text.chars().fold(0, |column, character| {
        if character == '\t' {
            column + tab_width - column % tab_width
        } else {
            column + 1
        }
    })
}

/// The document's fold ranges: tree-based indentation blocks plus multiline
/// comments and strings.
fn folding_ranges_for_syntax(root: SyntaxNode<'_>) -> Vec<FormatFoldRange> {
    let source = LayoutSource::new(root);
    let root = LayoutNode::new(root, &source);
    let text = root.source_text();
    let layout = TreeIndentLayout::from_root(root, false);
    let indented_lines = text
        .lines()
        .enumerate()
        .filter_map(|(row, line)| {
            (!line.trim().is_empty()).then_some(IndentedLine {
                line: row as u32,
                depth: layout.depth(row),
                is_leading_closer: layout.is_leading_closer(row),
            })
        })
        .collect::<Vec<_>>();

    let mut ranges = collect_indent_fold_ranges(&indented_lines);
    ranges.extend(layout.literal_folds);
    ranges.sort_by_key(|range| (range.start_line, range.end_line, range.kind));
    ranges.dedup();
    ranges
}

/// Per-line indentation depths derived from the existing syntax and its
/// current layout, plus the comment and string ranges that can be folded.
/// `depth(row) = bracket_base(row) + continuation(row)`, where a row inside
/// multiline brackets sits one level deeper than the row its innermost bracket
/// opened on, and a row beginning with that bracket's closer sits at the
/// opener row's depth.
struct TreeIndentLayout {
    depths: Vec<usize>,
    literal_folds: Vec<FormatFoldRange>,
    leading_closer_rows: Vec<bool>,
}

impl TreeIndentLayout {
    fn from_root(root: LayoutNode<'_>, compact_factor_operators: bool) -> Self {
        let text = root.source_text();
        let line_count = text.lines().count().max(1);
        let brackets = collect_bracket_groups(root, line_count);
        let line_leads = line_leading_blank(text, line_count);
        let literal_folds = collect_literal_fold_ranges(root, &line_leads);
        let leading_closer_rows = (0..line_count)
            .map(|row| {
                brackets.iter().any(|group| {
                    group.close_row == row
                        && line_leads.get(row).copied() == Some(group.closing_delimiter_column)
                })
            })
            .collect();

        let mut depths: Vec<usize> = Vec::with_capacity(line_count);
        for row in 0..line_count {
            let bracket_base = innermost_open_bracket_row(row, &brackets).map_or(0, |open_row| {
                let opener_depth = depths[open_row];
                if closes_at_line_start(row, open_row, &brackets, &line_leads) {
                    opener_depth
                } else {
                    opener_depth + 1
                }
            });
            depths.push(
                bracket_base + line_continuation(row, root, &line_leads, compact_factor_operators),
            );
        }

        Self {
            depths,
            literal_folds,
            leading_closer_rows,
        }
    }

    fn depth(&self, row: usize) -> usize {
        self.depths.get(row).copied().unwrap_or(0)
    }

    fn is_leading_closer(&self, row: usize) -> bool {
        self.leading_closer_rows.get(row).copied().unwrap_or(false)
    }
}

/// Build folds from parser-classified comments and string literals.
/// Consecutive full-line `--` comments become one range, while each multiline
/// block comment and each multiline string (`"..."` or `///...///`) is its own
/// range. Keeping this tree-derived avoids mistaking comment markers inside
/// strings for comments, and string delimiters inside comments for strings.
fn collect_literal_fold_ranges(root: LayoutNode<'_>, line_leads: &[usize]) -> Vec<FormatFoldRange> {
    let mut line_comment_rows = Vec::new();
    let mut ranges = Vec::new();

    for node in root.descendants() {
        let start = node.start_position();
        let end = node.end_position();
        if node.is_line_comment() && line_leads.get(start.row).copied() == Some(start.column) {
            line_comment_rows.extend((start.row..=end.row).map(|row| row as u32));
        } else if node.is_block_comment() && start.row < end.row {
            ranges.push(FormatFoldRange {
                start_line: start.row as u32,
                end_line: end.row as u32,
                kind: FormatFoldKind::Comment,
            });
        } else if node.is_string_literal() && start.row < end.row {
            ranges.push(FormatFoldRange {
                start_line: start.row as u32,
                end_line: end.row as u32,
                kind: FormatFoldKind::Region,
            });
        }
    }

    line_comment_rows.sort_unstable();
    line_comment_rows.dedup();
    let mut block_start = None;
    let mut previous = None;
    for row in line_comment_rows {
        if previous.is_none_or(|previous_row| row != previous_row + 1) {
            append_line_comment_fold(&mut ranges, block_start, previous);
            block_start = Some(row);
        }
        previous = Some(row);
    }
    append_line_comment_fold(&mut ranges, block_start, previous);

    ranges
}

fn append_line_comment_fold(
    ranges: &mut Vec<FormatFoldRange>,
    start_line: Option<u32>,
    end_line: Option<u32>,
) {
    let (Some(start_line), Some(end_line)) = (start_line, end_line) else {
        return;
    };
    if start_line == end_line {
        return;
    }
    ranges.push(FormatFoldRange {
        start_line,
        end_line,
        kind: FormatFoldKind::Comment,
    });
}

/// A multiline bracket node, keyed by the row it opens on. Brackets that open on
/// the same row collapse to one indent level, so the `open_row` is the group id.
#[derive(Debug, Clone, Copy)]
struct BracketGroup {
    open_row: usize,
    close_row: usize,
    closing_delimiter_column: usize,
}

/// Collect every multiline bracket node (`(…)`, `{…}`, `[…]`, `<|…|>`) spanning
/// more than one row, keyed by open/close row for the depth computation. The
/// closer's width (`)` is 1, `|>` is 2) does not matter; only its start column.
fn collect_bracket_groups(root: LayoutNode<'_>, line_count: usize) -> Vec<BracketGroup> {
    let mut groups = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        // A multiline parenthesized block `(\n …\n)` indents its body like a
        // collection, though it is not a collection value.
        if node.is_delimited_expression() {
            let open_row = node.start_position().row;
            let close_position = node.end_position();
            if open_row < close_position.row {
                groups.push(BracketGroup {
                    open_row,
                    close_row: close_position.row,
                    closing_delimiter_column: close_position
                        .column
                        .saturating_sub(node.closing_delimiter_width()),
                });
            }
        }
        if node.is_error() {
            collect_unclosed_error_brackets(node, line_count, &mut groups);
        }
        stack.extend(node.children());
    }
    groups
}

/// Collect unmatched opener tokens retained inside an invalid syntax region.
/// Each unmatched opener still indents its
/// body, so it opens a group from its row to the last line of the document.
fn collect_unclosed_error_brackets(
    error: LayoutNode<'_>,
    line_count: usize,
    groups: &mut Vec<BracketGroup>,
) {
    let mut open_rows = Vec::new();
    for child in error.children() {
        if child.is_opening_delimiter() {
            open_rows.push(child.start_position().row);
        } else if child.is_closing_delimiter() {
            open_rows.pop();
        }
    }
    let last_row = line_count.saturating_sub(1);
    for open_row in open_rows {
        if open_row < last_row {
            groups.push(BracketGroup {
                open_row,
                close_row: last_row,
                closing_delimiter_column: usize::MAX,
            });
        }
    }
}

/// The leading-whitespace length (in bytes) of each line, used purely as layout
/// to test whether a closer is the first non-whitespace token on its line.
fn line_leading_blank(text: &str, line_count: usize) -> Vec<usize> {
    let mut leads = vec![0; line_count];
    for (row, line) in text.lines().enumerate() {
        if let Some(slot) = leads.get_mut(row) {
            *slot = line.len() - line.trim_start().len();
        }
    }
    leads
}

/// The row the innermost multiline bracket open across `row` opened on.
/// Brackets opening on the same row form one indent level, so the row
/// identifies it.
fn innermost_open_bracket_row(row: usize, brackets: &[BracketGroup]) -> Option<usize> {
    brackets
        .iter()
        .filter(|group| group.open_row < row && row <= group.close_row)
        .map(|group| group.open_row)
        .max()
}

/// Whether `row` begins with the closer of a bracket opened on `open_row`,
/// which then dedents back to the opener's level.
fn closes_at_line_start(
    row: usize,
    open_row: usize,
    brackets: &[BracketGroup],
    line_leads: &[usize],
) -> bool {
    let leading_blank = line_leads.get(row).copied().unwrap_or(0);
    brackets.iter().any(|group| {
        group.open_row == open_row
            && group.close_row == row
            && group.closing_delimiter_column == leading_blank
    })
}

/// Continuations add one level; bodies of inline conditionals add another
/// level so they sit deeper than the conditional's clause keywords.
fn line_continuation(
    row: usize,
    root: LayoutNode<'_>,
    line_leads: &[usize],
    compact_factor_operators: bool,
) -> usize {
    let Some(first) = first_leaf_on_row(root, row) else {
        return 0;
    };

    // (a) The first token is the start of the right operand of an infix
    // expression whose operator dangled on an earlier row (`a +\nb`,
    // `x ->\nbody`).
    if is_right_operand_first_token(first, row, compact_factor_operators) {
        return 1;
    }

    // (a') The first token is a binary operator leading the row, its left
    // operand on an earlier row (`(a\n+ b)`).
    if first.is_operator()
        && first
            .parent()
            .and_then(|parent| parent.binary_parts())
            .is_some_and(|parts| parts.left.start_position().row < row)
    {
        return 1;
    }

    // (b) The first token starts a clause body whose keyword is on an earlier row
    // (`then\n  f(x)`, `else\n  c`, `do\n  body`).
    if is_clause_body_first_token(first, row) {
        let inline_conditional = enclosing_clause_owner(first).is_some_and(|owner| {
            !(control_statement_is_standalone(owner, line_leads)
                || owner.is::<IfStatement>() && if_statement_is_else_if(owner))
        });
        return 1 + usize::from(inline_conditional);
    }

    // (c) The first token is an `else`/`then` keyword whose controlling `if` is
    // not standalone (a ternary `x := if … then\n … else …`); a standalone `if`
    // aligns its `else` through its bracket instead.
    if is_dangling_clause_keyword(first, row, line_leads) {
        return 1;
    }

    0
}

/// The leftmost leaf node starting on `row`.
fn first_leaf_on_row(root: LayoutNode<'_>, row: usize) -> Option<LayoutNode<'_>> {
    let mut best: Option<LayoutNode<'_>> = None;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.start_position().row > row || node.end_position().row < row {
            continue;
        }
        let is_leaf = node.child(0).is_none();
        if is_leaf && node.start_position().row == row && node.start_byte() < node.end_byte() {
            best = match best {
                Some(current) if current.start_byte() <= node.start_byte() => Some(current),
                _ => Some(node),
            };
        }
        stack.extend(node.children());
    }
    best
}

/// Whether `node` is the first token of the right operand of an infix expression
/// whose operator dangled on a row before `row`. Only spaced operators carry a
/// continuation: a compact operator like `*` left at line end (`a*\nb`) does not
/// indent its continuation, matching the line-final-operator spacing pass.
fn is_right_operand_first_token(
    node: LayoutNode<'_>,
    row: usize,
    compact_factor_operators: bool,
) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        let right = parent
            .binary_parts()
            .map(|parts| parts.right)
            .or_else(|| parent.lambda_body());
        if let Some((operator, right)) = parent.operator_node().zip(right) {
            let assignment_conditional = parent.is_assignment() && right.is::<IfStatement>();
            // Chained assignments share a value, so their heads align rather
            // than looking like successively nested bodies.
            let assignment_chain = parent.is_assignment() && right.is_assignment();
            if right.start_byte() == node.start_byte()
                && operator.start_position().row < row
                && is_spaced_line_final_operator(operator.text(), compact_factor_operators)
                && !assignment_conditional
                && !assignment_chain
            {
                return true;
            }
        }
        current = parent;
    }
    false
}

/// Whether `node` is the first token of a clause body (`then`/`else`/`do`/`list`)
/// whose clause keyword is on a row before `row`.
fn is_clause_body_first_token(node: LayoutNode<'_>, row: usize) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.is::<ThenClause>() || parent.is::<ElseClause>() {
            if let (Some(keyword), Some(body)) = (parent.child(0), parent.named_child(0)) {
                if body.start_byte() == node.start_byte() && keyword.start_position().row < row {
                    return true;
                }
            }
        }
        if parent.is::<LoopBody>() {
            for (keyword, body) in [
                (
                    direct_keyword::<Token![list]>(parent),
                    parent.listed_value(),
                ),
                (direct_keyword::<Token![do]>(parent), parent.ignored_value()),
            ] {
                let Some((keyword, body)) = keyword.zip(body) else {
                    continue;
                };
                if body.start_byte() == node.start_byte() && keyword.start_position().row < row {
                    return true;
                }
            }
        }
        if parent.is_except_clause() {
            let clause_body = direct_keyword::<Token![do]>(parent).zip(parent.clause_value());
            if let Some((keyword, body)) = clause_body {
                if body.start_byte() == node.start_byte() && keyword.start_position().row < row {
                    return true;
                }
            }
        }
        current = parent;
    }
    false
}

/// Whether `node` is an `else`/`then` clause keyword beginning `row` that
/// continues a non-standalone conditional expression. Two parse shapes occur:
///   * A proper clause keyword inside an `if` or `try`: a continuation only when
///     its owner is not standalone (a standalone owner aligns the clause through
///     bracket depth instead).
///   * A retained clause keyword without an enclosing conditional owner:
///     a continuation of the expression begun on an earlier line.
fn is_dangling_clause_keyword(node: LayoutNode<'_>, row: usize, line_leads: &[usize]) -> bool {
    if !is_clause_keyword_leaf(node) {
        return false;
    }
    if node.start_position().row != row {
        return false;
    }
    match enclosing_clause_owner(node) {
        Some(owner) => {
            !(control_statement_is_standalone(owner, line_leads)
                || owner.is::<IfStatement>() && if_statement_is_else_if(owner))
        }
        None => true,
    }
}

/// Whether a leaf is an `else`/`then` clause keyword. Normally these are the
/// keyword token kinds, but when a line-broken ternary is parsed the keyword is
/// demoted to a bare `symbol`; since `else`/`then` are reserved words no real
/// identifier carries that text, so a symbol spelled so is the misparsed keyword.
fn is_clause_keyword_leaf(node: LayoutNode<'_>) -> bool {
    if node.is_then_or_else_keyword() {
        return true;
    }
    node.is::<Symbol>()
        && (matches_token::<Token![else]>(node.text())
            || matches_token::<Token![then]>(node.text()))
}

/// The nearest enclosing conditional owner of a clause keyword.
fn enclosing_clause_owner(node: LayoutNode<'_>) -> Option<LayoutNode<'_>> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.is::<IfStatement>() || parent.is::<TryStatement>() {
            return Some(parent);
        }
        current = parent;
    }
    None
}

/// Whether a control statement begins its own line.
fn control_statement_is_standalone(node: LayoutNode<'_>, line_leads: &[usize]) -> bool {
    let row = node.start_position().row;
    let column = node.start_position().column;
    line_leads.get(row).copied().unwrap_or(0) >= column
}

fn if_statement_is_else_if(node: LayoutNode<'_>) -> bool {
    let Some(parent) = node.parent().filter(|parent| parent.is::<ElseClause>()) else {
        return false;
    };
    parent
        .named_child(0)
        .is_some_and(|body| body.id() == node.id())
        && parent
            .child(0)
            .is_some_and(|keyword| keyword.start_position().row == node.start_position().row)
}

/// A foldable block of consecutive lines (0-based, both bounds inclusive as
/// produced; the LSP conversion maps them onto `FoldingRange`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatFoldRange {
    pub start_line: u32,
    pub end_line: u32,
    pub kind: FormatFoldKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FormatFoldKind {
    Region,
    Comment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IndentedLine {
    line: u32,
    depth: usize,
    is_leading_closer: bool,
}

#[derive(Debug, Clone, Copy)]
struct OpenFoldRange {
    start_line: u32,
    depth: usize,
}

fn collect_indent_fold_ranges(lines: &[IndentedLine]) -> Vec<FormatFoldRange> {
    let mut ranges = Vec::new();
    let mut open_folds: Vec<OpenFoldRange> = Vec::new();
    let mut previous: Option<IndentedLine> = None;

    for &current in lines {
        if let Some(prev) = previous {
            if current.depth > prev.depth {
                open_folds.push(OpenFoldRange {
                    start_line: prev.line,
                    depth: prev.depth,
                });
            } else if current.depth < prev.depth {
                let fold_end = current.is_leading_closer.then_some(current).or(previous);
                while let Some(open_fold) = open_folds.last() {
                    if open_fold.depth >= current.depth {
                        close_fold_range(&mut ranges, *open_fold, fold_end);
                        open_folds.pop();
                    } else {
                        break;
                    }
                }
            }
        }
        previous = Some(current);
    }

    while let Some(open_fold) = open_folds.pop() {
        close_fold_range(&mut ranges, open_fold, previous);
    }

    ranges
}

fn close_fold_range(
    ranges: &mut Vec<FormatFoldRange>,
    range: OpenFoldRange,
    previous: Option<IndentedLine>,
) {
    let Some(previous) = previous else {
        return;
    };
    if previous.line <= range.start_line {
        return;
    }

    ranges.push(FormatFoldRange {
        start_line: range.start_line,
        end_line: previous.line,
        kind: FormatFoldKind::Region,
    });
}

fn normalize_whitespace(root: LayoutNode<'_>, options: &FormatOptions) -> Vec<FormatEdit> {
    let text = root.source_text();

    let mut edits = Vec::new();
    collect_format_edits(root, text, options, &mut edits);
    edits
}

fn collect_format_edits(
    node: LayoutNode<'_>,
    text: &str,
    options: &FormatOptions,
    edits: &mut Vec<FormatEdit>,
) {
    if node.is_missing() {
        return;
    }

    if !node.is_error() {
        if let Some(parts) = node.method_installation_head() {
            let start = parts.left.end_byte();
            let end = parts.right.start_byte();
            if text[start..end].bytes().all(|b| matches!(b, b' ' | b'\t')) {
                edits.push(FormatEdit {
                    start_byte: start,
                    end_byte: end,
                    replacement: " ".to_string(),
                });
            }
        }

        if node.is_comma() {
            push_comma_whitespace_edits(text, node, edits);
        }

        if node.is_semicolon() {
            let break_after_semicolon =
                options.break_after_semicolon && !semicolon_terminates_collection_entry(node);
            push_semicolon_whitespace_edits(text, node, break_after_semicolon, edits);
        }

        if let Some(operator) = node.operator_node() {
            if is_parenthesized_call(node) {
                // A call `f(...)` that is the head of a `:=` install reads as
                // installation syntax, so it is spaced (`f (Types) := …`); an
                // ordinary call is compacted (`f(x)`).
                if is_method_installation_call_head(node) {
                    push_call_gap_whitespace_edit(node, text, edits, " ");
                } else {
                    push_call_gap_whitespace_edit(node, text, edits, "");
                }
            } else {
                match operator_spacing(node, operator.text()) {
                    OperatorSpacing::Spaced => {
                        push_operator_whitespace_edits(text, operator, edits)
                    }
                    OperatorSpacing::Compact => {
                        push_compact_operator_whitespace_edits(text, operator, edits)
                    }
                    OperatorSpacing::Factor => {
                        if options.compact_factor_operators && binary_operator_all_factors(node) {
                            push_compact_operator_whitespace_edits(text, operator, edits);
                        } else {
                            push_operator_whitespace_edits(text, operator, edits);
                        }
                    }
                    OperatorSpacing::Prefix => {
                        push_prefix_operator_whitespace_edits(text, operator, edits, "")
                    }
                    OperatorSpacing::SpacedPrefix => {
                        push_prefix_operator_whitespace_edits(text, operator, edits, " ")
                    }
                    OperatorSpacing::None => {}
                }
            }
        }
    }

    for child in node.children() {
        collect_format_edits(child, text, options, edits);
    }
}

/// Per-operator spacing rule. Single source of truth consumed by the spacing
/// walk (`collect_format_edits`) and the indent continuation check
/// (`is_right_operand_first_token`). Folding the previously separate
/// `should_space_*`/`is_compact_*`/`is_spaced_line_final_*` tables into one
/// place removes the drift risk where the spaced and line-final tables forgot
/// to track each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperatorSpacing {
    /// Always surround with a space on each side (`a + b`, `x := y`).
    Spaced,
    /// Always collapse adjacent whitespace (`x^y`, `a#b`).
    Compact,
    /// Factor operator (`*`, `/`, `%`, `**`): compact when both operands are
    /// adjacent factors (`a*b*c`), spaced when a non-factor or continuation
    /// needs air (`a * f(b)`).
    Factor,
    /// Unary prefix form (`-x`, `#x`): collapse only the trailing space.
    Prefix,
    /// Unary prefix form read as a command (`<< x`): exactly one trailing space.
    SpacedPrefix,
    /// Not a spacing-relevant operator; leave its surrounding whitespace as-is.
    None,
}

macro_rules! matches_tokens {
    ($text:expr, $($token:ty),+ $(,)?) => {
        false $(|| matches_token::<$token>($text))+
    };
}

/// Spacing rule for the operator `node` carries, keyed by the parent's kind so
/// the same spelling can route differently in binary vs prefix context
/// (`-` in `BinaryExpr` is `Spaced`, in `PrefixExpr` is `Prefix`).
/// `LambdaExpression` shares the binary table: its `->` operator reads as a
/// spaced binary operator.
fn operator_spacing(parent: LayoutNode<'_>, operator: &str) -> OperatorSpacing {
    if parent.binary_operator().is_some() || parent.is::<LambdaExpression>() {
        binary_operator_spacing(operator)
    } else if parent.is_prefix_expr() {
        prefix_operator_spacing(operator)
    } else {
        OperatorSpacing::None
    }
}

fn binary_operator_spacing(operator: &str) -> OperatorSpacing {
    // Factor operators participate in the adjacency rule before the static
    // spaced/compact tables.
    if matches_tokens!(operator, Token![*], Token![/], Token![%], Token![**]) {
        return OperatorSpacing::Factor;
    }
    if matches_tokens!(
        operator,
        Token![==],
        Token![!=],
        Token![===],
        Token![=!=],
        Token![<<],
        Token![<],
        Token![>],
        Token![<=],
        Token![>=],
        Token![or],
        Token![??],
        Token![xor],
        Token![and],
        Token![||],
        Token![|],
        Token![^^],
        Token![&],
        Token![++],
        Token![+],
        Token![-],
        Token!["\\"],
        Token!["\\\\"],
        Token![:=],
        Token![=],
        Token![<-],
        Token![=>],
        Token![->],
        Token![/ /],
    ) {
        return OperatorSpacing::Spaced;
    }
    if matches_tokens!(
        operator,
        Token!["·"],
        Token!["⊠"],
        Token!["⧢"],
        Token![@],
        Token![@@],
        Token![@@?],
        Token![| _],
        Token![^],
        Token![^**],
        Token![^<],
        Token![^<=],
        Token![^>],
        Token![^>=],
        Token![_],
        Token![_ <],
        Token![_ <=],
        Token![_ >],
        Token![_ >=],
        Token![#],
        Token![#?],
    ) {
        return OperatorSpacing::Compact;
    }
    OperatorSpacing::None
}

fn prefix_operator_spacing(operator: &str) -> OperatorSpacing {
    if matches_token::<Token![<<]>(operator) {
        return OperatorSpacing::SpacedPrefix;
    }
    if matches_tokens!(
        operator,
        Token![+],
        Token![-],
        Token![*],
        Token![#],
        Token![<],
        Token![<=],
        Token![>],
        Token![>=],
        Token![?],
        Token![|-],
        Token![<===],
        Token![<==],
        Token![??],
    ) {
        return OperatorSpacing::Prefix;
    }
    OperatorSpacing::None
}

/// Whether a `Factor`-classified binary operator collapses to compact form:
/// `true` iff both operands are adjacent factors (atoms or sub-expressions),
/// in which case spacing (`a * b`) would visually break a tight product such as
/// `2*x*y`. Otherwise a space is required to separate the operands.
fn binary_operator_all_factors(node: LayoutNode<'_>) -> bool {
    let Some(left) = node.binary_parts().map(|parts| parts.left) else {
        return false;
    };
    let Some(right) = node.binary_parts().map(|parts| parts.right) else {
        return false;
    };
    is_adjacent_factor(left) && is_adjacent_factor(right)
}

/// Whether `operator`, when it dangles at the end of a line, takes a trailing
/// space and so signals that the following row is an indented continuation.
/// Used by the tree-driven indenter to decide right-operand indentation. The
/// line-final set is exactly the binary operators classified `Spaced` above
/// (Factor operators are excluded: a compact `*` left at line-end `a*\nb` does
/// not indent). Derived from `binary_operator_spacing` so a new spaced operator
/// cannot be added to one table and forgotten in the other.
fn is_spaced_line_final_operator(operator: &str, compact_factor_operators: bool) -> bool {
    match binary_operator_spacing(operator) {
        OperatorSpacing::Spaced => true,
        OperatorSpacing::Factor => !compact_factor_operators,
        _ => false,
    }
}

/// Whether a line break beside the binary `operator` goes before it, so the
/// operator starts the continuation row (`a\n+ b`) rather than ending the
/// previous one. Macaulay2 ends an expression at a newline whenever the text
/// so far is complete, so a leading operator only continues the expression
/// inside brackets, where a newline is always whitespace; elsewhere the break
/// stays after the operator. Bindings (`:=`, `=`, `=>`) keep their value on
/// the next row and so always end the line.
fn breaks_before_operator(operator: LayoutNode<'_>, compact_factor_operators: bool) -> bool {
    let Some(expression) = operator.parent() else {
        return false;
    };
    expression.binary_operator().is_some()
        && !expression.is_assignment()
        && !expression.is_option_assignment()
        && is_spaced_line_final_operator(operator.text(), compact_factor_operators)
        && std::iter::successors(expression.parent(), |node| node.parent())
            .any(|node| node.is_delimited_expression())
}

fn is_parenthesized_call(node: LayoutNode<'_>) -> bool {
    if node.binary_operator().is_none() {
        return false;
    }

    let Some(operator) = node.operator_node() else {
        return false;
    };
    let Some(right) = node.binary_parts().map(|parts| parts.right) else {
        return false;
    };

    // A call's argument list is a `sequence` (`f(a, b)`, `f()`) or, for a single
    // parenthesized argument, a `parenthesized_expression` (`f(x)`).
    operator.is_implicit_application() && (right.is::<Sequence>() || right.is_holder())
}

/// Whether a parenthesized call `f(...)` is the head of a `:=` method install
/// (`f(Types) := fn`) — i.e. it is the left operand of an enclosing `:=`. Such a
/// head is installation syntax and is spaced rather than compacted.
fn is_method_installation_call_head(node: LayoutNode<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.binary_operator().is_none() {
        return false;
    }
    let Some(operator) = parent.operator_node() else {
        return false;
    };
    if !operator.is::<Token![:=]>() {
        return false;
    }
    parent
        .binary_parts()
        .map(|parts| parts.left)
        .is_some_and(|left| {
            left.start_byte() == node.start_byte() && left.end_byte() == node.end_byte()
        })
}

fn push_call_gap_whitespace_edit(
    node: LayoutNode<'_>,
    text: &str,
    edits: &mut Vec<FormatEdit>,
    replacement: &'static str,
) {
    let Some(left) = node.binary_parts().map(|parts| parts.left) else {
        return;
    };
    let Some(right) = node.binary_parts().map(|parts| parts.right) else {
        return;
    };
    let gap = &text[left.end_byte()..right.start_byte()];
    if !gap.bytes().all(|byte| matches!(byte, b' ' | b'\t')) {
        return;
    }
    edits.push(FormatEdit {
        start_byte: left.end_byte(),
        end_byte: right.start_byte(),
        replacement: replacement.to_string(),
    });
}

fn is_adjacent_factor(node: LayoutNode<'_>) -> bool {
    node.is::<Symbol>()
        || node.is::<IntegerLiteral>()
        || node.is::<FloatLiteral>()
        || node.is::<StringLiteral>()
        || node.is::<RawStringLiteral>()
        || node.is_delimited_expression()
        || node.is_prefix_expr()
        || node.is_postfix_expr()
        || node.binary_operator().is_some()
}

fn push_operator_whitespace_edits(
    text: &str,
    operator: LayoutNode<'_>,
    edits: &mut Vec<FormatEdit>,
) {
    if let Some(start_byte) = same_line_horizontal_whitespace_start(text, operator.start_byte()) {
        edits.push(FormatEdit {
            start_byte,
            end_byte: operator.start_byte(),
            replacement: " ".to_string(),
        });
    }
    if let Some(end_byte) = same_line_horizontal_whitespace_end(text, operator.end_byte()) {
        edits.push(FormatEdit {
            start_byte: operator.end_byte(),
            end_byte,
            replacement: " ".to_string(),
        });
    }
}

fn push_compact_operator_whitespace_edits(
    text: &str,
    operator: LayoutNode<'_>,
    edits: &mut Vec<FormatEdit>,
) {
    let Some(start_byte) = same_line_horizontal_whitespace_start(text, operator.start_byte())
    else {
        return;
    };
    let Some(end_byte) = same_line_horizontal_whitespace_end(text, operator.end_byte()) else {
        return;
    };

    edits.push(FormatEdit {
        start_byte,
        end_byte: operator.start_byte(),
        replacement: String::new(),
    });
    edits.push(FormatEdit {
        start_byte: operator.end_byte(),
        end_byte,
        replacement: String::new(),
    });
}

fn push_prefix_operator_whitespace_edits(
    text: &str,
    operator: LayoutNode<'_>,
    edits: &mut Vec<FormatEdit>,
    separator: &str,
) {
    let Some(end_byte) = same_line_horizontal_whitespace_end(text, operator.end_byte()) else {
        return;
    };

    edits.push(FormatEdit {
        start_byte: operator.end_byte(),
        end_byte,
        replacement: separator.to_string(),
    });
}

fn push_comma_whitespace_edits(text: &str, comma: LayoutNode<'_>, edits: &mut Vec<FormatEdit>) {
    if let Some(start_byte) = same_line_horizontal_whitespace_start(text, comma.start_byte()) {
        edits.push(FormatEdit {
            start_byte,
            end_byte: comma.start_byte(),
            replacement: String::new(),
        });
    }

    let Some(end_byte) = same_line_horizontal_whitespace_end(text, comma.end_byte()) else {
        return;
    };
    let replacement = if comma.comma_borders_empty_slot() {
        ""
    } else {
        match text.as_bytes().get(end_byte) {
            Some(b')' | b']' | b'}' | b';') | None => "",
            _ => " ",
        }
    };

    edits.push(FormatEdit {
        start_byte: comma.end_byte(),
        end_byte,
        replacement: replacement.to_string(),
    });
}

fn semicolon_terminates_collection_entry(semicolon: LayoutNode<'_>) -> bool {
    semicolon
        .parent()
        .and_then(|muted| muted.parent())
        .is_some_and(|container| container.is_collection_expression())
}

fn push_semicolon_whitespace_edits(
    text: &str,
    semicolon: LayoutNode<'_>,
    break_after_semicolon: bool,
    edits: &mut Vec<FormatEdit>,
) {
    if let Some(start_byte) = same_line_horizontal_whitespace_start(text, semicolon.start_byte()) {
        edits.push(FormatEdit {
            start_byte,
            end_byte: semicolon.start_byte(),
            replacement: String::new(),
        });
    }

    let Some(end_byte) = same_line_horizontal_whitespace_end(text, semicolon.end_byte()) else {
        return;
    };
    let Some(next_byte) = text.as_bytes().get(end_byte) else {
        return;
    };
    if !matches!(next_byte, b'\n' | b'\r') {
        let replacement = if break_after_semicolon {
            "\n"
        } else if matches!(next_byte, b')' | b']' | b'}') {
            ""
        } else {
            " "
        };
        edits.push(FormatEdit {
            start_byte: semicolon.end_byte(),
            end_byte,
            replacement: replacement.to_string(),
        });
    }
}

fn same_line_horizontal_whitespace_start(text: &str, byte_index: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut start_byte = byte_index;
    while start_byte > 0 && matches!(bytes[start_byte - 1], b' ' | b'\t') {
        start_byte -= 1;
    }

    (start_byte > 0 && !matches!(bytes[start_byte - 1], b'\n' | b'\r')).then_some(start_byte)
}

fn same_line_horizontal_whitespace_end(text: &str, byte_index: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut end_byte = byte_index;
    while end_byte < bytes.len() && matches!(bytes[end_byte], b' ' | b'\t') {
        end_byte += 1;
    }

    (end_byte < bytes.len() && !matches!(bytes[end_byte], b'\n' | b'\r')).then_some(end_byte)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_metadata::SyntaxParser;

    // Exact-output fixtures specify a fixed style, independent of user defaults.
    fn fixture_options(tab_size: u32, insert_spaces: bool) -> FormatOptions {
        FormatOptions::from(&FormattingSettings {
            indent: Some(tab_size),
            use_tabs: Some(!insert_spaces),
            soft_line_width: Some(100),
            hard_line_width: Some(100),
            control_flow_layout: ControlFlowLayout::MultilineCompactElse,
            compact_factor_operators: false,
            break_after_semicolon: true,
        })
    }

    fn format_fixture_with_options(text: &str, options: &FormatOptions) -> String {
        let mut parser = SyntaxParser::new().expect("Macaulay2 parser should load");
        let tree = parser.parse_tree(text, None).expect("fixture should parse");
        let root = tree.root(text);
        format_document_with_options(root, options)
    }

    fn fold_fixture(text: &str) -> Vec<FormatFoldRange> {
        let mut parser = SyntaxParser::new().expect("Macaulay2 parser should load");
        let tree = parser.parse_tree(text, None).expect("fixture should parse");
        folding_ranges_for_syntax(tree.root(text))
    }

    fn format_fixture(text: &str) -> String {
        format_fixture_with_options(text, &fixture_options(4, true))
    }

    #[test]
    fn layout_edits_keep_the_supplied_syntax_and_token_contents() {
        let input = "  x:= f (\"λ 😀  \",1+2)  \r\n";
        let mut parser = SyntaxParser::new().unwrap();
        let tree = parser.parse_tree(input, None).unwrap();
        let root = tree.root(input);
        let mut source = LayoutSource::new(root);
        let options = fixture_options(2, true);
        let tokens = |node: LayoutNode<'_>| {
            node.descendants()
                .filter(|node| node.child_count() == 0 && !node.is_implicit_application())
                .map(|node| (node.id(), node.text().to_string()))
                .collect::<Vec<_>>()
        };
        let expected = tokens(LayoutNode::new(root, &source));
        let spacing = normalize_whitespace(LayoutNode::new(root, &source), &options);
        assert!(source.apply(spacing));
        let indentation = reindent_from_tree(LayoutNode::new(root, &source), &options);
        assert!(source.apply(indentation));
        assert_eq!(tokens(LayoutNode::new(root, &source)), expected);
        assert_eq!(source.into_text(), "x := f(\"λ 😀  \", 1 + 2)\r\n");
        assert_eq!(root.source_text(), input);
    }

    #[test]
    fn layout_rejects_edits_inside_literal_whitespace() {
        let input = "x = \"a  b\"\n";
        let mut parser = SyntaxParser::new().unwrap();
        let tree = parser.parse_tree(input, None).unwrap();
        let mut source = LayoutSource::new(tree.root(input));
        assert!(!source.apply(vec![FormatEdit {
            start_byte: 6,
            end_byte: 8,
            replacement: " ".to_string(),
        }]));
        assert!(!source.apply(vec![FormatEdit {
            start_byte: 7,
            end_byte: 7,
            replacement: "\n".to_string(),
        }]));
        assert_eq!(source.into_text(), input);
    }

    #[test]
    fn default_options_follow_configured_defaults() {
        let settings = FormattingSettings::default();
        let options = FormatOptions::default();
        let (soft, hard) = settings.line_widths();
        assert_eq!(options.soft_line_width, soft.map(|width| width as usize));
        assert_eq!(options.max_line_width, hard.map(|width| width as usize));
        assert_eq!(options.control_flow_layout, settings.control_flow_layout);
        assert_eq!(
            options.compact_factor_operators,
            settings.compact_factor_operators
        );
        assert_eq!(
            options.break_after_semicolon,
            settings.break_after_semicolon
        );
        // Explicit editor choices must not affect non-indentation settings.
        let editor_options = FormatOptions::from_settings(
            options.tab_width as u32,
            options.indent != "\t",
            &settings,
        );
        assert_eq!(options, editor_options);
    }

    #[test]
    fn hash_table_constructions_put_one_option_per_line() {
        assert_eq!(
            format_fixture("a = f({x=>1,y=>2}, {c=>3}, {d=>4,e=>5, f})\n"),
            "a = f({\n    x => 1,\n    y => 2\n}, {c => 3}, {d => 4, e => 5, f})\n",
            "a single option, or a list mixing options with values, stays on one line"
        );
        assert_eq!(
            format_fixture("h = new HashTable from {\"a\" => 1, \"b\" => {p => 2, q => 3}}\n"),
            concat!(
                "h = new HashTable from {\n",
                "    \"a\" => 1,\n",
                "    \"b\" => {\n",
                "        p => 2,\n",
                "        q => 3\n",
                "    }\n",
                "}\n",
            ),
            "nested tables are laid out at their own depth"
        );
    }

    #[test]
    fn hash_table_layout_is_stable() {
        let formatted = format_fixture("t = {x => 1, y => 2}\n");
        assert_eq!(formatted, "t = {\n    x => 1,\n    y => 2\n}\n");
        assert_eq!(format_fixture(&formatted), formatted);
    }

    #[test]
    fn folds_consecutive_full_line_comments_as_one_range() {
        assert_eq!(
            fold_fixture("-- first line\n  -- second line\n-- third line\nx = 1\n-- lone line\n"),
            vec![FormatFoldRange {
                start_line: 0,
                end_line: 2,
                kind: FormatFoldKind::Comment,
            }]
        );
    }

    #[test]
    fn keeps_separate_line_comment_blocks_separate() {
        assert_eq!(
            fold_fixture("-- one\n-- two\n\n-- three\n-- four\n"),
            vec![
                FormatFoldRange {
                    start_line: 0,
                    end_line: 1,
                    kind: FormatFoldKind::Comment,
                },
                FormatFoldRange {
                    start_line: 3,
                    end_line: 4,
                    kind: FormatFoldKind::Comment,
                },
            ]
        );
    }

    #[test]
    fn folds_each_multiline_block_comment() {
        assert_eq!(
            fold_fixture("-* first\nmiddle\nlast *-\nx = 1\n-* another\ncomment *-\n"),
            vec![
                FormatFoldRange {
                    start_line: 0,
                    end_line: 2,
                    kind: FormatFoldKind::Comment,
                },
                FormatFoldRange {
                    start_line: 4,
                    end_line: 5,
                    kind: FormatFoldKind::Comment,
                },
            ]
        );
    }

    #[test]
    fn marks_comment_folds_for_lsp_clients() {
        let mut parser = SyntaxParser::new().unwrap();
        let text = "-- first\n-- second\n";
        let tree = parser.parse_tree(text, None).unwrap();
        let root = tree.root(text);
        let ranges = folding_ranges(root);

        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].kind, Some(FoldingRangeKind::Comment));
    }

    #[test]
    fn folds_multiline_quoted_and_raw_strings() {
        assert_eq!(
            fold_fixture("x = \"first\nsecond\nthird\"\ny = ///\nraw\n///\nz = \"one line\"\n"),
            vec![
                FormatFoldRange {
                    start_line: 0,
                    end_line: 2,
                    kind: FormatFoldKind::Region,
                },
                FormatFoldRange {
                    start_line: 3,
                    end_line: 5,
                    kind: FormatFoldKind::Region,
                },
            ]
        );
    }

    #[test]
    fn folds_string_nested_in_indented_block() {
        assert_eq!(
            fold_fixture("f = () -> (\n    s := \"a\n    b\";\n    s\n)\n"),
            vec![
                FormatFoldRange {
                    start_line: 0,
                    end_line: 4,
                    kind: FormatFoldKind::Region,
                },
                FormatFoldRange {
                    start_line: 1,
                    end_line: 2,
                    kind: FormatFoldKind::Region,
                },
            ]
        );
    }

    #[test]
    fn does_not_fold_string_delimiters_inside_block_comment() {
        assert_eq!(
            fold_fixture("-* a \"b\nc\" *-\n"),
            vec![FormatFoldRange {
                start_line: 0,
                end_line: 1,
                kind: FormatFoldKind::Comment,
            }]
        );
    }

    #[test]
    fn does_not_treat_inline_or_string_markers_as_comment_blocks() {
        assert!(fold_fixture("x = \"-- not a comment\"\ny = 1 -- inline\n-- lone\n").is_empty());
    }

    #[test]
    fn trims_trailing_whitespace_without_reflowing_code() {
        assert_eq!(format_fixture("x := 1  \n  y = 2\t\n"), "x := 1\ny = 2\n");
        assert_eq!(format_fixture("x := 1  "), "x := 1");
    }

    #[test]
    fn spaces_parenthesized_method_install_head() {
        // An install head reads as installation syntax: `f (Types) := …`.
        assert_eq!(
            format_fixture("f(ZZ,ZZ) := (i,j) -> i\n"),
            "f (ZZ, ZZ) := (i, j) -> i\n"
        );
    }

    #[test]
    fn ordinary_call_stays_compact() {
        // A plain call is not an install; it keeps `f(args)` with no space.
        assert_eq!(format_fixture("g(5)\n"), "g(5)\n");
        assert_eq!(format_fixture("h(1, 2)\n"), "h(1, 2)\n");
    }

    #[test]
    fn normalizes_operator_whitespace() {
        assert_eq!(format_fixture("x+y\n"), "x + y\n");
        assert_eq!(format_fixture("x+ y\n"), "x + y\n");
        assert_eq!(format_fixture("x   +y\n"), "x + y\n");
        assert_eq!(format_fixture("x\t+\t y\n"), "x + y\n");
        assert_eq!(format_fixture("x:=y\n"), "x := y\n");
        assert_eq!(format_fixture("f:=x->x+1\n"), "f := x -> x + 1\n");
        assert_eq!(
            format_fixture("f(x, Strategy=>LongPolynomial)\n"),
            "f(x, Strategy => LongPolynomial)\n"
        );
        assert_eq!(format_fixture("8*delta/2\n"), "8 * delta / 2\n");
        assert_eq!(format_fixture("x**y // z\n"), "x ** y // z\n");
        assert_eq!(format_fixture("x<<y\n"), "x << y\n");
        assert_eq!(format_fixture("x ^ y # 0\n"), "x^y#0\n");
    }

    #[test]
    fn compacts_unary_prefix_operators() {
        assert_eq!(
            format_fixture("iCoeff := - jRow#0;\n"),
            "iCoeff := -jRow#0;\n"
        );
        assert_eq!(format_fixture("x := not true\n"), "x := not true\n");
        assert_eq!(format_fixture("a-b\n"), "a - b\n");
    }

    #[test]
    fn spaces_prefix_print_operator() {
        assert_eq!(format_fixture("<<\"x\"\n"), "<< \"x\"\n");
        assert_eq!(format_fixture("f = () -> (<<   x)\n"), "f = () -> (<< x)\n");
    }

    #[test]
    fn operators_lead_continuation_rows_inside_brackets() {
        for (input, expected) in [
            ("(a and\nb)\n", "(a\n        and b)\n"),
            ("f(a <<\n  b)\n", "f(a\n        << b)\n"),
            // Outside brackets a leading operator would end the expression.
            ("a <<\nb\n", "a <<\n    b\n"),
            // Bindings keep their value on the next row.
            ("(x :=\ny)\n", "(x :=\n        y)\n"),
            ("{x =>\ny}\n", "{x =>\n        y}\n"),
        ] {
            let formatted = format_fixture(input);
            assert_eq!(formatted, expected);
            assert_eq!(format_fixture(&formatted), formatted);
        }
    }

    #[test]
    fn wraps_long_lines_before_operators_inside_brackets() {
        let options = FormatOptions {
            max_line_width: Some(30),
            soft_line_width: Some(30),
            ..fixture_options(4, true)
        };
        assert_eq!(
            format_fixture_with_options("x = (alpha + beta + gamma + delta)\n", &options),
            "x = (alpha + beta + gamma\n        + delta)\n"
        );
        assert_eq!(
            format_fixture_with_options("stdio << alpha << beta << gamma\n", &options),
            "stdio << alpha << beta <<\n    gamma\n"
        );
    }

    #[test]
    fn aligns_chained_method_installations() {
        let mut parser = SyntaxParser::new().expect("Macaulay2 parser should load");
        for (input, expected) in [
            (
                "plainText Option    :=\n    plainText Nothing := x -> \"\"\n",
                "plainText Option :=\nplainText Nothing := x -> \"\"\n",
            ),
            (
                "(\nf (ZZ)    :=\nf (QQ) :=\nf (RR) := x ->\nx\n)\n",
                "(\n    f (ZZ) :=\n    f (QQ) :=\n    f (RR) := x ->\n        x\n)\n",
            ),
            ("a   =\r\n b =\r\n 1\r\n", "a =\r\nb =\r\n    1\r\n"),
        ] {
            let formatted = format_fixture(input);
            assert_eq!(formatted, expected);
            assert!(parser
                .parse_tree(&formatted, None)
                .is_some_and(|tree| !tree.root(&formatted).has_error()));
            assert_eq!(format_fixture(&formatted), formatted);
        }
    }

    #[test]
    fn normalizes_operator_spacing_beside_line_breaks() {
        for (input, expected) in [
            ("a    +\nb\n", "a +\n    b\n"),
            ("x\t:=\n y\n", "x :=\n    y\n"),
            ("f = x    ->\n x\n", "f = x ->\n    x\n"),
        ] {
            let formatted = format_fixture(input);
            assert_eq!(formatted, expected);
            assert_eq!(format_fixture(&formatted), formatted);
        }
    }

    #[test]
    fn indents_operator_continuations() {
        let options = fixture_options(3, true);

        assert_eq!(
            format_fixture_with_options("a + \nb +\nc\n", &options),
            "a +\n   b +\n   c\n"
        );
        assert_eq!(
            format_fixture_with_options("f := (a +\nb +\nc)\n", &options),
            "f := (a\n      + b\n      + c)\n"
        );
        assert_eq!(
            format_fixture_with_options("-----\ntop = 1\n", &options),
            "-----\ntop = 1\n"
        );
        assert_eq!(
            format_fixture_with_options(
                "expandMacro (Macro, String) := String => (m, block) ->\n\
                 resultSource (transformOf m)(tokenStream parseMacroTree block)\n",
                &options,
            ),
            "expandMacro (Macro, String) := String => (m, block) ->\n\
             \x20  resultSource (transformOf m)(tokenStream parseMacroTree block)\n"
        );
    }

    #[test]
    fn normalizes_comma_whitespace() {
        assert_eq!(format_fixture("f(x,y)\n"), "f(x, y)\n");
        assert_eq!(format_fixture("f(x ,y)\n"), "f(x, y)\n");
        assert_eq!(format_fixture("f(x,  y)\n"), "f(x, y)\n");
        assert_eq!(format_fixture("f(x ,  y)\n"), "f(x, y)\n");
        assert_eq!(format_fixture("f(x,)\n"), "f(x,)\n");
        assert_eq!(format_fixture("f(x,\ny)\n"), "f(x,\n    y)\n");
        assert_eq!(
            format_fixture("values := (,a,,)\n"),
            "values := (,a,,)\n",
            "explicit null slots remain visually empty"
        );
    }

    #[test]
    fn wraps_long_calls_at_parsed_comma_boundaries() {
        let options = FormatOptions {
            max_line_width: Some(28),
            ..fixture_options(4, true)
        };

        assert_eq!(
            format_fixture_with_options("result = concatenate(alpha, beta, gamma)\n", &options),
            "result = concatenate(alpha,\n    beta, gamma)\n"
        );
    }

    #[test]
    fn hard_width_triggers_wrapping_toward_the_soft_width() {
        let input = "result = concatenate(alpha, beta, gamma)\n";
        let below_hard_limit = FormatOptions {
            soft_line_width: Some(28),
            max_line_width: Some(50),
            ..fixture_options(4, true)
        };
        assert_eq!(format_fixture_with_options(input, &below_hard_limit), input);

        let above_hard_limit = FormatOptions {
            soft_line_width: Some(28),
            max_line_width: Some(35),
            ..fixture_options(4, true)
        };
        assert_eq!(
            format_fixture_with_options(input, &above_hard_limit),
            "result = concatenate(alpha,\n    beta, gamma)\n"
        );
    }

    #[test]
    fn wraps_long_collections_with_the_shared_rule_engine() {
        let options = FormatOptions {
            max_line_width: Some(26),
            ..fixture_options(4, true)
        };

        assert_eq!(
            format_fixture_with_options("values = {alpha, beta, gamma, delta}\n", &options),
            "values = {alpha, beta,\n    gamma, delta}\n"
        );
    }

    #[test]
    fn wraps_long_binary_expressions_after_spaced_operators() {
        let options = FormatOptions {
            max_line_width: Some(26),
            ..fixture_options(4, true)
        };

        assert_eq!(
            format_fixture_with_options("result = alpha + beta + gamma + delta\n", &options),
            "result = alpha + beta +\n    gamma + delta\n"
        );
    }

    #[test]
    fn wrapping_preserves_literal_contents_and_is_idempotent() {
        let options = FormatOptions {
            max_line_width: Some(20),
            ..fixture_options(4, true)
        };
        let formatted = format_fixture_with_options(
            "value = \"alpha, beta + gamma and a very long literal\"\n",
            &options,
        );

        assert_eq!(
            formatted,
            "value =\n    \"alpha, beta + gamma and a very long literal\"\n"
        );
        assert_eq!(format_fixture_with_options(&formatted, &options), formatted);
    }

    #[test]
    fn wrapping_can_be_disabled_and_preserves_crlf_when_enabled() {
        let input = "result = concatenate(alpha, beta, gamma)\r\n";
        let disabled = FormatOptions {
            max_line_width: None,
            ..fixture_options(4, true)
        };
        assert_eq!(format_fixture_with_options(input, &disabled), input);

        let enabled = FormatOptions {
            max_line_width: Some(28),
            ..fixture_options(4, true)
        };
        assert_eq!(
            format_fixture_with_options(input, &enabled),
            "result = concatenate(alpha,\r\n    beta, gamma)\r\n"
        );
    }

    #[test]
    fn wrapped_calls_collections_and_operators_remain_parseable_and_stable() {
        let options = FormatOptions {
            max_line_width: Some(24),
            ..fixture_options(4, true)
        };
        let mut parser = SyntaxParser::new().expect("Macaulay2 parser should load");

        for source in [
            "result = concatenate(alpha, beta, gamma)\n",
            "values = {alpha, beta, gamma, delta}\n",
            "result = alpha + beta + gamma + delta\n",
            "nested = outer(alpha, inner(beta, gamma), delta)\n",
        ] {
            let formatted = format_fixture_with_options(source, &options);
            let has_error = parser
                .parse_tree(&formatted, None)
                .map(|tree| tree.root(&formatted).has_error())
                .expect("formatted fixture should parse");
            assert!(!has_error, "wrapped source should parse:\n{formatted}");
            assert_eq!(format_fixture_with_options(&formatted, &options), formatted);
        }
    }

    #[test]
    fn normalizes_semicolon_whitespace() {
        assert_eq!(format_fixture("i=0;j=0;\n"), "i = 0;\nj = 0;\n");
        assert_eq!(format_fixture("i = 0 ; j = 0\n"), "i = 0;\nj = 0\n");
        assert_eq!(format_fixture("i = 0;\n j = 0\n"), "i = 0;\nj = 0\n");
        assert_eq!(
            format_fixture("\"x;y\" -- comment a;b\n"),
            "\"x;y\" -- comment a;b\n"
        );
        assert_eq!(
            format_fixture("f := (i=0;j=0;)\n"),
            "f := (i = 0;\n    j = 0;\n)\n"
        );
    }

    #[test]
    fn line_end_open_brackets_create_single_extra_indent_scope() {
        assert_eq!(
            format_fixture(
                "scan(docKeys, key -> (\nbaseName := recordNameFromDocKey key;\nif db#?key then result#baseName = append(result#baseName ?? {}, hashTable {\"key\" => key, \"raw\" => db#key});\n)\n);\n"
            ),
            concat!(
                "scan(docKeys, key -> (\n",
                "    baseName := recordNameFromDocKey key;\n",
                "    if db#?key then\n",
                "        result#baseName = append(result#baseName ?? {}, hashTable {\n",
                "            \"key\" => key,\n",
                "            \"raw\" => db#key\n",
                "        });\n",
                ")\n",
                ");\n",
            ),
            "a bracket opened on a continuation line indents relative to that line"
        );
    }

    #[test]
    fn inline_open_brackets_do_not_force_multiline_closer() {
        // A ternary `if` (value position) is left inline, so its inline
        // `then(...)` brackets are not pushed onto their own lines.
        assert_eq!(
            format_fixture("x := if a then(b) else(c)\n"),
            "x := if a then(b) else(c)\n"
        );
    }

    #[test]
    fn uses_configured_indent_width() {
        let options = fixture_options(2, true);

        assert_eq!(
            format_fixture_with_options("f := (i=0;j=0;)\n", &options),
            "f := (i = 0;\n  j = 0;\n)\n"
        );
        assert_eq!(
            format_fixture_with_options("f := (i=0;j=0;)\n", &fixture_options(4, false)),
            "f := (i = 0;\n\tj = 0;\n)\n"
        );
    }

    #[test]
    fn can_restore_compact_factor_operators() {
        let options = FormatOptions {
            compact_factor_operators: true,
            ..fixture_options(4, true)
        };

        assert_eq!(
            format_fixture_with_options("8 * delta / 2\n", &options),
            "8*delta/2\n"
        );
    }

    #[test]
    fn can_keep_semicolon_separated_statements_inline() {
        let options = FormatOptions {
            break_after_semicolon: false,
            ..fixture_options(4, true)
        };

        assert_eq!(
            format_fixture_with_options("i=0;j=0;\n", &options),
            "i = 0; j = 0;\n"
        );
    }

    #[test]
    fn server_settings_override_lsp_format_options() {
        let settings = crate::settings::ServerSettings::from_value(&serde_json::json!({
            "formatting": {
                "indent": 2,
                "useTabs": false,
                "compactFactorOperators": true,
                "breakAfterSemicolon": false
            }
        }))
        .settings;
        let options = FormatOptions::from_settings(8, false, &settings.formatting);

        assert_eq!(
            format_fixture_with_options("f := (\na * b;c\n)\n", &options),
            "f := (\n  a*b; c\n)\n"
        );
    }

    #[test]
    fn normalizes_parenthesized_call_whitespace() {
        assert_eq!(format_fixture("f (x,y)\n"), "f(x, y)\n");
        assert_eq!(format_fixture("f \t (x)\n"), "f(x)\n");
        assert_eq!(
            format_fixture("random (source gens Ip2, R^{-d})\n"),
            "random(source gens Ip2, R^{-d})\n"
        );
        assert_eq!(format_fixture("f x\n"), "f x\n");
    }

    #[test]
    fn preserves_non_operator_text() {
        assert_eq!(
            format_fixture("\"x+y\" -- comment a+b\n"),
            "\"x+y\" -- comment a+b\n"
        );
        assert_eq!(format_fixture("-x+y\n"), "-x + y\n");
        assert_eq!(format_fixture("R=QQ[a..d]\n"), "R = QQ[a..d]\n");
        assert_eq!(format_fixture("x#0 + y\n"), "x#0 + y\n");
        assert_eq!(format_fixture("x\n+y\n"), "x\n+y\n");
    }

    #[test]
    fn preserves_literal_and_comment_bytes_with_mixed_line_endings() {
        fn protected_text(text: &str) -> Vec<String> {
            let mut parser = SyntaxParser::new().expect("Macaulay2 parser should load");
            let tree = parser.parse_tree(text, None).expect("source should parse");
            let root = tree.root(text);
            assert!(!root.has_error(), "source should parse:\n{text}");
            root.descendants()
                .filter(|node| node.is_string_literal() || node.is_comment())
                .map(|node| node.text().to_string())
                .collect()
        }

        let input = concat!(
            "f=x->(\r\n",
            "s:=\"first  \n\t x+y; a=>b  \n  \";\r\n",
            "r:=///raw  \n\t \n  if x then y; {a=>1,b=>2}\n///;\r\n",
            "-* comment  \n\t x+y; {a=>1,b=>2}  \n  *-\r\n",
            "-- keep comment spaces  \r\n",
            "s+r\r\n",
            ")\r\n",
        );
        let expected = protected_text(input);
        let options = FormatOptions {
            max_line_width: Some(24),
            ..fixture_options(2, false)
        };
        let formatted = format_fixture_with_options(input, &options);
        assert_ne!(formatted, input, "surrounding code should be formatted");
        assert_eq!(protected_text(&formatted), expected);
        assert_eq!(format_fixture_with_options(&formatted, &options), formatted);
    }

    #[test]
    fn preserves_multiline_string_contents() {
        assert_eq!(
            format_fixture("x=///\n  keep spaces  \n///\ny=1\n"),
            "x = ///\n  keep spaces  \n///\ny = 1\n"
        );
        assert_eq!(
            format_fixture("x=\"first\n  keep spaces  \nlast\"\ny=1\n"),
            "x = \"first\n  keep spaces  \nlast\"\ny = 1\n"
        );
        assert_eq!(
            format_fixture("  x=///first line  \n  keep spaces  \n///\n"),
            "x = ///first line  \n  keep spaces  \n///\n",
            "formatting the prefix must not trim content before a literal newline"
        );
    }

    #[test]
    fn preserves_crlf_line_endings() {
        // A CRLF document must keep CRLF: the formatter must not silently rewrite
        // every line ending to LF (which would churn the whole file on save).
        assert_eq!(format_fixture("x=1\r\ny=2\r\n"), "x = 1\r\ny = 2\r\n");
        // Verbatim multiline-string content keeps its CRLF too.
        assert_eq!(
            format_fixture("x=///\r\n  keep  \r\n///\r\n"),
            "x = ///\r\n  keep  \r\n///\r\n"
        );
    }

    #[test]
    fn expands_a_single_then_body_when_line_breaks_are_parseable() {
        assert_eq!(
            format_fixture("if a then b else c\n"),
            "if a then b else c\n"
        );
        assert_eq!(format_fixture("if a then b\n"), "if a then\n    b\n");
    }

    #[test]
    fn leaves_ternary_if_untouched() {
        assert_eq!(
            format_fixture("x := if a then f(x) else g(y)\n"),
            "x := if a then f(x) else g(y)\n"
        );
    }

    #[test]
    fn formats_valid_nodes_when_other_nodes_have_parse_errors() {
        assert_eq!(
            format_fixture("x+y\nbad := (\na:=b\n"),
            "x + y\nbad := (\n    a := b\n"
        );
    }

    #[test]
    fn formats_example_file_despite_parser_gaps() {
        let formatted = format_fixture(include_str!("../../tests/fixtures/formatting_example.m2"));

        assert!(formatted.contains("k := ceiling((-3 + sqrt(9.0 + 8 * delta)) / 2);"));
        assert!(formatted.contains("K = ZZ / 101;"));
        assert!(formatted.contains("randomPlanePoints = (delta, R) -> ("));
        assert!(formatted.contains("random(source gens Ip2, R^{-d})"));
        assert!(formatted.contains("SyzygyLimit => 60"));
    }

    #[test]
    fn indents_inline_conditional_bodies_deeper_than_else() {
        let input = concat!(
            "(\n",
            "    resolved := if opts.ResolveLink === null then\n",
            "        resolveDocumentationLink(link, {}, \"markdown\", \"\")\n",
            "        else opts.ResolveLink link;\n",
            ")\n",
        );
        for insert_spaces in [true, false] {
            let options = fixture_options(4, insert_spaces);
            let indent = options.indent.clone();
            let expected = format!(
                "(\n{indent}resolved := if opts.ResolveLink === null then\n\
                 {indent}{indent}{indent}resolveDocumentationLink(link, {{}}, \"markdown\", \"\")\n\
                 {indent}{indent}else opts.ResolveLink link;\n)\n"
            );
            let formatted = format_fixture_with_options(input, &options);
            assert_eq!(formatted, expected);
            assert_eq!(format_fixture_with_options(&formatted, &options), formatted);

            let multiline = FormatOptions {
                control_flow_layout: ControlFlowLayout::Multiline,
                ..options
            };
            let expected = expected.replace(
                "else opts.ResolveLink link;",
                &format!("else\n{indent}{indent}{indent}opts.ResolveLink link;"),
            );
            let formatted = format_fixture_with_options(input, &multiline);
            assert_eq!(formatted, expected);
            assert_eq!(
                format_fixture_with_options(&formatted, &multiline),
                formatted
            );
        }
    }

    #[test]
    fn indents_else_branches_of_line_broken_if_expression() {
        // A line-broken `if`/`else` chain is only valid M2 inside brackets: at
        // global scope the newline after a completable `if … then …` ends the
        // expression and the following `else` is a syntax error. Within `( … )`
        // the newline is whitespace, so the whole chain is one expression. Each
        // `else if …` and the final `else` use the conventional flat C-style
        // alignment even though the CST represents a nested `if`.
        let chain = "extracted := (\n\
             if kind === \"function\" then\n\
             extractFunc(name, db)\n\
             else if kind === \"operator\" then extractOperator(name, db)\n\
             else extractObject(name, db)\n\
             );\n";
        let formatted = format_fixture(chain);

        assert_eq!(
            formatted,
            "extracted := (\n\
             \x20   if kind === \"function\" then\n\
             \x20       extractFunc(name, db)\n\
             \x20   else if kind === \"operator\" then\n\
             \x20       extractOperator(name, db)\n\
             \x20   else extractObject(name, db)\n\
             );\n"
        );
        // The result is stable under re-formatting.
        assert_eq!(format_fixture(&formatted), formatted);
    }

    #[test]
    fn aligns_every_clause_in_a_multi_else_if_chain() {
        let chain = "result := (\n\
             if a then one\n\
             else if b then two\n\
             else if c then three\n\
             else four\n\
             )\n";

        assert_eq!(
            format_fixture(chain),
            "result := (\n\
             \x20   if a then\n\
             \x20       one\n\
             \x20   else if b then\n\
             \x20       two\n\
             \x20   else if c then\n\
             \x20       three\n\
             \x20   else four\n\
             )\n"
        );
    }

    #[test]
    fn control_flow_layout_variants_choose_the_final_else_shape() {
        let text = "value := (\nif a then one else if b then two else three\n)\n";

        let compact = FormatOptions {
            control_flow_layout: ControlFlowLayout::Compact,
            ..fixture_options(4, true)
        };
        assert_eq!(
            format_fixture_with_options(text, &compact),
            "value := (\n    if a then one else if b then two else three\n)\n"
        );

        let multiline = FormatOptions {
            control_flow_layout: ControlFlowLayout::Multiline,
            ..fixture_options(4, true)
        };
        assert_eq!(
            format_fixture_with_options(text, &multiline),
            "value := (\n\
             \x20   if a then\n\
             \x20       one\n\
             \x20   else if b then\n\
             \x20       two\n\
             \x20   else\n\
             \x20       three\n\
             )\n"
        );

        assert_eq!(
            format_fixture(text),
            "value := (\n\
             \x20   if a then\n\
             \x20       one\n\
             \x20   else if b then\n\
             \x20       two\n\
             \x20   else three\n\
             )\n"
        );
    }

    #[test]
    fn expands_while_for_and_try_clause_bodies_analogously() {
        let text = "value := (\nwhile ready do work;\nfor i to 2 list i;\ntry x then y else z\n)\n";

        assert_eq!(
            format_fixture(text),
            "value := (\n\
             \x20   while ready do\n\
             \x20       work;\n\
             \x20   for i to 2 list\n\
             \x20       i;\n\
             \x20   try x then\n\
             \x20       y\n\
             \x20   else z\n\
             )\n"
        );
    }

    #[test]
    fn keeps_empty_control_and_lambda_bodies_inline() {
        let text = "if ready then (\n)\n\
                    while ready do (\n)\n\
                    for i to 2 list (\n)\n\
                    f = x -> (\n)\n\
                    value := (\n\
                    try (\n) then (\n) else (\n)\n\
                    )\n";

        assert_eq!(
            format_fixture(text),
            "if ready then ()\n\
             while ready do ()\n\
             for i to 2 list ()\n\
             f = x -> ()\n\
             value := (\n\
             \x20   try () then () else ()\n\
             )\n"
        );
    }

    #[test]
    fn joins_else_to_a_preceding_closing_delimiter() {
        let text = "value := (\n\
                    if ready then (\n\
                    work\n\
                    )\n\
                    else (\n\
                    fallback\n\
                    )\n\
                    )\n";

        let formatted = format_fixture(text);
        assert_eq!(
            formatted,
            "value := (\n\
             \x20   if ready then (\n\
             \x20       work\n\
             \x20   ) else (\n\
             \x20       fallback\n\
             \x20   )\n\
             )\n"
        );
        assert_eq!(format_fixture(&formatted), formatted);
    }

    #[test]
    fn keeps_delimited_control_bodies_beside_every_clause_keyword() {
        let options = FormatOptions {
            control_flow_layout: ControlFlowLayout::Multiline,
            ..fixture_options(4, true)
        };
        let mut parser = SyntaxParser::new().expect("Macaulay2 parser should load");

        for (source, clause) in [
            ("value := (\nif ready then (\nwork\n)\n)\n", "then ("),
            ("value := (\nwhile ready do (\nwork\n)\n)\n", "do ("),
            ("value := (\nfor i to 2 do (\nwork\n)\n)\n", "do ("),
            ("value := (\nfor i to 2 list (\ni\n)\n)\n", "list ("),
            (
                "value := (\ntry work then (\ndone\n) else (\nfailed\n)\n)\n",
                "else (",
            ),
            (
                "value := (\ntry work then done except err do (\nerr\n)\n)\n",
                "do (",
            ),
        ] {
            let formatted = format_fixture_with_options(source, &options);

            assert!(
                formatted.contains(clause),
                "the clause opener should remain inline:\n{formatted}"
            );
            assert!(
                parser
                    .parse_tree(&formatted, None)
                    .is_some_and(|tree| !tree.root(&formatted).has_error()),
                "the formatted control statement should parse:\n{formatted}"
            );
            assert_eq!(format_fixture_with_options(&formatted, &options), formatted);
        }
    }

    #[test]
    fn keeps_the_except_binder_with_except_and_breaks_after_do() {
        let text = "value := (\ntry 1/x then result except err do err\n)\n";

        assert_eq!(
            format_fixture(text),
            "value := (\n\
             \x20   try 1 / x then\n\
             \x20       result\n\
             \x20   except err do\n\
             \x20       err\n\
             )\n"
        );
    }

    #[test]
    fn indents_body_of_else_parenthesized_block() {
        // The `else (` block reopens a scope on a line that also closes one, so
        // its body must indent just like the `then (` block body.
        assert_eq!(
            format_fixture("if cond then (\na;\nb\n) else (\nc;\nd\n)\n"),
            "if cond then (\n    a;\n    b\n) else (\n    c;\n    d\n)\n"
        );
    }

    #[test]
    fn keeps_top_level_symbols_unindented_after_comment_dividers() {
        let formatted = format_fixture(include_str!("../../tests/fixtures/comment_dividers.m2"));

        assert!(formatted.contains("\nprimitive = (L) -> ("));
        assert!(formatted.contains("\ntoZZ = (L) -> ("));
        assert!(!formatted.contains("\n     toZZ = (L) -> ("));
    }
}
