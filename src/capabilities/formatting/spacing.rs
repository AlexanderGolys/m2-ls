//! The whitespace between tokens on one line. A gap beside a line break is
//! indentation, never spacing, and is left to the other passes.
//!
//! Each gap is decided by the first rule that applies:
//!
//! 1. a gap inside a string or comment is never touched;
//! 2. the gap before a trailing comment is one space;
//! 3. just inside brackets, `spacing.bracketsInside` for that kind of pair;
//!    an empty pair closes up;
//! 4. before `,` and `;`, nothing;
//! 5. after `,`, `spacing.comma`, or `spacing.lambdaParamComma` between lambda
//!    parameters; nothing before a closer or beside an empty slot (`f(a,,b)`);
//! 6. after `;` with more code on the line, `spacing.semicolon`;
//! 7. between a keyword and a bracket, `spacing.keywordParen`: `then (`;
//! 8. between a function and its parenthesized argument, `spacing.callParen`;
//! 9. around a binary operator, `spacing.operators`, tight for `+ - * /`
//!    inside an index bracket (`x#(i+1)`) under `spacing.indexContextTight`;
//! 10. between a prefix or postfix operator and its operand, tight unless
//!     `spacing.operators.spaced` lists it (`<< x`).
//!
//! Anything else keeps its spacing. Under `spacing.preserveAlignment`, padding
//! of more than one space before an operator, after a comma or before a
//! trailing comment stays when the token it pads sits at the same column on
//! the neighbouring line:
//!
//! ```m2
//! isDocumentableThing    String :=
//! isDocumentableThing  Sequence := key -> false
//! ```

use std::collections::HashSet;
use std::ops::Range;

use m2_syn::nodes::{
    BinaryOperator, DoClause, Element, ElementsPack, ElseClause, Expr, ExprAssign, ExprBinary,
    ExprField, ExprFunction, ExprIf, ExprOption, ExprPostfix, ExprPrefix, ExprReturn, ExprWhile,
    ListClause, ParamsPack, Stmt, ThenClause,
};
use m2_syn::visit::{self, Visit};
use m2_syn::{CommentKind, Punctuated, Spanned};

use super::layout::LayoutSource;
use super::{Bracket, FormatEdit, FormatStyle, TextLine, Whitespace};
use crate::node_metadata::SyntaxSnapshot;
use crate::settings::spacing::Spacing;
use crate::settings::{Keepable, TrailingComment};
use crate::source::SourceNavigation;
use crate::syntax::{AssignmentSyntax, AssignmentTarget, AssignmentTraversal, ExpressionSyntax};

/// Whether the innermost bracket around a node indexes, as the parentheses
/// of `x#(i+1)` and `x_(n-1)` do: a bracket that is the right operand of an
/// indexing operator.
#[derive(Debug, Default)]
pub(super) struct IndexContext {
    /// Right operands of indexing operators, by allocation.
    indices: HashSet<*const Expr>,
    /// For each enclosing bracket, innermost last, whether it indexes.
    enclosing: Vec<bool>,
}

impl IndexContext {
    /// The operators tight in an index under `spacing.indexContextTight`.
    pub(super) const ARITHMETIC: [&str; 4] = ["+", "-", "*", "/"];
    /// The operators whose bracketed right operand is an index. Member access
    /// `.` is one too, but its member is always a symbol, never a bracket.
    const INDEXING: [&str; 3] = ["_", "#", "^"];

    /// Remember `node`'s right operand if it is an index.
    pub(super) fn note(&mut self, node: &ExprBinary) {
        if Self::INDEXING.contains(&node.op.spelling()) {
            self.indices.insert(std::ptr::from_ref(node.right.as_ref()));
        }
    }

    /// Enter `expression`; returns whether it is a bracket, to pass to
    /// [`Self::leave`] after its children.
    pub(super) fn enter(&mut self, expression: &Expr) -> bool {
        let bracket = expression.delimiter().is_some();
        if bracket {
            self.enclosing
                .push(self.indices.contains(&std::ptr::from_ref(expression)));
        }
        bracket
    }

    /// Leave an expression [`Self::enter`] reported as `bracket`.
    pub(super) fn leave(&mut self, bracket: bool) {
        if bracket {
            self.enclosing.pop();
        }
    }

    /// Whether the innermost enclosing bracket indexes.
    pub(super) fn is_inside(&self) -> bool {
        self.enclosing.last() == Some(&true)
    }
}

/// Padding that lines a token up with the same token on a neighbouring line.
struct Alignment<'a> {
    /// The text as written.
    text: &'a str,
    /// Its lines.
    lines: Vec<TextLine>,
    /// Columns per tab.
    tab_width: usize,
}

impl Alignment<'_> {
    /// Whether the text at `token` also starts at the same column of the
    /// nearest non-blank line before or after it, after a space there too:
    /// the dashes of a rule line do not line up with a comment's `--`.
    fn holds(&self, token: Range<usize>) -> bool {
        let row = TextLine::row_of(&self.lines, token.start);
        let line = self.lines[row];
        let column = Whitespace::width(&self.text[line.start_byte..token.start], self.tab_width);
        let spelling = &self.text[token];
        let nearest = |rows: &mut dyn Iterator<Item = usize>| {
            rows.map(|row| self.lines[row])
                .find(|line| !line.is_blank(self.text))
        };
        [
            nearest(&mut (0..row).rev()),
            nearest(&mut (row + 1..self.lines.len())),
        ]
        .into_iter()
        .flatten()
        .any(|neighbour| {
            self.byte_at_column(neighbour, column).is_some_and(|byte| {
                self.text[byte..neighbour.end_byte].starts_with(spelling)
                    && self.text[..byte].ends_with([' ', '\t'])
            })
        })
    }

    /// The byte of `line` that starts at visual `column`, if one does.
    fn byte_at_column(&self, line: TextLine, column: usize) -> Option<usize> {
        let text = line.text(self.text);
        text.char_indices()
            .map(|(offset, _)| offset)
            .chain([text.len()])
            .find(|offset| Whitespace::width(&text[..*offset], self.tab_width) >= column)
            .filter(|offset| Whitespace::width(&text[..*offset], self.tab_width) == column)
            .map(|offset| line.start_byte + offset)
    }
}

/// The spacing edits of one document.
pub(super) struct HorizontalSpacing<'a> {
    /// The document as written.
    layout: &'a LayoutSource,
    /// The style in effect.
    style: &'a FormatStyle<'a>,
    /// Where padding lines tokens up.
    alignment: Alignment<'a>,
    /// Which brackets index.
    index: IndexContext,
    /// The edits collected so far.
    edits: Vec<FormatEdit>,
}

impl<'a> HorizontalSpacing<'a> {
    /// Comment lines of this many dashes and no other text are section
    /// rules, which `comments.bannerWidth` evens out: the Core library's are
    /// 77 dashes in 95 % of cases, the rest within this range.
    const BANNER_DASHES: std::ops::RangeInclusive<usize> = 60..=90;

    /// Every spacing edit of the document in `layout`, laid out as written,
    /// in the order of the module documentation's rules.
    pub(super) fn edits(
        syntax: &SyntaxSnapshot,
        layout: &'a LayoutSource,
        style: &'a FormatStyle<'a>,
    ) -> Vec<FormatEdit> {
        let text = layout.text();
        let mut spacing = Self {
            layout,
            style,
            alignment: Alignment {
                text,
                lines: TextLine::all(text),
                tab_width: style.settings.indent.tab_width.get(),
            },
            index: IndexContext::default(),
            edits: Vec::new(),
        };
        spacing.comments(syntax);
        for bracket in Bracket::all(syntax, layout) {
            spacing.inside(&bracket);
        }
        syntax.visit(&mut spacing);
        spacing.edits
    }

    /// Set the gap `bytes`, if it is spaces and tabs on one line, to
    /// `spacing`; under `preserveAlignment` padding wider than one space
    /// stays when the token at `aligned` lines up.
    fn gap(&mut self, bytes: Range<usize>, spacing: Spacing, aligned: Option<Range<usize>>) {
        let Some(width) = self.layout.horizontal_gap(bytes.clone()) else {
            return;
        };
        let text = self.layout.text();
        if spacing == Spacing::Tight && Whitespace::fuses(&text[..bytes.start], &text[bytes.end..])
        {
            return;
        }
        let keeps_alignment = width > 1
            && self.style.settings.spacing.preserve_alignment
            && aligned.is_some_and(|token| self.alignment.holds(token));
        if !keeps_alignment {
            self.edits.push(FormatEdit::new(bytes, spacing.padding()));
        }
    }

    /// The gap before `token` on its line, if anything precedes it there.
    fn before(&self, token: &Range<usize>) -> Option<Range<usize>> {
        Whitespace::start_before(self.layout.text(), token.start).map(|start| start..token.start)
    }

    /// The gap after `token` on its line, if anything follows it there.
    fn after(&self, token: &Range<usize>) -> Option<Range<usize>> {
        Whitespace::end_after(self.layout.text(), token.end).map(|end| token.end..end)
    }

    /// Rule 2 and banners: the gap before each trailing line comment, and
    /// the dashes of each rule-only comment line.
    fn comments(&mut self, syntax: &SyntaxSnapshot) {
        let text = self.layout.text();
        let banner = self
            .style
            .settings
            .comments
            .banner_width
            .filter(|width| *width > 0);
        for comment in syntax.cells().iter().flat_map(|cell| cell.trivia()) {
            if comment.kind() != CommentKind::Line {
                continue;
            }
            let Some(bytes) = self.layout.syntax_range(comment) else {
                continue;
            };
            let written = &text[bytes.clone()];
            let start = bytes.start + written.len() - written.trim_start().len();
            let end = bytes.start + written.trim_end().len();
            match self.before(&(start..start)) {
                Some(gap) => {
                    if self.style.settings.spacing.trailing_comment == TrailingComment::OneSpace {
                        self.gap(gap, Spacing::Spaced, Some(start..start + 2));
                    }
                }
                None => {
                    let dashes = &text[start..end];
                    if let Some(width) = banner.filter(|_| {
                        dashes.bytes().all(|byte| byte == b'-')
                            && Self::BANNER_DASHES.contains(&dashes.len())
                    }) {
                        self.edits
                            .push(FormatEdit::new(start..end, "-".repeat(width)));
                    }
                }
            }
        }
    }

    /// Rule 3: just inside one bracket pair.
    fn inside(&mut self, bracket: &Bracket) {
        let Some(close) = &bracket.close else {
            return;
        };
        let interior = bracket.open.end..close.start;
        if self.layout.horizontal_gap(interior.clone()).is_some() {
            self.edits.push(FormatEdit::new(interior, ""));
            return;
        }
        if let Keepable::Fixed(spacing) =
            self.style.settings.spacing.brackets_inside.of(bracket.kind)
        {
            for gap in [self.after(&bracket.open), self.before(close)]
                .into_iter()
                .flatten()
            {
                self.gap(gap, spacing, None);
            }
        }
    }

    /// Rules 4 and 5 for the commas of `values`, written with `spacing`;
    /// `empty` tells an empty slot.
    fn commas<T: Spanned>(
        &mut self,
        values: &Punctuated<T>,
        spacing: Spacing,
        empty: impl Fn(&T) -> bool,
    ) {
        let mut previous = values.first();
        for (comma, next) in values.pairs() {
            let Some(comma) = self.layout.syntax_range(comma) else {
                previous = next;
                continue;
            };
            if let Some(gap) = self.before(&comma) {
                self.edits.push(FormatEdit::new(gap, ""));
            }
            if let Some(gap) = self.after(&comma) {
                let text = self.layout.text();
                let closes = matches!(text.as_bytes()[gap.end], b')' | b']' | b'}' | b';')
                    || text[gap.end..].starts_with("|>");
                if closes || empty(previous) || empty(next) {
                    self.edits.push(FormatEdit::new(gap, ""));
                } else {
                    let token =
                        gap.end..gap.end + text[gap.end..].chars().next().map_or(0, char::len_utf8);
                    self.gap(gap, spacing, Some(token));
                }
            }
            previous = next;
        }
    }

    /// Rules 4 and 6 for one `;`.
    fn semicolon(&mut self, semicolon: &impl Spanned) {
        let Some(semicolon) = self.layout.syntax_range(semicolon) else {
            return;
        };
        if let Some(gap) = self.before(&semicolon) {
            self.edits.push(FormatEdit::new(gap, ""));
        }
        if let Some(gap) = self.after(&semicolon) {
            let text = self.layout.text();
            let spacing = if matches!(text.as_bytes()[gap.end], b')' | b']' | b'}') {
                Spacing::Tight
            } else {
                self.style.settings.spacing.semicolon
            };
            self.gap(gap, spacing, None);
        }
    }

    /// Rules 7 and 8: the gap between `left` and the bracket `right` starts
    /// with, as `setting` says.
    fn before_bracket(&mut self, left: &impl Spanned, right: &Expr, setting: Keepable<Spacing>) {
        let Keepable::Fixed(spacing) = setting else {
            return;
        };
        if right.delimiter().is_none() {
            return;
        }
        if let (Some(left), Some(right)) = (
            self.layout.syntax_range(left),
            self.layout.syntax_range(right),
        ) {
            if left.end <= right.start {
                self.gap(left.end..right.start, spacing, None);
            }
        }
    }

    /// Rule 7 for a keyword and the expression after it.
    fn keyword(&mut self, keyword: &impl Spanned, expression: &Expr) {
        self.before_bracket(
            keyword,
            expression,
            self.style.settings.spacing.keyword_paren,
        );
    }

    /// Rule 9 for a binary operator: both gaps, tight only when both are on
    /// the operator's line so that it never runs into a line break.
    fn binary(&mut self, operator: &impl Spanned) {
        let Some(bytes) = self.layout.syntax_range(operator) else {
            return;
        };
        let spelling = &self.layout.text()[bytes.clone()];
        let Some(spacing) = self.style.binary_spacing(spelling, self.index.is_inside()) else {
            return;
        };
        let (before, after) = (self.before(&bytes), self.after(&bytes));
        if spacing == Spacing::Tight && (before.is_none() || after.is_none()) {
            return;
        }
        if let Some(gap) = before {
            self.gap(gap, spacing, Some(bytes.clone()));
        }
        if let Some(gap) = after {
            self.gap(gap, spacing, None);
        }
    }

    /// Rule 10 for a prefix (`operand_after`) or postfix operator.
    fn unary(&mut self, operator: &impl Spanned, operand_after: bool) {
        let Some(bytes) = self.layout.syntax_range(operator) else {
            return;
        };
        let spacing = self
            .style
            .settings
            .spacing
            .operators
            .unary(&self.layout.text()[bytes.clone()]);
        let gap = if operand_after {
            self.after(&bytes)
        } else {
            self.before(&bytes)
        };
        if let Some(gap) = gap {
            self.gap(gap, spacing, None);
        }
    }
}

impl<'ast> Visit<'ast> for HorizontalSpacing<'_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        let bracket = self.index.enter(expression);
        visit::visit_expr(self, expression);
        self.index.leave(bracket);
    }

    fn visit_expr_binary(&mut self, node: &'ast ExprBinary) {
        if matches!(node.op, BinaryOperator::Space(_)) {
            if matches!(node.right.as_ref(), Expr::Paren(_) | Expr::Sequence(_)) {
                self.before_bracket(
                    node.left.as_ref(),
                    &node.right,
                    self.style.settings.spacing.call_paren,
                );
            }
        } else {
            self.binary(&node.op);
        }
        self.index.note(node);
        visit::visit_expr_binary(self, node);
    }

    fn visit_expr_field(&mut self, node: &'ast ExprField) {
        self.binary(&node.op);
        visit::visit_expr_field(self, node);
    }

    fn visit_expr_assign(&mut self, node: &'ast ExprAssign) {
        self.binary(&node.op);
        if let AssignmentTarget::Method { method, signature } = node.parts().target {
            self.before_bracket(method, signature, self.style.settings.spacing.call_paren);
        }
        node.visit_installing(self);
    }

    fn visit_expr_option(&mut self, node: &'ast ExprOption) {
        self.binary(&node.fat_arrow_token);
        visit::visit_expr_option(self, node);
    }

    fn visit_expr_function(&mut self, node: &'ast ExprFunction) {
        self.binary(&node.arrow_token);
        visit::visit_expr_function(self, node);
    }

    fn visit_expr_prefix(&mut self, node: &'ast ExprPrefix) {
        self.unary(&node.op, true);
        visit::visit_expr_prefix(self, node);
    }

    fn visit_expr_postfix(&mut self, node: &'ast ExprPostfix) {
        self.unary(&node.op, false);
        visit::visit_expr_postfix(self, node);
    }

    fn visit_elements_pack(&mut self, node: &'ast ElementsPack) {
        self.commas(&node.elems, self.style.comma, |element| {
            matches!(element, Element::Empty(_))
        });
        visit::visit_elements_pack(self, node);
    }

    fn visit_params_pack(&mut self, node: &'ast ParamsPack) {
        self.commas(&node.params, self.style.lambda_param_comma, |_| false);
        visit::visit_params_pack(self, node);
    }

    fn visit_stmt(&mut self, node: &'ast Stmt) {
        self.semicolon(&node.semi_token);
        visit::visit_stmt(self, node);
    }

    fn visit_cell(&mut self, node: &'ast m2_syn::nodes::Cell) {
        if let Some(semicolon) = &node.semi_token {
            self.semicolon(semicolon);
        }
        visit::visit_cell(self, node);
    }

    fn visit_expr_if(&mut self, node: &'ast ExprIf) {
        self.keyword(&node.if_token, &node.cond);
        visit::visit_expr_if(self, node);
    }

    fn visit_expr_while(&mut self, node: &'ast ExprWhile) {
        self.keyword(&node.while_token, &node.cond);
        visit::visit_expr_while(self, node);
    }

    fn visit_expr_return(&mut self, node: &'ast ExprReturn) {
        if let Some(value) = &node.expr {
            self.keyword(&node.return_token, value);
        }
        visit::visit_expr_return(self, node);
    }

    fn visit_then_clause(&mut self, node: &'ast ThenClause) {
        self.keyword(&node.then_token, &node.expr);
        visit::visit_then_clause(self, node);
    }

    fn visit_else_clause(&mut self, node: &'ast ElseClause) {
        self.keyword(&node.else_token, &node.expr);
        visit::visit_else_clause(self, node);
    }

    fn visit_do_clause(&mut self, node: &'ast DoClause) {
        self.keyword(&node.do_token, &node.expr);
        visit::visit_do_clause(self, node);
    }

    fn visit_list_clause(&mut self, node: &'ast ListClause) {
        self.keyword(&node.list_token, &node.expr);
        visit::visit_list_clause(self, node);
    }
}
