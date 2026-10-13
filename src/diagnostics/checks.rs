//! The occasions on which diagnostics are spawned from a completed analysis:
//! each syntax node, each method installation, and the whole document.
//!
//! Each occasion has a module whose `spawn` runs that occasion's checks. A
//! check inspects what its occasion provides and spawns the diagnostics it
//! finds into the [`DiagnosticSink`], under whichever rules apply.

mod document;
mod installation;
mod node;

use m2_syn::nodes::SourceFile;
use tower_lsp::lsp_types::Range as TextRange;

use super::{DiagnosticRule, Finding, M2Diagnostic};
use crate::analysis::{Analysis, MethodInstallation};
use crate::node_metadata::{visit_source_nodes, M2Node};
use crate::object_registry::{ObjectRegistry, ObjectRegistryView};
use crate::source::{DocumentSource, SourceNavigation};

/// What the node occasion provides: the node, the analysis of its document,
/// and the source and the type knowledge positioned at the node.
struct NodeContext<'context, 'tree> {
    analysis: &'context Analysis,
    node: M2Node<'tree>,
    source: &'context DocumentSource,
    knowledge: &'context ObjectRegistryView<'context>,
}

/// What the installation occasion provides: the installation, the analysis
/// that characterized it, and the type knowledge positioned at it.
struct InstallationContext<'context> {
    analysis: &'context Analysis,
    installation: &'context MethodInstallation,
    knowledge: &'context ObjectRegistryView<'context>,
}

/// What the document occasion provides: the analysis, the syntax root, and the
/// source of the document.
struct DocumentContext<'context, 'tree> {
    analysis: &'context Analysis,
    root: M2Node<'tree>,
    source: &'context DocumentSource,
}

/// The diagnostics spawned for one document, in the order they were found.
#[derive(Debug, Default)]
struct DiagnosticSink {
    diagnostics: Vec<M2Diagnostic>,
}

impl DiagnosticSink {
    /// Spawns a diagnostic of `rule` describing `finding`.
    fn spawn(&mut self, rule: &'static DiagnosticRule, finding: Finding) {
        self.diagnostics.push(M2Diagnostic { rule, finding });
    }
}

/// Every diagnostic spawned for one analyzed document: on each syntax node in
/// source order, then on each method installation, then on the whole document.
pub fn collect(
    analysis: &Analysis,
    root: M2Node,
    syntax: Option<&SourceFile>,
    source: &DocumentSource,
    knowledge: &ObjectRegistry,
) -> Vec<M2Diagnostic> {
    let mut sink = DiagnosticSink::default();
    visit_source_nodes(root, syntax, |node| {
        let knowledge = knowledge.at(source.position_for_node(node));
        let context = NodeContext {
            analysis,
            node,
            source,
            knowledge: &knowledge,
        };
        node::spawn(&context, &mut sink);
    });
    for installation in analysis.installations() {
        let knowledge = knowledge.at(installation.span.start);
        let context = InstallationContext {
            analysis,
            installation,
            knowledge: &knowledge,
        };
        installation::spawn(&context, &mut sink);
    }
    let context = DocumentContext {
        analysis,
        root,
        source,
    };
    document::spawn(&context, &mut sink);
    sink.diagnostics
}

impl NodeContext<'_, '_> {
    /// The span of the node under inspection.
    fn node_range(&self) -> TextRange {
        self.source.range_for_node(self.node)
    }
}

#[cfg(test)]
mod tests {
    use crate::diagnostics::{catalog, DiagnosticCode, DiagnosticRule, RelatedNote};
    use crate::document::DocumentSnapshot;
    use crate::object_registry::ObjectRegistry;

    /// The installation and assignment-shape diagnostics a source reports,
    /// against the real builtin catalog.
    fn installation_findings(builtins: &ObjectRegistry, source: &str) -> Vec<DiagnosticCode> {
        let document = DocumentSnapshot::from_text(source.to_string(), builtins)
            .expect("fixture should parse");
        let installation_codes = [
            catalog::INSTALL_NO_EFFECT.code,
            catalog::INSTALL_ARITY.code,
            catalog::MULTIPLE_ASSIGNMENT_TARGETS.code,
            catalog::EXPLICIT_INSTALL_REQUIRED.code,
        ];
        document
            .diagnostics()
            .iter()
            .map(|reported| reported.rule.code)
            .filter(|code| installation_codes.contains(code))
            .collect()
    }

    /// M2 evaluates `x <- v` by assigning a symbol `x` directly and otherwise
    /// calling the one-type method `symbol <-` of `class x` with `(x, v)`
    /// (`assigntofun` in M2's `d/evaluate.d`). `installMethod` accepts any other
    /// `<-` domain, but nothing ever calls those methods.
    #[test]
    fn left_arrow_methods_take_effect_only_for_one_non_symbol_type() {
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let findings = |installation: &str| {
            installation_findings(
                &builtins,
                &format!("T = new Type of BasicList\n{installation}\n"),
            )
        };

        assert_eq!(findings("installMethod(symbol <-, T, (x, v) -> v)"), []);
        assert_eq!(
            findings("installMethod(symbol <-, T, T, (x, y, v) -> v)"),
            [catalog::INSTALL_NO_EFFECT.code],
            "a two-type method is never dispatched, and its three parameters are the right arity"
        );
        assert_eq!(
            findings("installMethod(symbol <-, Symbol, (x, v) -> v)"),
            [catalog::INSTALL_NO_EFFECT.code]
        );
        assert_eq!(
            findings("installMethod(symbol <-, Keyword, (x, v) -> v)"),
            [catalog::INSTALL_NO_EFFECT.code],
            "a keyword is a symbol, so it is assigned before any lookup too"
        );
    }

    /// Parallel assignment assigns each target by its own kind: symbols bind,
    /// parts store, and operator or method targets install, all in one statement.
    #[test]
    fn parallel_assignment_targets_are_assigned_component_wise() {
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let findings = |source: &str| installation_findings(&builtins, source);

        assert_eq!(
            findings(concat!(
                "T = new Type of BasicList\n",
                "g = method()\n",
                "(a, T + T, g ZZ, (b, c)) := (1, (x, y) -> x, n -> n, (3, 4))\n",
            )),
            []
        );
        assert_eq!(
            findings("L = new MutableList from {0, 0}\n(p, L#0, (q, w)) = (1, 2, (3, 4))\n"),
            []
        );
        assert_eq!(
            findings("(a, 1) = (2, 3)\n"),
            [catalog::MULTIPLE_ASSIGNMENT_TARGETS.code]
        );
        assert_eq!(
            findings("T = new Type of BasicList\n(T <- T, d) := ((x, y, v) -> v, 8)\n"),
            [catalog::EXPLICIT_INSTALL_REQUIRED.code],
            "a `<-` target is grammatical but never installs a callable method"
        );
    }

    /// The related notes, as start line and message, of each finding of `rule`
    /// in `source`, checked against the real builtin catalog.
    fn notes_of(rule: &DiagnosticRule, source: &str) -> Vec<Vec<(u32, String)>> {
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = DocumentSnapshot::from_text(source.to_string(), &builtins)
            .expect("fixture should parse");
        document
            .diagnostics()
            .iter()
            .filter(|reported| reported.rule.code == rule.code)
            .map(|reported| {
                reported
                    .finding
                    .notes
                    .iter()
                    .map(|RelatedNote { range, message }| (range.start.line, message.clone()))
                    .collect()
            })
            .collect()
    }

    /// A finding about a name points at where the name got the value that
    /// makes the code wrong, so the reader sees both sides of the problem.
    #[test]
    fn findings_about_a_binding_point_at_its_definition() {
        assert_eq!(
            notes_of(&catalog::INSTALL_NO_EFFECT, "f = x -> x\nf ZZ := n -> n\n"),
            [[(0, "`f` is defined here as a plain function".to_string())]]
        );
    }
}
