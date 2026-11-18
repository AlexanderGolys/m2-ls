//! Shared structural operations on generated expressions.

use m2_syn::nodes::*;
use m2_syn::visit::{self, Visit};
use m2_syn::{Delimiter, DelimiterValue, Span, Spanned};

use crate::analysis::Dispatch;
use crate::analysis::QuotedName;
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
    fn assigned_function(&self) -> Option<&ExprFunction>;
}

impl ExpressionSyntax for Expr {
    fn delimiter(&self) -> Option<Delimiter> {
        match self {
            Expr::Array(node) => Some(node.delimiter.delimiter()),
            Expr::List(node) => Some(node.delimiter.delimiter()),
            Expr::Sequence(node) => Some(node.delimiter.delimiter()),
            Expr::AngleBar(node) => Some(node.delimiter.delimiter()),
            Expr::Paren(node) => Some(node.delimiter.delimiter()),
            _ => None,
        }
    }
    fn unparenthesized(&self) -> Option<&Expr> {
        let mut expression = self;
        while let Self::Paren(holder) = expression {
            expression = holder.expr.as_deref()?;
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
            Self::Sequence(sequence) => sequence
                .elems
                .as_ref()
                .map_or_else(Vec::new, PackedArguments::arguments),
            _ => vec![Some(expression)],
        }
    }

    fn collection_elements(&self) -> Option<Vec<Option<&Expr>>> {
        let contents = match self {
            Self::Sequence(node) => &node.elems,
            Self::List(node) => &node.elems,
            Self::Array(node) => &node.elems,
            Self::AngleBar(node) => &node.elems,
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
            Self::Paren(holder) => holder.expr.as_deref().into_iter().collect(),
            _ => self
                .collection_elements()
                .into_iter()
                .flatten()
                .flatten()
                .collect(),
        }
    }

    fn assigned_function(&self) -> Option<&ExprFunction> {
        match self.unparenthesized()? {
            Self::Function(lambda) => Some(lambda),
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
impl PackedArguments for Elements {
    fn arguments(&self) -> Vec<Option<&Expr>> {
        match self {
            Self::Single(expression) => vec![Some(expression)],
            Self::Pack(pack) => pack
                .elems
                .iter()
                .map(|component| match component {
                    Element::Expr(expression) => Some(expression.as_ref()),
                    Element::Empty(_) => None,
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

impl LambdaSyntax for ExprFunction {
    fn body(&self) -> &Expr {
        &self.body
    }
    fn dispatch(&self) -> Dispatch {
        match &self.params {
            FunctionParams::Variadic(_) => Dispatch::Variadic,
            FunctionParams::List(_) => Dispatch::Fixed(self.bound_symbols().len()),
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
    List(&'ast ExprSequence),
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
    Operator(&'ast Expr),
    /// An assignment target that does not match a known shape.
    Expression(&'ast Expr),
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
        let operator = match &self.op {
            AssignOperator::Global(_) => AssignmentOperator::Global,
            AssignOperator::Local(_) => AssignmentOperator::Local,
            AssignOperator::Evaluated(_) => AssignmentOperator::Evaluated,
            AssignOperator::Augmented(_) => AssignmentOperator::Augmented,
        };
        let left = self.left.unparenthesized().unwrap_or(&self.left);
        let target = match (operator, left) {
            (AssignmentOperator::Evaluated, _) => AssignmentTarget::Evaluated(left),
            (AssignmentOperator::Augmented, _) => AssignmentTarget::Augmented(left),
            (_, Expr::Symbol(symbol)) => AssignmentTarget::Symbol(symbol),
            (_, Expr::Sequence(sequence)) => AssignmentTarget::List(sequence),
            (AssignmentOperator::Local, Expr::Binary(binary))
                if matches!(binary.op, BinaryOperator::Space(_)) =>
            {
                AssignmentTarget::Method {
                    method: &binary.left,
                    signature: &binary.right,
                }
            }
            (AssignmentOperator::Local, Expr::New(new)) => AssignmentTarget::New(new),
            (_, Expr::Binary(_) | Expr::Field(_)) => AssignmentTarget::Operator(left),
            _ => AssignmentTarget::Expression(left),
        };
        let installation = matches!(
            target,
            AssignmentTarget::Method { .. }
                | AssignmentTarget::Operator(_)
                | AssignmentTarget::New(_)
        ) && operator == AssignmentOperator::Local;
        let (value, codomain) = match (installation, self.right.as_ref()) {
            (true, Expr::Option(annotation)) => {
                (annotation.value.as_ref(), Some(annotation.key.as_ref()))
            }
            _ => (self.right.as_ref(), None),
        };
        AssignmentParts {
            target,
            operator,
            operator_span: self.op.try_span(),
            value,
            codomain,
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
            Self::Evaluated(expression)
            | Self::Augmented(expression)
            | Self::Expression(expression) => expression,
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

impl BoundSymbols for ExprFunction {
    fn bound_symbols(&self) -> Vec<&Symbol> {
        match &self.params {
            FunctionParams::Variadic(symbol) => vec![symbol],
            FunctionParams::List(list) => match &list.params.contents {
                None => Vec::new(),
                Some(Params::Single(symbol)) => vec![symbol],
                Some(Params::Pack(pack)) => pack.params.iter().collect(),
            },
        }
    }
}

impl BoundSymbols for ExprSequence {
    fn bound_symbols(&self) -> Vec<&Symbol> {
        self.elems
            .as_ref()
            .into_iter()
            .flat_map(PackedArguments::arguments)
            .flatten()
            .flat_map(BoundSymbols::bound_symbols)
            .collect()
    }
}

impl BoundSymbols for Expr {
    fn bound_symbols(&self) -> Vec<&Symbol> {
        match self.unparenthesized() {
            Some(Expr::Symbol(symbol)) => vec![symbol],
            Some(Expr::Sequence(sequence)) => sequence.bound_symbols(),
            _ => Vec::new(),
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
impl ClauseSyntax for OfClause {
    fn clause_value(&self) -> &Expr {
        &self.expr
    }
}
impl ClauseSyntax for FromClause {
    fn clause_value(&self) -> &Expr {
        &self.expr
    }
}

/// Value carried by `return`, `break`, or `continue`.
pub trait ControlSyntax {
    /// The transferred value, absent for a bare keyword.
    fn transferred_value(&self) -> Option<&Expr>;
}

impl ControlSyntax for Expr {
    fn transferred_value(&self) -> Option<&Expr> {
        match self {
            Self::Break(node) => node.expr.as_deref(),
            Self::Continue(node) => node.expr.as_deref(),
            Self::Return(node) => node.expr.as_deref(),
            _ => None,
        }
    }
}

/// The applications within visited syntax, in preorder.
#[derive(Debug, Default)]
pub struct Applications<'ast>(pub Vec<&'ast ExprBinary>);

impl<'ast> Visit<'ast> for Applications<'ast> {
    fn visit_expr_binary(&mut self, call: &'ast ExprBinary) {
        if matches!(call.op, BinaryOperator::Space(_)) {
            self.0.push(call);
        }
        visit::visit_expr_binary(self, call);
    }
}

/// The symbols within visited syntax that read a binding, in preorder: every
/// symbol except a member key, the `key` of `x.key`, which names itself, and a
/// quoted name, which denotes the `Symbol` object whatever the name is bound to.
#[derive(Debug, Default)]
pub struct BindingReferences<'ast>(pub Vec<&'ast Symbol>);

impl<'ast> Visit<'ast> for BindingReferences<'ast> {
    fn visit_symbol(&mut self, symbol: &'ast Symbol) {
        self.0.push(symbol);
    }

    fn visit_expr_quote(&mut self, _quote: &'ast ExprQuote) {}

    fn visit_expr_field(&mut self, access: &'ast ExprField) {
        self.visit_expr(&access.base);
    }
}

/// The names within visited syntax that a quote (`symbol x`, `global x`, …)
/// turns into the `Symbol` object of that name, in preorder.
#[derive(Debug, Default)]
pub struct QuotedNames<'ast>(pub Vec<&'ast Symbol>);

impl<'ast> Visit<'ast> for QuotedNames<'ast> {
    fn visit_expr_quote(&mut self, quote: &'ast ExprQuote) {
        self.0.push(quote.quoted_name());
        visit::visit_expr_quote(self, quote);
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
    fn visit_expr_field(&mut self, access: &'ast ExprField) {
        if self.encloses(&access.member) {
            self.found.member_key = true;
        }
        visit::visit_expr_field(self, access);
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
        let mut tokens: Vec<&dyn Spanned> = vec![&self.for_token];
        match &self.iter {
            Some(ForIter::In(clause)) => tokens.push(&clause.in_token),
            Some(ForIter::Range(range)) => {
                tokens.extend(
                    range
                        .from_clause
                        .as_ref()
                        .map(|clause| &clause.from_token as &dyn Spanned),
                );
                tokens.extend(
                    range
                        .to_clause
                        .as_ref()
                        .map(|clause| &clause.to_token as &dyn Spanned),
                );
            }
            None => {}
        }
        tokens.extend(
            self.when_clause
                .as_ref()
                .map(|clause| &clause.when_token as &dyn Spanned),
        );
        tokens.extend(loop_keywords(&self.body));
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
            self.of_clause
                .as_ref()
                .map(|clause| &clause.of_token as &dyn Spanned),
        );
        tokens.extend(
            self.from_clause
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
            self.then_clause
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

/// The `list` and `do` keywords of a loop body.
fn loop_keywords(body: &LoopBody) -> Vec<&dyn Spanned> {
    let mut tokens: Vec<&dyn Spanned> = Vec::new();
    tokens.extend(
        body.list_clause
            .as_ref()
            .map(|step| &step.list_token as &dyn Spanned),
    );
    tokens.extend(
        body.do_clause
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
    struct Divisions<'ast>(Vec<&'ast ExprBinary>);

    impl<'ast> Visit<'ast> for Divisions<'ast> {
        fn visit_expr_binary(&mut self, operation: &'ast ExprBinary) {
            if operation.op.spelling() == <Token![/]>::SPELLING {
                self.0.push(operation);
            }
            visit::visit_expr_binary(self, operation);
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
                    division.left.unparenthesized(),
                    Some(Expr::Binary(call)) if matches!(call.op, BinaryOperator::Space(_))
                ),
                "division must follow application in {source:?}"
            );
            assert!(
                matches!(
                    text.text_for_syntax(division.right.as_ref()),
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
                    application.right.unparenthesized(),
                    Some(Expr::Binary(operation))
                        if operation.op.spelling() == <Token![/]>::SPELLING
                ),
                "explicit grouping controls precedence in {source:?}"
            );
        }
    }
}
