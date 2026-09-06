//! Typed access to Tree-sitter nodes and traversals.

use std::{collections::HashSet, iter};

use m2_syn::nodes::{
    AngleBarList, Array, Cell, ElseClause, Empty, Expr, ExprCollection as Collection,
    ExprFor as ForLoop, ExprIf as IfStatement, ExprLambda as LambdaExpression,
    ExprNew as NewStatement, ExprOption as OptionExpression, ExprPack,
    ExprQuote as QuoteExpression, ExprTry as TryStatement, ExprWhile as WhileLoop, FloatLiteral,
    IntegerLiteral, List, LoopBody, RawStringLiteral, Sequence, SourceFile, Statement,
    StringLiteral, Symbol, ThenClause,
};
use m2_syn::visit::{self, Visit};
use m2_syn::{Spanned, Token};
use tree_sitter::{Node, Point};

/// Snapshot-local identity of one Tree-sitter syntax node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SyntaxNodeId(usize);

#[derive(Debug, Clone, Copy)]
pub struct M2Node<'tree> {
    node: Node<'tree>,
    source: &'tree str,
}

pub trait M2SyntaxKind {
    fn matches(node: M2Node<'_>) -> bool;
}

/// Every kind below names a *named* grammar node. Two of them — `symbol` and
/// `list` — are also the spelling of an anonymous keyword token (`symbol x`,
/// `for i to n list …`), and tree-sitter reports an anonymous token's kind as
/// its literal text. Matching on the kind string alone would therefore read
/// those keywords as an identifier or a list expression, so namedness is part
/// of the match rather than something each caller has to remember.
macro_rules! syntax_kinds {
    ($($syntax:ty => $kind:literal),* $(,)?) => {
        $(
            impl M2SyntaxKind for $syntax {
                fn matches(node: M2Node<'_>) -> bool {
                    node.node.is_named() && node.raw_kind() == $kind
                }
            }
        )*
    };
}

syntax_kinds!(
    AngleBarList => "angle_bar_list",
    Array => "array",
    ElseClause => "else_clause",
    ForLoop => "for_loop",
    IfStatement => "if_statement",
    LambdaExpression => "lambda_expression",
    List => "list",
    LoopBody => "loop_body",
    NewStatement => "new_statement",
    OptionExpression => "option",
    QuoteExpression => "quote_expression",
    RawStringLiteral => "raw_string_literal",
    Sequence => "sequence",
    SourceFile => "source_file",
    StringLiteral => "string_literal",
    Symbol => "symbol",
    ThenClause => "then_clause",
    TryStatement => "try_statement",
    WhileLoop => "while_loop",
);

impl M2SyntaxKind for FloatLiteral {
    fn matches(node: M2Node<'_>) -> bool {
        node.raw_kind() == "float_literal"
            || (node.raw_kind() == "integer_literal" && node.text().contains('p'))
    }
}

impl M2SyntaxKind for IntegerLiteral {
    fn matches(node: M2Node<'_>) -> bool {
        node.raw_kind() == "integer_literal" && !node.text().contains('p')
    }
}

macro_rules! token_kinds {
    ($([$($token:tt)*]),* $(,)?) => {
        $(
            impl M2SyntaxKind for Token![$($token)*] {
                fn matches(node: M2Node<'_>) -> bool {
                    !node.node.is_named()
                        && node.text() == <Self as m2_syn::Token>::SPELLING
                }
            }
        )*
    };
}

token_kinds!(
    [,], [:=], [;], [TEST], [break], [breakpoint], [catch], [continue], [do], [elapsedTime],
    [elapsedTiming], [else], [finish], [for], [from], [global], [if], [in], [list], [local], [new],
    [of], [profile], [return], [shield], [step], [symbol], [then], [threadLocal], [threadVariable],
    [throw], [time], [timing], [to], [try], [when], [while],
);

/// Declares the token groups the rest of the crate asks about. A `Token!` type
/// matches anonymous leaves only, so membership is the whole question and no
/// group restates the namedness test.
macro_rules! token_groups {
    ($(
        $(#[$group_doc:meta])*
        $group:ident => [[$($first:tt)*] $(, [$($rest:tt)*])* $(,)?]
    ),* $(,)?) => {
        impl M2Node<'_> {
            $(
                $(#[$group_doc])*
                pub fn $group(&self) -> bool {
                    self.is::<Token![$($first)*]>()
                        $(|| self.is::<Token![$($rest)*]>())*
                }
            )*
        }
    };
}

token_groups!(
    is_comma => [[,]],

    is_semicolon => [[;]],

    /// An anonymous keyword token (`if`, `then`, `for`, `return`, `time`, ...) —
    /// the bare keyword leaves, not the named clause/statement nodes that contain
    /// them. Used for keyword highlighting.
    is_keyword_token => [
        [if], [then], [else], [from], [to], [when], [do], [in], [of], [list], [for], [while],
        [break], [continue], [return], [try], [catch], [throw], [time], [timing], [elapsedTime],
        [elapsedTiming], [profile], [shield], [TEST], [breakpoint], [finish], [new], [step],
    ],

    /// An anonymous binding-modifier keyword token (`global`, `local`, `symbol`,
    /// `threadVariable`, `threadLocal`).
    is_modifier_token => [[global], [local], [symbol], [threadVariable], [threadLocal]],

    /// The `then`/`else` keyword tokens, which open the clauses an `if` indents.
    is_then_or_else_keyword => [[then], [else]],
);

pub fn visit_source_nodes<'tree>(
    root: M2Node<'tree>,
    syntax: Option<&SourceFile>,
    mut visit: impl FnMut(M2Node<'tree>),
) {
    if let Some(syntax) = syntax {
        SyntaxNodeWalker::new(root, visit).visit_source_file(syntax);
    } else {
        root.descendants().for_each(&mut visit);
    }
}

pub fn visit_expression_nodes<'tree>(
    root: M2Node<'tree>,
    syntax: Option<&Expr>,
    mut visit: impl FnMut(M2Node<'tree>),
) {
    if let Some(syntax) = syntax {
        SyntaxNodeWalker::new(root, visit).visit_expr(syntax);
    } else {
        root.descendants().for_each(&mut visit);
    }
}

struct SyntaxNodeWalker<'tree, F> {
    root: M2Node<'tree>,
    visit: F,
    seen: HashSet<SyntaxNodeId>,
}

impl<'tree, F> SyntaxNodeWalker<'tree, F>
where
    F: FnMut(M2Node<'tree>),
{
    fn new(root: M2Node<'tree>, visit: F) -> Self {
        Self {
            root,
            visit,
            seen: HashSet::new(),
        }
    }

    fn record<Syntax: Spanned + ?Sized>(&mut self, syntax: &Syntax) {
        if let Some(node) = self.root.descendant_for_syntax(syntax) {
            if self.seen.insert(node.id()) {
                (self.visit)(node);
            }
        }
    }
}

impl<'ast, 'tree, F> Visit<'ast> for SyntaxNodeWalker<'tree, F>
where
    F: FnMut(M2Node<'tree>),
{
    fn visit_expr(&mut self, node: &'ast Expr) {
        self.record(node);
        visit::visit_expr(self, node);
    }

    fn visit_expr_collection(&mut self, node: &'ast Collection) {
        self.record(node);
        visit::visit_expr_collection(self, node);
    }

    fn visit_symbol(&mut self, node: &'ast Symbol) {
        self.record(node);
        visit::visit_symbol(self, node);
    }

    fn visit_expr_pack(&mut self, node: &'ast ExprPack) {
        self.record(node);
        visit::visit_expr_pack(self, node);
    }

    fn visit_cell(&mut self, node: &'ast Cell) {
        self.record(node);
        visit::visit_cell(self, node);
    }

    fn visit_statement(&mut self, node: &'ast Statement) {
        self.record(node);
        visit::visit_statement(self, node);
    }

    fn visit_empty(&mut self, node: &'ast Empty) {
        self.record(node);
        visit::visit_empty(self, node);
    }
}

impl<'tree> M2Node<'tree> {
    pub(super) fn new(node: Node<'tree>, source: &'tree str) -> Self {
        Self { node, source }
    }

    pub fn text(&self) -> &'tree str {
        &self.source[self.node.start_byte()..self.node.end_byte()]
    }

    pub fn is<T: M2SyntaxKind>(&self) -> bool {
        T::matches(*self)
    }

    pub fn is_symbol_like(&self) -> bool {
        self.is::<Symbol>()
            || self.parent().is_some_and(|parent| {
                parent.is::<QuoteExpression>()
                    && parent
                        .child_by_field_name("token")
                        .is_some_and(|token| token.id() == self.id())
            })
    }

    pub fn is_literal(&self) -> bool {
        self.is::<IntegerLiteral>()
            || self.is::<FloatLiteral>()
            || self.is::<StringLiteral>()
            || self.is::<RawStringLiteral>()
    }

    pub fn is_string_literal(&self) -> bool {
        self.is::<StringLiteral>() || self.is::<RawStringLiteral>()
    }

    pub fn is_collection_expression(&self) -> bool {
        self.is::<Sequence>()
            || self.is::<List>()
            || self.is::<Array>()
            || self.is::<AngleBarList>()
    }

    pub fn is_delimited_expression(&self) -> bool {
        self.is_collection_expression() || self.is_holder()
    }

    pub fn is_parameter_container(&self) -> bool {
        self.is::<Sequence>() || self.is::<List>() || self.is_holder()
    }

    pub fn is_sequence(&self) -> bool {
        self.is::<Sequence>() || self.is_expr_pack()
    }

    pub fn is_expr_pack(&self) -> bool {
        self.raw_kind() == "naked_sequence"
    }

    pub fn is_nothing_value(&self) -> bool {
        self.is_muted_statement() || self.is_empty_component()
    }

    pub fn is_comment(&self) -> bool {
        matches!(self.raw_kind(), "line_comment" | "block_comment")
    }

    pub fn is_line_comment(&self) -> bool {
        self.raw_kind() == "line_comment"
    }

    pub fn is_block_comment(&self) -> bool {
        self.raw_kind() == "block_comment"
    }

    pub fn is_control_transfer(&self) -> bool {
        matches!(
            self.raw_kind(),
            "return_statement" | "break_statement" | "continue_statement"
        )
    }

    pub fn control_transfer_value(&self) -> Option<M2Node<'tree>> {
        self.named_children()
            .next()
            .or_else(|| {
                let parent = self.parent()?;
                let left = parent.child_by_field_name("left")?;
                (parent.is_space_application() && left.id() == self.id())
                    .then(|| parent.child_by_field_name("right"))
                    .flatten()
            })
            .or_else(|| self.commented_control_transfer_value())
    }

    fn commented_control_transfer_value(&self) -> Option<M2Node<'tree>> {
        let error = self.parent().filter(M2Node::is_error)?;
        if error.start_byte() != self.start_byte() || error.end_byte() != self.end_byte() {
            return None;
        }

        let comment = error.next_named_sibling()?;
        if !comment.is_line_comment() {
            return None;
        }

        let mut sibling = comment.next_named_sibling()?;
        while sibling.is_comment() {
            sibling = sibling.next_named_sibling()?;
        }
        let statement = sibling.is_muted_statement().then_some(sibling)?;
        let value = statement
            .named_children()
            .find(|child| !child.is_comment())?;
        (value.start_position().row > self.end_position().row).then_some(value)
    }

    pub fn is_recoverable_control_transfer_error(&self) -> bool {
        if !self.is_error() {
            return false;
        }
        let mut children = self.named_children();
        children.next().is_some_and(|transfer| {
            transfer.is_control_transfer()
                && children.next().is_none()
                && transfer.commented_control_transfer_value().is_some()
        })
    }

    pub fn is_keyword_statement(&self) -> bool {
        self.is::<IfStatement>()
            || self.is::<ForLoop>()
            || self.is::<WhileLoop>()
            || self.is::<NewStatement>()
            || self.is::<TryStatement>()
    }

    pub fn is_keyword_clause(&self) -> bool {
        self.is_iteration_range()
            || self.is::<LoopBody>()
            || self.is::<ThenClause>()
            || self.is::<ElseClause>()
            || self.is_except_clause()
    }

    pub fn is_holder(&self) -> bool {
        self.raw_kind() == "parenthesized_expression"
    }

    pub fn is_muted_statement(&self) -> bool {
        self.raw_kind() == "muted"
    }

    pub fn is_empty_component(&self) -> bool {
        self.raw_kind() == "empty_component"
    }

    pub fn is_source_cell(&self) -> bool {
        matches!(self.raw_kind(), "cell" | "muted")
    }

    pub fn is_iteration_range(&self) -> bool {
        self.raw_kind() == "iteration_range"
    }

    pub fn is_except_clause(&self) -> bool {
        self.raw_kind() == "except_clause"
    }

    pub fn is_debug_expr(&self) -> bool {
        self.raw_kind() == "debug_clause"
    }

    pub fn is_return_expr(&self) -> bool {
        self.raw_kind() == "return_statement"
    }

    pub fn is_break_expr(&self) -> bool {
        self.raw_kind() == "break_statement"
    }

    pub fn is_continue_expr(&self) -> bool {
        self.raw_kind() == "continue_statement"
    }

    pub fn is_prefix_expr(&self) -> bool {
        self.raw_kind() == "prefix_expression"
    }

    pub fn is_postfix_expr(&self) -> bool {
        self.raw_kind() == "postfix_expression"
    }

    pub fn is_binary_expr(&self) -> bool {
        self.raw_kind() == "binary_expression" && !self.is_adjacent_expr()
    }

    pub fn is_adjacent_expr(&self) -> bool {
        self.raw_kind() == "binary_expression"
            && self
                .child_by_field_name("operator")
                .is_some_and(|operator| operator.is_implicit_application())
    }

    pub fn closing_delimiter_width(&self) -> usize {
        if self.is::<AngleBarList>() {
            2
        } else {
            1
        }
    }

    fn raw_kind(&self) -> &'tree str {
        self.node.kind()
    }

    /// A human-facing label for the node's grammar kind, for diagnostic messages
    /// only (e.g. "Missing: )"). This is a display value, never branched on, so it
    /// stays correct across grammar renames.
    pub fn syntax_label(&self) -> &'tree str {
        self.raw_kind()
    }

    /// The implicit-application operator: the `SPACE` token tree-sitter inserts
    /// between a function and its juxtaposed argument (`sin x`, `f(x)`).
    pub fn is_implicit_application(&self) -> bool {
        self.raw_kind() == "SPACE"
    }

    pub fn has_binary_operator<T: m2_syn::Token>(&self) -> bool {
        self.binary_operator()
            .is_some_and(super::matches_token::<T>)
    }

    /// An opening collection delimiter: `(`, `{`, `[`, or `<|`.
    pub fn is_opening_delimiter(&self) -> bool {
        matches!(self.raw_kind(), "(" | "{" | "[" | "<|")
    }

    /// A closing collection delimiter: `)`, `}`, `]`, or `|>`.
    pub fn is_closing_delimiter(&self) -> bool {
        matches!(self.raw_kind(), ")" | "}" | "]" | "|>")
    }
}

impl<'tree> M2Node<'tree> {
    /// The operator token's source text of a binary expression (`:=`, `=>`, ...),
    /// or `None` if this is not a binary expression. Comparing this *parsed* text
    /// against operator spellings is reading code, not node-type names, so it is
    /// safe and rename-stable.
    pub fn binary_operator(&self) -> Option<&'tree str> {
        if self.is_adjacent_expr() {
            return Some("SPACE");
        }
        self.child_by_field_name("left")?;
        self.child_by_field_name("right")?;
        self.child_by_field_name("operator").map(|operator| {
            if operator.is_implicit_application() {
                "SPACE"
            } else {
                operator.text()
            }
        })
    }

    /// An assignment expression (`=`, `:=`, `<-`).
    pub fn is_assignment(&self) -> bool {
        self.binary_operator().is_some_and(|operator| {
            super::matches_token::<Token![=]>(operator)
                || super::matches_token::<Token![:=]>(operator)
                || super::matches_token::<Token![<-]>(operator)
        })
    }

    /// An option assignment (`key => value`).
    pub fn is_option_assignment(&self) -> bool {
        self.has_binary_operator::<Token![=>]>()
    }

    pub fn property_key(&self) -> Option<Self> {
        let right = self.child_by_field_name("right")?;
        let operator = self.binary_operator()?;
        (((super::matches_token::<Token![#]>(operator)
            || super::matches_token::<Token![#?]>(operator))
            && right.is_string_literal())
            || ((super::matches_token::<Token![.]>(operator)
                || super::matches_token::<Token![.?]>(operator))
                && right.is_symbol_like()))
        .then_some(right)
    }

    /// An implicit application `f x` / `f(x)`: a binary expression whose operator
    /// is the inserted `SPACE` token.
    pub fn is_space_application(&self) -> bool {
        self.is_adjacent_expr()
    }
}

impl<'tree> M2Node<'tree> {
    /// Whether this node is the `operator` field of its own parent.
    pub fn is_operator(&self) -> bool {
        self.parent()
            .and_then(|parent| parent.child_by_field_name("operator"))
            .is_some_and(|operator| operator.id() == self.id())
    }

    /// Whether `other`'s byte span lies within this node's span.
    pub fn contains(&self, other: M2Node<'_>) -> bool {
        self.start_byte() <= other.start_byte() && other.end_byte() <= self.end_byte()
    }

    /// The text *inside* a string literal's delimiters — the value without the
    /// surrounding `"`/`"` or `///`/`///` — located via the parser's own delimiter
    /// tokens (first and last child), never by scanning for quote characters.
    /// Escaped quotes, raw-string `/////`, and `"`-in-comment / `--`-in-string
    /// nesting are the parser's concern, not ours. Returns `None` for a non-string
    /// node or a literal missing a delimiter (e.g. an unterminated string).
    pub fn string_literal_inner_text(&self) -> Option<&'tree str> {
        if !self.is_string_literal() {
            return None;
        }
        let child_count = self.node.child_count();
        if child_count < 2 {
            return None;
        }
        let open = self.node.child(0)?;
        let close = self.node.child((child_count - 1) as u32)?;
        let (start, end) = (open.end_byte(), close.start_byte());
        (start <= end).then(|| &self.source[start..end])
    }

    pub fn child_by_field_name(&self, name: &str) -> Option<M2Node<'tree>> {
        let requested_name = name;
        let name = match (self.raw_kind(), name) {
            ("new_statement", "type") => "class",
            ("structured_binding" | "local_structured_binding", "left") => "binding_pack",
            ("quote_expression", "symbol") => "token",
            _ => name,
        };
        let source = self.source;
        self.node
            .child_by_field_name(name)
            .map(|node| M2Node::new(node, source))
            .or_else(|| {
                (self.raw_kind() == "binary_expression" && requested_name == "left")
                    .then(|| self.node.child_by_field_name("left_operand"))
                    .flatten()
                    .map(|node| M2Node::new(node, source))
            })
            .or_else(|| {
                (self.raw_kind() == "quote_expression" && name == "specifier")
                    .then(|| self.children().find(M2Node::is_modifier_token))
                    .flatten()
            })
            .or_else(|| {
                (self.raw_kind() == "except_clause" && name == "value")
                    .then(|| {
                        let exception = self.node.child_by_field_name("exception")?;
                        self.named_children()
                            .find(|child| child.node.id() != exception.id())
                    })
                    .flatten()
            })
            .or_else(|| {
                (self.raw_kind() == "try_statement" && name == "value")
                    .then(|| self.named_children().next())
                    .flatten()
            })
    }

    pub fn parent(&self) -> Option<M2Node<'tree>> {
        let source = self.source;
        self.node.parent().map(|node| M2Node::new(node, source))
    }

    pub fn ancestors(self) -> impl Iterator<Item = M2Node<'tree>> {
        iter::successors(self.parent(), |node| node.parent())
    }

    pub fn enclosing_node(
        self,
        predicate: impl Fn(&M2Node<'tree>) -> bool,
    ) -> Option<M2Node<'tree>> {
        iter::once(self)
            .chain(self.ancestors())
            .find(|node| predicate(node))
    }

    pub fn root(self) -> M2Node<'tree> {
        self.ancestors().last().unwrap_or(self)
    }

    pub fn children(&self) -> impl Iterator<Item = M2Node<'tree>> + '_ {
        let source = self.source;
        let mut cursor = self.node.walk();
        self.node
            .children(&mut cursor)
            .map(|node| M2Node::new(node, source))
            .collect::<Vec<_>>()
            .into_iter()
    }

    pub fn child(&self, index: u32) -> Option<M2Node<'tree>> {
        let source = self.source;
        self.node.child(index).map(|node| M2Node::new(node, source))
    }

    pub fn named_child(&self, index: u32) -> Option<M2Node<'tree>> {
        let source = self.source;
        self.node
            .named_child(index)
            .map(|node| M2Node::new(node, source))
    }

    pub fn named_children(&self) -> impl Iterator<Item = M2Node<'tree>> + '_ {
        let source = self.source;
        let mut cursor = self.node.walk();
        self.node
            .named_children(&mut cursor)
            .map(|node| M2Node::new(node, source))
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn next_named_sibling(&self) -> Option<M2Node<'tree>> {
        self.node
            .next_named_sibling()
            .map(|node| M2Node::new(node, self.source))
    }

    /// Return the smallest descendant spanning the requested point range.
    pub fn descendant_for_point_range(&self, start: Point, end: Point) -> Option<M2Node<'tree>> {
        let source = self.source;
        let node = if start == end {
            let next = Point::new(start.row, start.column.saturating_add(1));
            self.node
                .descendant_for_point_range(start, next)
                .or_else(|| self.node.descendant_for_point_range(start, end))
        } else {
            self.node.descendant_for_point_range(start, end)
        };
        node.map(|node| M2Node::new(node, source))
    }

    pub fn descendant_for_syntax<Syntax>(&self, syntax: &Syntax) -> Option<M2Node<'tree>>
    where
        Syntax: Spanned + ?Sized,
    {
        let (start, end) = super::syntax_byte_range(syntax)?;
        let source = self.source;
        let mut node = self
            .node
            .descendant_for_byte_range(start, end)
            .map(|node| M2Node::new(node, source))?;
        while !node.node.is_named() {
            node = node.parent()?;
        }
        Some(node)
    }

    /// The semantic element slots of a comma-delimited collection.
    ///
    /// The grammar exposes zero-width `empty_component` nodes for empty comma
    /// slots, so they remain in this iterator and count toward arity. Expressions terminated
    /// by `;` are wrapped in `muted` and do not contribute a value; comments are
    /// syntax extras rather than elements, so both are skipped here.
    pub fn collection_elements(&self) -> impl Iterator<Item = M2Node<'tree>> + '_ {
        self.named_children()
            .filter(|child| !child.is_muted_statement() && !child.is_comment())
    }

    /// The final value directly produced by a grouping/cell node.
    ///
    /// A trailing `muted` child means the final expression was silenced and the
    /// container produces no value node. Earlier muted expressions and comments
    /// do not obscure a later ordinary value.
    pub fn final_value_child(&self) -> Option<M2Node<'tree>> {
        self.named_children()
            .filter(|child| !child.is_comment())
            .last()
            .filter(|child| !child.is_muted_statement())
    }

    pub fn is_first_collection_element(&self, child: M2Node<'_>) -> bool {
        self.collection_elements()
            .next()
            .is_some_and(|first| first.id() == child.id())
    }

    /// Whether this comma borders a zero-width empty collection slot. Named
    /// sibling APIs can step past zero-width nodes, so compare the parser-owned
    /// slot boundaries with the comma boundaries instead.
    pub fn comma_borders_empty_slot(&self) -> bool {
        if !self.is_comma() {
            return false;
        }
        self.parent().is_some_and(|parent| {
            parent.named_children().any(|sibling| {
                sibling.is_nothing_value()
                    && (sibling.start_byte() == self.end_byte()
                        || sibling.end_byte() == self.start_byte())
            })
        })
    }

    /// Pre-order depth-first traversal of this subtree: the node itself, then
    /// every descendant in source order. Anonymous tokens (punctuation,
    /// delimiters, the inserted `SPACE` application operator) are included, so
    /// callers filter via typed predicates as usual — matching the other walk
    /// methods above. The iterator borrows `&self` (the cursor holds a
    /// tree-sitter borrow), but the yielded `M2Node<'tree>` items outlive the
    /// iterator, like `children()` above.
    pub fn descendants(&self) -> impl Iterator<Item = M2Node<'tree>> + '_ {
        let source = self.source;
        let mut cursor = self.node.walk();
        let mut reached_root = false;
        iter::from_fn(move || {
            if reached_root {
                return None;
            }
            let node = M2Node::new(cursor.node(), source);
            if !cursor.goto_first_child() && !cursor.goto_next_sibling() {
                loop {
                    if !cursor.goto_parent() {
                        reached_root = true;
                        break;
                    }
                    if cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
            Some(node)
        })
    }

    pub fn symbols(&self) -> impl Iterator<Item = M2Node<'tree>> + '_ {
        self.descendants().filter(M2Node::is_symbol_like)
    }

    pub fn start_byte(&self) -> usize {
        self.node.start_byte()
    }

    pub fn end_byte(&self) -> usize {
        self.node.end_byte()
    }

    pub fn trimmed_end_byte(&self) -> usize {
        self.start_byte() + self.text().trim_end().len()
    }

    pub fn id(&self) -> SyntaxNodeId {
        SyntaxNodeId(self.node.id())
    }

    pub fn start_position(&self) -> Point {
        self.node.start_position()
    }

    pub fn end_position(&self) -> Point {
        self.node.end_position()
    }

    pub fn child_count(&self) -> usize {
        self.node.child_count()
    }

    pub fn is_error(&self) -> bool {
        self.node.is_error()
    }

    pub fn is_missing(&self) -> bool {
        self.node.is_missing()
    }

    #[cfg(test)]
    pub fn has_error(&self) -> bool {
        self.node.has_error()
    }
}
