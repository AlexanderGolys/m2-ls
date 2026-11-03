//! Syntax-only collection of scopes and bindings, performed before semantic
//! enrichment.
//!
//! The first pass walks the typed AST once and produces two things:
//!
//! - a [`ScopeTree`]: every region of source that changes where a name is
//!   bound, with its parent;
//! - a stream of [`BindingFact`]s: every place a symbol is bound, with the
//!   scope it lands in and whether it may be visible outside the file.
//!
//! Nothing here looks at types, values, or other files. The semantic pass in
//! the parent module consumes the facts in source order and links them into
//! bindings, binding states, and references.
//!
//! # Macaulay2 scoping in brief
//!
//! Macaulay2 has two binding operators with different reach:
//!
//! - `x := v` (and function parameters, and `for` loop variables) *declares* a
//!   new local in the innermost **dictionary**. Only functions and `for` loops
//!   own dictionaries; `if`, `while`, and `try` do not, so a local declared in
//!   an `if` branch is still visible after the `if`.
//! - `x = v` *assigns*: it rebinds whichever binding of `x` is visible, and
//!   creates a global when none is. Its effect can therefore always reach the
//!   top level, which is why every assignment is a potential export.
//!
//! # Scope kinds
//!
//! The walker opens one scope per construct below. Each kind decides which of
//! the walker's two binding targets it moves (see [`ScopeKind`]):
//!
//! | construct                      | kind          | declares here | assigns here |
//! |--------------------------------|---------------|:-------------:|:------------:|
//! | `x -> body`                    | `Function`    | yes           | yes          |
//! | `for x … do body`              | `ForLoop`     | yes           | no           |
//! | `if` / `try` clause            | `Branch`      | no            | yes          |
//! | `while` / `for` clause         | `LoopClause`  | no            | no           |
//!
//! Branch clauses collect assignments because an assignment inside one only
//! happens on that path; the semantic pass resolves it outward to the binding
//! it actually updates. Loop clauses keep assignments in the enclosing scope,
//! since the loop as a whole either runs them or not.
//!
//! Scopes are pushed in pre-order, so a scope's index is always greater than
//! its parent's, and the innermost scope containing a position is the last one
//! that contains it.

use m2_syn::nodes::{
    Cell, Expr, ExprAssign as Assignment, ExprAssignParallel as ParallelAssignment,
    ExprAssignSimple as SimpleBinding, ExprFor as ForLoop, ExprIf as IfStatement,
    ExprLambda as LambdaExpression, ExprOpBinAdj as AdjacentExpr, ExprTry as TryStatement,
    ExprWhile as WhileLoop, LoopBody, Symbol, TryFallback, WhenCondition,
};
use m2_syn::visit::{self, Visit};
use m2_syn::Spanned;
use tower_lsp::lsp_types::{Position, Range as TextRange};

use crate::macaulay2::Macaulay2Conventions;
use crate::meta::BindingRole;
use crate::node_metadata::SyntaxSnapshot;
use crate::object_registry::ObjectName;
use crate::source::{ByteRange, SourceNavigation, SyntaxSource};
use crate::string_literal::LiteralSource;
use crate::util::TextRangeExt;

use crate::syntax::{BoundSymbols, ExpressionSyntax};

/// Index of a scope inside its [`ScopeTree`].
pub type ScopeId = usize;

/// The file-wide scope every [`ScopeTree`] starts with; bindings in it are
/// globals.
const ROOT_SCOPE: ScopeId = 0;

/// How a binding operator affects its target name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingEffect {
    /// Introduces a new local in the innermost dictionary (`:=`, parameters,
    /// `for` variables).
    Declare,
    /// Rebinds the visible binding of the name, or creates a global (`=`).
    Assign,
}

/// One place where a symbol is bound.
#[derive(Debug, Clone)]
pub struct BindingFact {
    /// The bound name.
    pub name: ObjectName,
    /// Range of the symbol being bound.
    pub target: TextRange,
    /// Range of the bound value, when the binding has one (parameters and
    /// loop variables do not).
    pub value: Option<TextRange>,
    /// Range of the enclosing top-level cell, shown as the binding's
    /// definition site.
    pub definition: TextRange,
    /// Whether the binding is an ordinary variable or a parameter.
    pub role: BindingRole,
    /// Whether the binding declares a new local or assigns an existing name.
    pub effect: BindingEffect,
    /// The scope that owns the binding: the declaration scope for
    /// [`BindingEffect::Declare`], the assignment scope for
    /// [`BindingEffect::Assign`].
    pub scope: ScopeId,
    /// Whether the binding may be visible outside the file: every global
    /// declaration and every assignment.
    pub potential_export: bool,
}

impl BindingFact {
    /// The global a dynamically created binding makes, such as a ring
    /// generator bound when its ring is constructed, anchored on the typed
    /// `target` naming it.
    ///
    /// The definition is the enclosing top-level cell when that cell yields a
    /// value, else the target itself.
    pub fn dynamic_global(
        name: ObjectName,
        target: &(impl Spanned + ?Sized),
        source: &(impl SyntaxSource + ?Sized),
    ) -> Option<Self> {
        let bytes = source.syntax_bytes(target)?;
        let cell = source
            .syntax()
            .cells_intersecting(bytes.start..bytes.start)
            .iter()
            .find(|cell| cell.document_range().contains(&bytes.start));
        let definition = match cell.map(|cell| (cell, cell.syntax())) {
            Some((_, Some(syntax))) if syntax.semicolon.is_none() => {
                source.range_for_syntax(syntax)
            }
            Some((cell, None)) => Some(source.range_for_bytes(cell.document_range())),
            _ => None,
        };
        let target = source.range_for_bytes(bytes);
        Some(Self {
            name,
            target,
            value: None,
            definition: definition.unwrap_or(target),
            role: BindingRole::Ordinary,
            effect: BindingEffect::Assign,
            scope: ROOT_SCOPE,
            potential_export: true,
        })
    }

    /// Where `newPackage "P"` anchors the binding of `P`: the text between
    /// the quotes of its name string.
    pub(super) fn package_name_bytes(
        call: &AdjacentExpr,
        source: &(impl SourceNavigation + ?Sized),
    ) -> Option<ByteRange> {
        if call.left_operand.symbol_name() != Some(Macaulay2Conventions::PACKAGE_CONSTRUCTOR) {
            return None;
        }
        let argument = call.right_operand.arguments().into_iter().next()??;
        argument
            .literal_content_bytes(source)
            .filter(|content| !content.is_empty())
    }

    /// The global binding `newPackage "P"` makes of `P`, anchored on the text
    /// inside the name string.
    fn package_declaration(
        call: &AdjacentExpr,
        definition: TextRange,
        source: &(impl SourceNavigation + ?Sized),
    ) -> Option<Self> {
        let content = Self::package_name_bytes(call, source)?;
        Some(Self {
            name: ObjectName::new(source.text().get(content.clone())?),
            target: source.range_for_bytes(content),
            value: source.range_for_syntax(call),
            definition,
            role: BindingRole::Ordinary,
            effect: BindingEffect::Assign,
            scope: ROOT_SCOPE,
            potential_export: true,
        })
    }
}

/// Runs the first pass over `syntax`.
///
/// Every collected [`BindingFact`] is handed to `collect` in source order,
/// together with the complete scope tree, so the consumer may look up scopes
/// that open after the fact (e.g. a later assignment's owner). The tree is
/// returned for the semantic pass to keep.
pub fn walk(
    syntax: &SyntaxSnapshot,
    source: &(impl SourceNavigation + ?Sized),
    mut collect: impl FnMut(BindingFact, &ScopeTree),
) -> ScopeTree {
    let mut walker = TypedWalker::new(source);
    syntax.visit(&mut walker);
    for fact in walker.bindings {
        collect(fact, &walker.scopes);
    }
    walker.scopes
}

/// One region of source that changes where names are bound.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Scope {
    /// Source extent used to locate this item.
    range: TextRange,
    /// `None` only for [`ROOT_SCOPE`].
    parent: Option<ScopeId>,
}

/// Every scope of a file, indexed by [`ScopeId`] in pre-order.
#[derive(Debug)]
pub(super) struct ScopeTree {
    /// Lexical scopes collected in traversal order.
    scopes: Vec<Scope>,
}

impl Default for ScopeTree {
    /// A tree holding only the file-wide root scope.
    fn default() -> Self {
        Self {
            scopes: vec![Scope {
                range: TextRange::new(pos!(), pos_max!()),
                parent: None,
            }],
        }
    }
}

impl ScopeTree {
    /// Appends a lexical scope and returns its index in traversal order.
    fn push(&mut self, range: TextRange, parent: ScopeId) -> ScopeId {
        self.scopes.push(Scope {
            range,
            parent: Some(parent),
        });
        self.scopes.len() - 1
    }

    /// The outermost scope spanning exactly `range`.
    pub(super) fn with_range(&self, range: TextRange) -> Option<ScopeId> {
        self.scopes.iter().position(|scope| scope.range == range)
    }

    /// The innermost scope containing `position`. Scopes nest and are stored
    /// in pre-order, so that is the last scope containing it.
    pub(super) fn at(&self, position: Position) -> Option<ScopeId> {
        self.scopes
            .iter()
            .rposition(|scope| scope.range.contains_position(position))
    }

    /// The number of scopes, including the root.
    pub(super) fn len(&self) -> usize {
        self.scopes.len()
    }

    /// The enclosing scope; `None` for the root and for unknown ids.
    pub(super) fn parent(&self, scope: ScopeId) -> Option<ScopeId> {
        self.scopes.get(scope)?.parent
    }
}

/// What a construct does to name binding; see the table in the module docs.
enum ScopeKind {
    /// A lambda: owns a dictionary and collects the assignments in its body.
    Function,
    /// A whole `for` loop: owns the dictionary of its loop variable.
    ForLoop,
    /// One clause of an `if` or `try`.
    Branch,
    /// The condition, domain, or body of a `while` or `for` loop.
    LoopClause,
}

impl ScopeKind {
    /// Whether `:=` inside this scope declares into it.
    fn owns_dictionary(&self) -> bool {
        matches!(self, Self::Function | Self::ForLoop)
    }

    /// Whether `=` inside this scope is recorded against it.
    fn owns_assignments(&self) -> bool {
        matches!(self, Self::Function | Self::Branch)
    }
}

/// The walker's position in the scope tree.
#[derive(Clone, Copy)]
struct TraversalContext {
    /// The innermost scope of any kind, which parents the next scope opened.
    scope: ScopeId,
    /// Where `=` records its binding: the innermost function or branch.
    assignment_scope: ScopeId,
    /// Where `:=`, parameters, and loop variables declare: the innermost
    /// function or `for` loop.
    declaration_scope: ScopeId,
}

impl TraversalContext {
    /// Initial traversal context with every binding target set to the document scope.
    const ROOT: Self = Self {
        scope: ROOT_SCOPE,
        assignment_scope: ROOT_SCOPE,
        declaration_scope: ROOT_SCOPE,
    };

    /// The context inside a newly opened `scope` of the given `kind`.
    fn enter(self, scope: ScopeId, kind: &ScopeKind) -> Self {
        Self {
            scope,
            assignment_scope: if kind.owns_assignments() {
                scope
            } else {
                self.assignment_scope
            },
            declaration_scope: if kind.owns_dictionary() {
                scope
            } else {
                self.declaration_scope
            },
        }
    }
}

/// The AST visitor behind [`walk`].
struct TypedWalker<'source, Source: ?Sized> {
    /// Source data supplying text and coordinate conversions.
    source: &'source Source,
    /// Lexical scopes collected in traversal order.
    scopes: ScopeTree,
    /// Bindings collected for the containing analysis or scope.
    bindings: Vec<BindingFact>,
    /// Inherited semantic context for the current traversal.
    context: TraversalContext,
    /// Range of the top-level cell being walked.
    definition: TextRange,
}

impl<'source, Source: SourceNavigation + ?Sized> TypedWalker<'source, Source> {
    /// Starts syntax-only scope collection at the document root with an empty binding-fact stream.
    fn new(source: &'source Source) -> Self {
        Self {
            source,
            scopes: ScopeTree::default(),
            bindings: Vec::new(),
            context: TraversalContext::ROOT,
            definition: source.full_range(),
        }
    }

    /// The source range of typed syntax; `None` when it has no source bytes.
    fn range_of(&self, syntax: &(impl Spanned + ?Sized)) -> Option<TextRange> {
        self.source.range_for_syntax(syntax)
    }

    /// Runs `visit` with `syntax` as the definition site of the bindings found.
    fn with_definition(&mut self, syntax: &(impl Spanned + ?Sized), visit: impl FnOnce(&mut Self)) {
        let inherited = self.definition;
        self.definition = self.range_of(syntax).unwrap_or(inherited);
        visit(self);
        self.definition = inherited;
    }

    /// Runs `visit` inside a new scope of `kind` covering `range`. Without a
    /// range, `visit` runs in the current scope.
    fn enter_scope(
        &mut self,
        range: Option<TextRange>,
        kind: ScopeKind,
        visit: impl FnOnce(&mut Self),
    ) {
        let Some(range) = range else {
            return visit(self);
        };
        let inherited = self.context;
        let scope = self.scopes.push(range, inherited.scope);
        self.context = inherited.enter(scope, &kind);
        visit(self);
        self.context = inherited;
    }

    /// Visits `expr` inside a scope of `kind` that covers the whole `clause`,
    /// keyword included.
    fn visit_clause(&mut self, clause: &(impl Spanned + ?Sized), expr: &Expr, kind: ScopeKind) {
        self.enter_scope(self.range_of(clause), kind, |walker| {
            walker.visit_expr(expr)
        });
    }

    /// Records one [`BindingFact`] per symbol, all sharing `value`.
    fn bind<'ast>(
        &mut self,
        symbols: impl IntoIterator<Item = &'ast Symbol>,
        value: Option<&Expr>,
        role: BindingRole,
        effect: BindingEffect,
    ) {
        let scope = match effect {
            BindingEffect::Declare => self.context.declaration_scope,
            BindingEffect::Assign => self.context.assignment_scope,
        };
        let value = value.and_then(|value| self.range_of(value));
        for symbol in symbols {
            let Some(target) = self.range_of(symbol) else {
                continue;
            };
            self.bindings.push(BindingFact {
                name: ObjectName::new(symbol.text()),
                target,
                value,
                definition: self.definition,
                role,
                effect,
                scope,
                potential_export: effect == BindingEffect::Assign || scope == ROOT_SCOPE,
            });
        }
    }

    /// Walks a `for` loop of any form: the loop variable is declared in the
    /// loop's own scope, and the domain (visited by `visit_domain`), the
    /// `when` condition, and the body each get a loop-clause scope.
    fn visit_for_loop(
        &mut self,
        node: &ForLoop,
        variable: &Symbol,
        condition: Option<&WhenCondition>,
        body: &LoopBody,
        visit_domain: impl FnOnce(&mut Self),
    ) {
        self.enter_scope(self.range_of(node), ScopeKind::ForLoop, |walker| {
            walker.bind(
                [variable],
                None,
                BindingRole::Parameter,
                BindingEffect::Declare,
            );
            visit_domain(walker);
            if let Some(condition) = condition {
                walker.enter_scope(
                    walker.range_of(condition),
                    ScopeKind::LoopClause,
                    |walker| walker.visit_when_condition(condition),
                );
            }
            walker.enter_scope(walker.range_of(body), ScopeKind::LoopClause, |walker| {
                visit::visit_loop_body(walker, body)
            });
        });
    }
}

impl<'ast, Source: SourceNavigation + ?Sized> Visit<'ast> for TypedWalker<'_, Source> {
    fn visit_cell(&mut self, node: &'ast Cell) {
        self.with_definition(node, |walker| visit::visit_cell(walker, node));
    }

    fn visit_expr_assign(&mut self, node: &'ast Assignment) {
        let ordinary = BindingRole::Ordinary;
        match node {
            Assignment::Simple(SimpleBinding::Global(binding)) => self.bind(
                [&binding.variable],
                Some(&binding.value),
                ordinary,
                BindingEffect::Assign,
            ),
            Assignment::Simple(SimpleBinding::Local(binding)) => self.bind(
                [&binding.variable],
                Some(&binding.value),
                ordinary,
                BindingEffect::Declare,
            ),
            Assignment::Parallel(ParallelAssignment::Global(assignment)) => self.bind(
                assignment.argument_list.bound_symbols(),
                Some(&assignment.value),
                ordinary,
                BindingEffect::Assign,
            ),
            Assignment::Parallel(ParallelAssignment::Local(assignment)) => self.bind(
                assignment.argument_list.bound_symbols(),
                Some(&assignment.value),
                ordinary,
                BindingEffect::Declare,
            ),
            // These update an existing object or install a method; none binds
            // a plain symbol.
            Assignment::Evaluated(_)
            | Assignment::Augmented(_)
            | Assignment::Install(_)
            | Assignment::Op(_) => {}
        }
        visit::visit_expr_assign(self, node);
    }

    /// Juxtaposition covers calls such as `newPackage "P"`, which bind `P`.
    fn visit_expr_op_bin_adj(&mut self, node: &'ast AdjacentExpr) {
        if let Some(fact) = BindingFact::package_declaration(node, self.definition, self.source) {
            self.bindings.push(fact);
        }
        visit::visit_expr_op_bin_adj(self, node);
    }

    fn visit_expr_lambda(&mut self, node: &'ast LambdaExpression) {
        let parameters = node.bound_symbols();
        self.enter_scope(self.range_of(node), ScopeKind::Function, |walker| {
            walker.bind(
                parameters,
                None,
                BindingRole::Parameter,
                BindingEffect::Declare,
            );
            visit::visit_expr_lambda(walker, node);
        });
    }

    fn visit_expr_if(&mut self, node: &'ast IfStatement) {
        self.visit_clause(&node.condition, &node.condition, ScopeKind::Branch);
        self.visit_clause(&node.then_clause, &node.then_clause.expr, ScopeKind::Branch);
        if let Some(clause) = &node.else_clause {
            self.visit_clause(clause, &clause.expr, ScopeKind::Branch);
        }
    }

    fn visit_expr_for(&mut self, node: &'ast ForLoop) {
        match node {
            ForLoop::Bare(for_loop) => self.visit_for_loop(
                node,
                &for_loop.variable,
                for_loop.condition.as_ref(),
                &for_loop.body,
                |_| {},
            ),
            // `from a to b`: both bounds share one clause spanning from the
            // first present bound to the last.
            ForLoop::Range(for_loop) => self.visit_for_loop(
                node,
                &for_loop.variable,
                for_loop.condition.as_ref(),
                &for_loop.body,
                |walker| {
                    let lower = for_loop
                        .lower_bound
                        .as_ref()
                        .and_then(|bound| walker.range_of(bound));
                    let upper = for_loop
                        .upper_bound
                        .as_ref()
                        .and_then(|bound| walker.range_of(bound));
                    let domain = lower
                        .into_iter()
                        .chain(upper)
                        .reduce(|lower, upper| TextRange::new(lower.start, upper.end));
                    walker.enter_scope(domain, ScopeKind::LoopClause, |walker| {
                        if let Some(bound) = &for_loop.lower_bound {
                            walker.visit_range_bound_from(bound);
                        }
                        if let Some(bound) = &for_loop.upper_bound {
                            walker.visit_range_bound_to(bound);
                        }
                    });
                },
            ),
            ForLoop::In(for_loop) => self.visit_for_loop(
                node,
                &for_loop.variable,
                for_loop.condition.as_ref(),
                &for_loop.body,
                |walker| {
                    walker.visit_clause(
                        &for_loop.collection,
                        &for_loop.collection,
                        ScopeKind::LoopClause,
                    )
                },
            ),
        }
    }

    fn visit_expr_while(&mut self, node: &'ast WhileLoop) {
        self.visit_clause(&node.condition, &node.condition, ScopeKind::LoopClause);
        self.enter_scope(self.range_of(&node.body), ScopeKind::LoopClause, |walker| {
            visit::visit_loop_body(walker, &node.body)
        });
    }

    fn visit_expr_try(&mut self, node: &'ast TryStatement) {
        self.visit_clause(&node.value, &node.value, ScopeKind::Branch);
        if let Some(clause) = &node.on_success {
            self.visit_clause(clause, &clause.expr, ScopeKind::Branch);
        }
        match &node.fallback {
            Some(TryFallback::Except(clause)) => {
                self.visit_clause(clause, &clause.value, ScopeKind::Branch)
            }
            Some(TryFallback::Else(clause)) => {
                self.visit_clause(clause, &clause.expr, ScopeKind::Branch)
            }
            None => {}
        }
    }
}
