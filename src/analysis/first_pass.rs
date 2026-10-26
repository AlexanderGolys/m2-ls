//! Syntax-only collection performed before semantic enrichment.

use crate::node_metadata::TypedSource;
use std::cmp::Ordering;

use m2_syn::nodes::{
    ArgumentListContent, AssignmentArgument as AssignmentPackComponent,
    AssignmentList as AssignmentPack, Expr, ExprAssign as Assignment,
    ExprAssignParallel as ParallelAssignment, ExprAssignSimple as SimpleBinding,
    ExprFor as ForLoop, ExprIf as IfStatement, ExprLambda as LambdaExpression,
    ExprOpBinAdj as AdjacentExpr, ExprTry as TryStatement, ExprWhile as WhileLoop, LoopBody,
    Symbol, TryFallback, WhenCondition,
};
use m2_syn::visit::{self, Visit};
use m2_syn::Spanned;
use tower_lsp::lsp_types::{Position, Range as TextRange};

use crate::meta::BindingRole;
use crate::node_metadata::SyntaxNode;
use crate::object_registry::ObjectName;
use crate::package_index::declared_package_name;
use crate::source::SourceNavigation;
use crate::util::TextRangeExt;

use super::{is_value_cell, source_cell, Dispatch};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingEffect {
    Declare,
    Assign,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Scope {
    range: TextRange,
    parent: Option<usize>,
    assignments_may_escape: bool,
    function_dispatch: Option<Dispatch>,
}

#[derive(Debug)]
pub(super) struct ScopeTree {
    scopes: Vec<Scope>,
}

impl Default for ScopeTree {
    fn default() -> Self {
        Self {
            scopes: vec![Scope {
                range: TextRange::new(pos!(), pos_max!()),
                parent: None,
                assignments_may_escape: false,
                function_dispatch: None,
            }],
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct TraversalContext {
    pub scope: usize,
    pub assignment_scope: usize,
    /// Where `:=` declares: the innermost function or `for` loop, each of which
    /// owns a dictionary. `if`, `while`, and `try` clauses do not, so a local
    /// declared in one stays visible after it.
    pub declaration_scope: usize,
}

#[derive(Debug, Clone)]
pub struct BindingFact {
    pub name: ObjectName,
    pub target: TextRange,
    pub value: Option<TextRange>,
    pub definition: TextRange,
    pub role: BindingRole,
    pub effect: BindingEffect,
    pub scope: usize,
    pub potential_export: bool,
}

impl BindingFact {
    pub fn from_node(
        target: SyntaxNode<'_>,
        value: Option<SyntaxNode<'_>>,
        role: BindingRole,
        effect: BindingEffect,
        scope: usize,
        potential_export: bool,
        source: &(impl SourceNavigation + ?Sized),
    ) -> Self {
        Self {
            name: ObjectName::new(target.text()),
            target: source.range_for_node(target),
            value: value.map(|value| source.range_for_node(value)),
            definition: source.range_for_node(
                source_cell(target)
                    .filter(|cell| is_value_cell(*cell))
                    .unwrap_or(target),
            ),
            role,
            effect,
            scope,
            potential_export,
        }
    }

    /// The global binding `newPackage "P"` makes of `P`, anchored on the text
    /// inside the name string.
    fn package_declaration(
        name: SyntaxNode<'_>,
        call: SyntaxNode<'_>,
        definition: TextRange,
        source: &(impl SourceNavigation + ?Sized),
    ) -> Option<Self> {
        Some(Self {
            name: ObjectName::new(name.string_literal_inner_text()?),
            target: package_name_range(name, source)?,
            value: Some(source.range_for_node(call)),
            definition,
            role: BindingRole::Ordinary,
            effect: BindingEffect::Assign,
            scope: 0,
            potential_export: true,
        })
    }
}

/// Where the binding of a `newPackage` name string is anchored.
pub(super) fn package_name_range(
    name: SyntaxNode<'_>,
    source: &(impl SourceNavigation + ?Sized),
) -> Option<TextRange> {
    name.string_literal_inner_bytes()
        .map(|bytes| source.range_for_bytes(bytes))
}

pub fn syntax_range(
    syntax: &(impl Spanned + ?Sized),
    source: &(impl SourceNavigation + ?Sized),
) -> Option<TextRange> {
    Some(source.range_for_bytes(source.syntax_bytes(syntax)?))
}

pub fn walk(
    syntax: &TypedSource,
    root: SyntaxNode<'_>,
    source: &(impl SourceNavigation + ?Sized),
    mut collect: impl FnMut(BindingFact, &ScopeTree),
) -> ScopeTree {
    let mut walker = TypedWalker::new(root, source);
    syntax.visit(&mut walker);
    for fact in walker.bindings {
        collect(fact, &walker.scopes);
    }
    walker.scopes
}

struct TypedWalker<'tree, 'source, Source: ?Sized> {
    root: SyntaxNode<'tree>,
    source: &'source Source,
    scopes: ScopeTree,
    bindings: Vec<BindingFact>,
    context: TraversalContext,
    definition: TextRange,
}

impl<'tree, 'source, Source: SourceNavigation + ?Sized> TypedWalker<'tree, 'source, Source> {
    fn new(root: SyntaxNode<'tree>, source: &'source Source) -> Self {
        Self {
            root,
            source,
            scopes: ScopeTree::default(),
            bindings: Vec::new(),
            context: TraversalContext {
                scope: 0,
                assignment_scope: 0,
                declaration_scope: 0,
            },
            definition: source.full_range(),
        }
    }

    fn with_definition(&mut self, syntax: &(impl Spanned + ?Sized), visit: impl FnOnce(&mut Self)) {
        let inherited = self.definition;
        self.definition = syntax_range(syntax, self.source).unwrap_or(inherited);
        visit(self);
        self.definition = inherited;
    }

    fn with_owner_scope(
        &mut self,
        syntax: &(impl Spanned + ?Sized),
        owns_assignments: bool,
        assignments_may_escape: bool,
        function_dispatch: Option<Dispatch>,
        visit: impl FnOnce(&mut Self),
    ) {
        let inherited = self.context;
        let Some(range) = syntax_range(syntax, self.source) else {
            visit(self);
            return;
        };
        let scope = self.scopes.push_range(
            range,
            inherited.scope,
            assignments_may_escape,
            function_dispatch,
        );
        self.context = TraversalContext {
            scope,
            assignment_scope: if owns_assignments {
                scope
            } else {
                inherited.assignment_scope
            },
            declaration_scope: scope,
        };
        visit(self);
        self.context = inherited;
    }

    fn with_control_scope(
        &mut self,
        range: TextRange,
        kind: ControlFlowScope,
        visit: impl FnOnce(&mut Self),
    ) {
        let inherited = self.context;
        let scope =
            self.scopes
                .push_range(range, inherited.scope, kind.assignments_may_escape(), None);
        self.context = TraversalContext {
            scope,
            assignment_scope: kind.assignment_scope(scope, inherited.assignment_scope),
            declaration_scope: inherited.declaration_scope,
        };
        visit(self);
        self.context = inherited;
    }

    fn with_syntax_scope(
        &mut self,
        syntax: &(impl Spanned + ?Sized),
        kind: ControlFlowScope,
        visit: impl FnOnce(&mut Self),
    ) {
        let Some(range) = syntax_range(syntax, self.source) else {
            visit(self);
            return;
        };
        self.with_control_scope(range, kind, visit);
    }

    fn bind_symbol(
        &mut self,
        symbol: &Symbol,
        value: Option<&Expr>,
        role: BindingRole,
        effect: BindingEffect,
    ) {
        let Some(target) = syntax_range(symbol, self.source) else {
            return;
        };
        let scope = match effect {
            BindingEffect::Declare => self.context.declaration_scope,
            BindingEffect::Assign => self.context.assignment_scope,
        };
        self.bindings.push(BindingFact {
            name: ObjectName::new(symbol.text()),
            target,
            value: value.and_then(|value| syntax_range(value, self.source)),
            definition: self.definition,
            role,
            effect,
            scope,
            potential_export: match effect {
                BindingEffect::Declare => scope == 0,
                BindingEffect::Assign => scope == 0 || self.scopes.assignments_may_escape(scope),
            },
        });
    }

    fn bind_symbols<'ast>(
        &mut self,
        symbols: impl IntoIterator<Item = &'ast Symbol>,
        value: Option<&Expr>,
        role: BindingRole,
        effect: BindingEffect,
    ) {
        for symbol in symbols {
            self.bind_symbol(symbol, value, role, effect);
        }
    }

    fn visit_for_parts(
        &mut self,
        owner: &ForLoop,
        variable: &Symbol,
        condition: Option<&WhenCondition>,
        body: &LoopBody,
        visit_domain: impl FnOnce(&mut Self),
    ) {
        self.with_owner_scope(owner, false, false, None, |walker| {
            walker.bind_symbol(
                variable,
                None,
                BindingRole::Parameter,
                BindingEffect::Declare,
            );
            visit_domain(walker);
            if let Some(condition) = condition {
                walker.with_syntax_scope(condition, ControlFlowScope::LoopClause, |walker| {
                    walker.visit_when_condition(condition)
                });
            }
            walker.with_syntax_scope(body, ControlFlowScope::LoopClause, |walker| {
                visit::visit_loop_body(walker, body)
            });
        });
    }
}

impl<'ast, Source: SourceNavigation + ?Sized> Visit<'ast> for TypedWalker<'_, '_, Source> {
    fn visit_cell(&mut self, node: &'ast m2_syn::nodes::Cell) {
        self.with_definition(node, |walker| visit::visit_cell(walker, node));
    }

    fn visit_expr_assign(&mut self, node: &'ast Assignment) {
        match node {
            Assignment::Simple(SimpleBinding::Global(binding)) => self.bind_symbol(
                &binding.variable,
                Some(&binding.value),
                BindingRole::Ordinary,
                BindingEffect::Assign,
            ),
            Assignment::Simple(SimpleBinding::Local(binding)) => self.bind_symbol(
                &binding.variable,
                Some(&binding.value),
                BindingRole::Ordinary,
                BindingEffect::Declare,
            ),
            Assignment::Parallel(ParallelAssignment::Global(assignment)) => self.bind_symbols(
                symbols_in_assignment_pack(&assignment.argument_list),
                Some(&assignment.value),
                BindingRole::Ordinary,
                BindingEffect::Assign,
            ),
            Assignment::Parallel(ParallelAssignment::Local(assignment)) => self.bind_symbols(
                symbols_in_assignment_pack(&assignment.argument_list),
                Some(&assignment.value),
                BindingRole::Ordinary,
                BindingEffect::Declare,
            ),
            Assignment::Evaluated(_)
            | Assignment::Augmented(_)
            | Assignment::Install(_)
            | Assignment::Op(_) => {}
        }
        visit::visit_expr_assign(self, node);
    }

    fn visit_expr_op_bin_adj(&mut self, node: &'ast AdjacentExpr) {
        if let Some(fact) = self.root.related(node).and_then(|call| {
            BindingFact::package_declaration(
                declared_package_name(call)?,
                call,
                self.definition,
                self.source,
            )
        }) {
            self.bindings.push(fact);
        }
        visit::visit_expr_op_bin_adj(self, node);
    }

    fn visit_expr_lambda(&mut self, node: &'ast LambdaExpression) {
        self.with_owner_scope(
            node,
            true,
            true,
            Some(typed_function_dispatch(node)),
            |walker| {
                walker.bind_symbols(
                    symbols_in_lambda_parameters(node),
                    None,
                    BindingRole::Parameter,
                    BindingEffect::Declare,
                );
                visit::visit_expr_lambda(walker, node);
            },
        );
    }

    fn visit_expr_if(&mut self, node: &'ast IfStatement) {
        self.with_syntax_scope(&node.condition, ControlFlowScope::Branch, |walker| {
            walker.visit_expr(&node.condition)
        });
        self.with_syntax_scope(&node.then_clause, ControlFlowScope::Branch, |walker| {
            walker.visit_expr(&node.then_clause.expr)
        });
        if let Some(clause) = node.else_clause.as_ref() {
            self.with_syntax_scope(clause, ControlFlowScope::Branch, |walker| {
                walker.visit_expr(&clause.expr)
            });
        }
    }

    fn visit_expr_for(&mut self, node: &'ast ForLoop) {
        match node {
            ForLoop::Bare(for_loop) => self.visit_for_parts(
                node,
                &for_loop.variable,
                for_loop.condition.as_ref(),
                &for_loop.body,
                |_| {},
            ),
            ForLoop::Range(for_loop) => self.visit_for_parts(
                node,
                &for_loop.variable,
                for_loop.condition.as_ref(),
                &for_loop.body,
                |walker| {
                    let lower_range = for_loop
                        .lower_bound
                        .as_ref()
                        .and_then(|bound| syntax_range(bound, walker.source));
                    let upper_range = for_loop
                        .upper_bound
                        .as_ref()
                        .and_then(|bound| syntax_range(bound, walker.source));
                    let domain_range = match (lower_range, upper_range) {
                        (Some(lower), Some(upper)) => Some(TextRange::new(lower.start, upper.end)),
                        (Some(range), None) | (None, Some(range)) => Some(range),
                        (None, None) => None,
                    };
                    if let Some(range) = domain_range {
                        walker.with_control_scope(range, ControlFlowScope::LoopClause, |walker| {
                            if let Some(bound) = for_loop.lower_bound.as_ref() {
                                walker.visit_range_bound_from(bound);
                            }
                            if let Some(bound) = for_loop.upper_bound.as_ref() {
                                walker.visit_range_bound_to(bound);
                            }
                        });
                    }
                },
            ),
            ForLoop::In(for_loop) => self.visit_for_parts(
                node,
                &for_loop.variable,
                for_loop.condition.as_ref(),
                &for_loop.body,
                |walker| {
                    walker.with_syntax_scope(
                        for_loop.collection.as_ref(),
                        ControlFlowScope::LoopClause,
                        |walker| walker.visit_expr(&for_loop.collection),
                    );
                },
            ),
        }
    }

    fn visit_expr_while(&mut self, node: &'ast WhileLoop) {
        self.with_syntax_scope(&node.condition, ControlFlowScope::LoopClause, |walker| {
            walker.visit_expr(&node.condition)
        });
        self.with_syntax_scope(&node.body, ControlFlowScope::LoopClause, |walker| {
            visit::visit_loop_body(walker, &node.body)
        });
    }

    fn visit_expr_try(&mut self, node: &'ast TryStatement) {
        self.with_syntax_scope(&node.value, ControlFlowScope::Branch, |walker| {
            walker.visit_expr(&node.value)
        });
        if let Some(clause) = node.on_success.as_ref() {
            self.with_syntax_scope(clause, ControlFlowScope::Branch, |walker| {
                walker.visit_expr(&clause.expr)
            });
        }
        if let Some(fallback) = node.fallback.as_ref() {
            match fallback {
                TryFallback::Except(clause) => {
                    self.with_syntax_scope(clause, ControlFlowScope::Branch, |walker| {
                        walker.visit_expr(&clause.value)
                    });
                }
                TryFallback::Else(clause) => {
                    self.with_syntax_scope(clause, ControlFlowScope::Branch, |walker| {
                        walker.visit_expr(&clause.expr)
                    });
                }
            }
        }
    }
}

fn symbols_in_lambda_parameters(lambda: &LambdaExpression) -> Vec<&Symbol> {
    match lambda {
        LambdaExpression::Variadic(lambda) => vec![&lambda.argument],
        LambdaExpression::List(lambda) => match lambda.arguments.contents.as_ref() {
            None => Vec::new(),
            Some(ArgumentListContent::Symbol(symbol)) => vec![symbol],
            Some(ArgumentListContent::Pack(arguments)) => arguments.arguments.iter().collect(),
        },
    }
}

fn typed_function_dispatch(lambda: &LambdaExpression) -> Dispatch {
    match lambda {
        LambdaExpression::Variadic(_) => Dispatch::Variadic,
        LambdaExpression::List(lambda) => Dispatch::Fixed(match &lambda.arguments.contents {
            None => 0,
            Some(ArgumentListContent::Symbol(_)) => 1,
            Some(ArgumentListContent::Pack(arguments)) => arguments.arguments.len(),
        }),
    }
}

fn symbols_in_assignment_pack(binding: &AssignmentPack) -> Vec<&Symbol> {
    let mut symbols = Vec::new();
    if let Some(components) = &binding.contents.contents {
        match components {
            m2_syn::nodes::AssignmentListContent::Argument(component) => {
                collect_assignment_symbol(component, &mut symbols)
            }
            m2_syn::nodes::AssignmentListContent::Pack(components) => {
                for component in &components.arguments {
                    collect_assignment_symbol(component, &mut symbols);
                }
            }
        }
    }
    symbols
}

fn collect_assignment_symbol<'ast>(
    component: &'ast AssignmentPackComponent,
    symbols: &mut Vec<&'ast Symbol>,
) {
    match component {
        AssignmentPackComponent::Symbol(symbol) => symbols.push(symbol),
        AssignmentPackComponent::List(pack) => {
            symbols.extend(symbols_in_assignment_pack(pack));
        }
        AssignmentPackComponent::Empty(_)
        | AssignmentPackComponent::Op(_)
        | AssignmentPackComponent::Evaluated(_) => {}
    }
}

impl ScopeTree {
    fn push_range(
        &mut self,
        range: TextRange,
        parent: usize,
        assignments_may_escape: bool,
        function_dispatch: Option<Dispatch>,
    ) -> usize {
        let scope = self.scopes.len();
        self.scopes.push(Scope {
            range,
            parent: Some(parent),
            assignments_may_escape,
            function_dispatch,
        });
        scope
    }

    pub(super) fn owned_by(
        &self,
        node: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
    ) -> usize {
        let range = source.range_for_node(node);
        self.with_range(range)
            .or_else(|| {
                self.scopes
                    .iter()
                    .enumerate()
                    .filter(|(_, scope)| {
                        scope.range.start == range.start && scope.range.end <= range.end
                    })
                    .max_by_key(|(_, scope)| scope.range.end)
                    .map(|(scope, _)| scope)
            })
            .expect("scope owner should be registered before semantic collection")
    }

    pub(super) fn with_range(&self, range: TextRange) -> Option<usize> {
        self.scopes.iter().position(|scope| scope.range == range)
    }

    pub(super) fn at(&self, position: Position) -> Option<usize> {
        self.scopes
            .iter()
            .enumerate()
            .filter(|(_, scope)| scope.range.contains_position(position))
            .min_by(|(_, left), (_, right)| {
                if left.range.is_inside(right.range) {
                    Ordering::Less
                } else if right.range.is_inside(left.range) {
                    Ordering::Greater
                } else {
                    Ordering::Equal
                }
            })
            .map(|(scope, _)| scope)
    }

    pub(super) fn len(&self) -> usize {
        self.scopes.len()
    }

    pub(super) fn parent(&self, scope: usize) -> Option<usize> {
        self.scopes.get(scope)?.parent
    }

    pub(super) fn assignments_may_escape(&self, scope: usize) -> bool {
        self.scopes[scope].assignments_may_escape
    }

    pub(super) fn function_dispatch(
        &self,
        owner: SyntaxNode,
        source: &(impl SourceNavigation + ?Sized),
    ) -> Option<Dispatch> {
        self.scopes[self.owned_by(owner, source)].function_dispatch
    }
}

#[derive(Clone, Copy)]
pub(super) enum ControlFlowScope {
    Branch,
    LoopClause,
}

impl ControlFlowScope {
    pub(super) fn assignment_scope(self, nested: usize, inherited: usize) -> usize {
        match self {
            Self::Branch => nested,
            Self::LoopClause => inherited,
        }
    }

    fn assignments_may_escape(self) -> bool {
        matches!(self, Self::Branch)
    }
}

pub(super) fn nested_symbols<'tree>(
    node: SyntaxNode<'tree>,
    contains_symbols: impl Fn(&SyntaxNode<'tree>) -> bool + Copy,
) -> Vec<SyntaxNode<'tree>> {
    if node.is::<Symbol>() {
        return vec![node];
    }
    if !contains_symbols(&node) {
        return Vec::new();
    }
    node.children()
        .flat_map(|child| nested_symbols(child, contains_symbols))
        .collect()
}
