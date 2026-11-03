//! Inline values: what an expression evaluates to where the program
//! determines it statically, shown beside it as `S_6 = z`. Values come from
//! the analysis, which records them beyond types; nothing is evaluated by
//! running Macaulay2.

use m2_syn::nodes::Expr;
use m2_syn::visit::{self, Visit};
use tower_lsp::lsp_types::{
    InlineValue, InlineValueServerCapabilities, InlineValueText, OneOf, Range,
};

use crate::analysis::{InferenceSession, TypeChecker};
use crate::document::DocumentSnapshot;
use crate::source::{SourceNavigation, SyntaxSource};
use crate::typesystem::TypeKnowledge;

/// Protocol capability advertising support for inline values.
pub fn inline_value_provider_capability() -> Option<OneOf<bool, InlineValueServerCapabilities>> {
    Some(OneOf::Left(true))
}

/// The inline values of the expressions of `document` lying within `range`:
/// each variable of a ring the program writes out that an expression
/// selects, by index `S_6` or by name `"x"_A`.
pub fn inline_values_response(
    document: &DocumentSnapshot,
    range: Range,
    knowledge: &(impl TypeKnowledge + ?Sized),
) -> Vec<InlineValue> {
    let syntax = document.syntax();
    let Some(start) = document.byte_for_position(range.start) else {
        return Vec::new();
    };
    if range.start > range.end {
        return Vec::new();
    }
    let end = document
        .byte_for_position(range.end)
        .unwrap_or(document.text().len());
    let snapshot = syntax;
    let checker = TypeChecker::new(document.analysis(), knowledge);
    let session = InferenceSession::new(&checker, document, snapshot);
    let mut collector = InlineValueCollector {
        document,
        range,
        session: &session,
        values: Vec::new(),
    };
    for cell in snapshot.cells_intersecting(start..end) {
        if let Some(cell) = cell.syntax() {
            collector.visit_cell(cell);
        }
    }
    collector.values
}

/// Collects ring selections in the requested source range using typed expressions.
struct InlineValueCollector<'ast, 'query, K: ?Sized> {
    /// Source and coordinate mapping for the retained revision.
    document: &'query DocumentSnapshot,
    /// Client-requested range.
    range: Range,
    /// Shared inference cache for this request.
    session: &'query InferenceSession<'ast, 'query, DocumentSnapshot, K>,
    /// Values in source traversal order.
    values: Vec<InlineValue>,
}

impl<'ast, K: TypeKnowledge + ?Sized> Visit<'ast> for InlineValueCollector<'ast, '_, K> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        let Some(range) = self.document.range_for_syntax(expression) else {
            return;
        };
        if range.start > self.range.end || range.end < self.range.start {
            return;
        }
        if !matches!(expression, Expr::Holder(_))
            && self.range.start <= range.start
            && range.end <= self.range.end
        {
            if let Some(Ok(selected)) = self.session.ring_values(0).selected(expression) {
                if let Some(text) = self
                    .document
                    .syntax_bytes(expression)
                    .and_then(|bytes| self.document.text().get(bytes))
                {
                    self.values.push(InlineValue::Text(InlineValueText {
                        range,
                        text: format!("{text} = {}", selected.variable),
                    }));
                }
            }
        }
        visit::visit_expr(self, expression);
    }
}
