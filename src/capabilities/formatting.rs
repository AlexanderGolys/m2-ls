//! Tree-sitter-guided formatting and folding ranges for Macaulay2 source.

use m2_syn::nodes::{
    ElseClause, ExprFor as ForLoop, ExprIf as IfStatement, ExprLambda as LambdaExpression,
    ExprTry as TryStatement, ExprWhile as WhileLoop, FloatLiteral, IntegerLiteral, LoopBody,
    RawStringLiteral, Sequence, StringLiteral, Symbol, ThenClause,
};
use m2_syn::Token;
use tower_lsp::lsp_types::{
    DocumentFormattingOptions, FoldingRange, FoldingRangeKind, FoldingRangeProviderCapability,
    OneOf, TextEdit,
};

use crate::node_metadata::{matches_token, M2Node, M2Parser};
use crate::source::SourceNavigation;

pub trait FormattingConfiguration {
    fn indent_width(&self) -> Option<u32>;
    fn use_tabs(&self) -> Option<bool>;
    fn line_widths(&self) -> (Option<u32>, Option<u32>);
    fn control_flow_layout(&self) -> ControlFlowLayout;
    fn compact_factor_operators(&self) -> bool;
    fn break_after_semicolon(&self) -> bool;
}

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
        Self {
            indent: "    ".to_string(),
            tab_width: 4,
            soft_line_width: Some(100),
            max_line_width: Some(100),
            control_flow_layout: ControlFlowLayout::MultilineCompactElse,
            compact_factor_operators: false,
            break_after_semicolon: true,
        }
    }
}

impl FormatOptions {
    pub fn new(tab_size: u32, insert_spaces: bool) -> Self {
        let tab_width = tab_size.max(1) as usize;
        let indent = if insert_spaces {
            " ".repeat(tab_width)
        } else {
            "\t".to_string()
        };

        Self {
            indent,
            tab_width,
            ..Self::default()
        }
    }

    pub fn from_configuration(
        tab_size: u32,
        insert_spaces: bool,
        configuration: &(impl FormattingConfiguration + ?Sized),
    ) -> Self {
        let tab_size = configuration.indent_width().unwrap_or(tab_size);
        let use_tabs = configuration.use_tabs().unwrap_or(!insert_spaces);
        let mut options = Self::new(tab_size, !use_tabs);
        let (soft_line_width, hard_line_width) = configuration.line_widths();
        options.soft_line_width = soft_line_width.map(|width| width as usize);
        options.max_line_width = hard_line_width.map(|width| width as usize);
        options.control_flow_layout = configuration.control_flow_layout();
        options.compact_factor_operators = configuration.compact_factor_operators();
        options.break_after_semicolon = configuration.break_after_semicolon();
        options
    }
}

pub fn document_formatting_provider_capability() -> Option<OneOf<bool, DocumentFormattingOptions>> {
    Some(OneOf::Left(true))
}

pub fn folding_range_provider_capability() -> Option<FoldingRangeProviderCapability> {
    Some(FoldingRangeProviderCapability::Simple(true))
}

pub fn document_formatting_text_edits(
    source: &(impl SourceNavigation + ?Sized),
    tab_size: u32,
    insert_spaces: bool,
    configuration: &(impl FormattingConfiguration + ?Sized),
) -> Vec<TextEdit> {
    let text = source.text();
    let options = FormatOptions::from_configuration(tab_size, insert_spaces, configuration);
    let formatted = format_document_text_with_options(text, &options);
    if formatted == text {
        return Vec::new();
    }

    vec![TextEdit {
        range: source.full_range(),
        new_text: formatted,
    }]
}

pub fn folding_ranges(text: &str) -> Vec<FoldingRange> {
    folding_ranges_for_text(text)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FormatEdit {
    start_byte: usize,
    end_byte: usize,
    replacement: &'static str,
}

#[cfg(test)]
pub fn format_document_text(text: &str) -> String {
    format_document_text_with_options(text, &FormatOptions::default())
}

/// Format the whole document: whitespace normalization around operators and
/// punctuation, tree-derived indentation, and width-based wrapping at parsed
/// syntax boundaries.
pub fn format_document_text_with_options(text: &str, options: &FormatOptions) -> String {
    let newline = detect_line_ending(text);
    let formatted = normalize_whitespace(text, options);
    let formatted = inline_empty_bodies(&formatted);
    let formatted = format_control_flow(&formatted, options, newline);
    let formatted = reindent_from_tree(&formatted, options, newline);
    let formatted = wrap_long_lines(&formatted, options, newline);
    let formatted = separate_line_end_bracket_closers(&formatted, newline);
    let mut formatted = reindent_from_tree(&formatted, options, newline);

    if text.ends_with('\n') {
        formatted.push_str(newline);
    }

    formatted
}

fn separate_line_end_bracket_closers(text: &str, newline: &'static str) -> String {
    let Some(mut parser) = M2Parser::new() else {
        return text.to_string();
    };
    let Some(root) = parser.parse(text) else {
        return text.to_string();
    };
    let mut edits = Vec::new();
    for node in root.descendants().filter(M2Node::is_delimited_expression) {
        let children = node.children().collect::<Vec<_>>();
        let (Some(opener), Some(closer)) = (children.first(), children.last()) else {
            continue;
        };
        if !opener.is_opening_delimiter()
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
            replacement: newline,
        });
    }
    apply_format_edits(text, edits)
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

fn inline_empty_bodies(text: &str) -> String {
    let Some(mut parser) = M2Parser::new() else {
        return text.to_string();
    };
    let Some(root) = parser.parse(text) else {
        return text.to_string();
    };
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
                replacement: "",
            });
        }
    }
    apply_format_edits(text, edits)
}

fn empty_body_role(node: M2Node<'_>) -> bool {
    if !node.is_delimited_expression() {
        return false;
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.is::<LambdaExpression>() {
        return parent
            .child_by_field_name("body")
            .is_some_and(|body| body.id() == node.id());
    }
    if parent.is_keyword_clause() {
        return parent
            .named_child(0)
            .is_some_and(|body| body.id() == node.id());
    }
    if parent.is::<LoopBody>() {
        return ["listed_value", "ignored_value"]
            .into_iter()
            .filter_map(|field| parent.child_by_field_name(field))
            .any(|body| body.id() == node.id());
    }
    parent.is::<TryStatement>()
}

fn format_control_flow(text: &str, options: &FormatOptions, newline: &'static str) -> String {
    if options.control_flow_layout == ControlFlowLayout::Compact {
        return text.to_string();
    }

    let Some(mut parser) = M2Parser::new() else {
        return text.to_string();
    };
    let Some(root) = parser.parse(text) else {
        return text.to_string();
    };
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
            for (keyword, field) in [
                (direct_keyword::<Token![list]>(clause), "listed_value"),
                (direct_keyword::<Token![do]>(clause), "ignored_value"),
            ] {
                let Some((keyword, body)) = keyword.zip(clause.child_by_field_name(field)) else {
                    continue;
                };
                push_control_clause_body_break(keyword, body, text, newline, &mut edits);
            }
            continue;
        }
        if clause.is_except_clause() && clause.child_by_field_name("exception").is_some() {
            push_control_flow_break(
                text,
                same_line_horizontal_whitespace_start(text, clause.start_byte()),
                clause.start_byte(),
                newline,
                &mut edits,
            );
            if let Some((keyword, body)) =
                direct_keyword::<Token![do]>(clause).zip(clause.child_by_field_name("value"))
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

    apply_format_edits(text, edits)
}

fn direct_keyword<'tree, Keyword: m2_syn::Token>(node: M2Node<'tree>) -> Option<M2Node<'tree>> {
    node.children()
        .find(|child| child.is_keyword_token() && matches_token::<Keyword>(child.text()))
}

fn push_control_clause_body_break(
    keyword: M2Node<'_>,
    body: M2Node<'_>,
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

fn keep_else_after_closer(clause: M2Node<'_>, text: &str, edits: &mut Vec<FormatEdit>) -> bool {
    let Some(body) = clause.parent().and_then(|owner| {
        owner
            .children()
            .filter(|candidate| {
                candidate.is::<ThenClause>() && candidate.end_byte() <= clause.start_byte()
            })
            .max_by_key(M2Node::end_byte)
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
            replacement: " ",
        });
    }
    true
}

fn control_clause_breaks_are_parseable(clause: M2Node<'_>, text: &str) -> bool {
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
        replacement: newline,
    });
}

/// Re-indent every line of already-normalized `text` from a fresh parse: parse #2
/// of the two-parse design (parse #1 ran in `normalize_whitespace`). Each line's
/// leading whitespace is rebuilt as `options.indent.repeat(depth)` from the
/// tree-derived depth; lines inside a multiline string/raw-string are emitted
/// verbatim so their interior spacing is preserved.
fn reindent_from_tree(text: &str, options: &FormatOptions, newline: &str) -> String {
    let layout = TreeIndentLayout::build(text, options.compact_factor_operators);
    text.lines()
        .enumerate()
        .map(|(row, line)| {
            if layout.is_literal_line(row) {
                return line.to_string();
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return String::new();
            }
            let mut indented = options.indent.repeat(layout.depth(row));
            if layout.preserves_literal_trailing_space(row) {
                indented.push_str(line.trim_start());
            } else {
                indented.push_str(trimmed);
            }
            indented
        })
        .collect::<Vec<_>>()
        .join(newline)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineBreakCandidate {
    start_byte: usize,
    end_byte: usize,
}

trait LineBreakRule {
    fn collect(
        &self,
        node: M2Node<'_>,
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
        node: M2Node<'_>,
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
        node: M2Node<'_>,
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
        node: M2Node<'_>,
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
        node: M2Node<'_>,
        text: &str,
        options: &FormatOptions,
        candidates: &mut Vec<LineBreakCandidate>,
    ) {
        let Some(parent) = node.parent() else {
            return;
        };
        if node.is_operator()
            && (parent.binary_operator().is_some() || parent.is::<LambdaExpression>())
            && is_spaced_line_final_operator(node.text(), options.compact_factor_operators)
        {
            push_line_break_candidate_after(node, text, candidates);
        }
    }
}

fn push_line_break_candidate_after(
    node: M2Node<'_>,
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

fn wrap_long_lines(text: &str, options: &FormatOptions, newline: &'static str) -> String {
    let Some(max_line_width) = options.max_line_width else {
        return text.to_string();
    };
    let soft_line_width = options
        .soft_line_width
        .unwrap_or(max_line_width)
        .min(max_line_width);
    let rules = (DelimiterBreakRule, (CommaBreakRule, OperatorBreakRule));
    wrap_long_lines_with_rules(
        text,
        options,
        newline,
        soft_line_width,
        max_line_width,
        &rules,
    )
}

fn wrap_long_lines_with_rules(
    text: &str,
    options: &FormatOptions,
    newline: &'static str,
    soft_line_width: usize,
    max_line_width: usize,
    rules: &(impl LineBreakRule + ?Sized),
) -> String {
    let mut formatted = text.to_string();
    loop {
        let candidates = collect_line_break_candidates(&formatted, options, rules);
        if candidates.is_empty() {
            break;
        }

        let edits = select_line_breaks(
            &formatted,
            &candidates,
            soft_line_width,
            max_line_width,
            options.tab_width,
            newline,
        );
        if edits.is_empty() {
            break;
        }

        let wrapped = apply_format_edits(&formatted, edits);
        if wrapped == formatted {
            break;
        }
        formatted = reindent_from_tree(&wrapped, options, newline);
    }
    formatted
}

fn collect_line_break_candidates(
    text: &str,
    options: &FormatOptions,
    rules: &(impl LineBreakRule + ?Sized),
) -> Vec<LineBreakCandidate> {
    let Some(mut parser) = M2Parser::new() else {
        return Vec::new();
    };
    let Some(root) = parser.parse(text) else {
        return Vec::new();
    };

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
                replacement: newline,
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

/// The document's fold ranges, derived from tree-based indentation depths.
pub fn folding_ranges_for_text(text: &str) -> Vec<FormatFoldRange> {
    let layout = TreeIndentLayout::build(text, false);
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
    ranges.extend(layout.comment_folds);
    ranges.sort_by_key(|range| (range.start_line, range.end_line, range.kind));
    ranges
}

/// Per-line indentation depths derived from a tree-sitter parse of normalized
/// text, plus the rows that lie inside a multiline string and must be left
/// verbatim and the comment ranges that can be folded.
/// `depth(row) = bracket_depth(row) + continuation(row)`.
struct TreeIndentLayout {
    depths: Vec<usize>,
    literal_rows: Vec<bool>,
    literal_start_rows: Vec<bool>,
    comment_folds: Vec<FormatFoldRange>,
    leading_closer_rows: Vec<bool>,
}

impl TreeIndentLayout {
    fn build(text: &str, compact_factor_operators: bool) -> Self {
        let line_count = text.lines().count().max(1);
        let empty_layout = || Self {
            depths: vec![0; line_count],
            literal_rows: vec![false; line_count],
            literal_start_rows: vec![false; line_count],
            comment_folds: Vec::new(),
            leading_closer_rows: vec![false; line_count],
        };
        let Some(mut parser) = M2Parser::new() else {
            return empty_layout();
        };
        let Some(root) = parser.parse(text) else {
            return empty_layout();
        };
        let brackets = collect_bracket_groups(root, line_count);
        let (literal_rows, literal_start_rows) = collect_literal_rows(root, line_count);
        let line_leads = line_leading_blank(text, line_count);
        let comment_folds = collect_comment_fold_ranges(root, &line_leads);
        let leading_closer_rows = (0..line_count)
            .map(|row| {
                brackets.iter().any(|group| {
                    group.close_row == row
                        && line_leads.get(row).copied() == Some(group.closing_delimiter_column)
                })
            })
            .collect();

        let depths = (0..line_count)
            .map(|row| {
                bracket_depth(row, &brackets, &line_leads)
                    + line_continuation(row, root, &line_leads, compact_factor_operators)
            })
            .collect();

        Self {
            depths,
            literal_rows,
            literal_start_rows,
            comment_folds,
            leading_closer_rows,
        }
    }

    fn depth(&self, row: usize) -> usize {
        self.depths.get(row).copied().unwrap_or(0)
    }

    fn is_literal_line(&self, row: usize) -> bool {
        self.literal_rows.get(row).copied().unwrap_or(false)
    }

    fn preserves_literal_trailing_space(&self, row: usize) -> bool {
        self.literal_start_rows.get(row).copied().unwrap_or(false)
    }

    fn is_leading_closer(&self, row: usize) -> bool {
        self.leading_closer_rows.get(row).copied().unwrap_or(false)
    }
}

/// Build folds from parser-classified comments. Consecutive full-line `--`
/// comments become one range, while each multiline block comment is its own
/// range. Keeping this tree-derived avoids mistaking comment markers inside
/// strings for comments.
fn collect_comment_fold_ranges(root: M2Node<'_>, line_leads: &[usize]) -> Vec<FormatFoldRange> {
    let mut line_comment_rows = Vec::new();
    let mut ranges = Vec::new();

    for node in root.descendants() {
        let start = node.start_position();
        let end = node.end_position();
        if node.is_line_comment()
            && start.row == end.row
            && line_leads.get(start.row).copied() == Some(start.column)
        {
            line_comment_rows.push(start.row as u32);
        } else if node.is_block_comment() && start.row < end.row {
            ranges.push(FormatFoldRange {
                start_line: start.row as u32,
                end_line: end.row as u32,
                kind: FormatFoldKind::Comment,
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
fn collect_bracket_groups(root: M2Node<'_>, line_count: usize) -> Vec<BracketGroup> {
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

/// Recover unclosed opener tokens that tree-sitter leaves loose inside an ERROR
/// node (e.g. a `(` at EOF with no `)`). Each unmatched opener still indents its
/// body, so it opens a group from its row to the last line of the document.
fn collect_unclosed_error_brackets(
    error: M2Node<'_>,
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

/// Rows whose start lies strictly inside a multiline string node (the second and
/// later rows of a `"…"` or `///…///` literal). Their contents are preserved
/// verbatim, never re-indented.
fn collect_literal_rows(root: M2Node<'_>, line_count: usize) -> (Vec<bool>, Vec<bool>) {
    let mut literal = vec![false; line_count];
    let mut literal_starts = vec![false; line_count];
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_string_literal() {
            let start_row = node.start_position().row;
            let end_row = node.end_position().row;
            if start_row < end_row {
                if let Some(slot) = literal_starts.get_mut(start_row) {
                    *slot = true;
                }
            }
            for row in (start_row + 1)..=end_row {
                if let Some(slot) = literal.get_mut(row) {
                    *slot = true;
                }
            }
        }
        stack.extend(node.children());
    }
    (literal, literal_starts)
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

/// `bracket_depth(row)` = (distinct open-rows of multiline brackets currently
/// open across this row) minus (distinct groups whose closer begins this row).
/// The latter dedents a leading closing delimiter back to the opener level.
fn bracket_depth(row: usize, brackets: &[BracketGroup], line_leads: &[usize]) -> usize {
    let leading_blank = line_leads.get(row).copied().unwrap_or(0);
    let mut active = Vec::new();
    let mut leading_closed = Vec::new();
    for group in brackets {
        if group.open_row < row && row <= group.close_row && !active.contains(&group.open_row) {
            active.push(group.open_row);
        }
        if group.close_row == row
            && group.closing_delimiter_column == leading_blank
            && !leading_closed.contains(&group.open_row)
        {
            leading_closed.push(group.open_row);
        }
    }
    active.len().saturating_sub(leading_closed.len())
}

/// `continuation(row)` is a flat +1 for a line that continues an expression or a
/// clause body broken onto a later line (see the rule cases inline).
fn line_continuation(
    row: usize,
    root: M2Node<'_>,
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

    // (b) The first token starts a clause body whose keyword is on an earlier row
    // (`then\n  f(x)`, `else\n  c`, `do\n  body`).
    if is_clause_body_first_token(first, row) {
        return 1;
    }

    // (c) The first token is an `else`/`then` keyword whose controlling `if` is
    // not standalone (a ternary `x := if … then\n … else …`); a standalone `if`
    // aligns its `else` via bracket_depth instead.
    if is_dangling_clause_keyword(first, row, line_leads) {
        return 1;
    }

    0
}

/// The leftmost leaf node starting on `row`.
fn first_leaf_on_row(root: M2Node<'_>, row: usize) -> Option<M2Node<'_>> {
    let mut best: Option<M2Node<'_>> = None;
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
    node: M2Node<'_>,
    row: usize,
    compact_factor_operators: bool,
) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        let right_field = if parent.binary_operator().is_some() {
            Some("right")
        } else if parent.is::<LambdaExpression>() {
            Some("body")
        } else {
            None
        };
        if let Some((operator, right)) = right_field.and_then(|right_field| {
            Some((
                parent.child_by_field_name("operator")?,
                parent.child_by_field_name(right_field)?,
            ))
        }) {
            let assignment_conditional = parent.is_assignment() && right.is::<IfStatement>();
            if right.start_byte() == node.start_byte()
                && operator.start_position().row < row
                && is_spaced_line_final_operator(operator.text(), compact_factor_operators)
                && !assignment_conditional
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
fn is_clause_body_first_token(node: M2Node<'_>, row: usize) -> bool {
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
            for (keyword, field) in [
                (direct_keyword::<Token![list]>(parent), "listed_value"),
                (direct_keyword::<Token![do]>(parent), "ignored_value"),
            ] {
                let Some((keyword, body)) = keyword.zip(parent.child_by_field_name(field)) else {
                    continue;
                };
                if body.start_byte() == node.start_byte() && keyword.start_position().row < row {
                    return true;
                }
            }
        }
        if parent.is_except_clause() {
            let clause_body =
                direct_keyword::<Token![do]>(parent).zip(parent.child_by_field_name("value"));
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
///   * An orphaned keyword that tree-sitter, recovering a line-broken ternary,
///     demotes to a leading `symbol` with no enclosing `if` — always the
///     continuation of the ternary begun on an earlier line.
fn is_dangling_clause_keyword(node: M2Node<'_>, row: usize, line_leads: &[usize]) -> bool {
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
fn is_clause_keyword_leaf(node: M2Node<'_>) -> bool {
    if node.is_then_or_else_keyword() {
        return true;
    }
    node.is::<Symbol>()
        && (matches_token::<Token![else]>(node.text())
            || matches_token::<Token![then]>(node.text()))
}

/// The nearest enclosing conditional owner of a clause keyword.
fn enclosing_clause_owner(node: M2Node<'_>) -> Option<M2Node<'_>> {
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
fn control_statement_is_standalone(node: M2Node<'_>, line_leads: &[usize]) -> bool {
    let row = node.start_position().row;
    let column = node.start_position().column;
    line_leads.get(row).copied().unwrap_or(0) >= column
}

fn if_statement_is_else_if(node: M2Node<'_>) -> bool {
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

fn normalize_whitespace(text: &str, options: &FormatOptions) -> String {
    let Some(mut parser) = M2Parser::new() else {
        return text.to_string();
    };
    let Some(root) = parser.parse(text) else {
        return text.to_string();
    };

    let mut edits = Vec::new();
    collect_format_edits(root, text, options, &mut edits);
    apply_format_edits(text, edits)
}

fn collect_format_edits(
    node: M2Node<'_>,
    text: &str,
    options: &FormatOptions,
    edits: &mut Vec<FormatEdit>,
) {
    if node.is_missing() {
        return;
    }

    if !node.is_error() {
        if node.is_comma() {
            push_comma_whitespace_edits(text, node, edits);
        }

        if node.is_semicolon() {
            let break_after_semicolon =
                options.break_after_semicolon && !semicolon_terminates_collection_entry(node);
            push_semicolon_whitespace_edits(text, node, break_after_semicolon, edits);
        }

        if let Some(operator) = node.child_by_field_name("operator") {
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
                        push_prefix_operator_whitespace_edits(text, operator, edits)
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
fn operator_spacing(parent: M2Node<'_>, operator: &str) -> OperatorSpacing {
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
        Token![<<],
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
fn binary_operator_all_factors(node: M2Node<'_>) -> bool {
    let Some(left) = node.child_by_field_name("left") else {
        return false;
    };
    let Some(right) = node.child_by_field_name("right") else {
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

fn is_parenthesized_call(node: M2Node<'_>) -> bool {
    if node.binary_operator().is_none() {
        return false;
    }

    let Some(operator) = node.child_by_field_name("operator") else {
        return false;
    };
    let Some(right) = node.child_by_field_name("right") else {
        return false;
    };

    // A call's argument list is a `sequence` (`f(a, b)`, `f()`) or, for a single
    // parenthesized argument, a `parenthesized_expression` (`f(x)`).
    operator.is_implicit_application() && (right.is::<Sequence>() || right.is_holder())
}

/// Whether a parenthesized call `f(...)` is the head of a `:=` method install
/// (`f(Types) := fn`) — i.e. it is the left operand of an enclosing `:=`. Such a
/// head is installation syntax and is spaced rather than compacted.
fn is_method_installation_call_head(node: M2Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.binary_operator().is_none() {
        return false;
    }
    let Some(operator) = parent.child_by_field_name("operator") else {
        return false;
    };
    if !operator.is::<Token![:=]>() {
        return false;
    }
    parent.child_by_field_name("left").is_some_and(|left| {
        left.start_byte() == node.start_byte() && left.end_byte() == node.end_byte()
    })
}

fn push_call_gap_whitespace_edit(
    node: M2Node<'_>,
    text: &str,
    edits: &mut Vec<FormatEdit>,
    replacement: &'static str,
) {
    let Some(left) = node.child_by_field_name("left") else {
        return;
    };
    let Some(right) = node.child_by_field_name("right") else {
        return;
    };
    let gap = &text[left.end_byte()..right.start_byte()];
    if !gap.bytes().all(|byte| matches!(byte, b' ' | b'\t')) {
        return;
    }
    edits.push(FormatEdit {
        start_byte: left.end_byte(),
        end_byte: right.start_byte(),
        replacement,
    });
}

fn is_adjacent_factor(node: M2Node<'_>) -> bool {
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

fn push_operator_whitespace_edits(text: &str, operator: M2Node<'_>, edits: &mut Vec<FormatEdit>) {
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
        replacement: " ",
    });
    edits.push(FormatEdit {
        start_byte: operator.end_byte(),
        end_byte,
        replacement: " ",
    });
}

fn push_compact_operator_whitespace_edits(
    text: &str,
    operator: M2Node<'_>,
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
        replacement: "",
    });
    edits.push(FormatEdit {
        start_byte: operator.end_byte(),
        end_byte,
        replacement: "",
    });
}

fn push_prefix_operator_whitespace_edits(
    text: &str,
    operator: M2Node<'_>,
    edits: &mut Vec<FormatEdit>,
) {
    let Some(end_byte) = same_line_horizontal_whitespace_end(text, operator.end_byte()) else {
        return;
    };

    edits.push(FormatEdit {
        start_byte: operator.end_byte(),
        end_byte,
        replacement: "",
    });
}

fn push_comma_whitespace_edits(text: &str, comma: M2Node<'_>, edits: &mut Vec<FormatEdit>) {
    if let Some(start_byte) = same_line_horizontal_whitespace_start(text, comma.start_byte()) {
        edits.push(FormatEdit {
            start_byte,
            end_byte: comma.start_byte(),
            replacement: "",
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
        replacement,
    });
}

fn semicolon_terminates_collection_entry(semicolon: M2Node<'_>) -> bool {
    semicolon
        .parent()
        .and_then(|muted| muted.parent())
        .is_some_and(|container| container.is_collection_expression())
}

fn push_semicolon_whitespace_edits(
    text: &str,
    semicolon: M2Node<'_>,
    break_after_semicolon: bool,
    edits: &mut Vec<FormatEdit>,
) {
    if let Some(start_byte) = same_line_horizontal_whitespace_start(text, semicolon.start_byte()) {
        edits.push(FormatEdit {
            start_byte,
            end_byte: semicolon.start_byte(),
            replacement: "",
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
            replacement,
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

fn apply_format_edits(text: &str, mut edits: Vec<FormatEdit>) -> String {
    if edits.is_empty() {
        return text.to_string();
    }

    edits.sort_by_key(|edit| (edit.start_byte, edit.end_byte));
    let mut filtered_edits = Vec::with_capacity(edits.len());
    for edit in edits {
        if edit.start_byte == edit.end_byte && edit.replacement.is_empty() {
            continue;
        }
        if filtered_edits
            .last()
            .is_some_and(|previous: &FormatEdit| previous.end_byte > edit.start_byte)
        {
            return text.to_string();
        }
        filtered_edits.push(edit);
    }

    let mut formatted = text.to_string();
    for edit in filtered_edits.into_iter().rev() {
        formatted.replace_range(edit.start_byte..edit.end_byte, edit.replacement);
    }
    formatted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_consecutive_full_line_comments_as_one_range() {
        assert_eq!(
            folding_ranges_for_text(
                "-- first line\n  -- second line\n-- third line\nx = 1\n-- lone line\n"
            ),
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
            folding_ranges_for_text("-- one\n-- two\n\n-- three\n-- four\n"),
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
            folding_ranges_for_text("-* first\nmiddle\nlast *-\nx = 1\n-* another\ncomment *-\n"),
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
        let ranges = folding_ranges("-- first\n-- second\n");

        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].kind, Some(FoldingRangeKind::Comment));
    }

    #[test]
    fn does_not_treat_inline_or_string_markers_as_comment_blocks() {
        assert!(
            folding_ranges_for_text("x = \"-- not a comment\"\ny = 1 -- inline\n-- lone\n")
                .is_empty()
        );
    }

    #[test]
    fn trims_trailing_whitespace_without_reflowing_code() {
        assert_eq!(
            format_document_text("x := 1  \n  y = 2\t\n"),
            "x := 1\ny = 2\n"
        );
        assert_eq!(format_document_text("x := 1  "), "x := 1");
    }

    #[test]
    fn spaces_parenthesized_method_install_head() {
        // An install head reads as installation syntax: `f (Types) := …`.
        assert_eq!(
            format_document_text("f(ZZ,ZZ) := (i,j) -> i\n"),
            "f (ZZ, ZZ) := (i, j) -> i\n"
        );
    }

    #[test]
    fn ordinary_call_stays_compact() {
        // A plain call is not an install; it keeps `f(args)` with no space.
        assert_eq!(format_document_text("g(5)\n"), "g(5)\n");
        assert_eq!(format_document_text("h(1, 2)\n"), "h(1, 2)\n");
    }

    #[test]
    fn normalizes_operator_whitespace() {
        assert_eq!(format_document_text("x+y\n"), "x + y\n");
        assert_eq!(format_document_text("x+ y\n"), "x + y\n");
        assert_eq!(format_document_text("x   +y\n"), "x + y\n");
        assert_eq!(format_document_text("x\t+\t y\n"), "x + y\n");
        assert_eq!(format_document_text("x:=y\n"), "x := y\n");
        assert_eq!(format_document_text("f:=x->x+1\n"), "f := x -> x + 1\n");
        assert_eq!(
            format_document_text("f(x, Strategy=>LongPolynomial)\n"),
            "f(x, Strategy => LongPolynomial)\n"
        );
        assert_eq!(format_document_text("8*delta/2\n"), "8 * delta / 2\n");
        assert_eq!(format_document_text("x**y // z\n"), "x ** y // z\n");
        assert_eq!(format_document_text("x<<y\n"), "x << y\n");
        assert_eq!(format_document_text("x ^ y # 0\n"), "x^y#0\n");
    }

    #[test]
    fn compacts_unary_prefix_operators() {
        assert_eq!(
            format_document_text("iCoeff := - jRow#0;\n"),
            "iCoeff := -jRow#0;\n"
        );
        assert_eq!(format_document_text("x := not true\n"), "x := not true\n");
        assert_eq!(format_document_text("a-b\n"), "a - b\n");
    }

    #[test]
    fn indents_operator_continuations() {
        let options = FormatOptions::new(3, true);

        assert_eq!(
            format_document_text_with_options("a + \nb +\nc\n", &options),
            "a +\n   b +\n   c\n"
        );
        assert_eq!(
            format_document_text_with_options("f := (a +\nb +\nc)\n", &options),
            "f := (a +\n      b +\n      c)\n"
        );
        assert_eq!(
            format_document_text_with_options("-----\ntop = 1\n", &options),
            "-----\ntop = 1\n"
        );
        assert_eq!(
            format_document_text_with_options(
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
        assert_eq!(format_document_text("f(x,y)\n"), "f(x, y)\n");
        assert_eq!(format_document_text("f(x ,y)\n"), "f(x, y)\n");
        assert_eq!(format_document_text("f(x,  y)\n"), "f(x, y)\n");
        assert_eq!(format_document_text("f(x ,  y)\n"), "f(x, y)\n");
        assert_eq!(format_document_text("f(x,)\n"), "f(x,)\n");
        assert_eq!(format_document_text("f(x,\ny)\n"), "f(x,\n    y)\n");
        assert_eq!(
            format_document_text("values := (,a,,)\n"),
            "values := (,a,,)\n",
            "explicit null slots remain visually empty"
        );
    }

    #[test]
    fn wraps_long_calls_at_parsed_comma_boundaries() {
        let options = FormatOptions {
            max_line_width: Some(28),
            ..FormatOptions::default()
        };

        assert_eq!(
            format_document_text_with_options(
                "result = concatenate(alpha, beta, gamma)\n",
                &options
            ),
            "result = concatenate(alpha,\n    beta, gamma)\n"
        );
    }

    #[test]
    fn hard_width_triggers_wrapping_toward_the_soft_width() {
        let input = "result = concatenate(alpha, beta, gamma)\n";
        let below_hard_limit = FormatOptions {
            soft_line_width: Some(28),
            max_line_width: Some(50),
            ..FormatOptions::default()
        };
        assert_eq!(
            format_document_text_with_options(input, &below_hard_limit),
            input
        );

        let above_hard_limit = FormatOptions {
            soft_line_width: Some(28),
            max_line_width: Some(35),
            ..FormatOptions::default()
        };
        assert_eq!(
            format_document_text_with_options(input, &above_hard_limit),
            "result = concatenate(alpha,\n    beta, gamma)\n"
        );
    }

    #[test]
    fn wraps_long_collections_with_the_shared_rule_engine() {
        let options = FormatOptions {
            max_line_width: Some(26),
            ..FormatOptions::default()
        };

        assert_eq!(
            format_document_text_with_options("values = {alpha, beta, gamma, delta}\n", &options),
            "values = {alpha, beta,\n    gamma, delta}\n"
        );
    }

    #[test]
    fn wraps_long_binary_expressions_after_spaced_operators() {
        let options = FormatOptions {
            max_line_width: Some(26),
            ..FormatOptions::default()
        };

        assert_eq!(
            format_document_text_with_options("result = alpha + beta + gamma + delta\n", &options),
            "result = alpha + beta +\n    gamma + delta\n"
        );
    }

    #[test]
    fn wrapping_preserves_literal_contents_and_is_idempotent() {
        let options = FormatOptions {
            max_line_width: Some(20),
            ..FormatOptions::default()
        };
        let formatted = format_document_text_with_options(
            "value = \"alpha, beta + gamma and a very long literal\"\n",
            &options,
        );

        assert_eq!(
            formatted,
            "value =\n    \"alpha, beta + gamma and a very long literal\"\n"
        );
        assert_eq!(
            format_document_text_with_options(&formatted, &options),
            formatted
        );
    }

    #[test]
    fn wrapping_can_be_disabled_and_preserves_crlf_when_enabled() {
        let input = "result = concatenate(alpha, beta, gamma)\r\n";
        let disabled = FormatOptions {
            max_line_width: None,
            ..FormatOptions::default()
        };
        assert_eq!(format_document_text_with_options(input, &disabled), input);

        let enabled = FormatOptions {
            max_line_width: Some(28),
            ..FormatOptions::default()
        };
        assert_eq!(
            format_document_text_with_options(input, &enabled),
            "result = concatenate(alpha,\r\n    beta, gamma)\r\n"
        );
    }

    #[test]
    fn wrapped_calls_collections_and_operators_remain_parseable_and_stable() {
        let options = FormatOptions {
            max_line_width: Some(24),
            ..FormatOptions::default()
        };
        let mut parser = M2Parser::new().expect("Macaulay2 parser should load");

        for source in [
            "result = concatenate(alpha, beta, gamma)\n",
            "values = {alpha, beta, gamma, delta}\n",
            "result = alpha + beta + gamma + delta\n",
            "nested = outer(alpha, inner(beta, gamma), delta)\n",
        ] {
            let formatted = format_document_text_with_options(source, &options);
            let has_error = parser
                .parse(&formatted)
                .map(|root| root.has_error())
                .expect("formatted fixture should parse");
            assert!(!has_error, "wrapped source should parse:\n{formatted}");
            assert_eq!(
                format_document_text_with_options(&formatted, &options),
                formatted
            );
        }
    }

    #[test]
    fn normalizes_semicolon_whitespace() {
        assert_eq!(format_document_text("i=0;j=0;\n"), "i = 0;\nj = 0;\n");
        assert_eq!(format_document_text("i = 0 ; j = 0\n"), "i = 0;\nj = 0\n");
        assert_eq!(format_document_text("i = 0;\n j = 0\n"), "i = 0;\nj = 0\n");
        assert_eq!(
            format_document_text("\"x;y\" -- comment a;b\n"),
            "\"x;y\" -- comment a;b\n"
        );
        assert_eq!(
            format_document_text("f := (i=0;j=0;)\n"),
            "f := (i = 0;\n    j = 0;\n)\n"
        );
    }

    #[test]
    fn line_end_open_brackets_create_single_extra_indent_scope() {
        assert_eq!(
            format_document_text(
                "scan(docKeys, key -> (\nbaseName := recordNameFromDocKey key;\nif db#?key then result#baseName = append(result#baseName ?? {}, hashTable {\"key\" => key, \"raw\" => db#key});\n)\n);\n"
            ),
            "scan(docKeys, key -> (\n    baseName := recordNameFromDocKey key;\n    if db#?key then\n        result#baseName = append(result#baseName ?? {}, hashTable {\"key\" => key, \"raw\" => db#key});\n)\n);\n"
        );
    }

    #[test]
    fn inline_open_brackets_do_not_force_multiline_closer() {
        // A ternary `if` (value position) is left inline, so its inline
        // `then(...)` brackets are not pushed onto their own lines.
        assert_eq!(
            format_document_text("x := if a then(b) else(c)\n"),
            "x := if a then(b) else(c)\n"
        );
    }

    #[test]
    fn uses_configured_indent_width() {
        let options = FormatOptions::new(2, true);

        assert_eq!(
            format_document_text_with_options("f := (i=0;j=0;)\n", &options),
            "f := (i = 0;\n  j = 0;\n)\n"
        );
        assert_eq!(
            format_document_text_with_options("f := (i=0;j=0;)\n", &FormatOptions::new(4, false)),
            "f := (i = 0;\n\tj = 0;\n)\n"
        );
    }

    #[test]
    fn can_restore_compact_factor_operators() {
        let options = FormatOptions {
            compact_factor_operators: true,
            ..FormatOptions::default()
        };

        assert_eq!(
            format_document_text_with_options("8 * delta / 2\n", &options),
            "8*delta/2\n"
        );
    }

    #[test]
    fn can_keep_semicolon_separated_statements_inline() {
        let options = FormatOptions {
            break_after_semicolon: false,
            ..FormatOptions::default()
        };

        assert_eq!(
            format_document_text_with_options("i=0;j=0;\n", &options),
            "i = 0; j = 0;\n"
        );
    }

    #[test]
    fn server_settings_override_lsp_format_options() {
        let settings = crate::settings::ServerSettings::from_value(&serde_json::json!({
            "formatting": {
                "indentWidth": 2,
                "useTabs": false,
                "compactFactorOperators": true,
                "breakAfterSemicolon": false
            }
        }))
        .unwrap();
        let options = FormatOptions::from_configuration(8, false, settings.formatting());

        assert_eq!(
            format_document_text_with_options("f := (\na * b;c\n)\n", &options),
            "f := (\n  a*b; c\n)\n"
        );
    }

    #[test]
    fn normalizes_parenthesized_call_whitespace() {
        assert_eq!(format_document_text("f (x,y)\n"), "f(x, y)\n");
        assert_eq!(format_document_text("f \t (x)\n"), "f(x)\n");
        assert_eq!(
            format_document_text("random (source gens Ip2, R^{-d})\n"),
            "random(source gens Ip2, R^{-d})\n"
        );
        assert_eq!(format_document_text("f x\n"), "f x\n");
    }

    #[test]
    fn preserves_non_operator_text() {
        assert_eq!(
            format_document_text("\"x+y\" -- comment a+b\n"),
            "\"x+y\" -- comment a+b\n"
        );
        assert_eq!(format_document_text("-x+y\n"), "-x + y\n");
        assert_eq!(format_document_text("R=QQ[a..d]\n"), "R = QQ[a..d]\n");
        assert_eq!(format_document_text("x#0 + y\n"), "x#0 + y\n");
        assert_eq!(format_document_text("x\n+y\n"), "x\n+y\n");
    }

    #[test]
    fn preserves_multiline_string_contents() {
        assert_eq!(
            format_document_text("x=///\n  keep spaces  \n///\ny=1\n"),
            "x = ///\n  keep spaces  \n///\ny = 1\n"
        );
        assert_eq!(
            format_document_text("x=\"first\n  keep spaces  \nlast\"\ny=1\n"),
            "x = \"first\n  keep spaces  \nlast\"\ny = 1\n"
        );
        assert_eq!(
            format_document_text("  x=///first line  \n  keep spaces  \n///\n"),
            "x = ///first line  \n  keep spaces  \n///\n",
            "formatting the prefix must not trim content before a literal newline"
        );
    }

    #[test]
    fn preserves_crlf_line_endings() {
        // A CRLF document must keep CRLF: the formatter must not silently rewrite
        // every line ending to LF (which would churn the whole file on save).
        assert_eq!(format_document_text("x=1\r\ny=2\r\n"), "x = 1\r\ny = 2\r\n");
        // Verbatim multiline-string content keeps its CRLF too.
        assert_eq!(
            format_document_text("x=///\r\n  keep  \r\n///\r\n"),
            "x = ///\r\n  keep  \r\n///\r\n"
        );
    }

    #[test]
    fn expands_a_single_then_body_when_line_breaks_are_parseable() {
        assert_eq!(
            format_document_text("if a then b else c\n"),
            "if a then b else c\n"
        );
        assert_eq!(format_document_text("if a then b\n"), "if a then\n    b\n");
    }

    #[test]
    fn leaves_ternary_if_untouched() {
        assert_eq!(
            format_document_text("x := if a then f(x) else g(y)\n"),
            "x := if a then f(x) else g(y)\n"
        );
    }

    #[test]
    fn formats_valid_nodes_when_other_nodes_have_parse_errors() {
        assert_eq!(
            format_document_text("x+y\nbad := (\na:=b\n"),
            "x + y\nbad := (\n    a := b\n"
        );
    }

    #[test]
    fn formats_example_file_despite_parser_gaps() {
        let formatted =
            format_document_text(include_str!("../../tests/fixtures/formatting_example.m2"));

        assert!(formatted.contains("k := ceiling((-3 + sqrt(9.0 + 8 * delta)) / 2);"));
        assert!(formatted.contains("K = ZZ / 101;"));
        assert!(formatted.contains("randomPlanePoints = (delta, R) -> ("));
        assert!(formatted.contains("random(source gens Ip2, R^{-d})"));
        assert!(formatted.contains("SyzygyLimit => 60"));
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
        let formatted = format_document_text(chain);

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
        assert_eq!(format_document_text(&formatted), formatted);
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
            format_document_text(chain),
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
            ..FormatOptions::default()
        };
        assert_eq!(
            format_document_text_with_options(text, &compact),
            "value := (\n    if a then one else if b then two else three\n)\n"
        );

        let multiline = FormatOptions {
            control_flow_layout: ControlFlowLayout::Multiline,
            ..FormatOptions::default()
        };
        assert_eq!(
            format_document_text_with_options(text, &multiline),
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
            format_document_text(text),
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
            format_document_text(text),
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
            format_document_text(text),
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

        let formatted = format_document_text(text);
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
        assert_eq!(format_document_text(&formatted), formatted);
    }

    #[test]
    fn keeps_delimited_control_bodies_beside_every_clause_keyword() {
        let options = FormatOptions {
            control_flow_layout: ControlFlowLayout::Multiline,
            ..FormatOptions::default()
        };
        let mut parser = M2Parser::new().expect("Macaulay2 parser should load");

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
            let formatted = format_document_text_with_options(source, &options);

            assert!(
                formatted.contains(clause),
                "the clause opener should remain inline:\n{formatted}"
            );
            assert!(
                parser
                    .parse(&formatted)
                    .is_some_and(|root| !root.has_error()),
                "the formatted control statement should parse:\n{formatted}"
            );
            assert_eq!(
                format_document_text_with_options(&formatted, &options),
                formatted
            );
        }
    }

    #[test]
    fn keeps_the_except_binder_with_except_and_breaks_after_do() {
        let text = "value := (\ntry 1/x then result except err do err\n)\n";

        assert_eq!(
            format_document_text(text),
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
            format_document_text("if cond then (\na;\nb\n) else (\nc;\nd\n)\n"),
            "if cond then (\n    a;\n    b\n) else (\n    c;\n    d\n)\n"
        );
    }

    #[test]
    fn keeps_top_level_symbols_unindented_after_comment_dividers() {
        let formatted =
            format_document_text(include_str!("../../tests/fixtures/comment_dividers.m2"));

        assert!(formatted.contains("\nprimitive = (L) -> ("));
        assert!(formatted.contains("\ntoZZ = (L) -> ("));
        assert!(!formatted.contains("\n     toZZ = (L) -> ("));
    }
}
