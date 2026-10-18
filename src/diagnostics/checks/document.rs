//! The document occasion: diagnostics spawned once the whole document has been
//! seen.

use std::collections::HashSet;

use tower_lsp::lsp_types::SymbolKind;

use super::{DiagnosticSink, DocumentContext};
use crate::diagnostics::{catalog, Finding};
use crate::meta::BindingRole;
use crate::source::SourceNavigation;

/// Spawns every diagnostic that needs the whole document.
pub(super) fn spawn(context: &DocumentContext, sink: &mut DiagnosticSink) {
    unused_binding(context, sink);
    missing_end_package(context, sink);
}

fn missing_end_package(context: &DocumentContext, sink: &mut DiagnosticSink) {
    for declaration in context
        .packages
        .declarations()
        .iter()
        .filter(|declaration| declaration.closing.is_none())
    {
        let name = &declaration.name;
        sink.spawn(
            &catalog::MISSING_END_PACKAGE,
            Finding::new(
                declaration.name_range,
                format!(
                    "Package {name} is never closed; end its section with endPackage \"{name}\""
                ),
            ),
        );
    }
}

fn unused_binding(context: &DocumentContext, sink: &mut DiagnosticSink) {
    let &DocumentContext {
        analysis,
        root,
        source,
        ..
    } = context;
    let mut used_bindings = HashSet::new();
    for node in root.binding_references() {
        let name = node.text();
        let position = source.position_for_node(node);
        if let Some(binding_id) = analysis.binding_id_at(name, position) {
            if let Some(binding) = analysis.get_binding_at(name, position) {
                let node_range = source.range_for_node(node);
                if node_range != binding.range {
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
