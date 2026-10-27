//! Borrowed navigation over the single native m2-syn syntax snapshot.

use super::cells::SyntaxSnapshot;
use super::native::Entry;
use super::syntax_ref::{SyntaxRef, SyntaxType};
use super::{SyntaxTree, TypedSource};
use crate::source::ByteRange;
use crate::source::SourcePoint as Point;
use m2_syn::nodes::*;
use m2_syn::Token;
use std::iter;

/// Identity of a node in an immutable, retained native cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SyntaxNodeId(Option<super::cells::CellId>, usize);

/// A borrowed typed node and its source-navigation metadata.
#[derive(Debug, Clone, Copy)]
pub struct SyntaxNode<'tree> {
    tree: &'tree SyntaxTree,
    cell: Option<usize>,
    index: usize,
}

/// A generated type that can be selected in typed syntax.
pub trait SyntaxKind: SyntaxType {}
impl<T: SyntaxType> SyntaxKind for T {}

/// The three parsed operands of an infix expression.
#[derive(Debug, Clone, Copy)]
pub struct BinaryParts<'a> {
    pub left: SyntaxNode<'a>,
    pub operator: SyntaxNode<'a>,
    pub right: SyntaxNode<'a>,
}

impl<'tree> SyntaxNode<'tree> {
    pub(super) fn root_for(tree: &'tree SyntaxTree) -> Self {
        Self {
            tree,
            cell: None,
            index: 0,
        }
    }
    pub fn typed_source(&self) -> &'tree TypedSource {
        self.tree.syntax()
    }
    pub fn changed_ranges(&self) -> Vec<ByteRange> {
        self.tree.changed_ranges()
    }
    pub fn snapshot(&self) -> &'tree SyntaxSnapshot {
        self.tree.snapshot()
    }
    fn entry(&self) -> Option<&'tree Entry> {
        Some(&self.tree.indexes[self.cell?].entries()[self.index])
    }
    fn at(&self, index: usize) -> Self {
        Self { index, ..*self }
    }
    pub fn get<T: SyntaxType>(&self) -> Option<&'tree T> {
        let cell = self.cell?;
        let index = *self.entry()?.aliases.get(&T::type_tag())?;
        self.tree.indexes[cell].resolve(index)?.cast::<T>()
    }
    pub fn syntax(&self) -> Option<SyntaxRef<'tree>> {
        let cell = self.cell?;
        self.tree.indexes[cell].resolve(self.index)
    }
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
    pub fn is<T: SyntaxKind>(&self) -> bool {
        if self.cell.is_none() {
            T::type_tag() == SourceFile::type_tag()
        } else {
            self.entry()
                .is_some_and(|entry| entry.aliases.contains_key(&T::type_tag()))
        }
    }
    pub fn source_text(&self) -> &'tree str {
        self.tree.snapshot().source()
    }
    pub fn text(&self) -> &'tree str {
        &self.source_text()[self.start_byte()..self.end_byte()]
    }
    pub fn start_byte(&self) -> usize {
        self.cell.map_or(0, |cell| {
            self.tree.snapshot().cells()[cell].document_range().start + self.entry().unwrap().start
        })
    }
    pub fn end_byte(&self) -> usize {
        self.cell.map_or(self.source_text().len(), |cell| {
            (self.tree.snapshot().cells()[cell].document_range().start + self.entry().unwrap().end)
                .min(self.source_text().len())
        })
    }
    pub fn trimmed_end_byte(&self) -> usize {
        self.start_byte() + self.text().trim_end().len()
    }
    pub fn id(&self) -> SyntaxNodeId {
        SyntaxNodeId(
            self.cell.map(|cell| self.tree.indexes[cell].id()),
            self.index,
        )
    }
    pub fn parent(&self) -> Option<Self> {
        let cell = self.cell?;
        Some(
            self.tree.indexes[cell]
                .visible_parent(self.index)
                .map_or(Self::root_for(self.tree), |index| self.at(index)),
        )
    }
    pub fn ancestors(self) -> impl Iterator<Item = Self> {
        iter::successors(self.parent(), |node| node.parent())
    }
    pub fn root(self) -> Self {
        Self::root_for(self.tree)
    }
    pub fn enclosing_node(self, predicate: impl Fn(&Self) -> bool) -> Option<Self> {
        iter::once(self).chain(self.ancestors()).find(predicate)
    }
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
    pub fn named_children(&self) -> impl Iterator<Item = Self> + 'tree {
        self.children().filter(|node| !node.is_token())
    }
    pub fn child(&self, index: u32) -> Option<Self> {
        self.children().nth(index as usize)
    }
    pub fn named_child(&self, index: u32) -> Option<Self> {
        self.named_children().nth(index as usize)
    }
    pub fn child_count(&self) -> usize {
        self.children().count()
    }
    pub fn descendants(&self) -> impl Iterator<Item = Self> + 'tree {
        let mut stack = vec![*self];
        iter::from_fn(move || {
            let node = stack.pop()?;
            let children = node.children().collect::<Vec<_>>();
            stack.extend(children.into_iter().rev());
            Some(node)
        })
    }
    pub fn contains(&self, other: Self) -> bool {
        self.start_byte() <= other.start_byte() && other.end_byte() <= self.end_byte()
    }
    pub fn syntax_label(&self) -> &'static str {
        self.entry().map_or("SourceFile", |entry| entry.label)
    }
    pub fn is_token(&self) -> bool {
        self.entry().is_some_and(|entry| {
            entry.token || entry.boundary.is_some() || entry.aliases.contains_key(&Adj::type_tag())
        })
    }
    pub fn is_symbol_like(&self) -> bool {
        self.is::<Symbol>()
    }
    pub fn is_literal(&self) -> bool {
        self.is::<IntegerLiteral>() || self.is::<FloatLiteral>() || self.is_string_literal()
    }
    pub fn is_string_literal(&self) -> bool {
        self.is::<StringLiteral>() || self.is::<RawStringLiteral>()
    }
    pub fn is_collection_expression(&self) -> bool {
        self.is::<Sequence>()
            || self.is::<List>()
            || self.is::<Array>()
            || self.is::<AngleBarList>()
            || self.is::<AssignmentList>()
    }
    pub fn is_delimited_expression(&self) -> bool {
        self.is_collection_expression() || self.is_holder() || self.is::<AssignmentList>()
    }
    pub fn is_parameter_container(&self) -> bool {
        self.is_delimited_expression() || self.is::<ArgumentListContent>()
    }
    pub fn is_sequence(&self) -> bool {
        self.is::<Sequence>() || self.is_expr_pack()
    }
    pub fn is_expr_pack(&self) -> bool {
        self.is::<ExprPack>() && !self.is_collection_expression()
    }
    pub fn is_null_symbol(&self) -> bool {
        self.is::<Symbol>() && self.text() == "null"
    }
    pub fn is_nothing_value(&self) -> bool {
        self.is_muted_statement() || self.is_empty_component()
    }
    pub fn is_comment(&self) -> bool {
        self.entry().is_some_and(|entry| entry.comment)
    }
    pub fn is_line_comment(&self) -> bool {
        matches!(self.syntax(), Some(SyntaxRef::Trivia(trivia)) if trivia.kind() == m2_syn::TriviaKind::LineComment)
    }
    pub fn is_block_comment(&self) -> bool {
        matches!(self.syntax(), Some(SyntaxRef::Trivia(trivia)) if trivia.kind() == m2_syn::TriviaKind::BlockComment)
    }
    pub fn is_holder(&self) -> bool {
        self.is::<Holder>()
    }
    pub fn is_muted_statement(&self) -> bool {
        self.is::<Statement>()
            || self
                .get::<Cell>()
                .is_some_and(|cell| cell.semicolon.is_some())
    }
    pub fn is_empty_component(&self) -> bool {
        self.is::<Empty>()
    }
    pub fn is_source_cell(&self) -> bool {
        self.cell.is_some() && (self.index == 0 || self.is::<Statement>())
    }
    pub fn is_iteration_range(&self) -> bool {
        self.is::<RangeBoundFrom>() || self.is::<RangeBoundTo>() || self.is::<WhenCondition>()
    }
    pub fn is_except_clause(&self) -> bool {
        matches!(self.get::<TryFallback>(), Some(TryFallback::Except { .. }))
    }
    pub fn is_debug_expr(&self) -> bool {
        self.is::<ExprDebug>() || self.is::<ExprProfile>()
    }
    pub fn is_return_expr(&self) -> bool {
        self.is::<ExprControlReturn>()
    }
    pub fn is_break_expr(&self) -> bool {
        self.is::<ExprControlBreak>()
    }
    pub fn is_continue_expr(&self) -> bool {
        self.is::<ExprControlContinue>()
    }
    pub fn is_control_transfer(&self) -> bool {
        self.is_return_expr() || self.is_break_expr() || self.is_continue_expr()
    }
    pub fn is_prefix_expr(&self) -> bool {
        self.is::<ExprOpPrefix>()
    }
    pub fn is_postfix_expr(&self) -> bool {
        self.is::<ExprOpPostfix>()
    }
    pub fn is_binary_expr(&self) -> bool {
        self.is::<ExprOpBinRegular>() || self.is::<ExprOpBinMember>() || self.is::<ExprOpBinIndex>()
    }
    pub fn is_operator_expression(&self) -> bool {
        self.is_binary_expr()
            || self.is_adjacent_expr()
            || self.is_prefix_expr()
            || self.is_postfix_expr()
    }
    pub fn is_adjacent_expr(&self) -> bool {
        self.is::<ExprOpBinAdj>()
    }
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
    pub fn is_implicit_application(&self) -> bool {
        self.is::<Adj>()
    }
    pub fn is_assignment(&self) -> bool {
        self.is::<ExprAssign>() || self.is::<ExprAssignEvaluated>()
    }
    pub fn is_option_assignment(&self) -> bool {
        self.is::<ExprOption>()
    }
    pub fn is_opening_delimiter(&self) -> bool {
        self.entry()
            .is_some_and(|entry| entry.boundary == Some(true))
    }
    pub fn is_closing_delimiter(&self) -> bool {
        self.entry()
            .is_some_and(|entry| entry.boundary == Some(false))
    }
    pub fn closing_delimiter_width(&self) -> usize {
        if self.is::<AngleBarList>() {
            2
        } else {
            1
        }
    }
    pub fn is_keyword_statement(&self) -> bool {
        self.is::<ExprIf>()
            || self.is::<ExprFor>()
            || self.is::<ExprWhile>()
            || self.is::<ExprNew>()
            || self.is::<ExprTry>()
    }
    pub fn is_keyword_clause(&self) -> bool {
        self.is_iteration_range()
            || self.is::<LoopBody>()
            || self.is::<ThenClause>()
            || self.is::<ElseClause>()
            || self.is_except_clause()
    }
    pub fn is_comma(&self) -> bool {
        self.is::<Token![,]>()
    }
    pub fn is_semicolon(&self) -> bool {
        self.is::<Token![;]>()
    }
    pub fn is_keyword_token(&self) -> bool {
        self.is_token()
            && !self.is_implicit_application()
            && self.text().chars().next().is_some_and(char::is_alphabetic)
            && !self.is_modifier_token()
    }
    pub fn is_modifier_token(&self) -> bool {
        self.is::<Token![global]>()
            || self.is::<Token![local]>()
            || self.is::<Token![symbol]>()
            || self.is::<Token![threadLocal]>()
            || self.is::<Token![threadVariable]>()
    }
    pub fn is_then_or_else_keyword(&self) -> bool {
        self.is::<Token![then]>() || self.is::<Token![else]>()
    }
    pub fn parse_error(&self) -> Option<&'tree m2_syn::NativeParseError> {
        (self.index == 0)
            .then_some(self.cell?)
            .and_then(|cell| self.tree.snapshot().cells()[cell].parse_error())
    }
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
    pub fn is_error(&self) -> bool {
        self.entry().is_some_and(|entry| entry.error)
            || self.cell.is_some() && self.index == 0 && self.has_error()
    }
    pub fn is_missing(&self) -> bool {
        self.is_closing_delimiter() && self.start_byte() == self.end_byte()
    }
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
    pub fn start_position(&self) -> Point {
        self.tree.point_for_byte(self.start_byte())
    }
    pub fn end_position(&self) -> Point {
        self.tree.point_for_byte(self.end_byte())
    }
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
    pub fn symbols(&self) -> impl Iterator<Item = Self> + 'tree {
        self.descendants().filter(Self::is_symbol_like)
    }
    pub fn binding_references(&self) -> impl Iterator<Item = Self> + 'tree {
        self.symbols().filter(|node| !node.is_member_key())
    }
    pub fn is_member_key(&self) -> bool {
        self.is_symbol_like()
            && self
                .parent()
                .and_then(|node| node.property_key())
                .is_some_and(|key| key.id() == self.id())
    }
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
    pub fn final_value_child(&self) -> Option<Self> {
        self.named_children()
            .filter(|node| !node.is_comment())
            .last()
            .filter(|node| !node.is_muted_statement())
    }
    pub fn is_first_collection_element(&self, child: Self) -> bool {
        self.collection_elements()
            .next()
            .is_some_and(|node| node.id() == child.id())
    }
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

    pub fn string_literal_inner_text(&self) -> Option<&'tree str> {
        Some(&self.source_text()[self.string_literal_inner_bytes()?])
    }
    pub fn applied_symbol_name(&self) -> Option<&'tree str> {
        let call = self.get::<ExprOpBinAdj>()?;
        let head = self.related(call.left_operand.as_ref())?;
        head.is::<Symbol>().then(|| head.text())
    }
    pub fn method_installation_head(&self) -> Option<BinaryParts<'tree>> {
        let node = self.get::<ExprAssignInstallMethod>()?;
        Some(BinaryParts {
            left: self.related(node.method.as_ref())?,
            operator: self.related(&node.operator)?,
            right: self.related(node.signature.as_ref())?,
        })
    }
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
    pub fn binary_operator(&self) -> Option<&'tree str> {
        let parts = self.binary_parts()?;
        Some(if self.is_adjacent_expr() {
            "SPACE"
        } else {
            parts.operator.text()
        })
    }
    pub fn has_binary_operator<T: m2_syn::Token>(&self) -> bool {
        self.binary_operator()
            .is_some_and(super::matches_token::<T>)
    }
    pub fn property_key(&self) -> Option<Self> {
        let parts = self.binary_parts()?;
        ((self.is::<ExprOpBinMember>() && parts.right.is_symbol_like())
            || ((self.has_binary_operator::<Token![#]>()
                || self.has_binary_operator::<Token![#?]>())
                && parts.right.is_string_literal()))
        .then_some(parts.right)
    }
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
    pub fn lambda_parameters(&self) -> Option<Self> {
        match self.get::<ExprLambda>()? {
            ExprLambda::Variadic(n) => self.related(&n.argument),
            ExprLambda::List(n) => n.arguments.contents.as_ref().and_then(|n| self.related(n)),
        }
    }
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
    pub fn unary_operand(&self) -> Option<Self> {
        if let Some(n) = self.get::<ExprOpPrefix>() {
            return self.related(n.operand.as_ref());
        }
        if let Some(n) = self.get::<ExprOpPostfix>() {
            return self.related(n.operand.as_ref());
        }
        None
    }
    pub fn new_class(&self) -> Option<Self> {
        self.related(self.get::<ExprNew>()?.class.as_ref())
    }
    pub fn new_parent_clause(&self) -> Option<Self> {
        self.related(self.get::<ExprNew>()?.parent_class.as_ref()?)
    }
    pub fn new_instance_clause(&self) -> Option<Self> {
        self.related(self.get::<ExprNew>()?.instance.as_ref()?)
    }
    pub fn listed_value(&self) -> Option<Self> {
        self.related(self.get::<LoopBody>()?.list_step.as_ref()?.expr.as_ref())
    }
    pub fn ignored_value(&self) -> Option<Self> {
        self.related(self.get::<LoopBody>()?.do_step.as_ref()?.statement.as_ref())
    }
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
    pub fn quoted_symbol(&self) -> Option<Self> {
        self.quote_parts().map(|parts| parts.0)
    }
    #[cfg(test)]
    pub fn quote_modifier(&self) -> Option<Self> {
        self.quote_parts().map(|parts| parts.1)
    }
}

pub fn visit_source_nodes<'a>(
    root: SyntaxNode<'a>,
    _syntax: Option<&TypedSource>,
    mut visitor: impl FnMut(SyntaxNode<'a>),
) {
    for node in root.descendants() {
        visitor(node)
    }
}
pub fn visit_expression_nodes<'a>(
    root: SyntaxNode<'a>,
    _syntax: Option<&Expr>,
    mut visitor: impl FnMut(SyntaxNode<'a>),
) {
    for node in root.descendants() {
        visitor(node)
    }
}
