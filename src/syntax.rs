//! Shared structural operations on generated expressions.

use m2_syn::nodes::*;
use m2_syn::visit::{self, Visit};
use m2_syn::{Delimiter, DelimiterValue, Span, Spanned};

use crate::analysis::Dispatch;
use crate::source::{ByteRange, SourceNavigation};

/// Expression structure relevant to argument passing and nominal class inference.
pub trait ExpressionSyntax {
    /// Delimiters enclosing this collection or holder, if present.
    fn delimiter(&self) -> Option<Delimiter>;
    /// Peels holders that contribute a value; an empty holder has no value.
    fn unparenthesized(&self) -> Option<&Expr>;
    /// Symbol spelling when this exact expression is a symbol, without peeling holders.
    fn symbol_name(&self) -> Option<&str>;
    /// Positional arguments, retaining empty components as `None`.
    fn arguments(&self) -> Vec<Option<&Expr>>;
    /// Immediate elements of a collection, retaining empty slots.
    fn collection_elements(&self) -> Option<Vec<Option<&Expr>>>;
    /// Expressions a collection or holder directly contains, skipping empty slots.
    fn contained_expressions(&self) -> Vec<&Expr>;
    /// The function an assigned value supplies, seen through parentheses and a
    /// codomain annotation: the `x -> …` of `T => x -> …`.
    fn assigned_function(&self) -> Option<&ExprLambda>;
}

impl ExpressionSyntax for Expr {
    fn delimiter(&self) -> Option<Delimiter> {
        match self {
            Expr::Collection(ExprCollection::Array(node)) => Some(node.delimiter.delimiter()),
            Expr::Collection(ExprCollection::List(node)) => Some(node.delimiter.delimiter()),
            Expr::Collection(ExprCollection::Sequence(node)) => Some(node.delimiter.delimiter()),
            Expr::Collection(ExprCollection::AngleBar(node)) => Some(node.delimiter.delimiter()),
            Expr::Holder(node) => Some(node.delimiter.delimiter()),
            _ => None,
        }
    }
    fn unparenthesized(&self) -> Option<&Expr> {
        let mut expression = self;
        while let Self::Holder(holder) = expression {
            expression = holder.value.as_deref()?;
        }
        Some(expression)
    }

    fn symbol_name(&self) -> Option<&str> {
        match self {
            Self::Symbol(symbol) => Some(symbol.text()),
            _ => None,
        }
    }

    fn arguments(&self) -> Vec<Option<&Expr>> {
        let expression = self.unparenthesized().unwrap_or(self);
        match expression {
            Self::Collection(ExprCollection::Sequence(sequence)) => sequence
                .elements
                .as_ref()
                .map_or_else(Vec::new, PackedArguments::arguments),
            _ => vec![Some(expression)],
        }
    }

    fn collection_elements(&self) -> Option<Vec<Option<&Expr>>> {
        let contents = match self {
            Self::Collection(ExprCollection::Sequence(node)) => &node.elements,
            Self::Collection(ExprCollection::List(node)) => &node.elements,
            Self::Collection(ExprCollection::Array(node)) => &node.elements,
            Self::Collection(ExprCollection::AngleBar(node)) => &node.elements,
            _ => return None,
        };
        Some(
            contents
                .as_ref()
                .map_or_else(Vec::new, PackedArguments::arguments),
        )
    }

    fn contained_expressions(&self) -> Vec<&Expr> {
        match self {
            Self::Holder(holder) => holder.value.as_deref().into_iter().collect(),
            _ => self
                .collection_elements()
                .into_iter()
                .flatten()
                .flatten()
                .collect(),
        }
    }

    fn assigned_function(&self) -> Option<&ExprLambda> {
        match self.unparenthesized()? {
            Self::Lambda(lambda) => Some(lambda),
            Self::Option(annotation) => annotation.value.assigned_function(),
            _ => None,
        }
    }
}

/// Value slots in a generated packed argument list.
pub trait PackedArguments {
    /// Returns expressions and explicit empty slots, excluding muted statements.
    fn arguments(&self) -> Vec<Option<&Expr>>;
}
impl PackedArguments for PackedContent {
    fn arguments(&self) -> Vec<Option<&Expr>> {
        match self {
            Self::Expr(expression) => vec![Some(expression)],
            Self::Pack(pack) => pack
                .pack
                .iter()
                .map(|component| match component {
                    Component::Expr(expression) => Some(expression.as_ref()),
                    Component::Empty(_) => None,
                })
                .collect(),
        }
    }
}

/// Parameters and body of a function literal.
pub trait LambdaSyntax {
    /// Expression evaluated when the function is called.
    fn body(&self) -> &Expr;
    /// Call shape implied by the parameter list.
    fn dispatch(&self) -> Dispatch;
}

impl LambdaSyntax for ExprLambda {
    fn body(&self) -> &Expr {
        match self {
            Self::Variadic(lambda) => &lambda.body,
            Self::List(lambda) => &lambda.body,
        }
    }
    fn dispatch(&self) -> Dispatch {
        match self {
            Self::Variadic(_) => Dispatch::Variadic,
            Self::List(_) => Dispatch::Fixed(self.bound_symbols().len()),
        }
    }
}

/// The written operator of an assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssignmentOperator {
    /// `=`, rebinding the visible name or assigning through an operator.
    Global,
    /// `:=`, declaring a local or installing a method.
    Local,
    /// `<-`, assigning to the symbol a value evaluates to.
    Evaluated,
    /// A compound operator such as `+=`.
    Augmented,
}

/// What an assignment writes to.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum AssignmentTarget<'ast> {
    /// `x = …` and `x := …`.
    Symbol(&'ast Symbol),
    /// `(a, b) = …`.
    List(&'ast AssignmentList),
    /// The expression whose value names the target of `<-`.
    Evaluated(&'ast Expr),
    /// The target of a compound assignment such as `x += …`.
    Augmented(&'ast Expr),
    /// `f T := …`: the method function and its signature.
    Method {
        /// The method function receiving the installation.
        method: &'ast Expr,
        /// The domain written after the method function.
        signature: &'ast Expr,
    },
    /// `T + S := …` or `x_i = …`: an operator expression.
    Operator(&'ast ExprOp),
    /// `new T of P from I := …`.
    New(&'ast ExprNew),
}

/// The parts every assignment alternative shares.
#[derive(Debug, Clone)]
pub struct AssignmentParts<'ast> {
    /// What the assignment writes to.
    pub target: AssignmentTarget<'ast>,
    /// The written assignment operator.
    pub operator: AssignmentOperator,
    /// Where the assignment operator is written.
    pub operator_span: Option<Span>,
    /// The assigned value, after any codomain annotation.
    pub value: &'ast Expr,
    /// The codomain annotation of an installation, `T` of `f X := T => …`.
    pub codomain: Option<&'ast Expr>,
}

/// Structural view of the generated assignment alternatives.
pub trait AssignmentSyntax {
    /// Target, operator, and value of this assignment.
    fn parts(&self) -> AssignmentParts<'_>;
}

impl AssignmentSyntax for ExprAssign {
    fn parts(&self) -> AssignmentParts<'_> {
        fn parts<'ast>(
            target: AssignmentTarget<'ast>,
            operator: AssignmentOperator,
            token: &'ast impl Spanned,
            value: &'ast Expr,
        ) -> AssignmentParts<'ast> {
            AssignmentParts {
                target,
                operator,
                operator_span: token.try_span(),
                value,
                codomain: None,
            }
        }
        match self {
            Self::Simple(ExprAssignSimple::Global(node)) => parts(
                AssignmentTarget::Symbol(&node.variable),
                AssignmentOperator::Global,
                &node.operator,
                &node.value,
            ),
            Self::Simple(ExprAssignSimple::Local(node)) => parts(
                AssignmentTarget::Symbol(&node.variable),
                AssignmentOperator::Local,
                &node.operator,
                &node.value,
            ),
            Self::Parallel(ExprAssignParallel::Global(node)) => parts(
                AssignmentTarget::List(&node.argument_list),
                AssignmentOperator::Global,
                &node.operator,
                &node.value,
            ),
            Self::Parallel(ExprAssignParallel::Local(node)) => parts(
                AssignmentTarget::List(&node.argument_list),
                AssignmentOperator::Local,
                &node.operator,
                &node.value,
            ),
            Self::Evaluated(node) => parts(
                AssignmentTarget::Evaluated(&node.evaluated_domain),
                AssignmentOperator::Evaluated,
                &node.operator,
                &node.value,
            ),
            Self::Augmented(node) => parts(
                AssignmentTarget::Augmented(&node.target),
                AssignmentOperator::Augmented,
                &node.operator,
                &node.value,
            ),
            Self::Install(ExprAssignInstall::Method(node)) => AssignmentParts {
                codomain: node
                    .typical_value
                    .as_ref()
                    .map(|annotation| annotation.typical_value.as_ref()),
                ..parts(
                    AssignmentTarget::Method {
                        method: &node.method,
                        signature: &node.signature,
                    },
                    AssignmentOperator::Local,
                    &node.assign_op,
                    &node.function,
                )
            },
            Self::Install(ExprAssignInstall::Op(node)) => AssignmentParts {
                codomain: node
                    .typical_value
                    .as_ref()
                    .map(|annotation| annotation.typical_value.as_ref()),
                ..parts(
                    AssignmentTarget::Operator(&node.domain_expr),
                    AssignmentOperator::Local,
                    &node.assign_op,
                    &node.function,
                )
            },
            Self::Install(ExprAssignInstall::New(node)) => parts(
                AssignmentTarget::New(&node.new_statement),
                AssignmentOperator::Local,
                &node.operator,
                &node.function,
            ),
            Self::Op(node) => parts(
                AssignmentTarget::Operator(&node.argument_expr),
                AssignmentOperator::Global,
                &node.operator,
                &node.value_expr,
            ),
        }
    }
}

impl<'ast> AssignmentTarget<'ast> {
    /// Document bytes the target covers; a method target spans its function
    /// and signature.
    pub fn bytes(self, source: &(impl SourceNavigation + ?Sized)) -> Option<ByteRange> {
        let syntax: &dyn Spanned = match self {
            Self::Method { method, signature } => {
                let start = source.syntax_bytes(method)?.start;
                return Some(start..source.syntax_bytes(signature)?.end);
            }
            Self::Symbol(symbol) => symbol,
            Self::List(list) => list,
            Self::Evaluated(expression) | Self::Augmented(expression) => expression,
            Self::Operator(operation) => operation,
            Self::New(new) => new,
        };
        source.syntax_bytes(syntax)
    }

    /// Symbols the target binds: the symbol itself, or the symbols nested in a
    /// parallel assignment list.
    pub fn bound_symbols(self) -> Vec<&'ast Symbol> {
        match self {
            Self::Symbol(symbol) => vec![symbol],
            Self::List(list) => list.bound_symbols(),
            _ => Vec::new(),
        }
    }
}

/// Syntax that binds a list of symbols at once.
pub trait BoundSymbols {
    /// The symbols bound, in source order.
    fn bound_symbols(&self) -> Vec<&Symbol>;
}

impl BoundSymbols for ExprLambda {
    /// The parameters: `x` of `x -> …`, and `a`, `b` of `(a, b) -> …`.
    fn bound_symbols(&self) -> Vec<&Symbol> {
        match self {
            Self::Variadic(lambda) => vec![&lambda.argument],
            Self::List(lambda) => match &lambda.arguments.contents {
                None => Vec::new(),
                Some(ArgumentListContent::Symbol(symbol)) => vec![symbol],
                Some(ArgumentListContent::Pack(arguments)) => arguments.arguments.iter().collect(),
            },
        }
    }
}

/// The targets of a parallel assignment list.
pub trait AssignmentListSyntax {
    /// The immediate targets, in source order: `a` and `(b, c)` of `(a, (b, c))`.
    fn targets(&self) -> Vec<&AssignmentArgument>;
}

impl AssignmentListSyntax for AssignmentList {
    fn targets(&self) -> Vec<&AssignmentArgument> {
        match &self.contents.contents {
            None => Vec::new(),
            Some(AssignmentListContent::Argument(argument)) => vec![argument],
            Some(AssignmentListContent::Pack(arguments)) => arguments.arguments.iter().collect(),
        }
    }
}

impl BoundSymbols for AssignmentList {
    /// The symbols of a parallel assignment target such as `(a, (b, c))`,
    /// flattened through nested lists.
    fn bound_symbols(&self) -> Vec<&Symbol> {
        self.targets()
            .into_iter()
            .flat_map(BoundSymbols::bound_symbols)
            .collect()
    }
}

impl BoundSymbols for AssignmentArgument {
    /// Empty slots and operator or evaluated targets bind no symbol.
    fn bound_symbols(&self) -> Vec<&Symbol> {
        match self {
            Self::Symbol(symbol) => vec![symbol],
            Self::List(list) => list.bound_symbols(),
            Self::Empty(_) | Self::Op(_) | Self::Evaluated(_) => Vec::new(),
        }
    }
}

/// Expressions a control or clause construct wraps.
pub trait ClauseSyntax {
    /// The wrapped expression: `E` of `then E`, `else E`, `of E`, or `from E`.
    fn clause_value(&self) -> &Expr;
}

impl ClauseSyntax for ThenClause {
    fn clause_value(&self) -> &Expr {
        &self.expr
    }
}
impl ClauseSyntax for ElseClause {
    fn clause_value(&self) -> &Expr {
        &self.expr
    }
}
impl ClauseSyntax for OfClass {
    fn clause_value(&self) -> &Expr {
        &self.parent
    }
}
impl ClauseSyntax for FromInstance {
    fn clause_value(&self) -> &Expr {
        &self.instance
    }
}

/// Value carried by `return`, `break`, or `continue`.
pub trait ControlSyntax {
    /// The transferred value, absent for a bare keyword.
    fn transferred_value(&self) -> Option<&Expr>;
}

impl ControlSyntax for ExprControl {
    fn transferred_value(&self) -> Option<&Expr> {
        match self {
            Self::Break(node) => node.value.as_deref(),
            Self::Continue(node) => node.value.as_deref(),
            Self::Return(node) => node.value.as_deref(),
        }
    }
}

/// The applications within visited syntax, in preorder.
#[derive(Debug, Default)]
pub struct Applications<'ast>(pub Vec<&'ast ExprOpBinAdj>);

impl<'ast> Visit<'ast> for Applications<'ast> {
    fn visit_expr_op_bin_adj(&mut self, call: &'ast ExprOpBinAdj) {
        self.0.push(call);
        visit::visit_expr_op_bin_adj(self, call);
    }
}

/// The symbols within visited syntax that read a binding, in preorder: every
/// symbol except a member key, the `key` of `x.key`, which names itself.
#[derive(Debug, Default)]
pub struct BindingReferences<'ast>(pub Vec<&'ast Symbol>);

impl<'ast> Visit<'ast> for BindingReferences<'ast> {
    fn visit_symbol(&mut self, symbol: &'ast Symbol) {
        self.0.push(symbol);
    }

    fn visit_expr_op_bin_member(&mut self, access: &'ast ExprOpBinMember) {
        let (receiver, member) = match access {
            ExprOpBinMember::Dot(access) => (&access.receiver, &access.member),
            ExprOpBinMember::Optional(access) => (&access.receiver, &access.member),
        };
        self.visit_expr(receiver);
        if !matches!(member.as_ref(), Expr::Symbol(_)) {
            self.visit_expr(member);
        }
    }
}

/// The syntax enclosing a document byte: the expressions around it and the
/// symbol written there.
///
/// An expression encloses the bytes from its start up to, but excluding, its
/// end, so of two adjacent expressions only the one starting at the byte
/// encloses it. The query descends only into expressions enclosing the byte,
/// so it costs the depth of the syntax at the cursor, not its size.
#[derive(Debug, Default)]
pub struct CursorSyntax<'ast> {
    /// The expressions enclosing the byte, outermost first: each one directly
    /// contains the next.
    pub expressions: Vec<&'ast Expr>,
    /// The symbol written at the byte, in an expression or not: also the `x`
    /// of `x -> …` and of `x = …`.
    pub symbol: Option<&'ast Symbol>,
    /// Whether the symbol is a member key rather than a binding reference.
    pub member_key: bool,
}

impl<'ast> CursorSyntax<'ast> {
    /// The syntax of `cells` enclosing `byte`.
    pub fn at(
        cells: impl IntoIterator<Item = &'ast Cell>,
        byte: usize,
        source: &(impl SourceNavigation + ?Sized),
    ) -> Self {
        let mut visitor = CursorVisitor {
            source,
            byte,
            found: Self::default(),
        };
        for cell in cells {
            visitor.visit_cell(cell);
        }
        visitor.found
    }

    /// The innermost enclosing expression.
    pub fn innermost(&self) -> Option<&'ast Expr> {
        self.expressions.last().copied()
    }
}

/// Visitor collecting [`CursorSyntax`].
struct CursorVisitor<'ast, 'source, Source: ?Sized> {
    /// Source coordinates of the visited revision.
    source: &'source Source,
    /// The document byte under the cursor.
    byte: usize,
    /// The syntax found so far.
    found: CursorSyntax<'ast>,
}

impl<Source: SourceNavigation + ?Sized> CursorVisitor<'_, '_, Source> {
    /// Whether `syntax` encloses the cursor byte.
    fn encloses(&self, syntax: &(impl Spanned + ?Sized)) -> bool {
        self.source
            .syntax_bytes(syntax)
            .is_some_and(|bytes| bytes.contains(&self.byte))
    }
}

impl<'ast, Source: SourceNavigation + ?Sized> Visit<'ast> for CursorVisitor<'ast, '_, Source> {
    fn visit_expr_op_bin_member(&mut self, access: &'ast ExprOpBinMember) {
        let member = match access {
            ExprOpBinMember::Dot(access) => &access.member,
            ExprOpBinMember::Optional(access) => &access.member,
        };
        if let Expr::Symbol(symbol) = member.as_ref() {
            if self.encloses(symbol) {
                self.found.member_key = true;
            }
        }
        visit::visit_expr_op_bin_member(self, access);
    }
    fn visit_expr(&mut self, expression: &'ast Expr) {
        if self.encloses(expression) {
            self.found.expressions.push(expression);
            visit::visit_expr(self, expression);
        }
    }

    fn visit_symbol(&mut self, symbol: &'ast Symbol) {
        if self.encloses(symbol) {
            self.found.symbol = Some(symbol);
        }
    }
}

/// The keywords spelling a compound statement.
pub trait StatementKeywords {
    /// Where the statement's own keywords are written, in source order: `if`,
    /// `then`, `else` of a conditional, but none of a statement nested in it.
    /// Empty for an expression that is no compound statement.
    fn keywords(&self) -> Vec<Span>;
}

impl StatementKeywords for Expr {
    fn keywords(&self) -> Vec<Span> {
        match self {
            Self::If(node) => node.keywords(),
            Self::For(node) => node.keywords(),
            Self::While(node) => node.keywords(),
            Self::New(node) => node.keywords(),
            Self::Try(node) => node.keywords(),
            _ => Vec::new(),
        }
    }
}

impl StatementKeywords for ExprIf {
    fn keywords(&self) -> Vec<Span> {
        let mut tokens: Vec<&dyn Spanned> = vec![&self.if_token, &self.then_clause.then_token];
        tokens.extend(
            self.else_clause
                .as_ref()
                .map(|clause| &clause.else_token as &dyn Spanned),
        );
        spans(tokens)
    }
}

impl StatementKeywords for ExprFor {
    fn keywords(&self) -> Vec<Span> {
        let (body, mut tokens): (_, Vec<&dyn Spanned>) = match self {
            Self::Bare(node) => {
                let mut tokens: Vec<&dyn Spanned> = vec![&node.for_token];
                tokens.extend(when_keyword(&node.condition));
                (&node.body, tokens)
            }
            Self::Range(node) => {
                let mut tokens: Vec<&dyn Spanned> = vec![&node.for_token];
                tokens.extend(
                    node.lower_bound
                        .as_ref()
                        .map(|bound| &bound.from_token as &dyn Spanned),
                );
                tokens.extend(
                    node.upper_bound
                        .as_ref()
                        .map(|bound| &bound.to_token as &dyn Spanned),
                );
                tokens.extend(when_keyword(&node.condition));
                (&node.body, tokens)
            }
            Self::In(node) => {
                let mut tokens: Vec<&dyn Spanned> = vec![&node.for_token, &node.in_token];
                tokens.extend(when_keyword(&node.condition));
                (&node.body, tokens)
            }
        };
        tokens.extend(loop_keywords(body));
        spans(tokens)
    }
}

impl StatementKeywords for ExprWhile {
    fn keywords(&self) -> Vec<Span> {
        let mut tokens: Vec<&dyn Spanned> = vec![&self.while_token];
        tokens.extend(loop_keywords(&self.body));
        spans(tokens)
    }
}

impl StatementKeywords for ExprNew {
    fn keywords(&self) -> Vec<Span> {
        let mut tokens: Vec<&dyn Spanned> = vec![&self.new_token];
        tokens.extend(
            self.parent_class
                .as_ref()
                .map(|clause| &clause.of_token as &dyn Spanned),
        );
        tokens.extend(
            self.instance
                .as_ref()
                .map(|clause| &clause.from_token as &dyn Spanned),
        );
        spans(tokens)
    }
}

impl StatementKeywords for ExprTry {
    fn keywords(&self) -> Vec<Span> {
        let mut tokens: Vec<&dyn Spanned> = vec![&self.try_token];
        tokens.extend(
            self.on_success
                .as_ref()
                .map(|clause| &clause.then_token as &dyn Spanned),
        );
        match &self.fallback {
            Some(TryFallback::Else(clause)) => tokens.push(&clause.else_token),
            Some(TryFallback::Except(clause)) => {
                tokens.extend([&clause.except_token as &dyn Spanned, &clause.do_token]);
            }
            None => {}
        }
        spans(tokens)
    }
}

/// The spans of the written tokens among `tokens`.
fn spans(tokens: Vec<&dyn Spanned>) -> Vec<Span> {
    tokens.into_iter().filter_map(Spanned::try_span).collect()
}

/// The `when` keyword of a loop's filter.
fn when_keyword(condition: &Option<WhenCondition>) -> Option<&dyn Spanned> {
    condition
        .as_ref()
        .map(|condition| &condition.when_token as &dyn Spanned)
}

/// The `list` and `do` keywords of a loop body.
fn loop_keywords(body: &LoopBody) -> Vec<&dyn Spanned> {
    let mut tokens: Vec<&dyn Spanned> = Vec::new();
    tokens.extend(
        body.list_step
            .as_ref()
            .map(|step| &step.list_token as &dyn Spanned),
    );
    tokens.extend(
        body.do_step
            .as_ref()
            .map(|step| &step.do_token as &dyn Spanned),
    );
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_metadata::SyntaxSnapshot;
    use crate::source::DocumentSource;
    use m2_syn::Token;

    /// The divisions within visited syntax.
    #[derive(Default)]
    struct Divisions<'ast>(Vec<&'ast ExprOpBinRegular>);

    impl<'ast> Visit<'ast> for Divisions<'ast> {
        fn visit_expr_op_bin_regular(&mut self, operation: &'ast ExprOpBinRegular) {
            if operation.operator.spelling() == <Token![/]>::SPELLING {
                self.0.push(operation);
            }
            visit::visit_expr_op_bin_regular(self, operation);
        }
    }

    /// A parsed fixture with the source mapping its typed spans.
    fn parsed(source: &str) -> (SyntaxSnapshot, DocumentSource) {
        let syntax = SyntaxSnapshot::parse(source, None);
        assert!(
            syntax
                .cells()
                .iter()
                .all(|cell| cell.parse_error().is_none() && cell.lexer_diagnostics().is_empty()),
            "invalid fixture {source:?}"
        );
        let mut text = DocumentSource::new(source.to_string());
        text.register_syntax(&syntax);
        (syntax, text)
    }

    #[test]
    fn delimited_applications_bind_before_division_for_every_head() {
        for source in [
            "R[x]/I",
            "R [x] / I",
            "f[x]/g",
            "f [x] / g",
            "f<|x|>/g",
            "f <|x|> / g",
            "f{x}/g",
            "f(x)/g",
            "(f)[x]/g",
            "(f<|x|>)/g",
        ] {
            let (syntax, text) = parsed(source);
            let mut divisions = Divisions::default();
            syntax.visit(&mut divisions);
            let division = divisions
                .0
                .first()
                .unwrap_or_else(|| panic!("missing division in {source:?}"));
            assert!(
                matches!(
                    division.left_operand.unparenthesized(),
                    Some(Expr::Op(ExprOp::Bin(ExprOpBin::Adj(_))))
                ),
                "division must follow application in {source:?}"
            );
            assert!(
                matches!(
                    text.text_for_syntax(division.right_operand.as_ref()),
                    Some("I" | "g")
                ),
                "division must keep its RHS in {source:?}"
            );
        }
    }

    #[test]
    fn explicit_argument_grouping_keeps_division_inside_the_call() {
        for source in ["f([x]/g)", "f(<|x|>/g)", "R([x]/I)"] {
            let (syntax, text) = parsed(source);
            let mut applications = Applications::default();
            syntax.visit(&mut applications);
            let application = applications
                .0
                .first()
                .expect("fixture contains application");
            assert_eq!(
                text.text_for_syntax(*application),
                Some(source),
                "the call must enclose division in {source:?}"
            );
            assert!(
                matches!(
                    application.right_operand.unparenthesized(),
                    Some(Expr::Op(ExprOp::Bin(ExprOpBin::Regular(operation))))
                        if operation.operator.spelling() == <Token![/]>::SPELLING
                ),
                "explicit grouping controls precedence in {source:?}"
            );
        }
    }
}
