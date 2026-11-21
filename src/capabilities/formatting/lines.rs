//! Where lines break: the layout of multi-line constructs, blank lines, and
//! wrapping lines that exceed the width.
//!
//! Every edit here turns a gap between two tokens into a line break or back.
//! A break goes only where Macaulay2 reads a newline as whitespace: inside a
//! bracket, after an operator or assignment still waiting for its right
//! side, or before the `then` of an unfinished `if`. A gap after a comment is
//! never joined, since that would comment out the code it joins.

use std::ops::Range;

use m2_syn::nodes::{
    Cell, DoClause, ElementsPack, Expr, ExprAssign, ExprBinary, ExprFunction, ExprIf, ParamsPack,
};
use m2_syn::visit::{self, Visit};
use m2_syn::{Punctuated, Spanned};

use super::layout::LayoutSource;
use super::{Bracket, FormatEdit, TextLine, Whitespace};
use crate::node_metadata::SyntaxSnapshot;
use crate::settings::{
    CloserPlacement, ConditionBreaks, ElsePlacement, FormattingSettings, FunctionBodyLayout,
    SingleLineIf, StatementLayout, ThenPlacement,
};
use crate::source::SourceNavigation;
use crate::syntax::ExpressionSyntax;

/// How deeply each byte of a layout is nested in brackets.
struct Nesting {
    /// Where each bracket's interior starts, ascending.
    opens: Vec<usize>,
    /// Where each closed bracket's interior ends, ascending.
    closes: Vec<usize>,
}

impl Nesting {
    /// The nesting of `brackets`.
    fn new(brackets: &[Bracket]) -> Self {
        let mut opens = brackets
            .iter()
            .map(|bracket| bracket.open.end)
            .collect::<Vec<_>>();
        let mut closes = brackets
            .iter()
            .filter_map(|bracket| Some(bracket.close.as_ref()?.start))
            .collect::<Vec<_>>();
        opens.sort_unstable();
        closes.sort_unstable();
        Self { opens, closes }
    }

    /// How many brackets enclose `byte`.
    fn depth(&self, byte: usize) -> usize {
        self.opens.partition_point(|open| *open <= byte)
            - self.closes.partition_point(|close| *close < byte)
    }
}

/// Line-break edits around tokens of one layout.
struct Breaks<'a> {
    /// The layout as it stands.
    layout: &'a LayoutSource,
    /// The document's line terminator.
    newline: &'static str,
    /// The edits collected so far.
    edits: Vec<FormatEdit>,
}

impl Breaks<'_> {
    /// The text of the layout.
    fn text(&self) -> &str {
        self.layout.text()
    }

    /// Whether `byte` starts its line, after indentation.
    fn starts_line(&self, byte: usize) -> bool {
        Whitespace::start_before(self.text(), byte).is_none()
    }

    /// Whether only spaces, tabs or a comment follow `byte` on its line.
    fn ends_line(&self, byte: usize) -> bool {
        Whitespace::end_after(self.text(), byte)
            .is_none_or(|end| self.layout.starts_line_comment(end))
    }

    /// Whether `bytes` runs over more than one line.
    fn spans_lines(&self, bytes: &Range<usize>) -> bool {
        self.text()[bytes.clone()].contains('\n')
    }

    /// Break the line before `byte`, if code precedes it on its line.
    fn split_before(&mut self, byte: usize) {
        if let Some(start) = Whitespace::start_before(self.text(), byte) {
            self.edits.push(FormatEdit::new(start..byte, self.newline));
        }
    }

    /// Break the line after `byte`, if code follows it on its line.
    fn split_after(&mut self, byte: usize) {
        if !self.ends_line(byte) {
            let end = Whitespace::end_after(self.text(), byte).unwrap_or(byte);
            self.edits.push(FormatEdit::new(byte..end, self.newline));
        }
    }

    /// Join the line holding `byte` to the code before it, `padding` apart.
    fn join_before(&mut self, byte: usize, padding: &str) {
        let start = self.text()[..byte].trim_end().len();
        if start > 0 && self.spans_lines(&(start..byte)) && !self.layout.is_protected_at(start - 1)
        {
            self.edits.push(FormatEdit::new(start..byte, padding));
        }
    }

    /// Join the code after `byte` to its line, `padding` apart.
    fn join_after(&mut self, byte: usize, padding: &str) {
        let end = self.text().len() - self.text()[byte..].trim_start().len();
        if end < self.text().len() && self.spans_lines(&(byte..end)) {
            self.edits.push(FormatEdit::new(byte..end, padding));
        }
    }
}

/// One stage of [`LineStructure`]; each reads the rows the previous one left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Conditionals and the bodies of functions and loops.
    Clauses,
    /// One statement per line in multi-line brackets.
    Statements,
}

/// The `layout.*` rules, under `lines.breaks = "normalize"`.
pub(super) struct LineStructure<'a> {
    /// Edits against the current layout.
    breaks: Breaks<'a>,
    /// The settings in effect.
    settings: &'a FormattingSettings,
    /// Bracket nesting of the current layout.
    nesting: Nesting,
    /// The stage being collected.
    stage: Stage,
    /// How many `if` conditions enclose the node being visited.
    conditions: usize,
}

impl<'a> LineStructure<'a> {
    /// Apply every stage to `layout`, then place the closing brackets;
    /// returns whether anything changed. A stage decides on the columns it
    /// starts from, so callers re-indent and apply it again until it
    /// changes nothing.
    pub(super) fn apply(
        syntax: &SyntaxSnapshot,
        layout: &mut LayoutSource,
        settings: &FormattingSettings,
        newline: &'static str,
    ) -> bool {
        let mut changed = false;
        for stage in [Stage::Clauses, Stage::Statements] {
            let brackets = Bracket::all(syntax, layout);
            let mut structure = LineStructure {
                breaks: Breaks {
                    layout,
                    newline,
                    edits: Vec::new(),
                },
                settings,
                nesting: Nesting::new(&brackets),
                stage,
                conditions: 0,
            };
            syntax.visit(&mut structure);
            let edits = structure.breaks.edits;
            changed |= layout.apply(edits);
        }
        let edits = Self::closers(syntax, layout, settings, newline);
        layout.apply(edits) || changed
    }

    /// `layout.closeParen` and `layout.closeBrace` for every multi-line
    /// group opened at the end of a line; a group with content after its
    /// opener always hugs its closer.
    fn closers(
        syntax: &SyntaxSnapshot,
        layout: &LayoutSource,
        settings: &FormattingSettings,
        newline: &'static str,
    ) -> Vec<FormatEdit> {
        let mut breaks = Breaks {
            layout,
            newline,
            edits: Vec::new(),
        };
        for bracket in Bracket::all(syntax, layout) {
            let Some(close) = &bracket.close else {
                continue;
            };
            if !breaks.spans_lines(&(bracket.open.end..close.start)) {
                continue;
            }
            let placement = if !breaks.ends_line(bracket.open.end) {
                CloserPlacement::Hug
            } else if bracket.is_brace() {
                settings.layout.close_brace
            } else {
                settings.layout.close_paren
            };
            match placement {
                CloserPlacement::Hug if breaks.starts_line(close.start) => {
                    breaks.join_before(close.start, "");
                }
                CloserPlacement::OwnLine => breaks.split_before(close.start),
                CloserPlacement::Hug => {}
            }
        }
        breaks.edits
    }

    /// The bytes of `node` in the current layout.
    fn bytes(&self, node: &(impl Spanned + ?Sized)) -> Option<Range<usize>> {
        self.breaks.layout.syntax_range(node)
    }

    /// `layout.functionBody` for a body, `-> (…)` or `do (…)`: the line
    /// breaks after its opener when it must.
    fn body(&mut self, body: &Expr) {
        let statements = match body {
            Expr::Paren(paren) => paren.stmts.len() + usize::from(paren.expr.is_some()),
            Expr::Sequence(sequence) => {
                sequence.stmts.len() + usize::from(sequence.elems.is_some())
            }
            _ => return,
        };
        let breaks = match self.settings.layout.function_body {
            FunctionBodyLayout::Always => true,
            FunctionBodyLayout::BreakWhenMultiStatement => statements > 1,
            FunctionBodyLayout::Keep => false,
        };
        let opener = body.delimiter().filter(|_| breaks);
        if let Some(open) = opener.and_then(|delimiter| self.bytes(&delimiter.opening_span())) {
            self.breaks.split_after(open.end);
        }
    }

    /// One statement per line inside a multi-line bracket.
    fn statements(&mut self, semicolons: impl Iterator<Item = Range<usize>>, holder: &Expr) {
        let multi_line = self
            .bytes(holder)
            .is_some_and(|bytes| self.breaks.spans_lines(&bytes));
        if !multi_line || self.settings.layout.statements != StatementLayout::OnePerLine {
            return;
        }
        for semicolon in semicolons {
            let text = self.breaks.text();
            let closes = Whitespace::end_after(text, semicolon.end)
                .is_some_and(|end| matches!(text.as_bytes()[end], b')' | b']' | b'}' | b'|'));
            if !closes {
                self.breaks.split_after(semicolon.end);
            }
        }
    }

    /// The `layout` rules for `then` and `else` of one `if`.
    fn conditional(&mut self, node: &ExprIf) {
        let (Some(whole), Some(then)) =
            (self.bytes(node), self.bytes(&node.then_clause.then_token))
        else {
            return;
        };
        let condition = self.bytes(node.cond.as_ref());
        match self.settings.layout.then_placement {
            ThenPlacement::WithCondition => self.breaks.join_before(then.start, " "),
            ThenPlacement::OwnLine => {
                if condition.is_some_and(|condition| self.breaks.spans_lines(&condition)) {
                    self.breaks.split_before(then.start);
                }
            }
        }
        let Some(otherwise) = &node.else_clause else {
            return;
        };
        let Some(else_token) = self.bytes(&otherwise.else_token) else {
            return;
        };
        if let (Expr::If(_), Some(inner)) =
            (otherwise.expr.as_ref(), self.bytes(otherwise.expr.as_ref()))
        {
            self.breaks.join_before(inner.start, " ");
        }
        let multi_line = self.breaks.spans_lines(&whole);
        let joins = match self.settings.layout.single_line_if {
            SingleLineIf::WhenFits => multi_line && self.fits_joined(node, &whole),
            SingleLineIf::Never | SingleLineIf::Keep => false,
        };
        if joins {
            for keyword in [then.clone(), else_token] {
                self.breaks.join_before(keyword.start, " ");
                self.breaks.join_after(keyword.end, " ");
            }
            return;
        }
        let stays_multi_line =
            multi_line || self.settings.layout.single_line_if == SingleLineIf::Never;
        if !stays_multi_line || self.nesting.depth(whole.start) == 0 {
            return;
        }
        match self.settings.layout.else_placement {
            ElsePlacement::OwnLine => self.breaks.split_before(else_token.start),
            ElsePlacement::AfterClose => {
                if self.breaks.text()[..else_token.start]
                    .trim_end()
                    .ends_with(')')
                {
                    self.breaks.join_before(else_token.start, " ");
                }
            }
        }
    }

    /// Whether `node`, spread over lines only at its own `then` and `else`,
    /// fits the width once those lines are joined, with what precedes and
    /// follows it on its first and last line, as [`Wrapping`] measures it.
    fn fits_joined(&self, node: &ExprIf, whole: &Range<usize>) -> bool {
        let parts = [
            Some(node.cond.as_ref()),
            Some(node.then_clause.expr.as_ref()),
            node.else_clause.as_ref().map(|clause| clause.expr.as_ref()),
        ];
        let single_line_parts = parts.into_iter().flatten().all(|part| {
            self.bytes(part)
                .is_some_and(|bytes| !self.breaks.spans_lines(&bytes))
        });
        let (soft, hard) = self.settings.line_widths();
        let Some(width) = soft.or(hard) else {
            return single_line_parts;
        };
        let text = self.breaks.text();
        let line_start = text[..whole.start]
            .rfind('\n')
            .map_or(0, |newline| newline + 1);
        let line_end = whole.end
            + text[whole.end..]
                .find(['\r', '\n'])
                .unwrap_or(text.len() - whole.end);
        let joined = text[whole.start..line_end]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let column = Whitespace::width(
            &text[line_start..whole.start],
            self.settings.indent.tab_width.get(),
        );
        single_line_parts && column + joined.chars().count() <= width
    }

    /// `layout.conditionBreaks` for an `or` or `and` in a condition.
    fn condition_operator(&mut self, operator: Range<usize>) {
        match self.settings.layout.condition_breaks {
            ConditionBreaks::BeforeOperator if self.breaks.ends_line(operator.end) => {
                self.breaks.join_after(operator.end, " ");
                self.breaks.split_before(operator.start);
            }
            ConditionBreaks::AfterOperator if self.breaks.starts_line(operator.start) => {
                self.breaks.join_before(operator.start, " ");
                self.breaks.split_after(operator.end);
            }
            _ => {}
        }
    }

    /// The `;` ending each of `statements`.
    fn semicolons(&self, statements: &[m2_syn::nodes::Stmt]) -> Vec<Range<usize>> {
        statements
            .iter()
            .filter_map(|statement| self.bytes(&statement.semi_token))
            .collect()
    }
}

impl<'ast> Visit<'ast> for LineStructure<'_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        if self.stage == Stage::Statements {
            let semicolons = match expression {
                Expr::Paren(paren) => self.semicolons(&paren.stmts),
                Expr::Sequence(sequence) => self.semicolons(&sequence.stmts),
                _ => Vec::new(),
            };
            self.statements(semicolons.into_iter(), expression);
        }
        visit::visit_expr(self, expression);
    }

    fn visit_cell(&mut self, node: &'ast Cell) {
        let semicolon = node
            .semi_token
            .as_ref()
            .and_then(|semicolon| self.bytes(semicolon));
        if let (Stage::Statements, StatementLayout::OnePerLine, Some(semicolon)) =
            (self.stage, self.settings.layout.statements, semicolon)
        {
            self.breaks.split_after(semicolon.end);
        }
        visit::visit_cell(self, node);
    }

    fn visit_expr_if(&mut self, node: &'ast ExprIf) {
        if self.stage == Stage::Clauses {
            self.conditional(node);
        }
        self.conditions += 1;
        self.visit_expr(&node.cond);
        self.conditions -= 1;
        self.visit_then_clause(&node.then_clause);
        if let Some(otherwise) = &node.else_clause {
            self.visit_else_clause(otherwise);
        }
    }

    fn visit_expr_binary(&mut self, node: &'ast ExprBinary) {
        if self.stage == Stage::Clauses
            && self.conditions > 0
            && matches!(node.op.spelling(), "or" | "and")
        {
            if let Some(operator) = self.bytes(&node.op) {
                self.condition_operator(operator);
            }
        }
        visit::visit_expr_binary(self, node);
    }

    fn visit_expr_function(&mut self, node: &'ast ExprFunction) {
        if self.stage == Stage::Clauses {
            self.body(&node.body);
        }
        visit::visit_expr_function(self, node);
    }

    fn visit_do_clause(&mut self, node: &'ast DoClause) {
        if self.stage == Stage::Clauses {
            self.body(&node.expr);
        }
        visit::visit_do_clause(self, node);
    }
}

/// Blank lines and the end of the file: runs longer than
/// `lines.maxBlankLines` shrink, blank lines just inside a multi-line bracket
/// go under `lines.trimBlankLinesInBrackets`, and the file ends with one line
/// break under `lines.finalNewline`. No blank line is ever added.
pub(super) struct BlankLines;

impl BlankLines {
    /// The blank-line edits of `layout`.
    pub(super) fn edits(
        syntax: &SyntaxSnapshot,
        layout: &LayoutSource,
        settings: &FormattingSettings,
        newline: &'static str,
    ) -> Vec<FormatEdit> {
        let text = layout.text();
        let lines = TextLine::all(text);
        let brackets = Bracket::all(syntax, layout);
        let mut openers_ending_lines = brackets
            .iter()
            .map(|bracket| bracket.open.end)
            .collect::<Vec<_>>();
        openers_ending_lines.sort_unstable();
        let mut closers = brackets
            .iter()
            .filter_map(|bracket| Some(bracket.close.as_ref()?.start))
            .collect::<Vec<_>>();
        closers.sort_unstable();
        let code_rows = (0..lines.len())
            .filter(|row| !lines[*row].is_blank(text))
            .collect::<Vec<_>>();
        let mut edits = Vec::new();
        for pair in code_rows.windows(2) {
            let (above, below) = (lines[pair[0]], lines[pair[1]]);
            let blank = pair[1] - pair[0] - 1;
            let trims = settings.lines.trim_blank_lines_in_brackets
                && (openers_ending_lines
                    .binary_search(&above.content_end(text))
                    .is_ok()
                    || closers.binary_search(&below.content_start(text)).is_ok());
            let kept = if trims {
                0
            } else {
                blank.min(settings.lines.max_blank_lines)
            };
            if kept < blank {
                edits.push(FormatEdit::new(
                    above.end_byte..below.start_byte,
                    newline.repeat(kept + 1),
                ));
            }
        }
        if let Some(&first) = code_rows.first() {
            let blank = first.min(settings.lines.max_blank_lines);
            edits.push(FormatEdit::new(
                0..lines[first].start_byte,
                newline.repeat(blank),
            ));
        }
        if let (Some(&last), true) = (code_rows.last(), settings.lines.final_newline) {
            edits.push(FormatEdit::new(lines[last].end_byte..text.len(), newline));
        }
        edits
    }
}

/// How good a place to break a long line is, best first (§5.5 of the
/// formatting design, by how often Core lines end or start there).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum BreakRank {
    /// After the `(` or `{` of a call or list.
    AfterOpener,
    /// After a `,`; the outermost commas of a line come first.
    AfterComma(usize),
    /// Before `else`, `then`, `or`, `and`, `<<` or `|`, in that order.
    BeforeKeyword(usize),
    /// After `:=` or `=`.
    AfterAssignment,
    /// After any other binary operator, when nothing else applies.
    AfterOperator,
}

/// A gap a long line may break at.
#[derive(Debug, Clone)]
struct BreakPoint {
    /// The horizontal whitespace replaced by the break, possibly empty.
    gap: Range<usize>,
    /// How good a place it is.
    rank: BreakRank,
}

/// Breaks every line longer than `lines.softWidth` once, at its best
/// [`BreakPoint`]; [`super::formatted_layout`] repeats it until all fit.
pub(super) struct Wrapping<'a> {
    /// The layout as it stands.
    layout: &'a LayoutSource,
    /// Bracket nesting of the layout.
    nesting: Nesting,
    /// Candidate breaks.
    points: Vec<BreakPoint>,
}

impl<'a> Wrapping<'a> {
    /// The keywords and operators broken before, in order of preference.
    const BEFORE: [&str; 6] = ["else", "then", "or", "and", "<<", "|"];
    /// Operators whose operands never part: indexing and member access.
    const UNBROKEN: [&str; 4] = ["_", "#", "^", "."];

    /// The wrapping edits of `layout`.
    pub(super) fn edits(
        syntax: &SyntaxSnapshot,
        layout: &'a LayoutSource,
        settings: &FormattingSettings,
        newline: &'static str,
    ) -> Vec<FormatEdit> {
        let (soft, hard) = settings.line_widths();
        let Some(width) = soft.or(hard) else {
            return Vec::new();
        };
        let brackets = Bracket::all(syntax, layout);
        let mut wrapping = Self {
            layout,
            nesting: Nesting::new(&brackets),
            points: Vec::new(),
        };
        for bracket in &brackets {
            if bracket
                .close
                .as_ref()
                .is_some_and(|close| close.start > bracket.open.end)
            {
                wrapping.after(bracket.open.end, BreakRank::AfterOpener);
            }
        }
        syntax.visit(&mut wrapping);
        let text = layout.text();
        let tab_width = settings.indent.tab_width.get();
        let column = |line: TextLine, byte: usize| {
            Whitespace::width(&text[line.start_byte..byte], tab_width)
        };
        let mut points = wrapping.points;
        points.sort_by_key(|point| point.gap.start);
        let lines = TextLine::all(text);
        let mut edits = Vec::new();
        for line in lines {
            if column(line, line.end_byte) <= width {
                continue;
            }
            let first = points.partition_point(|point| point.gap.start < line.content_start(text));
            let last = points.partition_point(|point| point.gap.end <= line.end_byte);
            let on_line = &points[first..last.max(first)];
            let Some(best) = on_line.iter().map(|point| point.rank).min() else {
                continue;
            };
            let ranked = on_line.iter().filter(|point| point.rank == best);
            let chosen = ranked
                .clone()
                .rfind(|point| column(line, point.gap.start) <= width)
                .or_else(|| ranked.clone().next());
            if let Some(point) = chosen {
                edits.push(FormatEdit::new(point.gap.clone(), newline));
            }
        }
        edits
    }

    /// A break after `byte`, if code follows it on its line.
    fn after(&mut self, byte: usize, rank: BreakRank) {
        if let Some(end) = Whitespace::end_after(self.layout.text(), byte) {
            if !self.layout.starts_line_comment(end) {
                self.points.push(BreakPoint {
                    gap: byte..end,
                    rank,
                });
            }
        }
    }

    /// A break before `byte`, if code precedes it on its line.
    fn before(&mut self, byte: usize, rank: BreakRank) {
        if let Some(start) = Whitespace::start_before(self.layout.text(), byte) {
            self.points.push(BreakPoint {
                gap: start..byte,
                rank,
            });
        }
    }

    /// Breaks after the commas of `values`.
    fn commas<T: Spanned>(&mut self, values: &Punctuated<T>) {
        for (comma, _) in values.pairs() {
            if let Some(comma) = self.layout.syntax_range(comma) {
                let depth = self.nesting.depth(comma.start);
                self.after(comma.end, BreakRank::AfterComma(depth));
            }
        }
    }

    /// A break before a keyword or operator of [`Self::BEFORE`]: only inside
    /// brackets, where the newline cannot end the expression, except before
    /// `then`, which an unfinished `if` always waits for.
    fn before_keyword(&mut self, token: &impl Spanned) {
        let Some(bytes) = self.layout.syntax_range(token) else {
            return;
        };
        let spelling = &self.layout.text()[bytes.clone()];
        let Some(preference) = Self::BEFORE.iter().position(|keyword| *keyword == spelling) else {
            return;
        };
        if spelling == "then" || self.nesting.depth(bytes.start) > 0 {
            self.before(bytes.start, BreakRank::BeforeKeyword(preference));
        }
    }
}

impl<'ast> Visit<'ast> for Wrapping<'_> {
    fn visit_expr_binary(&mut self, node: &'ast ExprBinary) {
        let spelling = node.op.spelling();
        if Self::BEFORE.contains(&spelling) {
            self.before_keyword(&node.op);
        } else if !matches!(node.op, m2_syn::nodes::BinaryOperator::Space(_))
            && !Self::UNBROKEN.contains(&spelling)
        {
            if let Some(operator) = self.layout.syntax_range(&node.op) {
                self.after(operator.end, BreakRank::AfterOperator);
            }
        }
        visit::visit_expr_binary(self, node);
    }

    fn visit_expr_assign(&mut self, node: &'ast ExprAssign) {
        if let Some(operator) = self.layout.syntax_range(&node.op) {
            self.after(operator.end, BreakRank::AfterAssignment);
        }
        visit::visit_expr_assign(self, node);
    }

    fn visit_expr_if(&mut self, node: &'ast ExprIf) {
        self.before_keyword(&node.then_clause.then_token);
        if let Some(otherwise) = &node.else_clause {
            self.before_keyword(&otherwise.else_token);
        }
        visit::visit_expr_if(self, node);
    }

    fn visit_elements_pack(&mut self, node: &'ast ElementsPack) {
        self.commas(&node.elems);
        visit::visit_elements_pack(self, node);
    }

    fn visit_params_pack(&mut self, node: &'ast ParamsPack) {
        self.commas(&node.params);
        visit::visit_params_pack(self, node);
    }
}
