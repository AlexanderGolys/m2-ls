//! Declaration checks for predicates: the methods named `is` and an uppercase
//! letter, which return Boolean.

use m2_syn::nodes::{Expr, ExprAssign, ExprOp, ExprOpBin};
use m2_syn::visit::{self, Visit};

use super::{naming, DiagnosticSink, DocumentContext};
use crate::diagnostics::{catalog, Finding};
use crate::node_metadata::ParsedCell;
use crate::source::SourceNavigation;
use crate::syntax::{AssignmentOperator, AssignmentSyntax, AssignmentTarget, ExpressionSyntax};
use crate::typesystem::TypeRole;

/// Constructor whose assignment creates a source-owned method function.
const METHOD_CONSTRUCTOR: &str = "method";

/// Visits the global assignments that define a predicate.
pub(super) struct PredicateDeclaration<'context, 'source> {
    /// Complete syntax and binding analysis for the document.
    context: &'context DocumentContext<'source>,
    /// Findings accumulated by document checks.
    sink: &'context mut DiagnosticSink,
}

impl PredicateDeclaration<'_, '_> {
    /// Reports each predicate defined as a function closure, and each
    /// predicate method declared without Boolean as its typical value.
    pub(super) fn spawn(context: &DocumentContext<'_>, sink: &mut DiagnosticSink) {
        let mut checks = PredicateDeclaration { context, sink };
        for cell in context.syntax.cells().iter().filter_map(ParsedCell::syntax) {
            checks.visit_cell(cell);
        }
    }

    /// A global assignment of a predicate name.
    fn declaration(&mut self, assignment: &ExprAssign) {
        let parts = assignment.parts();
        let AssignmentTarget::Symbol(symbol) = parts.target else {
            return;
        };
        let name = symbol.text();
        if parts.operator != AssignmentOperator::Global || !naming::is_predicate(name) {
            return;
        }
        let Some(range) = self.context.source.range_for_syntax(symbol) else {
            return;
        };
        if parts.value.assigned_function().is_some() {
            self.sink.spawn(
                &catalog::PREDICATE_FUNCTION_CLOSURE,
                Finding::new(
                    range,
                    format!(
                        "`{name}` is a predicate: define it as a method, \
                         `{name} = method(TypicalValue => {})`, and install its implementations",
                        TypeRole::Boolean.object_name()
                    ),
                ),
            );
            return;
        }
        let Some(Expr::Op(ExprOp::Bin(ExprOpBin::Adj(call)))) = parts.value.unparenthesized()
        else {
            return;
        };
        if call.left_operand.symbol_name() != Some(METHOD_CONSTRUCTOR) {
            return;
        }
        let analysis = self.context.analysis;
        let Some(function) = analysis
            .assigned_binding_at(range)
            .and_then(|binding| analysis.function_for_binding(binding))
        else {
            return;
        };
        let boolean = TypeRole::Boolean.object_name();
        if function.is_method_function() && function.typical_value.as_ref() != Some(&boolean) {
            self.sink.spawn(
                &catalog::PREDICATE_TYPICAL_VALUE,
                Finding::new(
                    range,
                    format!(
                        "`{name}` is a predicate: declare `{name} = method(TypicalValue => {boolean})`"
                    ),
                ),
            );
        }
    }
}

impl<'ast> Visit<'ast> for PredicateDeclaration<'_, '_> {
    fn visit_expr_assign(&mut self, assignment: &'ast ExprAssign) {
        self.declaration(assignment);
        visit::visit_expr_assign(self, assignment);
    }
}
