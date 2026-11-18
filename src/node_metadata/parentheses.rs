//! Parentheses the syntax does not need: `x = (a*b) + c` parses as `x = a*b + c`.
//!
//! Only pairs whose place makes them plainly redundant are considered (see
//! [`Groupings`]), and each is then confirmed: its cell, parsed again without
//! it, must have the typed tree the original parse has with that
//! parenthesized expression replaced by its content, spans aside. Comments
//! are trivia beside the tree, so their layout may change; a removal pulling
//! code into a comment changes the tree. Cells are the smallest units parsed
//! on their own, so the check re-parses only the cell holding the pair, and a
//! removal that would split or merge cells fails it. So `f(x)` becomes `f x`
//! and `sin(x)^2`, which squares inside the call, `sin x^2`.
//!
//! Pairs are removed one at a time, each checked against the parse the
//! previous removals left, so the removals of a cell are redundant together
//! and not merely each on its own.

use std::collections::HashSet;
use std::ops::Range;
use std::ptr;
use std::sync::OnceLock;

use m2_syn::nodes::{
    BinaryOperator, Cell, Expr, ExprBinary, ExprField, ExprFunction, ExprParen, ExprPostfix,
    ExprPrefix,
};
use m2_syn::visit::{self, Visit};
use m2_syn::{DelimiterValue, PrettyDetail, PrettyEdge, PrettyNode, PrettyNodeRole, PrettyTree};
use m2_syn::{Span, Spanned};

use super::{binary_strength, binding_precedence, ParsedCell, SyntaxSnapshot};

/// Whether redundant parentheses around a call's argument go too: `f(x)` as
/// `f x`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum CallParentheses {
    /// `f(x)` becomes `f x`.
    Remove,
    /// `f(x)` stays.
    Keep,
}

/// A cell's text without its redundant parentheses, worked out once per
/// [`CallParentheses`] policy and parse.
#[derive(Debug, Default)]
pub struct UngroupedText {
    /// The text with call arguments' parentheses removed too.
    removing_calls: OnceLock<Option<String>>,
    /// The text keeping them.
    keeping_calls: OnceLock<Option<String>>,
}

impl UngroupedText {
    /// The text of `cell` under `calls`, `None` when nothing is redundant.
    pub fn of(&self, cell: &ParsedCell, calls: CallParentheses) -> Option<&str> {
        match calls {
            CallParentheses::Remove => &self.removing_calls,
            CallParentheses::Keep => &self.keeping_calls,
        }
        .get_or_init(|| Grouping::strip_redundant(cell, calls))
        .as_deref()
    }
}

/// A parenthesized expression of a cell, in the cell's bytes: in
/// `x = (a + b) * c` the parentheses are `4..5` and `10..11`, around the
/// content `5..10`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grouping {
    /// The opening parenthesis.
    opening: Range<usize>,
    /// The parenthesized expression.
    content: Range<usize>,
    /// The closing parenthesis.
    closing: Range<usize>,
    /// Whether the pair opens a call's argument that brackets nothing else,
    /// so the application needs its space: `f(x)` becomes `f x` and
    /// `(g h)(x)` `(g h) x`, while `f((a + b))` stays a call, `f(a + b)`.
    spaced_argument: bool,
}

impl Grouping {
    /// The groupings of `cell`, parsed from `text`, under `calls`, outer
    /// before inner.
    fn of(cell: &Cell, text: &str, calls: CallParentheses) -> Vec<Self> {
        let mut groupings = Groupings {
            text,
            calls,
            found: Vec::new(),
            bodies: HashSet::new(),
            arguments: HashSet::new(),
            operands: HashSet::new(),
            tighter: HashSet::new(),
        };
        groupings.visit_cell(cell);
        groupings.found
    }

    /// The parse of `cell` without this grouping, when it is redundant there,
    /// in the shortest spelling that parses the same. Each side is glued to
    /// its neighbour unless that breaks the parse, as `if(x)then` would; then
    /// a space separates them there: `sin(x)^2` becomes `sin x^2`.
    fn removed_from(&self, cell: &ParsedCell) -> Option<ParsedCell> {
        let original = cell.syntax()?.pretty_tree();
        let text = cell.source();
        let [opening, closing] = self.removed_bytes(text);
        let spacings = |range: &Range<usize>, applied: bool| match Self::joins_visible(text, range)
        {
            false => vec![""],
            true if applied => vec![" "],
            true => vec!["", " "],
        };
        let closing_spacings = spacings(&closing, false);
        let mut spellings = spacings(&opening, self.spaced_argument)
            .into_iter()
            .flat_map(|before| closing_spacings.iter().map(move |after| [before, *after]))
            .collect::<Vec<_>>();
        spellings.sort_by_key(|spacing| spacing.concat().len());
        spellings
            .into_iter()
            .map(|[before, after]| {
                [
                    &text[..opening.start],
                    before,
                    &text[opening.end..closing.start],
                    after,
                    &text[closing.end..],
                ]
                .concat()
            })
            .map(|stripped| SyntaxSnapshot::parse(&stripped, None))
            .find_map(|stripped| match stripped.cells() {
                [parsed] if self.is_redundant(&original, parsed) => Some(parsed.clone()),
                _ => None,
            })
    }

    /// The bytes each parenthesis removes: itself and the whitespace between
    /// it and the content, line breaks included, unless a comment lies there.
    fn removed_bytes(&self, text: &str) -> [Range<usize>; 2] {
        let blank = |gap: Range<usize>| text[gap].trim().is_empty();
        let opening = self.opening.start..if blank(self.opening.end..self.content.start) {
            self.content.start
        } else {
            self.opening.end
        };
        let closing = if blank(self.content.end..self.closing.start) {
            self.content.end
        } else {
            self.closing.start
        }..self.closing.end;
        [opening, closing]
    }

    /// Whether removing `range` makes two visible characters touch.
    fn joins_visible(text: &str, range: &Range<usize>) -> bool {
        let visible = |ch: Option<char>| ch.is_some_and(|ch| !ch.is_whitespace());
        visible(text[..range.start].chars().next_back())
            && visible(text[range.end..].chars().next())
    }

    /// Whether `stripped`, the parse of `cell` without this grouping, is the
    /// parse `original` describes with the grouping seen through.
    fn is_redundant(&self, original: &PrettyNode, stripped: &ParsedCell) -> bool {
        stripped.parse_error().is_none()
            && stripped.lexer_diagnostics().is_empty()
            && stripped
                .syntax()
                .is_some_and(|syntax| self.same_shape(original, &syntax.pretty_tree()))
    }

    /// Whether the trees agree in everything but spans, once this grouping in
    /// `original` is replaced by its content.
    fn same_shape(&self, original: &PrettyNode, stripped: &PrettyNode) -> bool {
        let original = self.ungrouped(original);
        original.name() == stripped.name()
            && original.role() == stripped.role()
            && original.detail().map(PrettyDetail::text)
                == stripped.detail().map(PrettyDetail::text)
            && original.children().len() == stripped.children().len()
            && original
                .children()
                .iter()
                .zip(stripped.children())
                .all(|(original, stripped)| {
                    original.label() == stripped.label()
                        && self.same_shape(original.node(), stripped.node())
                })
    }

    /// The content of this grouping when `node` is its expression, otherwise
    /// `node`. The expression is a category node over the parentheses' own
    /// syntax node, whose one child besides the two delimiter boundaries is
    /// the content's expression.
    fn ungrouped<'tree>(&self, node: &'tree PrettyNode) -> &'tree PrettyNode {
        let [only] = node.children() else {
            return node;
        };
        let parenthesized = only.node();
        let spans_parentheses = parenthesized.span().and_then(Span::range)
            == Some(self.opening.start..self.closing.end);
        if node.role() != PrettyNodeRole::Category
            || parenthesized.role() != PrettyNodeRole::Syntax
            || !spans_parentheses
        {
            return node;
        }
        parenthesized
            .children()
            .iter()
            .map(PrettyEdge::node)
            .find(|child| child.role() != PrettyNodeRole::Delimiter)
            .expect("a grouping holds its content")
    }
}

/// Collects a cell's groupings in pre-order: the pairs whose place makes them
/// plainly redundant, for the reparse to confirm. A pair qualifies when its
/// content is an atom, `(x)`, `({1, 2})` or `((…))`; when it is no operator's
/// operand, so keywords, commas, brackets or an assignment delimit it, as in
/// `if (c) then`, `{(a + b)}` or `x = (a + b)`; or when its content's operator
/// binds tighter than the one it is an operand of, as `(a*b) + c`, or as
/// tightly on the side it groups to, as `(a + b) + c`.
///
/// Parentheses holding statements are a block rather than a grouping, and
/// parentheses spanning lines stay, since they free their content from the
/// newline rule. So does a function's body, `x -> (…)`, which can then span lines and
/// gain statements; and so does a call's argument, `f(x)`, or the pair
/// opening it, `sin(x)^2`, when the policy keeps call parentheses.
struct Groupings<'source> {
    /// The cell's text.
    text: &'source str,
    /// Whether call arguments are groupings.
    calls: CallParentheses,
    /// Groupings found so far.
    found: Vec<Grouping>,
    /// Function bodies.
    bodies: HashSet<*const Expr>,
    /// The expressions call arguments start with.
    arguments: HashSet<*const Expr>,
    /// Operands of operators.
    operands: HashSet<*const Expr>,
    /// Operands whose content binds tighter than their operator.
    tighter: HashSet<*const Expr>,
}

impl Groupings<'_> {
    /// The expression `operand`'s text starts with: `(x)` in `(x)^2`. A call
    /// `sin(x)^2` applies `sin` to `(x)^2`, so its parentheses read as the
    /// call's without being its whole argument.
    fn opening(operand: &Expr) -> &Expr {
        match operand {
            Expr::Binary(binary) => Self::opening(&binary.left),
            Expr::Postfix(postfix) => Self::opening(&postfix.expr),
            Expr::Field(field) => Self::opening(&field.base),
            _ => operand,
        }
    }

    /// Whether parentheses around `content` are redundant wherever it stands.
    fn is_atom(content: &Expr) -> bool {
        matches!(content, Expr::Symbol(_) | Expr::Literal(_)) || Self::is_bracketed(content)
    }

    /// Whether `content` is written in brackets of its own.
    fn is_bracketed(content: &Expr) -> bool {
        matches!(
            content,
            Expr::List(_) | Expr::Array(_) | Expr::AngleBar(_) | Expr::Sequence(_) | Expr::Paren(_)
        )
    }

    /// Records the operands of an operator.
    fn operands<'ast>(&mut self, operands: impl IntoIterator<Item = &'ast Expr>) {
        self.operands
            .extend(operands.into_iter().map(ptr::from_ref));
    }

    /// Whether `grouping`, the cell's `parentheses` around `content`,
    /// qualifies.
    fn qualifies(&self, grouping: &Grouping, parentheses: &Expr, content: &Expr) -> bool {
        let at = ptr::from_ref(parentheses);
        let kept_call = self.calls == CallParentheses::Keep && self.arguments.contains(&at);
        !self.text[grouping.opening.start..grouping.closing.end].contains('\n')
            && !kept_call
            && !self.bodies.contains(&at)
            && (Self::is_atom(content)
                || !self.operands.contains(&at)
                || self.tighter.contains(&at))
    }
}

impl<'ast> Visit<'ast> for Groupings<'_> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        if let Expr::Paren(holder) = expression {
            let delimiter = holder.delimiter.delimiter();
            let grouping = holder
                .expr
                .as_deref()
                .filter(|_| holder.stmts.is_empty())
                .and_then(|content| {
                    let grouping = Grouping {
                        opening: delimiter.opening_span().range()?,
                        content: content.try_span()?.range()?,
                        closing: delimiter.closing_span()?.range()?,
                        spaced_argument: self.arguments.contains(&ptr::from_ref(expression))
                            && !Self::is_bracketed(content),
                    };
                    self.qualifies(&grouping, expression, content)
                        .then_some(grouping)
                });
            self.found.extend(grouping);
        }
        visit::visit_expr(self, expression);
    }

    fn visit_expr_binary(&mut self, node: &'ast ExprBinary) {
        if matches!(node.op, BinaryOperator::Space(_)) {
            self.arguments
                .insert(ptr::from_ref(Self::opening(&node.right)));
        }
        let outer = node.op.spelling();
        // `(x op y) outer z` regroups unless `outer` stops `op`'s right
        // operand; `x outer (y op z)` unless `op` continues `outer`'s.
        let grouped = |operand: &Expr, binds: &dyn Fn(&str) -> Option<bool>| {
            matches!(operand, Expr::Paren(ExprParen { expr: Some(content), .. })
                if matches!(content.as_ref(), Expr::Binary(inner)
                    if binds(inner.op.spelling()) == Some(true)))
        };
        let left = grouped(&node.left, &|inner| {
            Some(binary_strength(inner)? >= binding_precedence(outer)?)
        });
        let right = grouped(&node.right, &|inner| {
            Some(binding_precedence(inner)? > binary_strength(outer)?)
        });
        for (operand, tighter) in [(node.left.as_ref(), left), (node.right.as_ref(), right)] {
            if tighter {
                self.tighter.insert(ptr::from_ref(operand));
            }
        }
        self.operands([node.left.as_ref(), node.right.as_ref()]);
        visit::visit_expr_binary(self, node);
    }

    fn visit_expr_prefix(&mut self, node: &'ast ExprPrefix) {
        self.operands([node.expr.as_ref()]);
        visit::visit_expr_prefix(self, node);
    }

    fn visit_expr_postfix(&mut self, node: &'ast ExprPostfix) {
        self.operands([node.expr.as_ref()]);
        visit::visit_expr_postfix(self, node);
    }

    fn visit_expr_field(&mut self, node: &'ast ExprField) {
        self.operands([node.base.as_ref()]);
        visit::visit_expr_field(self, node);
    }

    fn visit_expr_function(&mut self, node: &'ast ExprFunction) {
        self.bodies.insert(ptr::from_ref(node.body.as_ref()));
        visit::visit_expr_function(self, node);
    }
}

impl Grouping {
    /// The text of `cell` without its redundant groupings under `calls`,
    /// `None` when it has none or does not parse. [`UngroupedText`] keeps the
    /// result.
    fn strip_redundant(cell: &ParsedCell, calls: CallParentheses) -> Option<String> {
        if !cell.lexer_diagnostics().is_empty() {
            return None;
        }
        let mut current = cell.clone();
        let mut kept = 0;
        // Groupings come outer first, and removing one keeps the order of the
        // rest, so the first `kept` are always the ones found necessary.
        while let Some(grouping) = Grouping::of(current.syntax()?, current.source(), calls)
            .into_iter()
            .nth(kept)
        {
            match grouping.removed_from(&current) {
                Some(stripped) => current = stripped,
                None => kept += 1,
            }
        }
        (current.source() != cell.source()).then(|| current.source().to_string())
    }
}

impl SyntaxSnapshot {
    /// Each cell with redundant parentheses under `calls`, as its document
    /// bytes and its text without them.
    pub fn redundant_parentheses(&self, calls: CallParentheses) -> Vec<(Range<usize>, String)> {
        self.cells()
            .iter()
            .filter_map(|cell| {
                Some((
                    cell.document_range(),
                    cell.without_redundant_parentheses(calls)?.to_string(),
                ))
            })
            .collect()
    }

    /// The document without its redundant parentheses under `calls`, sharing
    /// the cells that have none with this revision.
    pub fn without_redundant_parentheses(&self, calls: CallParentheses) -> Self {
        let mut text = self.source().to_string();
        for (range, replacement) in self.redundant_parentheses(calls).into_iter().rev() {
            text.replace_range(range, &replacement);
        }
        Self::parse(&text, Some(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `text` without its redundant parentheses under `calls`.
    fn ungrouped(text: &str, calls: CallParentheses) -> String {
        SyntaxSnapshot::parse(text, None)
            .without_redundant_parentheses(calls)
            .source()
            .to_string()
    }

    /// Call arguments lose their parentheses where the call parses the same
    /// without them, unless the policy keeps them.
    #[test]
    fn call_parentheses_go_unless_kept() {
        for (text, removed, kept) in [
            ("f(x)\n", "f x\n", "f(x)\n"),
            ("f (x)\n", "f x\n", "f (x)\n"),
            ("g(ZZ) := x -> x\n", "g ZZ := x -> x\n", "g(ZZ) := x -> x\n"),
            ("y = f(-1)\n", "y = f(-1)\n", "y = f(-1)\n"),
            ("y = sin(x)^2\n", "y = sin x^2\n", "y = sin(x)^2\n"),
            ("y = f(x)(z)\n", "y = f x z\n", "y = f(x)(z)\n"),
            ("y = f((a + b))\n", "y = f(a + b)\n", "y = f(a + b)\n"),
            ("y = (g h)(x)\n", "y = (g h) x\n", "y = (g h)(x)\n"),
            ("y = f(x^2)\n", "y = f x^2\n", "y = f(x^2)\n"),
            ("y = f(L_0)\n", "y = f L_0\n", "y = f(L_0)\n"),
            ("y = f(g x)\n", "y = f g x\n", "y = f(g x)\n"),
            ("y = f({1, 2})\n", "y = f{1, 2}\n", "y = f({1, 2})\n"),
            ("y = f(#L)\n", "y = f(#L)\n", "y = f(#L)\n"),
            // Adjacency to `(` binds tighter than prefix `#` and `*`, adjacency
            // to `[` looser: without its last pair, the leading prefix would
            // stop before `[]` instead of taking the whole application.
            ("y = *g([])\n", "y = *g([])\n", "y = *g([])\n"),
            (
                "y = #[]##()([])\n",
                "y = #[]##()([])\n",
                "y = #[]##()([])\n",
            ),
            ("y = f(a + b)\n", "y = f(a + b)\n", "y = f(a + b)\n"),
            ("y = f(x)!\n", "y = f x!\n", "y = f(x)!\n"),
            ("y = f(L)#0\n", "y = f L#0\n", "y = f(L)#0\n"),
        ] {
            assert_eq!(
                ungrouped(text, CallParentheses::Remove),
                removed,
                "{text:?}"
            );
            assert_eq!(ungrouped(text, CallParentheses::Keep), kept, "{text:?}");
        }
    }

    /// Parentheses go exactly where the cell parses the same without them;
    /// function bodies and blocks are not groupings at all.
    #[test]
    fn parentheses_go_where_the_cell_parses_the_same_without_them() {
        let ungrouped = |text| ungrouped(text, CallParentheses::Keep);
        for (text, expected) in [
            ("x = (a*b) + c\n", "x = a*b + c\n"),
            ("x = (a + b) * c\n", "x = (a + b) * c\n"),
            ("y = ((x))\n", "y = x\n"),
            ("if (x) then y\n", "if x then y\n"),
            ("if(x)then y\n", "if x then y\n"),
            ("z = ( a + b )\n", "z = a + b\n"),
            ("(f) x\n", "f x\n"),
            ("g = x -> (x + 1)\n", "g = x -> (x + 1)\n"),
            ("h = apply(L, (x -> x))\n", "h = apply(L, x -> x)\n"),
            ("g = (x) -> x\n", "g = (x) -> x\n"),
            ("h = apply(L, (x) -> x)\n", "h = apply(L, (x) -> x)\n"),
            ("if(x)== y then z\n", "if x== y then z\n"),
            ("x = (a; b)\n", "x = (a; b)\n"),
            ("x = 1 -(-2)\n", "x = 1 -(-2)\n"),
            ("g = L / (x -> x + 1)\n", "g = L / (x -> x + 1)\n"),
            ("a = (b = c)\n", "a = b = c\n"),
            ("v = x#(i)\n", "v = x#i\n"),
            ("w = (a + b) + (c + d)\n", "w = a + b + (c + d)\n"),
            ("x = (\n    a +\n    b\n)\n", "x = (\n    a +\n    b\n)\n"),
            ("x = (\n    a + b\n)\n", "x = (\n    a + b\n)\n"),
            ("x = (a\n    + b)\n", "x = (a\n    + b)\n"),
            ("y = {(a\n    + b)}\n", "y = {(a\n    + b)}\n"),
            ("f(x, (a + b))\n", "f(x, a + b)\n"),
            (
                "g = () -> (\n    z = (a\n        + b);\n    z\n)\n",
                "g = () -> (\n    z = (a\n        + b);\n    z\n)\n",
            ),
            ("-- (c)\nx = (1) -- (2)\n", "-- (c)\nx = 1 -- (2)\n"),
            ("x = (-- c\n    1)\n", "x = (-- c\n    1)\n"),
        ] {
            assert_eq!(ungrouped(text), expected, "{text:?}");
        }
    }
}
