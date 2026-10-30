//! Compatibility boundary for editor callers still using navigation nodes.
//!
//! Expression rules and their cache never use node tags or navigation identities.
//! This module selects typed inputs and translates completed results for editor callers.

use m2_syn::nodes::{Cell, Empty, Expr, ExprPack, Statement, Symbol};

use super::syntax::ExpressionSyntax;
use super::{InferType, InferenceContext};
use crate::node_metadata::SyntaxNode;
use crate::source::SourceNavigation;
use crate::typesystem::{InferredType, PositionedTypeKnowledge, TypeRole};

/// Navigation borrowed only for unmigrated editor callers.
pub struct InferenceNavigation<'ast> {
    /// Synthetic root owning the same generated expressions as the inference session.
    root: SyntaxNode<'ast>,
}

impl<'ast> InferenceNavigation<'ast> {
    /// Borrows the document's existing navigation view without rebuilding its tree.
    pub fn new(root: SyntaxNode<'ast>) -> Self {
        Self { root }
    }

    /// Selects the generated input expected by existing hint and diagnostic callers.
    pub fn infer_node(
        &self,
        node: SyntaxNode<'ast>,
        context: &dyn InferenceContext<'ast>,
    ) -> InferredType {
        if let Some(expression) = node.get::<Expr>() {
            return context.expression(expression);
        }
        if let Some(symbol) = node.get::<Symbol>() {
            return symbol.infer(context);
        }
        if let Some(pack) = node.get::<ExprPack>() {
            return pack.infer(context);
        }
        if let Some(empty) = node.get::<Empty>() {
            return empty.infer(context);
        }
        if let Some(statement) = node.get::<Statement>() {
            return statement.infer(context);
        }
        if let Some(cell) = node.get::<Cell>().filter(|cell| cell.semicolon.is_some()) {
            return context.exact(cell, TypeRole::Nothing);
        }
        InferredType::unknown()
    }

    /// Visits generated syntax directly, translating only the finished results for legacy callers.
    pub fn visit_types<S, K>(
        &self,
        session: &super::session::InferenceSession<'ast, '_, S, K>,
        callback: &mut dyn FnMut(SyntaxNode<'ast>, InferredType),
    ) where
        S: SourceNavigation + ?Sized,
        K: PositionedTypeKnowledge + ?Sized,
    {
        let mut visitor = InferredTypes {
            navigation: self,
            session,
            callback,
            emitted: std::collections::HashSet::new(),
        };
        self.root.typed_source().visit(&mut visitor);
    }
}

/// Generated visitor emitting inferred results after the operands they depend on.
struct InferredTypes<'ast, 'request, 'query, S: ?Sized, K: ?Sized> {
    /// Output adapter required by hint consumers not yet migrated to typed inputs.
    navigation: &'query InferenceNavigation<'ast>,
    /// The shared typed cache and positioned semantic context.
    session: &'query super::session::InferenceSession<'ast, 'request, S, K>,
    /// Receives a navigation location only after type inference has finished.
    callback: &'query mut dyn FnMut(SyntaxNode<'ast>, InferredType),
    /// Presentation identities already emitted through overlapping generated wrappers.
    emitted: std::collections::HashSet<crate::node_metadata::SyntaxNodeId>,
}

impl<'ast, S, K> InferredTypes<'ast, '_, '_, S, K>
where
    S: SourceNavigation + ?Sized,
    K: PositionedTypeKnowledge + ?Sized,
{
    /// Converts a completed result into its caller's location without inspecting its type.
    fn emit<T: crate::node_metadata::SyntaxType>(
        &mut self,
        syntax: &'ast T,
        inferred: InferredType,
    ) {
        if let Some(node) = self.navigation.root.related(syntax) {
            if self.emitted.insert(node.id()) {
                (self.callback)(node, inferred);
            }
        }
    }

    /// Evaluates a concrete non-expression rule before translating its output location.
    fn emit_rule<T: InferType + crate::node_metadata::SyntaxType>(&mut self, syntax: &'ast T) {
        let scope = self.session.scope_at(syntax);
        let inferred = syntax.infer(&self.session.context(scope));
        self.emit(syntax, inferred);
    }
}

impl<'ast, S, K> m2_syn::visit::Visit<'ast> for InferredTypes<'ast, '_, '_, S, K>
where
    S: SourceNavigation + ?Sized,
    K: PositionedTypeKnowledge + ?Sized,
{
    fn visit_expr(&mut self, expression: &'ast Expr) {
        m2_syn::visit::visit_expr(self, expression);
        match expression {
            Expr::Catch(_) | Expr::Throw(_) | Expr::Trap(_) => return,
            Expr::New(new) if new.class.symbol_name().is_none() => return,
            _ => {}
        }
        let inferred = self
            .session
            .infer(expression, self.session.scope_at(expression));
        self.emit(expression, inferred);
    }

    fn visit_symbol(&mut self, symbol: &'ast Symbol) {
        self.emit_rule(symbol);
    }
    fn visit_empty(&mut self, empty: &'ast Empty) {
        self.emit_rule(empty);
    }

    fn visit_expr_pack(&mut self, pack: &'ast ExprPack) {
        m2_syn::visit::visit_expr_pack(self, pack);
        if self
            .navigation
            .root
            .related(pack)
            .is_some_and(|node| node.is_expr_pack())
        {
            self.emit_rule(pack);
        }
    }

    fn visit_statement(&mut self, statement: &'ast Statement) {
        m2_syn::visit::visit_statement(self, statement);
        self.emit_rule(statement);
    }

    fn visit_cell(&mut self, cell: &'ast Cell) {
        m2_syn::visit::visit_cell(self, cell);
        if cell.semicolon.is_some() {
            let inferred = self
                .session
                .context(self.session.scope_at(cell))
                .exact(cell, TypeRole::Nothing);
            self.emit(cell, inferred);
        }
    }
}
