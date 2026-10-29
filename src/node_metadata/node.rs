//! Borrowed navigation over the single native m2-syn syntax snapshot.

use super::cells::SyntaxSnapshot;
use super::native::Entry;
use super::syntax_ref::{SyntaxRef, SyntaxType};
use super::{SyntaxTree, TypedSource};
use crate::source::ByteRange;
use crate::source::SourcePoint as Point;
use crate::string_literal::StringDelimiter;
use m2_syn::nodes::*;
use m2_syn::Token;
use std::iter;

/// Identity of a node in an immutable, retained native cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SyntaxNodeId(Option<super::cells::CellId>, usize);

/// A borrowed typed node and its source-navigation metadata.
#[derive(Debug, Clone, Copy)]
pub struct SyntaxNode<'tree> {
    /// Parsed syntax or the retained tree owning borrowed nodes.
    tree: &'tree SyntaxTree,
    /// Owning cell index, absent for the synthetic document root.
    cell: Option<usize>,
    /// Index selecting an entry in the enclosing representation.
    index: usize,
}

/// A generated type that can be selected in typed syntax.
pub trait SyntaxKind: SyntaxType {}
impl<T: SyntaxType> SyntaxKind for T {}

/// The three parsed operands of an infix expression.
#[derive(Debug, Clone, Copy)]
pub struct BinaryParts<'a> {
    /// Left operand of the expression.
    pub left: SyntaxNode<'a>,
    /// Operator spelling or syntax controlling this expression.
    pub operator: SyntaxNode<'a>,
    /// Right operand, when this operation has one.
    pub right: SyntaxNode<'a>,
}

impl<'tree> SyntaxNode<'tree> {
    /// Constructs the synthetic document root for a retained syntax tree.
    pub(super) fn root_for(tree: &'tree SyntaxTree) -> Self {
        Self {
            tree,
            cell: None,
            index: 0,
        }
    }
    /// Typed AST owner associated with this navigation view.
    pub fn typed_source(&self) -> &'tree TypedSource {
        self.tree.syntax()
    }
    /// Source ranges reparsed in the current document revision.
    pub fn changed_ranges(&self) -> Vec<ByteRange> {
        self.tree.changed_ranges()
    }
    /// Immutable cell parses and original source for this revision.
    pub fn snapshot(&self) -> &'tree SyntaxSnapshot {
        self.tree.snapshot()
    }
    /// Retained navigation metadata for this node, absent for the synthetic root.
    fn entry(&self) -> Option<&'tree Entry> {
        Some(&self.tree.indexes[self.cell?].entries()[self.index])
    }
    /// Navigates to another entry within the same owning cell.
    fn at(&self, index: usize) -> Self {
        Self { index, ..*self }
    }
    /// Retrieves this node's generated typed view, including aliases of category wrappers.
    pub fn get<T: SyntaxType>(&self) -> Option<&'tree T> {
        let cell = self.cell?;
        let index = *self.entry()?.aliases.get(&T::type_tag())?;
        self.tree.indexes[cell].resolve(index)?.cast::<T>()
    }
    /// Borrowed syntax backing this node, absent for synthetic or invalid entries.
    pub fn syntax(&self) -> Option<SyntaxRef<'tree>> {
        let cell = self.cell?;
        self.tree.indexes[cell].resolve(self.index)
    }
    /// Finds the navigation entry for an AST value by retained allocation identity.
    pub fn related<T: SyntaxType + ?Sized>(&self, syntax: &T) -> Option<Self> {
        let identity = syntax.syntax_ref().identity()?;
        let find = |cell: usize| {
            let index = *self.tree.indexes[cell].identities().get(&identity)?;
            Some(Self {
                tree: self.tree,
                cell: Some(cell),
                index: self.tree.indexes[cell].entries()[index].canonical,
            })
        };
        self.cell
            .and_then(find)
            .or_else(|| (0..self.tree.indexes.len()).find_map(find))
    }
    /// Whether this node exposes the requested generated syntax type.
    pub fn is<T: SyntaxKind>(&self) -> bool {
        if self.cell.is_none() {
            T::type_tag() == SourceFile::type_tag()
        } else {
            self.entry()
                .is_some_and(|entry| entry.aliases.contains_key(&T::type_tag()))
        }
    }
    /// Complete original document text for the owning revision.
    pub fn source_text(&self) -> &'tree str {
        self.tree.snapshot().source()
    }
    /// Source text covered by this node in the owning revision.
    pub fn text(&self) -> &'tree str {
        &self.source_text()[self.start_byte()..self.end_byte()]
    }
    /// Inclusive UTF-8 byte offset of this node in the document.
    pub fn start_byte(&self) -> usize {
        self.cell.map_or(0, |cell| {
            self.tree.snapshot().cells()[cell].document_range().start + self.entry().unwrap().start
        })
    }
    /// Exclusive UTF-8 byte offset of this node in the document.
    pub fn end_byte(&self) -> usize {
        self.cell.map_or(self.source_text().len(), |cell| {
            (self.tree.snapshot().cells()[cell].document_range().start + self.entry().unwrap().end)
                .min(self.source_text().len())
        })
    }
    /// End offset excluding trailing whitespace from this node's source text.
    pub fn trimmed_end_byte(&self) -> usize {
        self.start_byte() + self.text().trim_end().len()
    }
    /// Identity of this retained item, stable while its owning cell is reused.
    pub fn id(&self) -> SyntaxNodeId {
        SyntaxNodeId(
            self.cell.map(|cell| self.tree.indexes[cell].id()),
            self.index,
        )
    }
    /// Nearest visible enclosing node, absent for the document root.
    pub fn parent(&self) -> Option<Self> {
        let cell = self.cell?;
        Some(
            self.tree.indexes[cell]
                .visible_parent(self.index)
                .map_or(Self::root_for(self.tree), |index| self.at(index)),
        )
    }
    /// Visible enclosing nodes from nearest parent to document root.
    pub fn ancestors(self) -> impl Iterator<Item = Self> {
        iter::successors(self.parent(), |node| node.parent())
    }
    /// Synthetic root spanning the complete source document.
    pub fn root(self) -> Self {
        Self::root_for(self.tree)
    }
    /// Nearest enclosing node, including this one, accepted by the predicate.
    pub fn enclosing_node(self, predicate: impl Fn(&Self) -> bool) -> Option<Self> {
        iter::once(self).chain(self.ancestors()).find(predicate)
    }
    /// Visible child nodes in source order.
    pub fn children(&self) -> impl Iterator<Item = Self> + 'tree {
        let nodes = self.cell.map_or_else(
            || {
                (0..self.tree.indexes.len())
                    .map(|cell| Self {
                        tree: self.tree,
                        cell: Some(cell),
                        index: 0,
                    })
                    .collect()
            },
            |cell| {
                self.tree.indexes[cell]
                    .children(self.index)
                    .into_iter()
                    .map(|index| self.at(index))
                    .collect::<Vec<_>>()
            },
        );
        nodes.into_iter()
    }
    /// Direct named Markdown children, excluding anonymous punctuation tokens.
    pub fn named_children(&self) -> impl Iterator<Item = Self> + 'tree {
        self.children().filter(|node| !node.is_token())
    }
    /// Visible child at the requested zero-based position, when present.
    pub fn child(&self, index: u32) -> Option<Self> {
        self.children().nth(index as usize)
    }
    /// Direct named Markdown child with the requested grammar category.
    pub fn named_child(&self, index: u32) -> Option<Self> {
        self.named_children().nth(index as usize)
    }
    /// Number of visible direct children.
    pub fn child_count(&self) -> usize {
        self.children().count()
    }
    /// Preorder traversal including this node before its visible descendants.
    pub fn descendants(&self) -> impl Iterator<Item = Self> + 'tree {
        let mut stack = vec![*self];
        iter::from_fn(move || {
            let node = stack.pop()?;
            let children = node.children().collect::<Vec<_>>();
            stack.extend(children.into_iter().rev());
            Some(node)
        })
    }
    /// Tests membership in the represented selection or source extent.
    pub fn contains(&self, other: Self) -> bool {
        self.start_byte() <= other.start_byte() && other.end_byte() <= self.end_byte()
    }
    /// Generated type label used when describing syntax to the user.
    pub fn syntax_label(&self) -> &'static str {
        self.entry().map_or("SourceFile", |entry| entry.label)
    }
    /// Whether this node is lexical punctuation, a delimiter boundary, or implicit application.
    pub fn is_token(&self) -> bool {
        self.entry().is_some_and(|entry| {
            entry.token || entry.boundary.is_some() || entry.aliases.contains_key(&Adj::type_tag())
        })
    }
    /// Whether the node has a generated Symbol view.
    pub fn is_symbol_like(&self) -> bool {
        self.is::<Symbol>()
    }
    /// Whether this node is a numeric or string literal.
    pub fn is_literal(&self) -> bool {
        self.is::<IntegerLiteral>() || self.is::<FloatLiteral>() || self.is_string_literal()
    }
    /// Whether this node is a quoted or raw string literal.
    pub fn is_string_literal(&self) -> bool {
        self.is::<StringLiteral>() || self.is::<RawStringLiteral>()
    }
    /// Whether the node represents a collection or assignment argument list.
    pub fn is_collection_expression(&self) -> bool {
        self.is::<Sequence>()
            || self.is::<List>()
            || self.is::<Array>()
            || self.is::<AngleBarList>()
            || self.is::<AssignmentList>()
    }
    /// Whether the node is a collection, holder, or assignment argument list.
    pub fn is_delimited_expression(&self) -> bool {
        self.is_collection_expression() || self.is_holder() || self.is::<AssignmentList>()
    }
    /// Whether this container can contribute nested lambda parameter symbols.
    pub fn is_parameter_container(&self) -> bool {
        self.is_delimited_expression() || self.is::<ArgumentListContent>()
    }
    /// Whether the node represents a sequence or packed component list.
    pub fn is_sequence(&self) -> bool {
        self.is::<Sequence>() || self.is_expr_pack()
    }
    /// Whether the node is a component pack rather than its enclosing collection.
    pub fn is_expr_pack(&self) -> bool {
        self.is::<ExprPack>() && !self.is_collection_expression()
    }
    /// Whether the source symbol has the spelling null.
    pub fn is_null_symbol(&self) -> bool {
        self.is::<Symbol>() && self.text() == "null"
    }
    /// Whether the construct produces Nothing because it is muted or empty.
    pub fn is_nothing_value(&self) -> bool {
        self.is_muted_statement() || self.is_empty_component()
    }
    /// Whether this node is a comment.
    pub fn is_comment(&self) -> bool {
        self.entry().is_some_and(|entry| entry.comment)
    }
    /// Whether this node is a line comment.
    pub fn is_line_comment(&self) -> bool {
        matches!(self.syntax(), Some(SyntaxRef::Trivia(trivia)) if trivia.kind() == m2_syn::TriviaKind::LineComment)
    }
    /// Whether this node is a block comment.
    pub fn is_block_comment(&self) -> bool {
        matches!(self.syntax(), Some(SyntaxRef::Trivia(trivia)) if trivia.kind() == m2_syn::TriviaKind::BlockComment)
    }
    /// Whether parentheses hold a value rather than form a sequence.
    pub fn is_holder(&self) -> bool {
        self.is::<Holder>()
    }
    /// Whether a semicolon discards the statement value.
    pub fn is_muted_statement(&self) -> bool {
        self.is::<Statement>()
            || self
                .get::<Cell>()
                .is_some_and(|cell| cell.semicolon.is_some())
    }
    /// Whether this node is an empty slot in a packed expression.
    pub fn is_empty_component(&self) -> bool {
        self.is::<Empty>()
    }
    /// Whether the node is an independently framed top-level cell.
    pub fn is_source_cell(&self) -> bool {
        self.cell.is_some() && (self.index == 0 || self.is::<Statement>())
    }
    /// Whether this node forms a range used by iteration syntax.
    pub fn is_iteration_range(&self) -> bool {
        self.is::<RangeBoundFrom>() || self.is::<RangeBoundTo>() || self.is::<WhenCondition>()
    }
    /// Whether this clause handles an exception with an exception-name binding.
    pub fn is_except_clause(&self) -> bool {
        matches!(self.get::<TryFallback>(), Some(TryFallback::Except { .. }))
    }
    /// Whether this node is a debugging expression.
    pub fn is_debug_expr(&self) -> bool {
        self.is::<ExprDebug>() || self.is::<ExprProfile>()
    }
    /// Whether this expression transfers a return value to its enclosing function.
    pub fn is_return_expr(&self) -> bool {
        self.is::<ExprControlReturn>()
    }
    /// Whether this expression exits its receiving loop.
    pub fn is_break_expr(&self) -> bool {
        self.is::<ExprControlBreak>()
    }
    /// Whether this expression advances its receiving loop.
    pub fn is_continue_expr(&self) -> bool {
        self.is::<ExprControlContinue>()
    }
    /// Whether the expression is return, break, or continue.
    pub fn is_control_transfer(&self) -> bool {
        self.is_return_expr() || self.is_break_expr() || self.is_continue_expr()
    }
    /// Whether a prefix operator precedes this expression operand.
    pub fn is_prefix_expr(&self) -> bool {
        self.is::<ExprOpPrefix>()
    }
    /// Whether a postfix operator follows this expression operand.
    pub fn is_postfix_expr(&self) -> bool {
        self.is::<ExprOpPostfix>()
    }
    /// Whether this expression has an explicit binary operator.
    pub fn is_binary_expr(&self) -> bool {
        self.is::<ExprOpBinRegular>() || self.is::<ExprOpBinMember>() || self.is::<ExprOpBinIndex>()
    }
    /// Whether this expression uses a prefix, postfix, or binary operator.
    pub fn is_operator_expression(&self) -> bool {
        self.is_binary_expr()
            || self.is_adjacent_expr()
            || self.is_prefix_expr()
            || self.is_postfix_expr()
    }
    /// Whether juxtaposition applies the left expression to the right.
    pub fn is_adjacent_expr(&self) -> bool {
        self.is::<ExprOpBinAdj>()
    }
    /// Whether this node represents an implicit application between adjacent expressions.
    pub fn is_space_application(&self) -> bool {
        self.is_adjacent_expr()
    }
    /// A call whose arguments are bracketed, as `f(x)`, `f()` or `QQ[x]`,
    /// not one by juxtaposition, as `f x`.
    pub fn is_bracketed_call(&self) -> bool {
        self.is_space_application()
            && self
                .binary_parts()
                .is_some_and(|call| call.right.is_delimited_expression())
    }
    /// Whether this token is the zero-width operator of juxtaposition.
    pub fn is_implicit_application(&self) -> bool {
        self.is::<Adj>()
    }
    /// Whether the expression has a generated assignment view.
    pub fn is_assignment(&self) -> bool {
        self.is::<ExprAssign>() || self.is::<ExprAssignEvaluated>()
    }
    /// Whether the expression is an option key/value arrow.
    pub fn is_option_assignment(&self) -> bool {
        self.is::<ExprOption>()
    }
    /// Whether this node is a opening delimiter.
    pub fn is_opening_delimiter(&self) -> bool {
        self.entry()
            .is_some_and(|entry| entry.boundary == Some(true))
    }
    /// Whether this node is a closing delimiter.
    pub fn is_closing_delimiter(&self) -> bool {
        self.entry()
            .is_some_and(|entry| entry.boundary == Some(false))
    }
    /// Byte width of the represented closing delimiter.
    pub fn closing_delimiter_width(&self) -> usize {
        if self.is::<AngleBarList>() {
            2
        } else {
            1
        }
    }
    /// Whether the construct owns keywords highlighted together as one statement.
    pub fn is_keyword_statement(&self) -> bool {
        self.is::<ExprIf>()
            || self.is::<ExprFor>()
            || self.is::<ExprWhile>()
            || self.is::<ExprNew>()
            || self.is::<ExprTry>()
    }
    /// Whether the construct contributes a clause keyword to its enclosing statement.
    pub fn is_keyword_clause(&self) -> bool {
        self.is_iteration_range()
            || self.is::<LoopBody>()
            || self.is::<ThenClause>()
            || self.is::<ElseClause>()
            || self.is_except_clause()
    }
    /// Whether this node is a comma.
    pub fn is_comma(&self) -> bool {
        self.is::<Token![,]>()
    }
    /// Whether this node is a semicolon.
    pub fn is_semicolon(&self) -> bool {
        self.is::<Token![;]>()
    }
    /// Whether this generated token has a keyword spelling.
    pub fn is_keyword_token(&self) -> bool {
        self.is_token()
            && !self.is_implicit_application()
            && self.text().chars().next().is_some_and(char::is_alphabetic)
            && !self.is_modifier_token()
    }
    /// Whether this token selects a quote or binding modifier.
    pub fn is_modifier_token(&self) -> bool {
        self.is::<Token![global]>()
            || self.is::<Token![local]>()
            || self.is::<Token![symbol]>()
            || self.is::<Token![threadLocal]>()
            || self.is::<Token![threadVariable]>()
    }
    /// Whether this node is a then or else keyword.
    pub fn is_then_or_else_keyword(&self) -> bool {
        self.is::<Token![then]>() || self.is::<Token![else]>()
    }
    /// Fatal grammatical error for the owning cell, when one was recorded.
    pub fn parse_error(&self) -> Option<&'tree m2_syn::NativeParseError> {
        (self.index == 0)
            .then_some(self.cell?)
            .and_then(|cell| self.tree.snapshot().cells()[cell].parse_error())
    }
    /// Document byte ranges associated with this cell's parse and lexer errors.
    pub fn error_ranges(&self) -> Vec<ByteRange> {
        use m2_syn::{NativeParseError as Error, Spanned};
        let Some(cell) = self
            .cell
            .filter(|_| self.index == 0)
            .map(|cell| &self.tree.snapshot().cells()[cell])
        else {
            return Vec::new();
        };
        let mut spans = cell
            .lexer_diagnostics()
            .iter()
            .filter_map(Spanned::try_span)
            .collect::<Vec<_>>();
        if let Some(error) = cell.parse_error() {
            let span = match error {
                Error::Lex(error) => error.try_span(),
                Error::Tokenize(error) => error.try_span(),
                Error::Unexpected { span, .. }
                | Error::MissingOperand { span, .. }
                | Error::NewlineInApplication { span }
                | Error::InvalidAssignmentTarget { span }
                | Error::InvalidLambdaArguments { span }
                | Error::Unsupported { span, .. } => Some(span.clone()),
            };
            spans.extend(span);
        }
        spans
            .into_iter()
            .filter_map(|span| span.range())
            .map(|range| {
                cell.document_range().start + range.start..cell.document_range().start + range.end
            })
            .collect()
    }
    /// Whether this entry represents invalid cell syntax.
    pub fn is_error(&self) -> bool {
        self.entry().is_some_and(|entry| entry.error)
            || self.cell.is_some() && self.index == 0 && self.has_error()
    }
    /// Reports false: native failures are retained as cell diagnostics rather than synthetic missing nodes.
    pub fn is_missing(&self) -> bool {
        self.is_closing_delimiter() && self.start_byte() == self.end_byte()
    }
    /// Whether the node or a contained cell has lexical or grammatical diagnostics.
    pub fn has_error(&self) -> bool {
        self.cell.map_or_else(
            || {
                self.tree.snapshot().cells().iter().any(|cell| {
                    cell.parse_error().is_some() || !cell.lexer_diagnostics().is_empty()
                })
            },
            |cell| {
                self.tree.snapshot().cells()[cell].parse_error().is_some()
                    || !self.tree.snapshot().cells()[cell]
                        .lexer_diagnostics()
                        .is_empty()
            },
        )
    }
    /// Zero-based row and UTF-8 byte column at the node's start.
    pub fn start_position(&self) -> Point {
        self.tree.point_for_byte(self.start_byte())
    }
    /// Zero-based row and UTF-8 byte column at the node's end.
    pub fn end_position(&self) -> Point {
        self.tree.point_for_byte(self.end_byte())
    }
    /// Smallest visible descendant covering the requested byte-column positions.
    pub fn descendant_for_point_range(&self, start: Point, end: Point) -> Option<Self> {
        let start = self.tree.byte_for_point(start)?;
        let end = self.tree.byte_for_point(end)?;
        self.descendants()
            .filter(|node| {
                node.start_byte() <= start
                    && (if start == end {
                        start < node.end_byte()
                            || node.start_byte() == node.end_byte() && start == node.start_byte()
                    } else {
                        end <= node.end_byte()
                    })
            })
            .min_by_key(|node| {
                (
                    node.end_byte() - node.start_byte(),
                    node.is_implicit_application(),
                    std::cmp::Reverse(node.ancestors().count()),
                )
            })
    }
    /// Symbol occurrences reachable in preorder from this node.
    pub fn symbols(&self) -> impl Iterator<Item = Self> + 'tree {
        self.descendants().filter(Self::is_symbol_like)
    }
    /// Symbol occurrences that evaluate bindings rather than name member keys.
    pub fn binding_references(&self) -> impl Iterator<Item = Self> + 'tree {
        self.symbols().filter(|node| !node.is_member_key())
    }
    /// Whether the symbol names a receiver member rather than reading a binding.
    pub fn is_member_key(&self) -> bool {
        self.is_symbol_like()
            && self
                .parent()
                .and_then(|node| node.property_key())
                .is_some_and(|key| key.id() == self.id())
    }
    /// Value elements of a collection or packed expression, excluding separators.
    pub fn collection_elements(&self) -> impl Iterator<Item = Self> + 'tree {
        let mut values = Vec::new();
        for child in self
            .named_children()
            .filter(|node| !node.is_muted_statement() && !node.is_comment())
        {
            if child.is_expr_pack() {
                values.extend(child.collection_elements());
            } else {
                values.push(child);
            }
        }
        values.into_iter()
    }
    /// Final unmuted value contributed by this container.
    pub fn final_value_child(&self) -> Option<Self> {
        self.named_children()
            .filter(|node| !node.is_comment())
            .last()
            .filter(|node| !node.is_muted_statement())
    }
    /// Whether the supplied child is the first value element of this collection.
    pub fn is_first_collection_element(&self, child: Self) -> bool {
        self.collection_elements()
            .next()
            .is_some_and(|node| node.id() == child.id())
    }
    /// Whether this separator adjoins an empty packed argument position.
    pub fn comma_borders_empty_slot(&self) -> bool {
        self.is_comma()
            && self.parent().is_some_and(|parent| {
                parent.named_children().any(|node| {
                    node.is_empty_component()
                        && (node.start_byte() == self.end_byte()
                            || node.end_byte() == self.start_byte())
                })
            })
    }
    /// Document byte range of string contents, excluding verified closing delimiters.
    pub fn string_literal_inner_bytes(&self) -> Option<ByteRange> {
        if !self.is_string_literal() {
            return None;
        }
        use m2_syn::Spanned;
        let (width, content_end) = if let Some(node) = self.get::<StringLiteral>() {
            (
                1,
                node.elements
                    .last()
                    .and_then(Spanned::try_span)
                    .and_then(|span| span.end()),
            )
        } else {
            let node = self.get::<RawStringLiteral>()?;
            (
                3,
                node.elements
                    .last()
                    .and_then(Spanned::try_span)
                    .and_then(|span| span.end()),
            )
        };
        let local_start = self.entry()?.start;
        let content_end = content_end.unwrap_or(local_start + width);
        (content_end + width == self.entry()?.end)
            .then_some(self.start_byte() + width..self.end_byte() - width)
    }

    /// Original string contents without their opening and closing delimiters.
    pub fn string_literal_inner_text(&self) -> Option<&'tree str> {
        Some(&self.source_text()[self.string_literal_inner_bytes()?])
    }

    /// The string a string literal denotes, its escapes read; `None` for an
    /// escape Macaulay2 does not read.
    pub fn string_literal_value(&self) -> Option<String> {
        StringDelimiter::of(*self)?.decode(self.string_literal_inner_text()?)
    }
    /// Name of the callable symbol at the head of an application, when recognizable.
    pub fn applied_symbol_name(&self) -> Option<&'tree str> {
        let call = self.get::<ExprOpBinAdj>()?;
        let head = self.related(call.left_operand.as_ref())?;
        head.is::<Symbol>().then(|| head.text())
    }
    /// Operands and operator forming an explicitly installed method's head.
    pub fn method_installation_head(&self) -> Option<BinaryParts<'tree>> {
        let node = self.get::<ExprAssignInstallMethod>()?;
        Some(BinaryParts {
            left: self.related(node.method.as_ref())?,
            operator: self.related(&node.operator)?,
            right: self.related(node.signature.as_ref())?,
        })
    }
    /// Left operand, operator, and right operand from supported binary or assignment syntax.
    pub fn binary_parts(&self) -> Option<BinaryParts<'tree>> {
        macro_rules! parts {
            ($node:expr, $left:ident, $operator:ident, $right:ident) => {{
                let n = $node;
                Some(BinaryParts {
                    left: self.related(&n.$left)?,
                    operator: self.related(&n.$operator)?,
                    right: self.related(&n.$right)?,
                })
            }};
        }
        if let Some(n) = self.get::<ExprOpBinAdj>() {
            return parts!(n, left_operand, operator, right_operand);
        }
        if let Some(n) = self.get::<ExprOpBinRegular>() {
            return parts!(n, left_operand, operator, right_operand);
        }
        if let Some(n) = self.get::<ExprOpBinIndex>() {
            return parts!(n, receiver, operator, index);
        }
        if let Some(n) = self.get::<ExprOpBinMember>() {
            return match n {
                ExprOpBinMember::Dot(n) => parts!(n, receiver, operator, member),
                ExprOpBinMember::Optional(n) => parts!(n, receiver, operator, member),
            };
        }
        if let Some(n) = self.get::<ExprOption>() {
            return parts!(n, key, operator, value);
        }
        if let Some(n) = self.get::<ExprAssignEvaluated>() {
            return parts!(n, evaluated_domain, operator, value);
        }
        match self.get::<ExprAssign>()? {
            ExprAssign::Simple(ExprAssignSimple::Global(n)) => parts!(n, variable, operator, value),
            ExprAssign::Simple(ExprAssignSimple::Local(n)) => parts!(n, variable, operator, value),
            ExprAssign::Parallel(ExprAssignParallel::Global(n)) => {
                parts!(n, argument_list, operator, value)
            }
            ExprAssign::Parallel(ExprAssignParallel::Local(n)) => {
                parts!(n, argument_list, operator, value)
            }
            ExprAssign::Evaluated(n) => parts!(n, evaluated_domain, operator, value),
            ExprAssign::Augmented(n) => parts!(n, target, operator, value),
            ExprAssign::Op(n) => parts!(n, argument_expr, operator, value_expr),
            ExprAssign::Install(ExprAssignInstall::Method(n)) => {
                parts!(n, method, assign_op, function)
            }
            ExprAssign::Install(ExprAssignInstall::Op(n)) => {
                parts!(n, domain_expr, assign_op, function)
            }
            ExprAssign::Install(ExprAssignInstall::New(n)) => {
                parts!(n, new_statement, operator, function)
            }
        }
    }
    /// Original spelling of this node's binary operator, when present.
    pub fn binary_operator(&self) -> Option<&'tree str> {
        let parts = self.binary_parts()?;
        Some(if self.is_adjacent_expr() {
            "SPACE"
        } else {
            parts.operator.text()
        })
    }
    /// Whether the binary operator has the spelling of the requested generated token.
    pub fn has_binary_operator<T: m2_syn::Token>(&self) -> bool {
        self.binary_operator()
            .is_some_and(super::matches_token::<T>)
    }
    /// Whether the expression selects a part of its left operand instead of
    /// computing from both: `x_i`, `x.k`, `x.?k`, `x#i` or `x#?k`.
    pub fn is_access(&self) -> bool {
        self.is::<ExprOpBinMember>()
            || self.is::<ExprOpBinIndex>()
            || self.has_binary_operator::<Token![_]>()
            || self.has_binary_operator::<Token![#?]>()
    }
    /// Member symbol or literal string naming a property rather than a binding reference.
    pub fn property_key(&self) -> Option<Self> {
        let parts = self.binary_parts()?;
        ((self.is::<ExprOpBinMember>() && parts.right.is_symbol_like())
            || ((self.has_binary_operator::<Token![#]>()
                || self.has_binary_operator::<Token![#?]>())
                && parts.right.is_string_literal()))
        .then_some(parts.right)
    }
    /// Whether the parent uses this node as its operator token.
    pub fn is_operator(&self) -> bool {
        self.parent().is_some_and(|parent| {
            parent
                .binary_parts()
                .is_some_and(|parts| parts.operator.id() == self.id())
                || parent
                    .operator_node()
                    .is_some_and(|operator| operator.id() == self.id())
        })
    }
    /// Token supplying this expression's operator, when present.
    pub fn operator_node(&self) -> Option<Self> {
        if let Some(parts) = self.binary_parts() {
            return Some(parts.operator);
        }
        if let Some(n) = self.get::<ExprOpPrefix>() {
            return self.related(&n.operator);
        }
        if let Some(n) = self.get::<ExprOpPostfix>() {
            return self.related(&n.operator);
        }
        match self.get::<ExprLambda>()? {
            ExprLambda::Variadic(n) => self.related(&n.operator),
            ExprLambda::List(n) => self.related(&n.operator),
        }
    }
    /// Value carried by a return, break, or related control-transfer expression.
    pub fn control_transfer_value(&self) -> Option<Self> {
        if let Some(n) = self.get::<ExprControlReturn>() {
            return n.value.as_deref().and_then(|v| self.related(v));
        }
        if let Some(n) = self.get::<ExprControlBreak>() {
            return n.value.as_deref().and_then(|v| self.related(v));
        }
        self.get::<ExprControlContinue>()?
            .value
            .as_deref()
            .and_then(|v| self.related(v))
    }
    /// Expression evaluated by a lambda when the function is called.
    pub fn lambda_body(&self) -> Option<Self> {
        match self.get::<ExprLambda>()? {
            ExprLambda::Variadic(n) => self.related(n.body.as_ref()),
            ExprLambda::List(n) => self.related(n.body.as_ref()),
        }
    }
    /// The symbols a lambda binds, in order: `x` of `x -> …`, and `a`, `b`
    /// of `(a, b) -> …`.
    pub fn lambda_parameter_symbols(&self) -> Vec<Self> {
        let Some(lambda) = self.get::<ExprLambda>() else {
            return Vec::new();
        };
        match lambda {
            ExprLambda::Variadic(n) => self.related(&n.argument).into_iter().collect(),
            ExprLambda::List(n) => match &n.arguments.contents {
                None => Vec::new(),
                Some(ArgumentListContent::Symbol(symbol)) => {
                    self.related(symbol).into_iter().collect()
                }
                Some(ArgumentListContent::Pack(pack)) => pack
                    .arguments
                    .iter()
                    .filter_map(|symbol| self.related(symbol))
                    .collect(),
            },
        }
    }
    /// Syntax declaring the lambda's fixed or variadic parameters.
    pub fn lambda_parameters(&self) -> Option<Self> {
        match self.get::<ExprLambda>()? {
            ExprLambda::Variadic(n) => self.related(&n.argument),
            ExprLambda::List(n) => n.arguments.contents.as_ref().and_then(|n| self.related(n)),
        }
    }
    /// Condition expression tested by a conditional or loop construct.
    pub fn control_condition(&self) -> Option<Self> {
        if let Some(n) = self.get::<ExprIf>() {
            return self.related(n.condition.as_ref());
        }
        if let Some(n) = self.get::<ExprWhile>() {
            return self.related(n.condition.as_ref());
        }
        if let Some(n) = self.get::<WhenCondition>() {
            return self.related(n.condition.as_ref());
        }
        None
    }
    /// Expression contributed by this control-flow clause.
    pub fn clause_value(&self) -> Option<Self> {
        if let Some(n) = self.get::<ThenClause>() {
            return self.related(n.expr.as_ref());
        }
        if let Some(n) = self.get::<ElseClause>() {
            return self.related(n.expr.as_ref());
        }
        if let Some(n) = self.get::<ExprTry>() {
            return self.related(n.value.as_ref());
        }
        if let Some(n) = self.get::<OfClass>() {
            return self.related(n.parent.as_ref());
        }
        if let Some(n) = self.get::<FromInstance>() {
            return self.related(n.instance.as_ref());
        }
        if let Some(n) = self.get::<RangeBoundFrom>() {
            return self.related(n.value.as_ref());
        }
        if let Some(n) = self.get::<RangeBoundTo>() {
            return self.related(n.value.as_ref());
        }
        if let Some(TryFallback::Except(n)) = self.get::<TryFallback>() {
            return self.related(n.value.as_ref());
        }
        if self.is_control_transfer() {
            return self.control_transfer_value();
        }
        macro_rules! optional {
            ($n:expr) => {
                $n.value.as_deref().and_then(|n| self.related(n))
            };
        }
        if let Some(n) = self.get::<ExprDebug>() {
            return match n {
                ExprDebug::Test(n) => optional!(n),
                ExprDebug::Step(n) => optional!(n),
                ExprDebug::Finish(n) => optional!(n),
                ExprDebug::Shield(n) => optional!(n),
                ExprDebug::Breakpoint(n) => optional!(n),
            };
        }
        if let Some(n) = self.get::<ExprProfile>() {
            return match n {
                ExprProfile::ElapsedTime(n) => self.related(n.value.as_ref()),
                ExprProfile::ElapsedTiming(n) => self.related(n.value.as_ref()),
                ExprProfile::Profile(n) => self.related(n.value.as_ref()),
                ExprProfile::Time(n) => self.related(n.value.as_ref()),
                ExprProfile::Timing(n) => self.related(n.value.as_ref()),
            };
        }
        None
    }
    /// Operand of a prefix or postfix expression, when present.
    pub fn unary_operand(&self) -> Option<Self> {
        if let Some(n) = self.get::<ExprOpPrefix>() {
            return self.related(n.operand.as_ref());
        }
        if let Some(n) = self.get::<ExprOpPostfix>() {
            return self.related(n.operand.as_ref());
        }
        None
    }
    /// Class expression following the new keyword.
    pub fn new_class(&self) -> Option<Self> {
        self.related(self.get::<ExprNew>()?.class.as_ref())
    }
    /// Optional superclass clause of a new expression.
    pub fn new_parent_clause(&self) -> Option<Self> {
        self.related(self.get::<ExprNew>()?.parent_class.as_ref()?)
    }
    /// Optional source-instance clause of a new expression.
    pub fn new_instance_clause(&self) -> Option<Self> {
        self.related(self.get::<ExprNew>()?.instance.as_ref()?)
    }
    /// Value supplied by a list clause, when this node represents one.
    pub fn listed_value(&self) -> Option<Self> {
        self.related(self.get::<LoopBody>()?.list_step.as_ref()?.expr.as_ref())
    }
    /// Value evaluated by a do clause or muted statement.
    pub fn ignored_value(&self) -> Option<Self> {
        self.related(self.get::<LoopBody>()?.do_step.as_ref()?.statement.as_ref())
    }
    /// Quoted symbol and the keyword selecting its quotation mode.
    pub fn quote_parts(&self) -> Option<(Self, Self)> {
        macro_rules! parts {
            ($n:expr) => {
                Some((self.related(&$n.symbol)?, self.related(&$n.specifier)?))
            };
        }
        match self.get::<ExprQuote>()? {
            ExprQuote::Symbol(n) => parts!(n),
            ExprQuote::Global(n) => parts!(n),
            ExprQuote::Local(n) => parts!(n),
            ExprQuote::ThreadLocal(n) => parts!(n),
            ExprQuote::ThreadVariable(n) => parts!(n),
        }
    }
    /// Symbol returned by a quotation instead of evaluating its binding.
    pub fn quoted_symbol(&self) -> Option<Self> {
        self.quote_parts().map(|parts| parts.0)
    }
    #[cfg(test)]
    pub fn quote_modifier(&self) -> Option<Self> {
        self.quote_parts().map(|parts| parts.1)
    }
}

/// Visits navigation descendants of the source root in preorder.
pub fn visit_source_nodes<'a>(
    root: SyntaxNode<'a>,
    _syntax: Option<&TypedSource>,
    mut visitor: impl FnMut(SyntaxNode<'a>),
) {
    for node in root.descendants() {
        visitor(node)
    }
}
/// Visits navigation descendants of an expression in preorder.
pub fn visit_expression_nodes<'a>(
    root: SyntaxNode<'a>,
    _syntax: Option<&Expr>,
    mut visitor: impl FnMut(SyntaxNode<'a>),
) {
    for node in root.descendants() {
        visitor(node)
    }
}
