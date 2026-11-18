//! Suggests `for` for local counters initialized immediately before a `while`.
//!
//! The counter must advance exactly once at the end of each iteration, with no
//! other writes, `continue`, escaping closures, or uses outside the loop. The
//! suggested inclusive `to` bound is a fixed integer or an unchanged collection
//! length. Zero is the default start, so `from 0` is omitted.

use std::collections::HashSet;
use std::ptr;

use m2_syn::nodes::{AssignOperator, Elements, Expr, ExprAssign, ExprFunction, ExprParen, Literal};
use m2_syn::visit::{self, Visit};
use m2_syn::Token;

use super::{DiagnosticSink, DocumentContext};
use crate::analysis::InfixSyntax;
use crate::diagnostics::{catalog, Finding};
use crate::source::SourceNavigation;
use crate::syntax::{
    AssignmentOperator, AssignmentSyntax, AssignmentTarget, BindingReferences, ExpressionSyntax,
};

/// Examines neighboring statements in each typed block for a counted loop.
pub(super) struct CountedLoops<'context, 'document> {
    /// Analysis, syntax, and coordinates of the checked document.
    context: &'context DocumentContext<'document>,
    /// Findings accumulated by this diagnostic occasion.
    sink: &'context mut DiagnosticSink,
}

impl CountedLoops<'_, '_> {
    /// Checks every block in the retained native syntax.
    pub(super) fn spawn(context: &DocumentContext, sink: &mut DiagnosticSink) {
        context.syntax.visit(&mut CountedLoops { context, sink });
    }

    /// Checks one adjacent initializer/loop pair, allowing the loop to be the
    /// block's final value as well as a semicolon-terminated statement.
    fn check(&mut self, block: &ExprParen, index: usize) -> Option<()> {
        let Elements::Single(initializer) = &block.stmts[index].elems else {
            return None;
        };
        let Expr::Assign(binding) = initializer.as_ref() else {
            return None;
        };
        let (
            AssignOperator::Local(_),
            Expr::Symbol(variable),
            Expr::Literal(Literal::Integer(start)),
        ) = (&binding.op, binding.left.as_ref(), binding.right.as_ref())
        else {
            return None;
        };
        let initial = start.text().parse::<isize>().ok()?;
        let counter = variable.text();
        let next = match block.stmts.get(index + 1) {
            Some(statement) => match &statement.elems {
                Elements::Single(expression) => expression.as_ref(),
                Elements::Pack(_) => return None,
            },
            None => block.expr.as_deref()?,
        };
        let Expr::While(loop_) = next else {
            return None;
        };
        let comparison = loop_.cond.infix_operands()?;
        if comparison.left.symbol_name() != Some(counter)
            || !matches!(comparison.operator, "<" | "<=")
            || loop_.body.list_clause.is_some()
        {
            return None;
        }
        let bound = comparison.right.unparenthesized()?;
        match bound {
            Expr::Literal(Literal::Integer(integer)) => {
                integer.text().parse::<isize>().ok()?;
            }
            Expr::Prefix(prefix)
                if prefix.op.spelling() == <Token![#]>::SPELLING
                    && prefix.expr.symbol_name().is_some() => {}
            _ => return None,
        }
        let mut bound_references = BindingReferences::default();
        bound_references.visit_expr(bound);
        let bound_names = bound_references
            .0
            .iter()
            .map(|symbol| symbol.text().to_string())
            .collect::<HashSet<_>>();
        if bound_names.contains(counter) {
            return None;
        }
        let Expr::Paren(body) = loop_.body.do_clause.as_ref()?.expr.as_ref() else {
            return None;
        };
        let increment = match body.expr.as_deref() {
            Some(value) => value,
            None => match &body.stmts.last()?.elems {
                Elements::Single(expression) => expression.as_ref(),
                Elements::Pack(_) => return None,
            },
        };
        let increment = Self::unit_increment(increment, counter)?;
        let mut effects = CounterEffects {
            counter,
            bound_names,
            increment,
            changes_iteration: false,
            checking_condition: true,
        };
        effects.visit_expr(&loop_.cond);
        effects.checking_condition = false;
        effects.visit_expr_paren(body);
        if effects.changes_iteration || Self::used_outside_loop(block, index, counter) {
            return None;
        }
        let source = self.context.source;
        let from = if initial == 0 {
            String::new()
        } else {
            format!(" from {}", start.text())
        };
        let exclusive = if comparison.operator == <Token![<]>::SPELLING {
            " - 1"
        } else {
            ""
        };
        let end = source.text_for_syntax(bound)?.trim();
        self.sink.spawn(&catalog::PREFER_FOR_LOOP, Finding::new(
            source.range_for_syntax(loop_)?,
            format!("This counter advances by one each iteration; use `for {counter}{from} to {end}{exclusive} do` and omit the explicit initializer and final increment"),
        ));
        Some(())
    }

    /// Recognizes `counter += 1` and `counter = counter + 1` from typed
    /// assignments, returning the one write supplied implicitly by `for`.
    fn unit_increment<'ast>(expression: &'ast Expr, counter: &str) -> Option<&'ast ExprAssign> {
        let Expr::Assign(assignment) = expression else {
            return None;
        };
        if assignment.left.symbol_name() != Some(counter) {
            return None;
        }
        let value = match &assignment.op {
            AssignOperator::Augmented(operator)
                if operator.spelling() == <Token![+=]>::SPELLING =>
            {
                assignment.right.as_ref()
            }
            AssignOperator::Global(_) => {
                let addition = assignment.right.infix_operands()?;
                if addition.operator != <Token![+]>::SPELLING
                    || addition.left.symbol_name() != Some(counter)
                {
                    return None;
                }
                addition.right
            }
            _ => return None,
        };
        let Expr::Literal(Literal::Integer(integer)) = value else {
            return None;
        };
        (integer.text().parse::<u8>() == Ok(1)).then_some(assignment)
    }

    /// A `for` index has a narrower scope than the original local. Reject
    /// references elsewhere in the block, including forward references and
    /// references captured by callbacks created before or after the loop.
    fn used_outside_loop(block: &ExprParen, initializer: usize, counter: &str) -> bool {
        let mut references = BindingReferences::default();
        for (index, statement) in block.stmts.iter().enumerate() {
            if index != initializer && index != initializer + 1 {
                references.visit_stmt(statement);
            }
        }
        if initializer + 1 < block.stmts.len() {
            if let Some(value) = &block.expr {
                references.visit_expr(value);
            }
        }
        references.0.iter().any(|symbol| symbol.text() == counter)
    }
}

impl<'ast> Visit<'ast> for CountedLoops<'_, '_> {
    /// Checks each possible initializer before descending into nested blocks.
    fn visit_expr_paren(&mut self, block: &'ast ExprParen) {
        for index in 0..block.stmts.len() {
            self.check(block, index);
        }
        visit::visit_expr_paren(self, block);
    }
}

/// Rejects counter writes and control flow that a `for` loop would change.
struct CounterEffects<'ast> {
    /// Local variable controlling this loop.
    counter: &'ast str,
    /// Collection names whose lengths must remain fixed throughout iteration.
    bound_names: HashSet<String>,
    /// The sole permitted counter write, removed in a manual conversion.
    increment: &'ast ExprAssign,
    /// Whether iteration or visibility differs from the proposed `for` loop.
    changes_iteration: bool,
    /// Whether declarations would move from the outer block into the new index scope.
    checking_condition: bool,
}

impl CounterEffects<'_> {
    /// Includes rebinding, parallel assignment, and writes through a collection
    /// subscript, since extending a mutable collection changes its length.
    fn writes_bound(&self, target: AssignmentTarget<'_>) -> bool {
        let mut references = BindingReferences::default();
        match target {
            AssignmentTarget::Symbol(symbol) => references.0.push(symbol),
            AssignmentTarget::List(list) => references
                .0
                .extend(AssignmentTarget::List(list).bound_symbols()),
            AssignmentTarget::Operator(operation) => references.visit_expr(operation),
            AssignmentTarget::Augmented(expression) | AssignmentTarget::Evaluated(expression) => {
                references.visit_expr(expression)
            }
            _ => {}
        }
        references
            .0
            .iter()
            .any(|symbol| self.bound_names.contains(symbol.text()))
    }
}

impl<'ast> Visit<'ast> for CounterEffects<'_> {
    /// Detects assignments, augmented assignments, and parallel writes. Computed
    /// assignment targets are rejected because their destination is unknown.
    fn visit_expr_assign(&mut self, assignment: &'ast ExprAssign) {
        if !ptr::eq(assignment, self.increment) {
            let parts = assignment.parts();
            let target = parts.target;
            self.changes_iteration |=
                self.checking_condition && parts.operator == AssignmentOperator::Local;
            self.changes_iteration |= self.writes_bound(target);
            self.changes_iteration |= target
                .bound_symbols()
                .iter()
                .any(|symbol| symbol.text() == self.counter)
                || matches!(target, AssignmentTarget::Augmented(expression) if expression.symbol_name() == Some(self.counter))
                || matches!(target, AssignmentTarget::Evaluated(_));
        }
        visit::visit_expr_assign(self, assignment);
    }

    /// `continue` skips the explicit increment in a `while`, but increments the
    /// index of a `for`. Nested loops are conservatively excluded too.
    fn visit_expr(&mut self, expression: &'ast Expr) {
        self.changes_iteration |= matches!(expression, Expr::Continue(_));
        visit::visit_expr(self, expression);
    }

    /// A closure can expose the counter beyond the scope of a new `for` index.
    fn visit_expr_function(&mut self, _function: &'ast ExprFunction) {
        self.changes_iteration = true;
    }
}
