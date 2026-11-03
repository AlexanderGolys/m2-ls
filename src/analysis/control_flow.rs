//! Control-flow ownership: which function or loop receives each `return`,
//! `break`, and `continue`.
//!
//! A transfer belongs to its innermost enclosing function or loop step, which
//! a visitor knows as it descends: it keeps the stack of functions and loop
//! steps it is inside, and the callback functions that calls such as
//! `apply(L, x -> …)` pass on.

use std::collections::HashMap;
use std::ptr;

use m2_syn::nodes::{
    Cell, Expr, ExprCollection, ExprControl, ExprFor, ExprLambda, ExprOpBinAdj, ExprWhile,
    LoopBody, Symbol,
};
use m2_syn::visit::{self, Visit};
use m2_syn::Spanned;

use super::*;
use crate::object_registry::ObjectKnowledge;
use crate::syntax::ControlSyntax;

/// The callables whose callback argument runs as a loop body, so `break`
/// inside the callback stops the iteration.
const LOOP_CALLBACK_CALLABLES: [&str; 2] = ["apply", "scan"];

/// A `for` or `while` loop.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum LoopStatement<'ast> {
    /// A `for` loop.
    For(&'ast ExprFor),
    /// A `while` loop.
    While(&'ast ExprWhile),
}

impl<'ast> LoopStatement<'ast> {
    /// The loop as spanned syntax.
    pub fn syntax(self) -> &'ast dyn Spanned {
        match self {
            Self::For(node) => node,
            Self::While(node) => node,
        }
    }
}

/// The function or loop receiving a control transfer.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum ControlTransferTarget<'ast> {
    /// The function a `return` leaves.
    Function(&'ast ExprLambda),
    /// A loop whose `list` step collects the values its body yields.
    ListLoop(LoopStatement<'ast>),
    /// A loop whose `do` step evaluates its body only for its effects.
    DoLoop(LoopStatement<'ast>),
    /// A callback a loop-like callable invokes for each element.
    LoopCallback {
        /// The callback function.
        function: &'ast ExprLambda,
        /// The callable invoking it, such as `apply`.
        callable: &'ast Symbol,
    },
}

impl<'ast> ControlTransferTarget<'ast> {
    /// The function or loop owning the transfer.
    pub fn owner(self) -> &'ast dyn Spanned {
        match self {
            Self::Function(function) | Self::LoopCallback { function, .. } => function,
            Self::ListLoop(statement) | Self::DoLoop(statement) => statement.syntax(),
        }
    }

    /// Whether this target can receive the given transfer form.
    pub fn accepts(self, transfer: &ExprControl) -> bool {
        match (self, transfer) {
            (Self::Function(_), ExprControl::Return(_)) => true,
            (Self::ListLoop(_), ExprControl::Break(_) | ExprControl::Continue(_)) => true,
            (Self::LoopCallback { .. }, ExprControl::Break(_)) => true,
            (Self::DoLoop(_), ExprControl::Break(_)) => true,
            (Self::DoLoop(_), ExprControl::Continue(_)) => transfer.transferred_value().is_none(),
            _ => false,
        }
    }
}

/// A control transfer with the construct receiving it, when one does.
#[derive(Debug, Clone, Copy)]
pub struct ControlTransfer<'ast> {
    /// The `return`, `break`, or `continue`.
    pub transfer: &'ast ExprControl,
    /// The function or loop receiving it.
    pub target: Option<ControlTransferTarget<'ast>>,
}

impl Analysis {
    /// The control transfers of `cells`, in source order, with the function
    /// or loop receiving each.
    pub fn control_transfers<'ast>(
        &self,
        cells: impl IntoIterator<Item = &'ast Cell>,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl PositionedTypeKnowledge + ?Sized),
    ) -> Vec<ControlTransfer<'ast>> {
        let mut transfers = ControlTransfers {
            analysis: self,
            source,
            knowledge,
            owners: Vec::new(),
            loops: Vec::new(),
            callbacks: HashMap::new(),
            found: Vec::new(),
        };
        for cell in cells {
            transfers.visit_cell(cell);
        }
        transfers.found
    }

    /// Whether `callable`, invoking a callback, is the library loop callable
    /// of its name rather than a local binding or another object.
    fn is_loop_callback_callable(
        &self,
        callable: &Symbol,
        source: &(impl SourceNavigation + ?Sized),
        knowledge: &(impl PositionedTypeKnowledge + ?Sized),
    ) -> bool {
        let name = callable.text();
        let Some(position) = source.position_for_syntax(callable) else {
            return false;
        };
        let knowledge = knowledge.at_position(position);
        LOOP_CALLBACK_CALLABLES.contains(&name)
            && self
                .visible_source_binding_at(name, position, &knowledge)
                .is_none()
            && knowledge
                .get_record(&ObjectName::new(name))
                .and_then(|record| record.callable())
                .is_some()
    }
}

/// A construct a transfer inside it belongs to.
#[derive(Debug, Clone, Copy)]
enum Owner<'ast> {
    /// A function body.
    Function(&'ast ExprLambda),
    /// The `list` step of a loop.
    ListStep(LoopStatement<'ast>),
    /// The `do` step of a loop.
    DoStep(LoopStatement<'ast>),
}

/// Visitor pairing each control transfer with its owner.
struct ControlTransfers<'ast, 'analysis, Source: ?Sized, Knowledge: ?Sized> {
    /// Analysis resolving callback callables.
    analysis: &'analysis Analysis,
    /// Source coordinates of the visited revision.
    source: &'analysis Source,
    /// Object knowledge positioned at each callable.
    knowledge: &'analysis Knowledge,
    /// Functions and loop steps enclosing the visited syntax, innermost last.
    owners: Vec<Owner<'ast>>,
    /// Loops whose body is visited next, innermost last.
    loops: Vec<LoopStatement<'ast>>,
    /// Callback functions, by identity, with the callable passing them.
    callbacks: HashMap<*const ExprLambda, &'ast Symbol>,
    /// Transfers found so far.
    found: Vec<ControlTransfer<'ast>>,
}

impl<'ast, Source, Knowledge> ControlTransfers<'ast, '_, Source, Knowledge>
where
    Source: SourceNavigation + ?Sized,
    Knowledge: PositionedTypeKnowledge + ?Sized,
{
    /// Visits `visit` inside `owner`.
    fn within(&mut self, owner: Owner<'ast>, visit: impl FnOnce(&mut Self)) {
        self.owners.push(owner);
        visit(self);
        self.owners.pop();
    }

    /// The receiver of `transfer`: its innermost enclosing owner, when that
    /// owner can receive a transfer of its form.
    fn target(&self, transfer: &ExprControl) -> Option<ControlTransferTarget<'ast>> {
        Some(match *self.owners.last()? {
            Owner::Function(function) => match transfer {
                ExprControl::Return(_) => ControlTransferTarget::Function(function),
                ExprControl::Break(_) | ExprControl::Continue(_) => {
                    let callable = *self.callbacks.get(&ptr::from_ref(function))?;
                    self.analysis
                        .is_loop_callback_callable(callable, self.source, self.knowledge)
                        .then_some(ControlTransferTarget::LoopCallback { function, callable })?
                }
            },
            Owner::ListStep(statement) => ControlTransferTarget::ListLoop(statement),
            Owner::DoStep(statement) => ControlTransferTarget::DoLoop(statement),
        })
    }
}

/// The function a call passes as its final argument, seen through
/// parentheses: the `x -> …` of `apply(L, x -> …)`.
fn final_lambda_argument(call: &ExprOpBinAdj) -> Option<&ExprLambda> {
    let mut argument = call.right_operand.as_ref();
    loop {
        argument = match argument {
            Expr::Lambda(lambda) => return Some(lambda),
            Expr::Holder(holder) => holder.value.as_deref()?,
            Expr::Collection(ExprCollection::Sequence(_)) => {
                argument.collection_elements()?.into_iter().last()??
            }
            _ => return None,
        };
    }
}

impl<'ast, Source, Knowledge> Visit<'ast> for ControlTransfers<'ast, '_, Source, Knowledge>
where
    Source: SourceNavigation + ?Sized,
    Knowledge: PositionedTypeKnowledge + ?Sized,
{
    fn visit_expr_control(&mut self, transfer: &'ast ExprControl) {
        self.found.push(ControlTransfer {
            transfer,
            target: self.target(transfer),
        });
        visit::visit_expr_control(self, transfer);
    }

    fn visit_expr_lambda(&mut self, lambda: &'ast ExprLambda) {
        self.within(Owner::Function(lambda), |this| {
            visit::visit_expr_lambda(this, lambda);
        });
    }

    fn visit_expr_op_bin_adj(&mut self, call: &'ast ExprOpBinAdj) {
        if let (Expr::Symbol(callable), Some(callback)) =
            (call.left_operand.as_ref(), final_lambda_argument(call))
        {
            self.callbacks.insert(ptr::from_ref(callback), callable);
        }
        visit::visit_expr_op_bin_adj(self, call);
    }

    fn visit_expr_for(&mut self, node: &'ast ExprFor) {
        self.loops.push(LoopStatement::For(node));
        visit::visit_expr_for(self, node);
        self.loops.pop();
    }

    fn visit_expr_while(&mut self, node: &'ast ExprWhile) {
        self.loops.push(LoopStatement::While(node));
        visit::visit_expr_while(self, node);
        self.loops.pop();
    }

    fn visit_loop_body(&mut self, body: &'ast LoopBody) {
        let Some(&statement) = self.loops.last() else {
            return visit::visit_loop_body(self, body);
        };
        if let Some(step) = &body.list_step {
            self.within(Owner::ListStep(statement), |this| {
                this.visit_list_step(step)
            });
        }
        if let Some(step) = &body.do_step {
            self.within(Owner::DoStep(statement), |this| this.visit_do_step(step));
        }
    }
}
