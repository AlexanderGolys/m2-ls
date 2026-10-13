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
}

fn unused_binding(context: &DocumentContext, sink: &mut DiagnosticSink) {
    let &DocumentContext {
        analysis,
        root,
        source,
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
    #[test]
    fn a_member_key_does_not_use_the_binding_it_is_named_after() {
        let unused = |source: &str| {
            DocumentSnapshot::from_text(source.to_string(), &ObjectRegistry::default())
                .expect("fixture should parse")
                .diagnostics()
                .iter()
                .filter(|reported| reported.rule.code == catalog::UNUSED_BINDING.code)
                .map(|reported| reported.finding.message.clone())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            unused("g = () -> (value := 1; output.value)\ng()\n"),
            ["Unused variable value"]
        );
        assert_eq!(
            unused("g = () -> (value := 1; output#value)\ng()\n"),
            Vec::<String>::new()
        );
    }
}
