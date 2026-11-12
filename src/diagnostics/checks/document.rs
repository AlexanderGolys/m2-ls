//! The document occasion: diagnostics spawned once the whole document has been
//! seen.

use std::collections::HashSet;

use m2_syn::nodes::{Expr, ExprCollection, ExprOpBinRegular, ExprOption, Symbol};
use m2_syn::visit::{self, Visit};
use m2_syn::Token;
use tower_lsp::lsp_types::SymbolKind;

use super::{DiagnosticSink, DocumentContext};
use crate::diagnostics::{catalog, Finding};
use crate::meta::BindingRole;
use crate::node_metadata::ParsedCell;
use crate::source::SourceNavigation;
use crate::syntax::{BindingReferences, ExpressionSyntax};

/// The option of `method` holding the options a method function accepts.
const OPTIONS_KEY: &str = "Options";

/// Spawns every diagnostic that needs the whole document.
pub(super) fn spawn(context: &DocumentContext, sink: &mut DiagnosticSink) {
    unused_binding(context, sink);
    super::method_typical_value::MethodTypicalValue::spawn(context, sink);
    super::predicate_declaration::PredicateDeclaration::spawn(context, sink);
}

/// Checks the names of types and of optional arguments. A type is PascalCase
/// or uppercase, as is an option declared by `Options => {…}` or `{…} >> f`.
/// Functions and locals are free: rings, modules and other mathematical
/// objects keep the names their notation gives them.
pub(super) fn naming_conventions(context: &DocumentContext, sink: &mut DiagnosticSink) {
    for binding in context.analysis.bindings() {
        if binding.role == BindingRole::Parameter || binding.state.is_ring_generator {
            continue;
        }
        let name = binding.name.name();
        if binding.state.presentation_kind == SymbolKind::CLASS
            && !super::naming::follows_case(name, true)
        {
            sink.spawn(
                &catalog::NAMING_CONVENTION,
                Finding::new(
                    binding.range,
                    format!("Type names should use PascalCase or uppercase: `{name}`"),
                ),
            );
        }
    }
    let mut declarations = OptionDeclarations::default();
    for cell in context.syntax.cells().iter().filter_map(ParsedCell::syntax) {
        declarations.visit_cell(cell);
    }
    for key in declarations.keys {
        let name = key.text();
        let Some(range) = context.source.range_for_syntax(key) else {
            continue;
        };
        if !super::naming::follows_case(name, true) {
            sink.spawn(
                &catalog::NAMING_CONVENTION,
                Finding::new(
                    range,
                    format!("Optional argument names should use PascalCase: `{name}`"),
                ),
            );
        }
    }
}

/// The names of the optional arguments the visited syntax declares: the keys
/// of the list in `Options => {…}` and in the `{…} >> f` of a function.
#[derive(Default)]
struct OptionDeclarations<'ast> {
    /// The declared names, in source order.
    keys: Vec<&'ast Symbol>,
}

impl<'ast> OptionDeclarations<'ast> {
    /// Records the keys of the `key => value` entries of the list `declared`.
    fn declare(&mut self, declared: &'ast Expr) {
        let Some(Expr::Collection(ExprCollection::List(_))) = declared.unparenthesized() else {
            return;
        };
        for entry in declared
            .unparenthesized()
            .into_iter()
            .flat_map(Expr::contained_expressions)
        {
            if let Expr::Option(option) = entry {
                if let Expr::Symbol(key) = option.key.as_ref() {
                    self.keys.push(key);
                }
            }
        }
    }
}

impl<'ast> Visit<'ast> for OptionDeclarations<'ast> {
    fn visit_expr_option(&mut self, option: &'ast ExprOption) {
        if option.key.symbol_name() == Some(OPTIONS_KEY) {
            self.declare(&option.value);
        }
        visit::visit_expr_option(self, option);
    }

    fn visit_expr_op_bin_regular(&mut self, operation: &'ast ExprOpBinRegular) {
        if operation.operator.spelling() == <Token![>>]>::SPELLING {
            self.declare(&operation.left_operand);
        }
        visit::visit_expr_op_bin_regular(self, operation);
    }
}

/// Reports ordinary source bindings that have no value-reading references.
fn unused_binding(context: &DocumentContext, sink: &mut DiagnosticSink) {
    let &DocumentContext {
        analysis,
        syntax,
        source,
    } = context;
    let mut references = BindingReferences::default();
    for cell in syntax.cells().iter().filter_map(ParsedCell::syntax) {
        references.visit_cell(cell);
    }
    let mut used_bindings = HashSet::new();
    for symbol in references.0 {
        let name = symbol.text();
        let Some(range) = source.range_for_syntax(symbol) else {
            continue;
        };
        if let Some(binding_id) = analysis.binding_id_at(name, range.start) {
            if let Some(binding) = analysis.get_binding_at(name, range.start) {
                if range != binding.range {
                    used_bindings.insert(binding_id);
                }
            }
        }
    }

    for binding in analysis
        .bindings()
        .filter(|binding| binding.role == BindingRole::Ordinary)
        .filter(|binding| !binding.potential_export)
        .filter(|binding| !used_bindings.contains(&binding.binding_id))
    {
        let name = binding.name.name();
        if name.starts_with('_') {
            continue;
        }
        let noun = if binding.state.presentation_kind == SymbolKind::FUNCTION {
            "function"
        } else {
            "variable"
        };
        sink.spawn(
            &catalog::UNUSED_BINDING,
            Finding::new(binding.range, format!("Unused {noun} {name}")),
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::diagnostics::catalog;
    use crate::document::DocumentSnapshot;
    use crate::object_registry::ObjectRegistry;

    /// `output.value` reads `output#(global value)`, so a local named `value`
    /// that only appears as a member key is still unused.
    fn unused(source: &str) -> Vec<String> {
        DocumentSnapshot::from_text(source.to_string(), &ObjectRegistry::default())
            .expect("fixture should parse")
            .diagnostics()
            .iter()
            .filter(|reported| reported.rule.code == catalog::UNUSED_BINDING.code)
            .map(|reported| reported.finding.message.clone())
            .collect()
    }

    #[test]
    fn a_member_key_does_not_use_the_binding_it_is_named_after() {
        assert_eq!(
            unused("g = () -> (value := 1; output.value)\ng()\n"),
            ["Unused variable value"]
        );
        assert_eq!(
            unused("g = () -> (value := 1; output#value)\ng()\n"),
            Vec::<String>::new()
        );
    }

    /// `:=` makes a local of the enclosing function, so a name declared in an
    /// `if`, `while`, or `try` clause is read by its siblings and by code after
    /// it; only a `for` loop owns its body's locals.
    #[test]
    fn a_condition_declaration_is_used_by_its_branches_and_after() {
        assert_eq!(
            unused("f = x -> if (h := x) =!= null then (\" -- \", h)\nf 1\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            unused("f = x -> (if (h := x) =!= null then 1; h)\nf 1\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            unused("f = x -> (if x then (k := 5) else 7; k)\nf true\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            unused("f = x -> (while (h := x) =!= null do h)\nf 1\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            unused("f = x -> (try (k := x) else 7; k)\nf 1\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            unused("f = x -> (for i to x do (q := i); q)\nf 1\n"),
            ["Unused variable q"]
        );
    }
}
