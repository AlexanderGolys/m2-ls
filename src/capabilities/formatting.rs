//! Syntax-guided whitespace formatting and folding for Macaulay2 source.
//!
//! Each formatting pass reads one immutable parse and edits a coordinate-mapped
//! layout view. Whitespace changes update that mapping rather than reparsing the
//! source between passes. String literals, comments, and invalid regions remain
//! protected from edits. The pass order resolves spacing before control layout,
//! indentation, wrapping, and final closer placement.
//!
//! Folding combines multiline literals and comments with indentation boundaries;
//! it does not execute source or require the external builtin catalog.

use std::collections::VecDeque;

use m2_syn::nodes::{
    Component, ElseClause, Expr, ExprFor as ForLoop, ExprIf as IfStatement,
    ExprTry as TryStatement, ExprWhile as WhileLoop, List, LoopBody, PackedContent, Sequence,
    Symbol, ThenClause,
};
use m2_syn::visit::{self, Visit};
use m2_syn::{Delimiter, DelimiterValue, Token};
use tower_lsp::lsp_types::{
    DocumentFormattingOptions, FoldingRange, FoldingRangeKind, FoldingRangeProviderCapability,
    OneOf, TextEdit,
};

use crate::document::DocumentSnapshot;
use crate::node_metadata::{matches_token, SyntaxSnapshot};

mod layout;
use crate::settings::FormattingSettings;
use crate::source::{SourceNavigation, SyntaxSource};
use crate::syntax::{AssignmentSyntax, ExpressionSyntax};
use layout::LayoutSource;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
/// Placement of control-flow keywords and their bodies during formatting.
#[non_exhaustive]
pub enum ControlFlowLayout {
    /// Keeps control-flow expressions compact when their syntax permits it.
    Compact,
    /// Places control-flow clause bodies on separate lines.
    Multiline,
    /// Uses multiline bodies while retaining compact else placement where possible.
    #[default]
    MultilineCompactElse,
    /// `then` and `else` each start a line under their `if`, followed by
    /// their body; loops and `try` are laid out as in `Multiline`.
    LeadingKeywords,
}

/// Formatting settings resolved into the layout parameters the formatter
/// consults: indentation, line widths, control-flow layout and spacing
/// preferences. Built from [`FormattingSettings`]; the default follows the
/// default settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatOptions {
    /// Whitespace unit inserted for each indentation level.
    indent: String,
    /// Display columns occupied by a tab during width calculation.
    tab_width: usize,
    /// Preferred wrapping width, absent when wrapping is disabled.
    soft_line_width: Option<usize>,
    /// Maximum permitted line width before wrapping is required.
    max_line_width: Option<usize>,
    /// Configured placement of control keywords and their bodies.
    control_flow_layout: ControlFlowLayout,
    /// Whether multiplicative operators omit surrounding spaces.
    compact_factor_operators: bool,
    /// Whether a semicolon begins a new formatted statement line.
    break_after_semicolon: bool,
    /// Whether existing line boundaries are preserved.
    keep_line_breaks: bool,
    /// Explicit brace-spacing preference, absent to preserve the existing style.
    space_in_braces: Option<bool>,
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
        let keep_line_breaks = settings.keep_line_breaks;
        let (soft_line_width, hard_line_width) = if keep_line_breaks {
            (None, None)
        } else {
            settings.line_widths()
        };
        Self {
            indent: if use_tabs {
                "\t".to_string()
            } else {
                " ".repeat(tab_width)
            },
            tab_width,
            soft_line_width: soft_line_width.map(|width| width as usize),
            max_line_width: hard_line_width.map(|width| width as usize),
            control_flow_layout: if keep_line_breaks {
                ControlFlowLayout::Compact
            } else {
                settings.control_flow_layout
            },
            compact_factor_operators: settings.compact_factor_operators,
            break_after_semicolon: settings.break_after_semicolon && !keep_line_breaks,
            keep_line_breaks,
            space_in_braces: settings.space_in_braces,
        }
    }
}

/// Advertises whole-document formatting support in the initialization response.
pub fn document_formatting_provider_capability() -> Option<OneOf<bool, DocumentFormattingOptions>> {
    Some(OneOf::Left(true))
}

/// Advertises source-code and comment folding support in the initialization response.
pub fn folding_range_provider_capability() -> Option<FoldingRangeProviderCapability> {
    Some(FoldingRangeProviderCapability::Simple(true))
}

/// Computes the protocol edits needed to apply the configured whitespace layout.
pub fn document_formatting_text_edits(
    source: &DocumentSnapshot,
    tab_size: u32,
    insert_spaces: bool,
    settings: &FormattingSettings,
) -> Vec<TextEdit> {
    let text = source.text();
    let options = FormatOptions::from_settings(tab_size, insert_spaces, settings);
    let formatted = format_document_with_options(source.syntax(), &options);
    if formatted == text {
        return Vec::new();
    }

    vec![TextEdit {
        range: source.full_range(),
        new_text: formatted,
    }]
}

/// Foldable code, literal, and comment regions derived from the document syntax.
pub fn folding_ranges(syntax: &SyntaxSnapshot) -> Vec<FoldingRange> {
    folding_ranges_for_syntax(syntax)
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

/// A whitespace replacement expressed in the current layout's byte offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FormatEdit {
    /// Inclusive UTF-8 byte offset of the represented source interval.
    start_byte: usize,
    /// Exclusive UTF-8 byte offset of the represented source interval.
    end_byte: usize,
    /// Text replacing the selected source interval.
    replacement: String,
}

/// Format the whole document: whitespace normalization around operators and
/// punctuation, tree-derived indentation, and width-based wrapping at parsed
/// syntax boundaries.
pub fn format_document_with_options(syntax: &SyntaxSnapshot, options: &FormatOptions) -> String {
    formatted_layout(syntax, options).into_text()
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
    let syntax = SyntaxSnapshot::parse(&text, Some(source.syntax()));
    let layout = formatted_layout(&syntax, options);
    edit.new_text = layout.text_for_bytes(bytes.start..end).to_string();
}

/// Runs the ordered whitespace passes against one immutable syntax snapshot.
fn formatted_layout(syntax: &SyntaxSnapshot, options: &FormatOptions) -> LayoutSource {
    let newline = detect_line_ending(syntax.source());
    let mut layout = LayoutSource::new(syntax);
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
    if options.keep_line_breaks {
        passes.retain(|pass| !pass.moves_line_breaks());
    }
    while let Some(pass) = passes.pop_front() {
        let edits = match pass {
            FormatPass::Spacing => normalize_whitespace(syntax, &layout, options),
            FormatPass::LeadingOperators => {
                lead_rows_with_operators(syntax, &layout, options, newline)
            }
            FormatPass::Options => break_option_tables(syntax, &layout, newline),
            FormatPass::EmptyBodies => inline_empty_bodies(syntax, &layout),
            FormatPass::ControlFlow => format_control_flow(syntax, &layout, options, newline),
            FormatPass::Indent => reindent_from_syntax(syntax, &layout, options),
            FormatPass::Wrap => wrap_long_lines(syntax, &layout, options, newline),
            FormatPass::Closers => separate_line_end_bracket_closers(syntax, &layout, newline),
        };
        if layout.apply(edits) && matches!(pass, FormatPass::Wrap) {
            passes.push_front(FormatPass::Wrap);
            passes.push_front(FormatPass::Indent);
        }
    }
    layout
}

/// One ordered whitespace transformation in the formatter pipeline.
#[derive(Clone, Copy)]
enum FormatPass {
    /// Normalizes spaces around operators, separators, and delimiters.
    Spacing,
    /// Moves permitted leading operators into the configured line layout.
    LeadingOperators,
    /// Selects the callable's option-passing policy.
    Options,
    /// Compacts syntactically empty construct bodies.
    EmptyBodies,
    /// Applies the configured placement of control-flow clauses.
    ControlFlow,
    /// Applies indentation derived from syntax and source line structure.
    Indent,
    /// Wraps long lines at syntactically permitted boundaries.
    Wrap,
    /// Separates closing delimiters according to the surrounding line layout.
    Closers,
}

impl FormatPass {
    /// Whether the pass adds, removes, or moves line breaks; such passes are
    /// skipped when line breaks are kept as written.
    fn moves_line_breaks(self) -> bool {
        matches!(
            self,
            Self::LeadingOperators | Self::Options | Self::EmptyBodies | Self::Closers
        )
    }
}

/// Separates line end bracket closers by inserting whitespace at permitted source boundaries.
fn separate_line_end_bracket_closers(
    syntax: &SyntaxSnapshot,
    layout: &LayoutSource,
    newline: &'static str,
) -> Vec<FormatEdit> {
    let mut visitor = ClosingDelimiterBreaks {
        layout,
        newline,
        edits: Vec::new(),
    };
    syntax.visit(&mut visitor);
    visitor.edits
}

/// Separates typed closing delimiters from content when the opener ends a line.
struct ClosingDelimiterBreaks<'a> {
    /// Current source coordinate mapping.
    layout: &'a LayoutSource,
    /// Line ending of the document.
    newline: &'static str,
    /// Whitespace replacements discovered so far.
    edits: Vec<FormatEdit>,
}

impl ClosingDelimiterBreaks<'_> {
    /// Applies the layout rule to one generated delimiter pair.
    fn consider(&mut self, delimiter: Delimiter) {
        let Some(opener) = self.layout.syntax_range(&delimiter.opening_span()) else {
            return;
        };
        let Some(closer) = delimiter
            .closing_span()
            .and_then(|span| self.layout.syntax_range(&span))
        else {
            return;
        };
        let text = self.layout.text();
        if self.layout.point_for_byte(opener.start).row
            >= self.layout.point_for_byte(closer.start).row
            || !text[opener.end..]
                .split(['\n', '\r'])
                .next()
                .is_some_and(|suffix| suffix.bytes().all(|byte| matches!(byte, b' ' | b'\t')))
        {
            return;
        }
        let Some(start_byte) = same_line_horizontal_whitespace_start(text, closer.start) else {
            return;
        };
        self.edits.push(FormatEdit {
            start_byte,
            end_byte: closer.start,
            replacement: self.newline.to_string(),
        });
    }
}

impl<'ast> Visit<'ast> for ClosingDelimiterBreaks<'_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        let delimiter = expression.delimiter();
        if let Some(delimiter) = delimiter {
            self.consider(delimiter);
        }
        visit::visit_expr(self, expression);
    }

    fn visit_assignment_list(&mut self, list: &'ast m2_syn::nodes::AssignmentList) {
        self.consider(list.contents.delimiter.delimiter());
        visit::visit_assignment_list(self, list);
    }
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
fn break_option_tables(
    syntax: &SyntaxSnapshot,
    layout: &LayoutSource,
    newline: &'static str,
) -> Vec<FormatEdit> {
    let mut visitor = OptionTableBreaks {
        layout,
        newline,
        edits: Vec::new(),
    };
    syntax.visit(&mut visitor);
    visitor.edits
}

/// Adds breaks around the entries of typed option tables.
struct OptionTableBreaks<'a> {
    /// Current whitespace coordinates.
    layout: &'a LayoutSource,
    /// Line ending used by this document.
    newline: &'static str,
    /// Whitespace replacements gathered from visited lists.
    edits: Vec<FormatEdit>,
}

impl<'ast> Visit<'ast> for OptionTableBreaks<'_> {
    fn visit_list(&mut self, list: &'ast List) {
        if let Some(PackedContent::Pack(pack)) = &list.elements {
            if pack.pack.iter().all(|component| matches!(component, Component::Expr(value) if matches!(value.as_ref(), Expr::Option(_)))) {
                let delimiter = list.delimiter.delimiter();
                let first = list.statements.first().and_then(|statement| self.layout.syntax_range(statement)).or_else(|| self.layout.syntax_range(pack.pack.first()));
                let opening = self.layout.syntax_range(&delimiter.opening_span());
                let closing = delimiter.closing_span().and_then(|span| self.layout.syntax_range(&span));
                let last = pack.pack.iter().last().and_then(|component| self.layout.syntax_range(component));
                if let (Some(opening), Some(first), Some(last), Some(closing)) = (opening, first, last, closing) {
                    push_line_break_between(self.layout.text(), opening.end, first.start, self.newline, &mut self.edits);
                    for (comma, component) in pack.pack.pairs() {
                        if let (Some(comma), Some(component)) = (self.layout.syntax_range(comma), self.layout.syntax_range(component)) {
                            push_line_break_between(self.layout.text(), comma.end, component.start, self.newline, &mut self.edits);
                        }
                    }
                    push_line_break_between(self.layout.text(), last.end, closing.start, self.newline, &mut self.edits);
                }
            }
        }
        visit::visit_list(self, list);
    }
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
    syntax: &SyntaxSnapshot,
    layout: &LayoutSource,
    options: &FormatOptions,
    newline: &'static str,
) -> Vec<FormatEdit> {
    let mut visitor = LeadingOperators {
        layout,
        options,
        newline,
        depth: 0,
        edits: Vec::new(),
    };
    syntax.visit(&mut visitor);
    visitor.edits
}

/// Moves eligible typed binary operators to the start of continuation lines.
struct LeadingOperators<'a> {
    /// Current whitespace coordinates.
    layout: &'a LayoutSource,
    /// Formatting choices controlling factor operators.
    options: &'a FormatOptions,
    /// Document line ending.
    newline: &'static str,
    /// Number of enclosing delimited expressions.
    depth: usize,
    /// Collected whitespace edits.
    edits: Vec<FormatEdit>,
}

impl<'ast> Visit<'ast> for LeadingOperators<'_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        let delimited = expression.delimiter().is_some();
        self.depth += usize::from(delimited);
        visit::visit_expr(self, expression);
        self.depth -= usize::from(delimited);
    }

    fn visit_assignment_list(&mut self, list: &'ast m2_syn::nodes::AssignmentList) {
        self.depth += 1;
        visit::visit_assignment_list(self, list);
        self.depth -= 1;
    }

    fn visit_expr_op_bin_regular(&mut self, expression: &'ast m2_syn::nodes::ExprOpBinRegular) {
        let spans = (
            self.layout.syntax_range(expression.left_operand.as_ref()),
            self.layout.syntax_range(&expression.operator),
            self.layout.syntax_range(expression.right_operand.as_ref()),
        );
        if let (Some(left), Some(operator), Some(right)) = spans {
            let text = self.layout.text();
            if self.depth > 0
                && left.end <= operator.start
                && operator.end <= right.start
                && is_spaced_line_final_operator(
                    &text[operator.clone()],
                    self.options.compact_factor_operators,
                )
            {
                let before = &text[left.end..operator.start];
                let after = &text[operator.end..right.start];
                let dangles = before.bytes().all(|byte| matches!(byte, b' ' | b'\t'))
                    && after.bytes().all(|byte| byte.is_ascii_whitespace())
                    && after.contains('\n');
                if dangles {
                    self.edits.push(FormatEdit {
                        start_byte: left.end,
                        end_byte: operator.start,
                        replacement: self.newline.to_string(),
                    });
                    self.edits.push(FormatEdit {
                        start_byte: operator.end,
                        end_byte: right.start,
                        replacement: " ".to_string(),
                    });
                }
            }
        }
        visit::visit_expr_op_bin_regular(self, expression);
    }
}

/// Whitespace edits that keep syntactically empty bodies on a single line.
fn inline_empty_bodies(syntax: &SyntaxSnapshot, layout: &LayoutSource) -> Vec<FormatEdit> {
    let mut visitor = EmptyBodyEdits {
        layout,
        edits: Vec::new(),
    };
    syntax.visit(&mut visitor);
    visitor.edits
}

/// Whitespace edits for empty delimited bodies in typed clause contexts.
struct EmptyBodyEdits<'a> {
    /// Current formatter coordinate mapping.
    layout: &'a LayoutSource,
    /// Collected whitespace replacements.
    edits: Vec<FormatEdit>,
}

impl EmptyBodyEdits<'_> {
    /// Compacts one delimited body only when its contents are whitespace.
    fn consider(&mut self, body: &Expr) {
        let delimiter = body.delimiter();
        let Some(delimiter) = delimiter else {
            return;
        };
        let Some(open) = self.layout.syntax_range(&delimiter.opening_span()) else {
            return;
        };
        let Some(close) = delimiter
            .closing_span()
            .and_then(|span| self.layout.syntax_range(&span))
        else {
            return;
        };
        if open.end <= close.start
            && self.layout.text()[open.end..close.start]
                .bytes()
                .all(|byte| byte.is_ascii_whitespace())
        {
            self.edits.push(FormatEdit {
                start_byte: open.end,
                end_byte: close.start,
                replacement: String::new(),
            });
        }
    }
}

impl<'ast> Visit<'ast> for EmptyBodyEdits<'_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        match expression {
            Expr::Lambda(lambda) => self.consider(crate::syntax::LambdaSyntax::body(lambda)),
            Expr::Try(node) => self.consider(&node.value),
            _ => {}
        }
        visit::visit_expr(self, expression);
    }
    fn visit_then_clause(&mut self, clause: &'ast ThenClause) {
        self.consider(&clause.expr);
        visit::visit_then_clause(self, clause);
    }
    fn visit_else_clause(&mut self, clause: &'ast ElseClause) {
        self.consider(&clause.expr);
        visit::visit_else_clause(self, clause);
    }
    fn visit_range_bound_from(&mut self, clause: &'ast m2_syn::nodes::RangeBoundFrom) {
        self.consider(&clause.value);
        visit::visit_range_bound_from(self, clause);
    }
    fn visit_range_bound_to(&mut self, clause: &'ast m2_syn::nodes::RangeBoundTo) {
        self.consider(&clause.value);
        visit::visit_range_bound_to(self, clause);
    }
    fn visit_when_condition(&mut self, clause: &'ast m2_syn::nodes::WhenCondition) {
        self.consider(&clause.condition);
        visit::visit_when_condition(self, clause);
    }
    fn visit_list_step(&mut self, clause: &'ast m2_syn::nodes::ListStep) {
        self.consider(&clause.expr);
        visit::visit_list_step(self, clause);
    }
    fn visit_do_step(&mut self, clause: &'ast m2_syn::nodes::DoStep) {
        self.consider(&clause.statement);
        visit::visit_do_step(self, clause);
    }
}

/// Formats control flow using the current layout settings and source coordinates.
fn format_control_flow(
    syntax: &SyntaxSnapshot,
    layout: &LayoutSource,
    options: &FormatOptions,
    newline: &'static str,
) -> Vec<FormatEdit> {
    if options.control_flow_layout == ControlFlowLayout::Compact {
        return Vec::new();
    }
    let mut visitor = ControlFlowEdits {
        layout,
        layout_kind: options.control_flow_layout,
        newline,
        delimited_depth: 0,
        control: None,
        else_if_owner: None,
        edits: Vec::new(),
    };
    syntax.visit(&mut visitor);
    visitor.edits
}

/// Line-break permissions of the control statement owning the clauses being visited.
#[derive(Clone, Copy)]
struct ControlContext {
    /// Whether clause breaks keep the statement parseable.
    breaks_are_parseable: bool,
}

/// Clause break edits collected while carrying the enclosing control statement.
struct ControlFlowEdits<'a> {
    /// Current formatter coordinate mapping.
    layout: &'a LayoutSource,
    /// Requested control-flow layout.
    layout_kind: ControlFlowLayout,
    /// Line ending inserted by clause breaks.
    newline: &'static str,
    /// Number of collections, holders, and argument lists enclosing the current node.
    delimited_depth: usize,
    /// Nearest enclosing control statement, absent outside any.
    control: Option<ControlContext>,
    /// Context inherited by an `else if` chain from its outer conditional.
    else_if_owner: Option<ControlContext>,
    /// Collected whitespace replacements.
    edits: Vec<FormatEdit>,
}

impl ControlFlowEdits<'_> {
    /// Whether clause breaks of a control statement starting here are parseable.
    fn context_for(
        &self,
        statement: &impl m2_syn::Spanned,
        has_alternative: bool,
    ) -> ControlContext {
        let text = self.layout.text();
        let breaks_are_parseable = self.delimited_depth > 0
            || self.layout.syntax_range(statement).is_some_and(|range| {
                let line_start = text[..range.start].rfind('\n').map_or(0, |index| index + 1);
                text[line_start..range.start]
                    .bytes()
                    .all(|byte| matches!(byte, b' ' | b'\t'))
                    && !has_alternative
            });
        ControlContext {
            breaks_are_parseable,
        }
    }

    /// Runs `visit` with the given control statement as the enclosing one.
    fn within(&mut self, control: ControlContext, visit: impl FnOnce(&mut Self)) {
        let previous = self.control.replace(control);
        visit(self);
        self.control = previous;
    }

    /// Whether clause breaks are allowed for the enclosing control statement.
    fn breaks_are_parseable(&self) -> bool {
        self.control
            .is_none_or(|control| control.breaks_are_parseable)
    }

    /// Breaks the line before a clause keyword.
    fn break_before(&mut self, keyword: &impl m2_syn::Spanned) {
        let Some(range) = self.layout.syntax_range(keyword) else {
            return;
        };
        let text = self.layout.text();
        push_control_flow_break(
            text,
            same_line_horizontal_whitespace_start(text, range.start),
            range.start,
            self.newline,
            &mut self.edits,
        );
    }

    /// Breaks the line between a clause keyword and its body unless the body is delimited.
    fn break_after(&mut self, keyword: &impl m2_syn::Spanned, body: &Expr) {
        if body.delimiter().is_some() {
            return;
        }
        let (Some(keyword), Some(body)) = (
            self.layout.syntax_range(keyword),
            self.layout.syntax_range(body),
        ) else {
            return;
        };
        push_control_flow_break(
            self.layout.text(),
            Some(keyword.end),
            body.start,
            self.newline,
            &mut self.edits,
        );
    }

    /// Keeps an else keyword beside the preceding closer when that layout remains valid.
    fn keep_else_after_closer(
        &mut self,
        then_clause: &ThenClause,
        else_clause: &ElseClause,
    ) -> bool {
        let Some(delimiter) = then_clause.expr.delimiter() else {
            return false;
        };
        let (Some(closer), Some(keyword)) = (
            delimiter
                .closing_span()
                .and_then(|span| self.layout.syntax_range(&span)),
            self.layout.syntax_range(&else_clause.else_token),
        ) else {
            return false;
        };
        if closer.end > keyword.start
            || !self.layout.text()[closer.end..keyword.start]
                .bytes()
                .all(|byte| byte.is_ascii_whitespace())
        {
            return false;
        }
        if self.layout.point_for_byte(closer.end).row
            < self.layout.point_for_byte(keyword.start).row
        {
            self.edits.push(FormatEdit {
                start_byte: closer.end,
                end_byte: keyword.start,
                replacement: " ".to_string(),
            });
        }
        true
    }

    /// Whether the else keyword and an `if` body begin on the same row.
    fn is_else_if(&self, clause: &ElseClause) -> bool {
        let row = |node: &dyn m2_syn::Spanned| {
            self.layout
                .syntax_range(node)
                .map(|range| self.layout.point_for_byte(range.start).row)
        };
        matches!(clause.expr.as_ref(), Expr::If(_))
            && row(&clause.else_token) == row(clause.expr.as_ref())
    }

    /// Lays out the clauses of a then/else pair or a try's success and fallback clauses.
    fn then_clause(&mut self, owner_is_if: bool, clause: &ThenClause) {
        if !self.breaks_are_parseable() {
            return;
        }
        if owner_is_if && self.layout_kind == ControlFlowLayout::LeadingKeywords {
            self.break_before(&clause.then_token);
        } else {
            self.break_after(&clause.then_token, &clause.expr);
        }
    }

    /// Lays out an else clause following the optional preceding then clause.
    fn else_clause(
        &mut self,
        owner_is_if: bool,
        then_clause: Option<&ThenClause>,
        clause: &ElseClause,
    ) {
        if !self.breaks_are_parseable() {
            return;
        }
        if owner_is_if && self.layout_kind == ControlFlowLayout::LeadingKeywords {
            self.break_before(&clause.else_token);
            return;
        }
        if !then_clause.is_some_and(|then_clause| self.keep_else_after_closer(then_clause, clause))
        {
            self.break_before(&clause.else_token);
        }
        if matches!(clause.expr.as_ref(), Expr::If(_))
            || self.layout_kind == ControlFlowLayout::MultilineCompactElse
        {
            return;
        }
        self.break_after(&clause.else_token, &clause.expr);
    }
}

impl<'ast> Visit<'ast> for ControlFlowEdits<'_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        let delimited = expression.delimiter().is_some();
        self.delimited_depth += usize::from(delimited);
        visit::visit_expr(self, expression);
        self.delimited_depth -= usize::from(delimited);
    }
    fn visit_assignment_list(&mut self, list: &'ast m2_syn::nodes::AssignmentList) {
        self.delimited_depth += 1;
        visit::visit_assignment_list(self, list);
        self.delimited_depth -= 1;
    }
    fn visit_expr_if(&mut self, node: &'ast IfStatement) {
        let else_if_owner = self.else_if_owner.take();
        let control =
            else_if_owner.unwrap_or_else(|| self.context_for(node, node.else_clause.is_some()));
        self.within(control, |this| {
            this.then_clause(true, &node.then_clause);
            this.visit_expr(&node.condition);
            this.visit_then_clause(&node.then_clause);
            if let Some(clause) = &node.else_clause {
                this.else_clause(true, Some(&node.then_clause), clause);
                if this.is_else_if(clause) {
                    this.else_if_owner = Some(control);
                }
                this.visit_else_clause(clause);
                this.else_if_owner = None;
            }
        });
    }
    fn visit_expr_try(&mut self, node: &'ast TryStatement) {
        let control = self.context_for(node, node.fallback.is_some());
        self.within(control, |this| {
            if let Some(clause) = &node.on_success {
                this.then_clause(false, clause);
            }
            match &node.fallback {
                Some(m2_syn::nodes::TryFallback::Else(clause)) => {
                    this.else_clause(false, node.on_success.as_ref(), clause);
                }
                Some(m2_syn::nodes::TryFallback::Except(clause)) if this.breaks_are_parseable() => {
                    this.break_before(&clause.except_token);
                    this.break_after(&clause.do_token, &clause.value);
                }
                _ => {}
            }
            visit::visit_expr_try(this, node);
        });
    }
    fn visit_expr_for(&mut self, node: &'ast ForLoop) {
        let control = self.context_for(node, false);
        self.within(control, |this| visit::visit_expr_for(this, node));
    }
    fn visit_expr_while(&mut self, node: &'ast WhileLoop) {
        let control = self.context_for(node, false);
        self.within(control, |this| visit::visit_expr_while(this, node));
    }
    fn visit_loop_body(&mut self, body: &'ast LoopBody) {
        if self.breaks_are_parseable() {
            if let Some(step) = &body.list_step {
                self.break_after(&step.list_token, &step.expr);
            }
            if let Some(step) = &body.do_step {
                self.break_after(&step.do_token, &step.statement);
            }
        }
        visit::visit_loop_body(self, body);
    }
}

/// Appends control flow break to the current formatting edits or traversal results.
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
fn reindent_from_syntax(
    syntax: &SyntaxSnapshot,
    source: &LayoutSource,
    options: &FormatOptions,
) -> Vec<FormatEdit> {
    let text = source.text();
    let layout = TreeIndentLayout::from_syntax(syntax, source, options.compact_factor_operators);
    let protected = source.protected_ranges().collect::<Vec<_>>();
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

/// A permitted wrap point with its indentation and syntactic priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineBreakCandidate {
    /// Inclusive UTF-8 byte offset of the represented source interval.
    start_byte: usize,
    /// Exclusive UTF-8 byte offset of the represented source interval.
    end_byte: usize,
}

/// Wrap candidates after typed delimiters enclosing content.
struct DelimiterBreaks<'a> {
    /// Current whitespace coordinates.
    layout: &'a LayoutSource,
    /// Candidate locations collected so far.
    candidates: Vec<LineBreakCandidate>,
}

impl DelimiterBreaks<'_> {
    /// Records a break after an opening delimiter of a nonempty construct.
    fn consider(&mut self, delimiter: Delimiter, has_contents: bool) {
        let Some(open) = self.layout.syntax_range(&delimiter.opening_span()) else {
            return;
        };
        if !has_contents {
            let Some(close) = delimiter
                .closing_span()
                .and_then(|span| self.layout.syntax_range(&span))
            else {
                return;
            };
            if !self
                .layout
                .protected_ranges()
                .any(|range| open.end <= range.start && range.end <= close.start)
            {
                return;
            }
        }
        let Some(end_byte) = same_line_horizontal_whitespace_end(self.layout.text(), open.end)
        else {
            return;
        };
        self.candidates.push(LineBreakCandidate {
            start_byte: open.end,
            end_byte,
        });
    }
}

impl<'ast> Visit<'ast> for DelimiterBreaks<'_> {
    fn visit_list(&mut self, node: &'ast List) {
        self.consider(
            node.delimiter.delimiter(),
            !node.statements.is_empty() || node.elements.is_some(),
        );
        visit::visit_list(self, node);
    }
    fn visit_array(&mut self, node: &'ast m2_syn::nodes::Array) {
        self.consider(
            node.delimiter.delimiter(),
            !node.statements.is_empty() || node.elements.is_some(),
        );
        visit::visit_array(self, node);
    }
    fn visit_sequence(&mut self, node: &'ast Sequence) {
        self.consider(
            node.delimiter.delimiter(),
            !node.statements.is_empty() || node.elements.is_some(),
        );
        visit::visit_sequence(self, node);
    }
    fn visit_angle_bar_list(&mut self, node: &'ast m2_syn::nodes::AngleBarList) {
        self.consider(
            node.delimiter.delimiter(),
            !node.statements.is_empty() || node.elements.is_some(),
        );
        visit::visit_angle_bar_list(self, node);
    }
    fn visit_holder(&mut self, node: &'ast m2_syn::nodes::Holder) {
        self.consider(
            node.delimiter.delimiter(),
            !node.muted.is_empty() || node.value.is_some(),
        );
        visit::visit_holder(self, node);
    }
    fn visit_assignment_list(&mut self, node: &'ast m2_syn::nodes::AssignmentList) {
        self.consider(
            node.contents.delimiter.delimiter(),
            node.contents.contents.is_some(),
        );
        visit::visit_assignment_list(self, node);
    }
    fn visit_expr_lambda_list(&mut self, node: &'ast m2_syn::nodes::ExprLambdaList) {
        self.consider(
            node.arguments.delimiter.delimiter(),
            node.arguments.contents.is_some(),
        );
        visit::visit_expr_lambda_list(self, node);
    }
}

/// Wrap candidates after typed commas that do not border an empty slot.
struct CommaBreaks<'a> {
    /// Current formatter coordinates.
    layout: &'a LayoutSource,
    /// Candidate locations collected so far.
    candidates: Vec<LineBreakCandidate>,
    /// Whitespace normalization edits around commas.
    spacing: Vec<FormatEdit>,
}

impl CommaBreaks<'_> {
    /// Collects separators from one typed punctuated sequence.
    fn collect<T: m2_syn::Spanned>(
        &mut self,
        values: &m2_syn::Punctuated<T>,
        empty: impl Fn(&T) -> bool,
    ) {
        let mut previous = values.first();
        for (comma, next) in values.pairs() {
            let Some(bytes) = self.layout.syntax_range(comma) else {
                previous = next;
                continue;
            };
            let borders_empty = [previous, next]
                .into_iter()
                .filter(|value| empty(value))
                .filter_map(|value| self.layout.syntax_range(value))
                .any(|slot| slot.start == bytes.end || slot.end == bytes.start);
            if let Some(start_byte) =
                same_line_horizontal_whitespace_start(self.layout.text(), bytes.start)
            {
                self.spacing.push(FormatEdit {
                    start_byte,
                    end_byte: bytes.start,
                    replacement: String::new(),
                });
            }
            if let Some(end_byte) =
                same_line_horizontal_whitespace_end(self.layout.text(), bytes.end)
            {
                let replacement = if borders_empty
                    || matches!(
                        self.layout.text().as_bytes().get(end_byte),
                        Some(b')' | b']' | b'}' | b';') | None
                    ) {
                    ""
                } else {
                    " "
                };
                self.spacing.push(FormatEdit {
                    start_byte: bytes.end,
                    end_byte,
                    replacement: replacement.to_string(),
                });
            }
            if !borders_empty {
                if let Some(end_byte) =
                    same_line_horizontal_whitespace_end(self.layout.text(), bytes.end)
                {
                    self.candidates.push(LineBreakCandidate {
                        start_byte: bytes.end,
                        end_byte,
                    });
                }
            }
            previous = next;
        }
    }
}

impl<'ast> Visit<'ast> for CommaBreaks<'_> {
    fn visit_expr_pack(&mut self, node: &'ast m2_syn::nodes::ExprPack) {
        self.collect(&node.pack, |value| matches!(value, Component::Empty(_)));
        visit::visit_expr_pack(self, node);
    }
    fn visit_assignment_list_content_pack(
        &mut self,
        node: &'ast m2_syn::nodes::AssignmentListContentPack,
    ) {
        self.collect(&node.arguments, |value| {
            matches!(value, m2_syn::nodes::AssignmentArgument::Empty(_))
        });
        visit::visit_assignment_list_content_pack(self, node);
    }
    fn visit_argument_list_content_pack(
        &mut self,
        node: &'ast m2_syn::nodes::ArgumentListContentPack,
    ) {
        self.collect(&node.arguments, |_| false);
        visit::visit_argument_list_content_pack(self, node);
    }
}

/// Wrap points at typed operators, with enclosing delimiter context.
struct OperatorBreaks<'a> {
    /// Current formatter coordinates.
    layout: &'a LayoutSource,
    /// Whether factor operators stay compact.
    compact_factors: bool,
    /// Number of enclosing delimited constructs.
    depth: usize,
    /// Candidate locations.
    candidates: Vec<LineBreakCandidate>,
}

impl OperatorBreaks<'_> {
    /// Adds a candidate on the appropriate side of a spaced operator.
    fn consider(&mut self, operator: &impl m2_syn::Spanned, before: bool) {
        let Some(bytes) = self.layout.syntax_range(operator) else {
            return;
        };
        let text = self.layout.text();
        if !is_spaced_line_final_operator(&text[bytes.clone()], self.compact_factors) {
            return;
        }
        let range = if before {
            same_line_horizontal_whitespace_start(text, bytes.start).map(|start| start..bytes.start)
        } else {
            same_line_horizontal_whitespace_end(text, bytes.end).map(|end| bytes.end..end)
        };
        if let Some(range) = range {
            self.candidates.push(LineBreakCandidate {
                start_byte: range.start,
                end_byte: range.end,
            });
        }
    }
}

impl<'ast> Visit<'ast> for OperatorBreaks<'_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        let delimited = expression.delimiter().is_some();
        self.depth += usize::from(delimited);
        visit::visit_expr(self, expression);
        self.depth -= usize::from(delimited);
    }
    fn visit_assignment_list(&mut self, list: &'ast m2_syn::nodes::AssignmentList) {
        self.depth += 1;
        visit::visit_assignment_list(self, list);
        self.depth -= 1;
    }
    fn visit_expr_op_bin_regular(&mut self, node: &'ast m2_syn::nodes::ExprOpBinRegular) {
        self.consider(&node.operator, self.depth > 0);
        visit::visit_expr_op_bin_regular(self, node);
    }
    fn visit_expr_assign(&mut self, node: &'ast m2_syn::nodes::ExprAssign) {
        if let Some(span) = node.parts().operator_span {
            self.consider(&span, false);
        }
        visit::visit_expr_assign(self, node);
    }
    fn visit_expr_assign_evaluated(&mut self, node: &'ast m2_syn::nodes::ExprAssignEvaluated) {
        self.consider(&node.operator, false);
        visit::visit_expr_assign_evaluated(self, node);
    }
    fn visit_expr_option(&mut self, node: &'ast m2_syn::nodes::ExprOption) {
        self.consider(&node.operator, false);
        visit::visit_expr_option(self, node);
    }
    fn visit_expr_lambda_variadic(&mut self, node: &'ast m2_syn::nodes::ExprLambdaVariadic) {
        self.consider(&node.operator, false);
        visit::visit_expr_lambda_variadic(self, node);
    }
    fn visit_expr_lambda_list(&mut self, node: &'ast m2_syn::nodes::ExprLambdaList) {
        self.consider(&node.operator, false);
        visit::visit_expr_lambda_list(self, node);
    }
}

/// A source line with its byte extent and display-width information.
#[derive(Debug, Clone, Copy)]
struct TextLine {
    /// Inclusive UTF-8 byte offset of the represented source interval.
    start_byte: usize,
    /// Exclusive UTF-8 byte offset of the represented source interval.
    end_byte: usize,
}

/// Wraps long lines at permitted syntax boundaries using the configured widths.
fn wrap_long_lines(
    syntax: &SyntaxSnapshot,
    layout: &LayoutSource,
    options: &FormatOptions,
    newline: &'static str,
) -> Vec<FormatEdit> {
    let text = layout.text();
    let Some(max_line_width) = options.max_line_width else {
        return Vec::new();
    };
    let soft_line_width = options
        .soft_line_width
        .unwrap_or(max_line_width)
        .min(max_line_width);
    let mut operators = OperatorBreaks {
        layout,
        compact_factors: options.compact_factor_operators,
        depth: 0,
        candidates: Vec::new(),
    };
    syntax.visit(&mut operators);
    let mut candidates = operators.candidates;
    let mut delimiters = DelimiterBreaks {
        layout,
        candidates: Vec::new(),
    };
    syntax.visit(&mut delimiters);
    candidates.extend(delimiters.candidates);
    let mut commas = CommaBreaks {
        layout,
        candidates: Vec::new(),
        spacing: Vec::new(),
    };
    syntax.visit(&mut commas);
    candidates.extend(commas.candidates);
    candidates.sort_by_key(|candidate| (candidate.start_byte, candidate.end_byte));
    candidates.dedup();
    select_line_breaks(
        text,
        &candidates,
        soft_line_width,
        max_line_width,
        options.tab_width,
        newline,
    )
}

/// Selects line breaks according to the syntax and configured layout constraints.
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

/// Splits text into source lines retaining byte coordinates for wrapping.
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

/// Display width with tabs expanded using the configured tab stop.
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
fn folding_ranges_for_syntax(syntax: &SyntaxSnapshot) -> Vec<FormatFoldRange> {
    let source = LayoutSource::new(syntax);
    let text = source.text();
    let layout = TreeIndentLayout::from_syntax(syntax, &source, false);
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
    /// Indentation depth assigned to each source line.
    depths: Vec<usize>,
    /// Foldable ranges identified inside multiline literals.
    literal_folds: Vec<FormatFoldRange>,
    /// Lines beginning with a closing delimiter and requiring reduced indentation.
    leading_closer_rows: Vec<bool>,
}

impl TreeIndentLayout {
    /// Collects line indentation and folding information from the parsed layout.
    fn from_syntax(
        syntax: &SyntaxSnapshot,
        source: &LayoutSource,
        compact_factor_operators: bool,
    ) -> Self {
        let text = source.text();
        let line_count = text.lines().count().max(1);
        let brackets = collect_bracket_groups(syntax, source, line_count);
        let line_leads = line_leading_blank(text, line_count);
        let lines = text_lines(text);
        let literal_folds = collect_literal_fold_ranges(syntax, source, &line_leads);
        let mut right_operands = RightOperandContinuations {
            layout: source,
            compact_factors: compact_factor_operators,
            values: Vec::new(),
            leading_operators: Vec::new(),
            clause_bodies: Vec::new(),
            clause_keywords: Vec::new(),
            line_leads: &line_leads,
            inline_owner: None,
            else_if: false,
        };
        syntax.visit(&mut right_operands);
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
            depths
                .push(bracket_base + line_continuation(row, &line_leads, &lines, &right_operands));
        }

        Self {
            depths,
            literal_folds,
            leading_closer_rows,
        }
    }

    /// Indentation level assigned to the requested source row.
    fn depth(&self, row: usize) -> usize {
        self.depths.get(row).copied().unwrap_or(0)
    }

    /// Whether the requested line begins with a closing delimiter.
    fn is_leading_closer(&self, row: usize) -> bool {
        self.leading_closer_rows.get(row).copied().unwrap_or(false)
    }
}

/// Build folds from parser-classified comments and string literals.
/// Consecutive full-line `--` comments become one range, while each multiline
/// block comment and each multiline string (`"..."` or `///...///`) is its own
/// range. Keeping this tree-derived avoids mistaking comment markers inside
/// strings for comments, and string delimiters inside comments for strings.
fn collect_literal_fold_ranges(
    syntax: &SyntaxSnapshot,
    layout: &LayoutSource,
    line_leads: &[usize],
) -> Vec<FormatFoldRange> {
    let mut line_comment_rows = Vec::new();
    let mut strings = StringFolds {
        layout,
        ranges: Vec::new(),
    };
    syntax.visit(&mut strings);
    let mut ranges = strings.ranges;
    for comment in syntax.cells().iter().flat_map(|cell| cell.trivia()) {
        let Some(mut bytes) = layout.syntax_range(comment) else {
            continue;
        };
        let text = &layout.text()[bytes.clone()];
        bytes.start += text.len() - text.trim_start_matches([' ', '\t']).len();
        bytes.end -= text.len() - text.trim_end_matches(['\r', '\n']).len();
        let start = layout.point_for_byte(bytes.start);
        let end = layout.point_for_byte(bytes.end);
        if comment.kind() == m2_syn::TriviaKind::LineComment
            && line_leads.get(start.row).copied() == Some(start.column)
        {
            line_comment_rows.extend((start.row..=end.row).map(|row| row as u32));
        } else if comment.kind() == m2_syn::TriviaKind::BlockComment && start.row < end.row {
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

/// Multiline string folds found by visiting typed literal expressions.
struct StringFolds<'a> {
    /// Current formatter coordinate mapping.
    layout: &'a LayoutSource,
    /// Fold ranges collected so far.
    ranges: Vec<FormatFoldRange>,
}

impl<'ast> Visit<'ast> for StringFolds<'_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        if matches!(
            expression,
            Expr::Literal(
                m2_syn::nodes::ExprLiteral::String(_) | m2_syn::nodes::ExprLiteral::RawString(_)
            )
        ) {
            if let Some(bytes) = self.layout.syntax_range(expression) {
                let start = self.layout.point_for_byte(bytes.start);
                let end = self.layout.point_for_byte(bytes.end);
                if start.row < end.row {
                    self.ranges.push(FormatFoldRange {
                        start_line: start.row as u32,
                        end_line: end.row as u32,
                        kind: FormatFoldKind::Region,
                    });
                }
            }
        }
        visit::visit_expr(self, expression);
    }
}

/// Appends line comment fold to the accumulated result in source order.
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
    /// Line on which the foldable construct opens.
    open_row: usize,
    /// Line on which the foldable construct closes.
    close_row: usize,
    /// Display column at which the closing delimiter is aligned.
    closing_delimiter_column: usize,
}

/// Collect every multiline bracket node (`(…)`, `{…}`, `[…]`, `<|…|>`) spanning
/// more than one row, keyed by open/close row for the depth computation. The
/// closer's width (`)` is 1, `|>` is 2) does not matter; only its start column.
fn collect_bracket_groups(
    syntax: &SyntaxSnapshot,
    layout: &LayoutSource,
    line_count: usize,
) -> Vec<BracketGroup> {
    let mut visitor = BracketGroups {
        layout,
        line_count,
        groups: Vec::new(),
    };
    syntax.visit(&mut visitor);
    visitor.groups
}

/// Collects bracket indentation from generated delimiter fields.
struct BracketGroups<'a> {
    /// Current formatter coordinates.
    layout: &'a LayoutSource,
    /// Number of lines in the current layout.
    line_count: usize,
    /// Multiline delimiter pairs.
    groups: Vec<BracketGroup>,
}

impl BracketGroups<'_> {
    /// Records a complete multiline delimiter pair.
    fn consider(&mut self, delimiter: Delimiter) {
        let Some(open) = self.layout.syntax_range(&delimiter.opening_span()) else {
            return;
        };
        let open = self.layout.point_for_byte(open.start);
        let Some(close) = delimiter
            .closing_span()
            .and_then(|span| self.layout.syntax_range(&span))
        else {
            let last = self.line_count.saturating_sub(1);
            if open.row < last {
                self.groups.push(BracketGroup {
                    open_row: open.row,
                    close_row: last,
                    closing_delimiter_column: usize::MAX,
                });
            }
            return;
        };
        let close = self.layout.point_for_byte(close.start);
        if open.row < close.row {
            self.groups.push(BracketGroup {
                open_row: open.row,
                close_row: close.row,
                closing_delimiter_column: close.column,
            });
        }
    }
}

impl<'ast> Visit<'ast> for BracketGroups<'_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        let delimiter = expression.delimiter();
        if let Some(delimiter) = delimiter {
            self.consider(delimiter);
        }
        visit::visit_expr(self, expression);
    }
    fn visit_assignment_list(&mut self, list: &'ast m2_syn::nodes::AssignmentList) {
        self.consider(list.contents.delimiter.delimiter());
        visit::visit_assignment_list(self, list);
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
    line_leads: &[usize],
    lines: &[TextLine],
    continuations: &RightOperandContinuations<'_>,
) -> usize {
    let Some(start) = lines
        .get(row)
        .zip(line_leads.get(row))
        .map(|(line, lead)| line.start_byte + lead)
    else {
        return 0;
    };

    // (a) The first token is the start of the right operand of an infix
    // expression whose operator dangled on an earlier row (`a +\nb`,
    // `x ->\nbody`).
    if continuations
        .values
        .iter()
        .any(|&(operand, operator_row)| operand == start && operator_row < row)
    {
        return 1;
    }

    // (a') The first token is a binary operator leading the row, its left
    // operand on an earlier row (`(a\n+ b)`).
    if continuations
        .leading_operators
        .iter()
        .any(|&(operator, left_row)| operator == start && left_row < row)
    {
        return 1;
    }

    // (b) The first token starts a clause body whose keyword is on an earlier row
    // (`then\n  f(x)`, `else\n  c`, `do\n  body`).
    if let Some(&(_, _, inline_conditional)) = continuations
        .clause_bodies
        .iter()
        .find(|&&(body, keyword_row, _)| body == start && keyword_row < row)
    {
        return 1 + usize::from(inline_conditional);
    }

    // (c) The first token is an `else`/`then` keyword whose controlling `if` is
    // not standalone (a ternary `x := if … then\n … else …`); a standalone `if`
    // aligns its `else` through its bracket instead.
    if continuations.clause_keywords.contains(&start) {
        return 1;
    }

    0
}

/// Typed operands that can start a continuation line.
struct RightOperandContinuations<'a> {
    /// Current whitespace coordinates.
    layout: &'a LayoutSource,
    /// Whether factor operators are compact.
    compact_factors: bool,
    /// Operand start byte and operator row.
    values: Vec<(usize, usize)>,
    /// Operator start byte and the row where its left operand starts.
    leading_operators: Vec<(usize, usize)>,
    /// Clause body start and its keyword row.
    clause_bodies: Vec<(usize, usize, bool)>,
    /// Clause keywords continuing an inline or unowned expression.
    clause_keywords: Vec<usize>,
    /// Leading whitespace columns for determining standalone conditionals.
    line_leads: &'a [usize],
    /// Whether the nearest enclosing conditional is inline.
    inline_owner: Option<bool>,
    /// Whether the next conditional is the direct value of an else clause.
    else_if: bool,
}

impl RightOperandContinuations<'_> {
    /// Records a clause keyword when its enclosing conditional is inline.
    fn record_keyword(&mut self, keyword: &impl m2_syn::Spanned) {
        if self.inline_owner.unwrap_or(true) {
            if let Some(range) = self.layout.syntax_range(keyword) {
                self.clause_keywords.push(range.start);
            }
        }
    }
    /// Records the start of a clause body and its introducing keyword.
    fn record_clause(&mut self, keyword: &impl m2_syn::Spanned, body: &Expr) {
        if let (Some(keyword), Some(body)) = (
            self.layout.syntax_range(keyword),
            self.layout.syntax_range(body),
        ) {
            self.clause_bodies.push((
                body.start,
                self.layout.point_for_byte(keyword.start).row,
                self.inline_owner.unwrap_or(false),
            ));
        }
    }
    /// Determines whether a conditional starts after other text on its line.
    fn inline(&self, node: &impl m2_syn::Spanned) -> bool {
        self.layout.syntax_range(node).is_some_and(|range| {
            let point = self.layout.point_for_byte(range.start);
            self.line_leads.get(point.row).copied().unwrap_or(0) < point.column
        })
    }
    /// Records an operator and the beginning of its left operand.
    fn record_leading(&mut self, operator: &impl m2_syn::Spanned, left: &impl m2_syn::Spanned) {
        if let (Some(operator), Some(left)) = (
            self.layout.syntax_range(operator),
            self.layout.syntax_range(left),
        ) {
            self.leading_operators
                .push((operator.start, self.layout.point_for_byte(left.start).row));
        }
    }
    /// Records an operand when its operator uses continuation spacing.
    fn record(&mut self, operator: &impl m2_syn::Spanned, value: &Expr) {
        let (Some(operator), Some(value)) = (
            self.layout.syntax_range(operator),
            self.layout.syntax_range(value),
        ) else {
            return;
        };
        if is_spaced_line_final_operator(
            &self.layout.text()[operator.clone()],
            self.compact_factors,
        ) {
            self.values
                .push((value.start, self.layout.point_for_byte(operator.start).row));
        }
    }
}

impl<'ast> Visit<'ast> for RightOperandContinuations<'_> {
    fn visit_expr_if(&mut self, node: &'ast IfStatement) {
        let previous = self.inline_owner;
        let else_if = std::mem::take(&mut self.else_if);
        self.inline_owner = Some(self.inline(node) && !else_if);
        visit::visit_expr_if(self, node);
        self.inline_owner = previous;
    }
    fn visit_expr_try(&mut self, node: &'ast TryStatement) {
        let previous = self.inline_owner;
        self.inline_owner = Some(self.inline(node));
        visit::visit_expr_try(self, node);
        self.inline_owner = previous;
    }
    fn visit_then_clause(&mut self, node: &'ast ThenClause) {
        self.record_keyword(&node.then_token);
        self.record_clause(&node.then_token, &node.expr);
        visit::visit_then_clause(self, node);
    }
    fn visit_else_clause(&mut self, node: &'ast ElseClause) {
        self.record_keyword(&node.else_token);
        self.record_clause(&node.else_token, &node.expr);
        let previous = self.else_if;
        self.else_if = matches!(node.expr.as_ref(), Expr::If(_))
            && self
                .layout
                .syntax_range(&node.else_token)
                .zip(self.layout.syntax_range(node.expr.as_ref()))
                .is_some_and(|(keyword, body)| {
                    self.layout.point_for_byte(keyword.start).row
                        == self.layout.point_for_byte(body.start).row
                });
        visit::visit_else_clause(self, node);
        self.else_if = previous;
    }
    fn visit_symbol(&mut self, node: &'ast Symbol) {
        if matches_token::<Token![then]>(node.text()) || matches_token::<Token![else]>(node.text())
        {
            self.record_keyword(node);
        }
        visit::visit_symbol(self, node);
    }
    fn visit_list_step(&mut self, node: &'ast m2_syn::nodes::ListStep) {
        self.record_clause(&node.list_token, &node.expr);
        visit::visit_list_step(self, node);
    }
    fn visit_do_step(&mut self, node: &'ast m2_syn::nodes::DoStep) {
        self.record_clause(&node.do_token, &node.statement);
        visit::visit_do_step(self, node);
    }
    fn visit_try_fallback(&mut self, node: &'ast m2_syn::nodes::TryFallback) {
        if let m2_syn::nodes::TryFallback::Except(clause) = node {
            self.record_clause(&clause.do_token, &clause.value);
        }
        visit::visit_try_fallback(self, node);
    }
    fn visit_expr_op_bin_regular(&mut self, node: &'ast m2_syn::nodes::ExprOpBinRegular) {
        self.record(&node.operator, &node.right_operand);
        self.record_leading(&node.operator, &node.left_operand);
        visit::visit_expr_op_bin_regular(self, node);
    }
    fn visit_expr_op_bin_adj(&mut self, node: &'ast m2_syn::nodes::ExprOpBinAdj) {
        self.record_leading(&node.operator, &node.left_operand);
        visit::visit_expr_op_bin_adj(self, node);
    }
    fn visit_expr_op_bin_index(&mut self, node: &'ast m2_syn::nodes::ExprOpBinIndex) {
        self.record_leading(&node.operator, &node.receiver);
        visit::visit_expr_op_bin_index(self, node);
    }
    fn visit_expr_op_bin_member(&mut self, node: &'ast m2_syn::nodes::ExprOpBinMember) {
        match node {
            m2_syn::nodes::ExprOpBinMember::Dot(node) => {
                self.record_leading(&node.operator, &node.receiver)
            }
            m2_syn::nodes::ExprOpBinMember::Optional(node) => {
                self.record_leading(&node.operator, &node.receiver)
            }
        }
        visit::visit_expr_op_bin_member(self, node);
    }
    fn visit_expr_option(&mut self, node: &'ast m2_syn::nodes::ExprOption) {
        self.record(&node.operator, &node.value);
        self.record_leading(&node.operator, &node.key);
        visit::visit_expr_option(self, node);
    }
    fn visit_expr_assign(&mut self, node: &'ast m2_syn::nodes::ExprAssign) {
        let parts = node.parts();
        if let Some(operator) = &parts.operator_span {
            self.record_leading(operator, node);
        }
        if !matches!(parts.value, Expr::If(_) | Expr::Assign(_)) {
            if let Some(operator) = parts.operator_span {
                self.record(&operator, parts.value);
            }
        }
        visit::visit_expr_assign(self, node);
    }
    fn visit_expr_assign_evaluated(&mut self, node: &'ast m2_syn::nodes::ExprAssignEvaluated) {
        self.record_leading(&node.operator, &node.evaluated_domain);
        if !matches!(node.value.as_ref(), Expr::If(_) | Expr::Assign(_)) {
            self.record(&node.operator, &node.value);
        }
        visit::visit_expr_assign_evaluated(self, node);
    }
    fn visit_expr_lambda_variadic(&mut self, node: &'ast m2_syn::nodes::ExprLambdaVariadic) {
        self.record(&node.operator, &node.body);
        visit::visit_expr_lambda_variadic(self, node);
    }
    fn visit_expr_lambda_list(&mut self, node: &'ast m2_syn::nodes::ExprLambdaList) {
        self.record(&node.operator, &node.body);
        visit::visit_expr_lambda_list(self, node);
    }
}

/// A foldable block of consecutive lines (0-based, both bounds inclusive as
/// produced; the LSP conversion maps them onto `FoldingRange`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatFoldRange {
    /// First line of the represented source region.
    pub start_line: u32,
    /// Last line of the represented source region.
    pub end_line: u32,
    /// Semantic or presentation category of this item.
    pub kind: FormatFoldKind,
}

/// Whether a fold represents code structure or a comment block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FormatFoldKind {
    /// A foldable source-code region.
    Region,
    /// A foldable or classified comment span.
    Comment,
}

/// A nonempty source line classified by indentation and closing delimiters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IndentedLine {
    /// Source line to which this layout fact applies.
    line: u32,
    /// Indentation level assigned to the line or construct.
    depth: usize,
    /// Whether the line begins with a closing delimiter.
    is_leading_closer: bool,
}

/// A prospective fold awaiting a line that closes its indentation level.
#[derive(Debug, Clone, Copy)]
struct OpenFoldRange {
    /// First line of the represented source region.
    start_line: u32,
    /// Indentation level assigned to the line or construct.
    depth: usize,
}

/// Collects indent fold ranges from the supplied syntax and semantic context.
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

/// Closes fold range at the supplied source boundary.
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

/// Normalizes whitespace according to the resolved formatting settings.
fn normalize_whitespace(
    syntax: &SyntaxSnapshot,
    layout: &LayoutSource,
    options: &FormatOptions,
) -> Vec<FormatEdit> {
    let mut edits = Vec::new();
    let mut commas = CommaBreaks {
        layout,
        candidates: Vec::new(),
        spacing: Vec::new(),
    };
    syntax.visit(&mut commas);
    edits.extend(commas.spacing);
    let mut semicolons = SemicolonSpacing {
        layout,
        break_after: options.break_after_semicolon,
        in_collection: false,
        edits: Vec::new(),
    };
    syntax.visit(&mut semicolons);
    edits.extend(semicolons.edits);
    let mut braces = BraceSpacing {
        layout,
        space_in_braces: options.space_in_braces,
        edits: Vec::new(),
    };
    syntax.visit(&mut braces);
    edits.extend(braces.edits);
    let mut prefixes = PrefixSpacing {
        layout,
        edits: Vec::new(),
    };
    syntax.visit(&mut prefixes);
    edits.extend(prefixes.edits);
    let mut binaries = BinarySpacing {
        layout,
        compact_factors: options.compact_factor_operators,
        edits: Vec::new(),
    };
    syntax.visit(&mut binaries);
    edits.extend(binaries.edits);
    let mut calls = CallSpacing {
        layout,
        edits: Vec::new(),
    };
    syntax.visit(&mut calls);
    edits.extend(calls.edits);
    let mut assignments = AssignmentSpacing {
        layout,
        seen: std::collections::HashSet::new(),
        edits: Vec::new(),
    };
    syntax.visit(&mut assignments);
    edits.extend(assignments.edits);
    edits
}

/// Spacing around assignment, option and lambda operators.
struct AssignmentSpacing<'a> {
    /// Current formatter coordinates.
    layout: &'a LayoutSource,
    /// Operator ranges already visited through generated enum variants.
    seen: std::collections::HashSet<std::ops::Range<usize>>,
    /// Collected whitespace edits.
    edits: Vec<FormatEdit>,
}

impl AssignmentSpacing<'_> {
    /// Applies the existing binary spacing policy to a typed operator.
    fn consider(&mut self, operator: &impl m2_syn::Spanned) {
        let Some(bytes) = self.layout.syntax_range(operator) else {
            return;
        };
        if !self.seen.insert(bytes.clone()) {
            return;
        }
        let text = self.layout.text();
        let spacing = binary_operator_spacing(&text[bytes.clone()]);
        let before = same_line_horizontal_whitespace_start(text, bytes.start);
        let after = same_line_horizontal_whitespace_end(text, bytes.end);
        if spacing == OperatorSpacing::Spaced
            || (spacing == OperatorSpacing::Compact && before.is_some() && after.is_some())
        {
            let replacement = if spacing == OperatorSpacing::Spaced {
                " "
            } else {
                ""
            };
            if let Some(start_byte) = before {
                self.edits.push(FormatEdit {
                    start_byte,
                    end_byte: bytes.start,
                    replacement: replacement.to_string(),
                });
            }
            if let Some(end_byte) = after {
                self.edits.push(FormatEdit {
                    start_byte: bytes.end,
                    end_byte,
                    replacement: replacement.to_string(),
                });
            }
        }
    }
}

impl<'ast> Visit<'ast> for AssignmentSpacing<'_> {
    fn visit_expr_op_bin_index(&mut self, node: &'ast m2_syn::nodes::ExprOpBinIndex) {
        self.consider(&node.operator);
        visit::visit_expr_op_bin_index(self, node);
    }
    fn visit_expr_op_bin_member(&mut self, node: &'ast m2_syn::nodes::ExprOpBinMember) {
        match node {
            m2_syn::nodes::ExprOpBinMember::Dot(node) => self.consider(&node.operator),
            m2_syn::nodes::ExprOpBinMember::Optional(node) => self.consider(&node.operator),
        }
        visit::visit_expr_op_bin_member(self, node);
    }
    fn visit_expr_assign(&mut self, node: &'ast m2_syn::nodes::ExprAssign) {
        if let Some(span) = node.parts().operator_span {
            self.consider(&span);
        }
        visit::visit_expr_assign(self, node);
    }
    fn visit_expr_assign_evaluated(&mut self, node: &'ast m2_syn::nodes::ExprAssignEvaluated) {
        self.consider(&node.operator);
        visit::visit_expr_assign_evaluated(self, node);
    }
    fn visit_expr_option(&mut self, node: &'ast m2_syn::nodes::ExprOption) {
        self.consider(&node.operator);
        visit::visit_expr_option(self, node);
    }
    fn visit_expr_lambda_variadic(&mut self, node: &'ast m2_syn::nodes::ExprLambdaVariadic) {
        self.consider(&node.operator);
        visit::visit_expr_lambda_variadic(self, node);
    }
    fn visit_expr_lambda_list(&mut self, node: &'ast m2_syn::nodes::ExprLambdaList) {
        self.consider(&node.operator);
        visit::visit_expr_lambda_list(self, node);
    }
}

/// Spacing between typed call heads and argument or method-signature expressions.
struct CallSpacing<'a> {
    /// Current formatter coordinates.
    layout: &'a LayoutSource,
    /// Collected whitespace edits.
    edits: Vec<FormatEdit>,
}

impl CallSpacing<'_> {
    /// Replaces a horizontal gap between two typed expressions.
    fn gap(&mut self, left: &Expr, right: &Expr, replacement: &str) {
        let (Some(left), Some(right)) = (
            self.layout.syntax_range(left),
            self.layout.syntax_range(right),
        ) else {
            return;
        };
        if left.end <= right.start
            && self.layout.text()[left.end..right.start]
                .bytes()
                .all(|byte| matches!(byte, b' ' | b'\t'))
        {
            self.edits.push(FormatEdit {
                start_byte: left.end,
                end_byte: right.start,
                replacement: replacement.to_string(),
            });
        }
    }
}

impl<'ast> Visit<'ast> for CallSpacing<'_> {
    fn visit_expr_op_bin_adj(&mut self, call: &'ast m2_syn::nodes::ExprOpBinAdj) {
        if matches!(
            call.right_operand.as_ref(),
            Expr::Holder(_) | Expr::Collection(m2_syn::nodes::ExprCollection::Sequence(_))
        ) {
            self.gap(&call.left_operand, &call.right_operand, "");
        }
        visit::visit_expr_op_bin_adj(self, call);
    }
    fn visit_expr_assign_install_method(
        &mut self,
        installation: &'ast m2_syn::nodes::ExprAssignInstallMethod,
    ) {
        self.gap(&installation.method, &installation.signature, " ");
        visit::visit_expr_assign_install_method(self, installation);
    }
}

/// Spacing edits for generated regular binary expressions.
struct BinarySpacing<'a> {
    /// Current whitespace coordinates.
    layout: &'a LayoutSource,
    /// Whether adjacent factors use compact operators.
    compact_factors: bool,
    /// Collected whitespace replacements.
    edits: Vec<FormatEdit>,
}

impl BinarySpacing<'_> {
    /// Whether an expression can be written as an adjacent factor.
    fn is_factor(expression: &Expr) -> bool {
        matches!(
            expression,
            Expr::Symbol(_)
                | Expr::Literal(_)
                | Expr::Collection(_)
                | Expr::Holder(_)
                | Expr::Op(_)
        )
    }
}

impl<'ast> Visit<'ast> for BinarySpacing<'_> {
    fn visit_expr_op_bin_regular(&mut self, expression: &'ast m2_syn::nodes::ExprOpBinRegular) {
        if let Some(operator) = self.layout.syntax_range(&expression.operator) {
            let text = self.layout.text();
            let spacing = match binary_operator_spacing(&text[operator.clone()]) {
                OperatorSpacing::Factor
                    if self.compact_factors
                        && Self::is_factor(&expression.left_operand)
                        && Self::is_factor(&expression.right_operand) =>
                {
                    OperatorSpacing::Compact
                }
                OperatorSpacing::Factor => OperatorSpacing::Spaced,
                spacing => spacing,
            };
            let before = same_line_horizontal_whitespace_start(text, operator.start);
            let after = same_line_horizontal_whitespace_end(text, operator.end);
            if spacing == OperatorSpacing::Spaced
                || (spacing == OperatorSpacing::Compact && before.is_some() && after.is_some())
            {
                let replacement = if spacing == OperatorSpacing::Spaced {
                    " "
                } else {
                    ""
                };
                if let Some(start_byte) = before {
                    self.edits.push(FormatEdit {
                        start_byte,
                        end_byte: operator.start,
                        replacement: replacement.to_string(),
                    });
                }
                if let Some(end_byte) = after {
                    self.edits.push(FormatEdit {
                        start_byte: operator.end,
                        end_byte,
                        replacement: replacement.to_string(),
                    });
                }
            }
        }
        visit::visit_expr_op_bin_regular(self, expression);
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

/// Whether the operator text spells any of the listed token types:
/// `matches_tokens!(operator, Token![*], Token![/])`.
macro_rules! matches_tokens {
    ($text:expr, $($token:ty),+ $(,)?) => {
        false $(|| matches_token::<$token>($text))+
    };
}

/// Operator spacing required by an infix operator in the surrounding expression.
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

/// Operator spacing required by a prefix operator and its operand.
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

/// Whitespace after generated prefix operators.
struct PrefixSpacing<'a> {
    /// Current whitespace coordinates.
    layout: &'a LayoutSource,
    /// Collected whitespace replacements.
    edits: Vec<FormatEdit>,
}

impl<'ast> Visit<'ast> for PrefixSpacing<'_> {
    fn visit_expr_op_prefix(&mut self, expression: &'ast m2_syn::nodes::ExprOpPrefix) {
        if let Some(operator) = self.layout.syntax_range(&expression.operator) {
            let text = self.layout.text();
            let separator = match prefix_operator_spacing(&text[operator.clone()]) {
                OperatorSpacing::Prefix => Some(""),
                OperatorSpacing::SpacedPrefix => Some(" "),
                _ => None,
            };
            if let (Some(separator), Some(end_byte)) = (
                separator,
                same_line_horizontal_whitespace_end(text, operator.end),
            ) {
                self.edits.push(FormatEdit {
                    start_byte: operator.end,
                    end_byte,
                    replacement: separator.to_string(),
                });
            }
        }
        visit::visit_expr_op_prefix(self, expression);
    }
}

/// Semicolon spacing with the enclosing collection context.
struct SemicolonSpacing<'a> {
    /// Current whitespace coordinates.
    layout: &'a LayoutSource,
    /// Whether statements normally end a line.
    break_after: bool,
    /// Whether the immediate statement container is a collection.
    in_collection: bool,
    /// Collected whitespace replacements.
    edits: Vec<FormatEdit>,
}

impl<'ast> Visit<'ast> for SemicolonSpacing<'_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        let previous = self.in_collection;
        self.in_collection = matches!(expression, Expr::Collection(_));
        visit::visit_expr(self, expression);
        self.in_collection = previous;
    }
    fn visit_cell(&mut self, cell: &'ast m2_syn::nodes::Cell) {
        if let Some(semicolon) = &cell.semicolon {
            if let Some(bytes) = self.layout.syntax_range(semicolon) {
                push_semicolon_whitespace_edits(
                    self.layout.text(),
                    bytes,
                    self.break_after,
                    &mut self.edits,
                );
            }
        }
        visit::visit_cell(self, cell);
    }
    fn visit_statement(&mut self, statement: &'ast m2_syn::nodes::Statement) {
        if let Some(bytes) = self.layout.syntax_range(&statement.semicolon) {
            push_semicolon_whitespace_edits(
                self.layout.text(),
                bytes,
                self.break_after && !self.in_collection,
                &mut self.edits,
            );
        }
        visit::visit_statement(self, statement);
    }
}

/// Appends semicolon whitespace edits to the current formatting edits or traversal results.
fn push_semicolon_whitespace_edits(
    text: &str,
    semicolon: std::ops::Range<usize>,
    break_after_semicolon: bool,
    edits: &mut Vec<FormatEdit>,
) {
    if let Some(start_byte) = same_line_horizontal_whitespace_start(text, semicolon.start) {
        edits.push(FormatEdit {
            start_byte,
            end_byte: semicolon.start,
            replacement: String::new(),
        });
    }

    let Some(end_byte) = same_line_horizontal_whitespace_end(text, semicolon.end) else {
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
            start_byte: semicolon.end,
            end_byte,
            replacement: replacement.to_string(),
        });
    }
}

/// Spacing just inside typed list braces.
struct BraceSpacing<'a> {
    /// Current whitespace coordinates.
    layout: &'a LayoutSource,
    /// Configured padding, or none to preserve it.
    space_in_braces: Option<bool>,
    /// Collected whitespace replacements.
    edits: Vec<FormatEdit>,
}

impl<'ast> Visit<'ast> for BraceSpacing<'_> {
    fn visit_list(&mut self, list: &'ast List) {
        let delimiter = list.delimiter.delimiter();
        if let (Some(opener), Some(closer)) = (
            self.layout.syntax_range(&delimiter.opening_span()),
            delimiter
                .closing_span()
                .and_then(|span| self.layout.syntax_range(&span)),
        ) {
            self.space(opener, closer);
        }
        visit::visit_list(self, list);
    }
}

impl BraceSpacing<'_> {
    /// Applies brace padding without crossing line breaks or literal contents.
    fn space(&mut self, opener: std::ops::Range<usize>, closer: std::ops::Range<usize>) {
        if opener.end > closer.start {
            return;
        }
        let text = self.layout.text();
        let inside = &text[opener.end..closer.start];
        if inside.bytes().all(|byte| matches!(byte, b' ' | b'\t')) {
            self.edits.push(FormatEdit {
                start_byte: opener.end,
                end_byte: closer.start,
                replacement: String::new(),
            });
            return;
        }
        let Some(space_in_braces) = self.space_in_braces else {
            return;
        };
        let padding = if space_in_braces { " " } else { "" };
        if let Some(end_byte) = same_line_horizontal_whitespace_end(text, opener.end) {
            self.edits.push(FormatEdit {
                start_byte: opener.end,
                end_byte,
                replacement: padding.to_string(),
            });
        }
        if let Some(start_byte) = same_line_horizontal_whitespace_start(text, closer.start) {
            self.edits.push(FormatEdit {
                start_byte,
                end_byte: closer.start,
                replacement: padding.to_string(),
            });
        }
    }
}

/// Locates horizontal whitespace start without crossing a source line boundary.
fn same_line_horizontal_whitespace_start(text: &str, byte_index: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut start_byte = byte_index;
    while start_byte > 0 && matches!(bytes[start_byte - 1], b' ' | b'\t') {
        start_byte -= 1;
    }

    (start_byte > 0 && !matches!(bytes[start_byte - 1], b'\n' | b'\r')).then_some(start_byte)
}

/// Locates horizontal whitespace end without crossing a source line boundary.
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
    use crate::node_metadata::SyntaxSnapshot;

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
            keep_line_breaks: false,
            space_in_braces: None,
        })
    }

    fn format_fixture_with_options(text: &str, options: &FormatOptions) -> String {
        format_document_with_options(&SyntaxSnapshot::parse(text, None), options)
    }

    fn fold_fixture(text: &str) -> Vec<FormatFoldRange> {
        folding_ranges_for_syntax(&SyntaxSnapshot::parse(text, None))
    }

    fn format_fixture(text: &str) -> String {
        format_fixture_with_options(text, &fixture_options(4, true))
    }

    #[test]
    fn layout_edits_keep_the_supplied_syntax_and_token_contents() {
        let input = "  x:= f (\"λ 😀  \",1+2)  \r\n";
        let syntax = SyntaxSnapshot::parse(input, None);
        let mut source = LayoutSource::new(&syntax);
        let options = fixture_options(2, true);
        let spacing = normalize_whitespace(&syntax, &source, &options);
        assert!(source.apply(spacing));
        let indentation = reindent_from_syntax(&syntax, &source, &options);
        assert!(source.apply(indentation));
        assert_eq!(source.into_text(), "x := f(\"λ 😀  \", 1 + 2)\r\n");
        assert_eq!(syntax.source(), input);
    }

    #[test]
    fn layout_rejects_edits_inside_literal_whitespace() {
        let input = "x = \"a  b\"\n";
        let mut source = LayoutSource::new(&SyntaxSnapshot::parse(input, None));
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
    fn comma_wrap_candidates_exclude_empty_argument_boundaries() {
        let text = "f(a,,b,c)\n";
        let tree = SyntaxSnapshot::parse(text, None);
        let layout = LayoutSource::new(&tree);
        let mut visitor = CommaBreaks {
            layout: &layout,
            candidates: Vec::new(),
            spacing: Vec::new(),
        };
        tree.visit(&mut visitor);
        assert_eq!(
            visitor.candidates,
            vec![LineBreakCandidate {
                start_byte: 7,
                end_byte: 7
            }]
        );
    }

    #[test]
    fn operator_wrap_candidates_respect_delimiters_and_assignments() {
        let text = "(a + b)\nx = y\n";
        let tree = SyntaxSnapshot::parse(text, None);
        let layout = LayoutSource::new(&tree);
        let mut visitor = OperatorBreaks {
            layout: &layout,
            compact_factors: false,
            depth: 0,
            candidates: Vec::new(),
        };
        tree.visit(&mut visitor);
        assert_eq!(
            visitor.candidates,
            vec![
                LineBreakCandidate {
                    start_byte: 2,
                    end_byte: 3
                },
                LineBreakCandidate {
                    start_byte: 11,
                    end_byte: 12
                }
            ]
        );
    }

    #[test]
    fn operator_wrap_candidates_cover_evaluated_parallel_targets() {
        let text = "(x <- y, z) = (1, 2)\n";
        let tree = SyntaxSnapshot::parse(text, None);
        let layout = LayoutSource::new(&tree);
        let mut visitor = OperatorBreaks {
            layout: &layout,
            compact_factors: false,
            depth: 0,
            candidates: Vec::new(),
        };
        tree.visit(&mut visitor);
        assert!(visitor
            .candidates
            .iter()
            .any(|candidate| candidate.start_byte == 5 && candidate.end_byte == 6));
    }

    #[test]
    fn semicolon_spacing_distinguishes_blocks_from_collections() {
        let text = "f = () -> (a; b)\nx = {a; b}\n";
        let tree = SyntaxSnapshot::parse(text, None);
        let layout = LayoutSource::new(&tree);
        let mut visitor = SemicolonSpacing {
            layout: &layout,
            break_after: true,
            in_collection: false,
            edits: Vec::new(),
        };
        tree.visit(&mut visitor);
        let semicolons = text
            .match_indices(';')
            .map(|(index, _)| index + 1)
            .collect::<Vec<_>>();
        let replacement = |start| {
            visitor
                .edits
                .iter()
                .find(|edit| edit.start_byte == start)
                .map(|edit| edit.replacement.as_str())
        };
        assert_eq!(replacement(semicolons[0]), Some("\n"));
        assert_eq!(replacement(semicolons[1]), Some(" "));
    }

    #[test]
    fn indents_comments_without_changing_their_contents() {
        assert_eq!(
            format_fixture("f = () -> (\n-- comment  \nx = 1\n)\n"),
            "f = () -> (\n    -- comment  \n    x = 1\n)\n"
        );
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
    fn folds_comments_retained_before_a_fatal_lexical_error() {
        assert_eq!(
            fold_fixture("-- first\n-- second\nx = )\ny = 1\n"),
            vec![FormatFoldRange {
                start_line: 0,
                end_line: 1,
                kind: FormatFoldKind::Comment
            }]
        );
    }

    #[test]
    fn marks_comment_folds_for_lsp_clients() {
        let text = "-- first\n-- second\n";
        let ranges = folding_ranges(&SyntaxSnapshot::parse(text, None));

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
            assert!(!SyntaxSnapshot::parse(&formatted, None).has_error());
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

        for source in [
            "result = concatenate(alpha, beta, gamma)\n",
            "values = {alpha, beta, gamma, delta}\n",
            "result = alpha + beta + gamma + delta\n",
            "nested = outer(alpha, inner(beta, gamma), delta)\n",
        ] {
            let formatted = format_fixture_with_options(source, &options);
            let has_error = SyntaxSnapshot::parse(&formatted, None).has_error();
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
            let tree = SyntaxSnapshot::parse(text, None);
            assert!(!tree.has_error(), "source should parse:\n{text}");
            let layout = LayoutSource::new(&tree);
            layout
                .protected_ranges()
                .map(|range| text[range].to_string())
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

        let leading = FormatOptions {
            control_flow_layout: ControlFlowLayout::LeadingKeywords,
            ..fixture_options(4, true)
        };
        assert_eq!(
            format_fixture_with_options(text, &leading),
            "value := (\n\
             \x20   if a\n\
             \x20   then one\n\
             \x20   else if b\n\
             \x20   then two\n\
             \x20   else three\n\
             )\n"
        );
    }

    fn settings_options(settings: FormattingSettings) -> FormatOptions {
        FormatOptions::from_settings(4, true, &settings)
    }

    #[test]
    fn leading_keywords_keep_bodies_after_their_keyword() {
        let options = settings_options(FormattingSettings {
            control_flow_layout: ControlFlowLayout::LeadingKeywords,
            use_tabs: Some(false),
            ..FormattingSettings::default()
        });
        assert_eq!(
            format_fixture_with_options(
                "f = x -> (\nif x then (\na;\nb\n) else c\n)\nif top then 1\n",
                &options
            ),
            "f = x -> (\n    if x\n    then (\n        a;\n        b\n    )\n    else c\n)\nif top\nthen 1\n"
        );
    }

    #[test]
    fn space_in_braces_pads_strips_or_keeps_list_braces() {
        let text = "a = { b, {c}, {  } }\nd = {e,\n    f }\n";
        let with = |space_in_braces| {
            format_fixture_with_options(
                text,
                &settings_options(FormattingSettings {
                    space_in_braces,
                    use_tabs: Some(false),
                    ..FormattingSettings::default()
                }),
            )
        };
        assert_eq!(
            with(Some(true)),
            "a = { b, { c }, {} }\nd = { e,\n    f }\n"
        );
        assert_eq!(with(Some(false)), "a = {b, {c}, {}}\nd = {e,\n    f}\n");
        assert_eq!(with(None), "a = { b, {c}, {} }\nd = {e,\n    f }\n");
    }

    #[test]
    fn keep_line_breaks_changes_only_spacing_and_indentation() {
        let text = concat!(
            "f = x -> (\n",
            "a := {b,c};\n",
            "y := if x then s1\n",
            "else s2;\n",
            "q := a +\n",
            "b; r := 1\n",
            "    )\n",
            "g = {x => 1, y => 2}\n",
        );
        let options = settings_options(FormattingSettings {
            keep_line_breaks: true,
            use_tabs: Some(false),
            control_flow_layout: ControlFlowLayout::Multiline,
            break_after_semicolon: true,
            hard_line_width: Some(10),
            ..FormattingSettings::default()
        });
        let formatted = format_fixture_with_options(text, &options);
        assert_eq!(formatted.lines().count(), text.lines().count());
        assert_eq!(
            formatted,
            concat!(
                "f = x -> (\n",
                "    a := {b, c};\n",
                "    y := if x then s1\n",
                "        else s2;\n",
                "    q := a +\n",
                "        b; r := 1\n",
                ")\n",
                "g = {x => 1, y => 2}\n",
            )
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
                !SyntaxSnapshot::parse(&formatted, None).has_error(),
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
