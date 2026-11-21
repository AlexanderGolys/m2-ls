//! First uses of bare symbols not introduced by assignment or protection.

use std::collections::HashSet;

use m2_syn::nodes::{BinaryOperator, Expr, ExprBinary, ExprField, ExprQuote, Symbol};
use m2_syn::visit::{self, Visit};

use super::{DiagnosticSink, DocumentContext};
use tower_lsp::lsp_types::Position;

use crate::analysis::QuotedName;
use crate::diagnostics::{catalog, Finding};
use crate::node_metadata::ParsedCell;
use crate::object_registry::{ObjectKnowledge, ObjectName};
use crate::source::SourceNavigation;
use crate::syntax::ExpressionSyntax;

/// Tracks introductions in source order during complete document checks.
/// For example, `protect Strategy`
/// permits a later `{Strategy => value}` without inventing a value binding.
pub(super) struct SymbolIntroductions<'context, 'source> {
    /// Completed binding analysis and positioned source environment.
    context: &'context DocumentContext<'source>,
    /// Findings shared with other document checks.
    sink: &'context mut DiagnosticSink,
    /// Symbol names explicitly protected so far.
    protected: HashSet<String>,
    /// First unresolved occurrence already considered in each lexical scope.
    seen: HashSet<(usize, String)>,
}

impl SymbolIntroductions<'_, '_> {
    /// Checks each successfully parsed cell in source order.
    pub(super) fn spawn(context: &DocumentContext<'_>, sink: &mut DiagnosticSink) {
        let mut visitor = SymbolIntroductions {
            context,
            sink,
            protected: HashSet::new(),
            seen: HashSet::new(),
        };
        for cell in context.syntax.cells().iter().filter_map(ParsedCell::syntax) {
            visitor.visit_cell(cell);
        }
    }
}

impl SymbolIntroductions<'_, '_> {
    /// Whether an `export` ahead of `position` creates `name`, as
    /// `export {"Highlight"}` makes the option name `Highlight` a symbol of the
    /// package.
    fn exported_before(&self, name: &str, position: Position) -> bool {
        self.context
            .packages
            .exports_at(position)
            .iter()
            .any(|export| export.name.name() == name && export.range.end <= position)
    }
}

impl<'ast> Visit<'ast> for SymbolIntroductions<'_, '_> {
    fn visit_symbol(&mut self, symbol: &'ast Symbol) {
        let Some(range) = self.context.source.range_for_syntax(symbol) else {
            return;
        };
        let name = symbol.text();
        let analysis = self.context.analysis;
        let knowledge = self.context.knowledge.at(range.start);
        if analysis
            .visible_source_binding_at(name, range.start, &knowledge)
            .is_some()
            || knowledge.get_record(&ObjectName::new(name)).is_some()
            || self.protected.contains(name)
            || self.exported_before(name, range.start)
        {
            return;
        }
        let scope = analysis.find_scope_at(range.start).unwrap_or_default();
        if !self.seen.insert((scope, name.to_owned()))
            || self.sink.diagnostics.iter().any(|diagnostic| {
                diagnostic.rule.code == catalog::UNRESOLVED_VALUE.code
                    && diagnostic.finding.range == range
            })
        {
            return;
        }
        self.sink.spawn(&catalog::UNINTRODUCED_SYMBOL, Finding::new(range,
            format!("`{name}` is first used without a binding; assign it or introduce the symbolic constant with `protect {name}` before use")));
    }

    fn visit_expr_quote(&mut self, _quote: &'ast ExprQuote) {}

    fn visit_expr_field(&mut self, access: &'ast ExprField) {
        self.visit_expr(&access.base);
    }

    fn visit_expr_binary(&mut self, call: &'ast ExprBinary) {
        if matches!(call.op, BinaryOperator::Space(_))
            && call.left.symbol_name() == Some("protect")
            && self
                .context
                .source
                .position_for_syntax(call)
                .is_some_and(|position| {
                    self.context
                        .analysis
                        .visible_source_binding_at(
                            "protect",
                            position,
                            &self.context.knowledge.at(position),
                        )
                        .is_none()
                })
        {
            let argument = call.right.unparenthesized();
            let name = match argument {
                Some(Expr::Quote(quote)) => Some(quote.quoted_name().text()),
                Some(Expr::Symbol(symbol)) => Some(symbol.text()),
                _ => None,
            };
            if let Some(name) = name {
                self.protected.insert(name.to_owned());
            }
        }
        visit::visit_expr_binary(self, call);
    }
}
