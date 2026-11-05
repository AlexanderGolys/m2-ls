//! Declaration hints for methods whose explicit result annotations mostly agree.

use std::collections::HashMap;

use m2_syn::nodes::{Expr, ExprAssign, ExprOp, ExprOpBin};
use m2_syn::visit::{self, Visit};

use super::{DiagnosticSink, DocumentContext};
use crate::analysis::{CodomainOrigin, MethodDeclarationOption};
use crate::diagnostics::{catalog, Finding};
use crate::node_metadata::ParsedCell;
use crate::source::SourceNavigation;
use crate::syntax::{AssignmentSyntax, AssignmentTarget, ExpressionSyntax};

/// Constructor whose assignment creates a source-owned method function.
const METHOD_CONSTRUCTOR: &str = "method";
/// Smallest method family eligible for a shared-result hint.
const MINIMUM_INSTALLATIONS: usize = 5;
/// The strict majority threshold: more than three quarters of all installations.
const MAJORITY_NUMERATOR: usize = 3;
/// Denominator of the shared-codomain threshold.
const MAJORITY_DENOMINATOR: usize = 4;

/// Visits method declarations once the document's installations are all known.
pub(super) struct MethodTypicalValue<'context, 'source> {
    /// Complete syntax and binding analysis for the document.
    context: &'context DocumentContext<'source>,
    /// Findings accumulated by document checks.
    sink: &'context mut DiagnosticSink,
}

impl MethodTypicalValue<'_, '_> {
    /// Reports one hint on each eligible local method constructor assignment.
    pub(super) fn spawn(context: &DocumentContext<'_>, sink: &mut DiagnosticSink) {
        let mut checks = MethodTypicalValue { context, sink };
        for cell in context.syntax.cells().iter().filter_map(ParsedCell::syntax) {
            checks.visit_cell(cell);
        }
    }

    /// A declaration whose effective installations share a dominant annotation.
    fn declaration(&mut self, assignment: &ExprAssign) {
        let parts = assignment.parts();
        let AssignmentTarget::Symbol(symbol) = parts.target else {
            return;
        };
        let Some(Expr::Op(ExprOp::Bin(ExprOpBin::Adj(call)))) = parts.value.unparenthesized()
        else {
            return;
        };
        if call.left_operand.symbol_name() != Some(METHOD_CONSTRUCTOR) {
            return;
        }
        let Some(range) = self.context.source.range_for_syntax(symbol) else {
            return;
        };
        let analysis = self.context.analysis;
        let Some(binding) = analysis.assigned_binding_at(range) else {
            return;
        };
        let Some(function) = analysis.function_for_binding(binding) else {
            return;
        };
        if !function.is_method_function() || function.typical_value.is_some() {
            return;
        }
        let installations = analysis
            .effective_installations(function)
            .collect::<Vec<_>>();
        if installations.len() < MINIMUM_INSTALLATIONS {
            return;
        }
        let mut annotations = HashMap::new();
        for installation in &installations {
            if installation.codomain_origin == Some(CodomainOrigin::Annotation) {
                if let Some(codomain) = &installation.method.codomain {
                    *annotations.entry(codomain).or_insert(0usize) += 1;
                }
            }
        }
        let Some((codomain, count)) = annotations.into_iter().max_by_key(|(_, count)| *count)
        else {
            return;
        };
        if count * MAJORITY_DENOMINATOR <= installations.len() * MAJORITY_NUMERATOR {
            return;
        }
        let option = MethodDeclarationOption::TypicalValue.name();
        self.sink.spawn(
            &catalog::SHARED_METHOD_CODOMAIN,
            Finding::new(
                range,
                format!(
                    "{count} of {} installations of `{}` annotate `{codomain}`; consider setting `{option} => {codomain}` once in its `method(...)` declaration, retaining any differing per-method annotations.",
                    installations.len(), symbol.text(),
                ),
            ),
        );
    }
}

impl<'ast> Visit<'ast> for MethodTypicalValue<'_, '_> {
    fn visit_expr_assign(&mut self, assignment: &'ast ExprAssign) {
        self.declaration(assignment);
        visit::visit_expr_assign(self, assignment);
    }
}
