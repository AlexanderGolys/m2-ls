//! The occasions on which diagnostics are spawned from a completed analysis:
//! each typed expression, each method installation, and the whole document;
//! and, once the document is published, its location on disk.
//!
//! Each occasion has a module whose `spawn` runs that occasion's checks. A
//! check inspects what its occasion provides and spawns the diagnostics it
//! finds into the [`DiagnosticSink`], under whichever rules apply.

mod counted_loop;
mod document;
mod documentation;
mod expressions;
mod installation;
mod location;
mod method_typical_value;
mod naming;
mod native_dispatch;
mod orphan;
mod parse;
mod predicate_declaration;

use tower_lsp::lsp_types::Url;

use super::{DiagnosticRule, Finding, M2Diagnostic};
use crate::analysis::{Analysis, MethodInstallation};
use crate::document::DocumentSnapshot;
use crate::node_metadata::SyntaxSnapshot;
use crate::object_registry::{ObjectRegistry, ObjectRegistryView};
use crate::package_documentation::PackageDocumentation;
use crate::package_index::SourceResolver;
use crate::source::{DocumentSource, SourceNavigation, SyntaxNavigation};
use crate::source_package::SourcePackages;

/// What the installation occasion provides: the installation, the analysis
/// that characterized it, and the type knowledge positioned at it.
struct InstallationContext<'context> {
    /// Semantic facts for the current document snapshot.
    analysis: &'context Analysis,
    /// Method installation characterized by this item.
    installation: &'context MethodInstallation,
    /// Positioned object and type facts available to this request.
    knowledge: &'context ObjectRegistryView<'context>,
}

/// What the document occasion provides: the analysis, the syntax, and the
/// source of the document.
struct DocumentContext<'context> {
    /// Semantic facts for the current document snapshot.
    analysis: &'context Analysis,
    /// The syntax revision the analysis describes.
    syntax: &'context SyntaxSnapshot,
    /// Source data supplying text and coordinate conversions.
    source: &'context DocumentSource,
}

/// What the documentation occasion provides: the package documentation the
/// document writes, the packages it declares, the syntax and source it is
/// written in, and the knowledge of the packages the document loads.
struct DocumentationContext<'context> {
    /// Documentation extracted from the current document or selected object.
    documentation: &'context PackageDocumentation,
    /// Package declarations and imports visible to this operation.
    packages: &'context SourcePackages,
    /// Parsed syntax backing this item, when available.
    syntax: &'context SyntaxSnapshot,
    /// Source data supplying text and coordinate conversions.
    source: &'context DocumentSource,
    /// Positioned object and type facts available to this request.
    knowledge: &'context ObjectRegistry,
}

/// What the location occasion provides: the document, where it is, and where
/// Macaulay2 looks for the files it names.
struct LocationContext<'context> {
    /// Immutable document snapshot being queried.
    document: &'context DocumentSnapshot,
    /// Document location associated with the request.
    uri: &'context Url,
    /// Candidate providers or source resolver supplying this operation.
    sources: &'context SourceResolver,
}

/// The diagnostics spawned for one document, in the order they were found.
#[derive(Debug, Default)]
struct DiagnosticSink {
    /// Findings associated with this request or collected by this pass.
    diagnostics: Vec<M2Diagnostic>,
}

impl DiagnosticSink {
    /// Spawns a diagnostic of `rule` describing `finding`.
    fn spawn(&mut self, rule: &'static DiagnosticRule, finding: Finding) {
        self.diagnostics.push(M2Diagnostic { rule, finding });
    }
}

/// Shared semantic inputs, all derived from the same retained syntax snapshot.
pub struct DiagnosticInputs<'a> {
    /// Whether this document is analyzed as library implementation code, which
    /// is reported on for syntax errors only.
    pub primary_code: bool,
    /// Semantic facts for the current document snapshot.
    pub analysis: &'a Analysis,
    /// Parsed syntax backing this item, when available.
    pub syntax: &'a SyntaxSnapshot,
    /// Source data supplying text and coordinate conversions.
    pub source: &'a DocumentSource,
    /// Positioned object and type facts available to this request.
    pub knowledge: &'a ObjectRegistry,
    /// Package declarations and imports visible to this operation.
    pub packages: &'a SourcePackages,
    /// Documentation extracted from the current document or selected object.
    pub documentation: &'a PackageDocumentation,
}

/// Every diagnostic spawned for one analyzed document: on each cell's
/// expressions in source order, then on each method installation, then on the
/// whole document, then on the package documentation it writes.
pub fn collect(inputs: DiagnosticInputs<'_>, live: bool) -> Vec<M2Diagnostic> {
    let DiagnosticInputs {
        primary_code,
        analysis,
        syntax,
        source,
        knowledge,
        packages,
        documentation,
    } = inputs;
    // A live update re-checks everything from its first changed cell on:
    // what follows an edit may depend on it, while the findings before it
    // are kept by the document.
    let first_change = syntax.changed_ranges().first().map(|cell| cell.start);
    let selected =
        |range: std::ops::Range<usize>| !live || first_change.is_none_or(|start| start < range.end);
    let mut sink = DiagnosticSink::default();
    let navigation = SyntaxNavigation::new(source, syntax);
    let context = expressions::ExpressionContext {
        analysis,
        source: &navigation,
        knowledge,
        first_change: first_change.filter(|_| live),
    };
    for cell in syntax.cells() {
        if !selected(cell.document_range()) {
            continue;
        }
        parse::spawn(cell, source, &mut sink);
        if !primary_code {
            expressions::spawn(cell, &context, &mut sink);
        }
    }
    if primary_code {
        return sink.diagnostics;
    }
    for installation in analysis.installations() {
        if !source
            .bytes_for_range(installation.span)
            .is_some_and(selected)
        {
            continue;
        }
        let knowledge = knowledge.at(installation.span.start);
        let context = InstallationContext {
            analysis,
            installation,
            knowledge: &knowledge,
        };
        installation::spawn(&context, &mut sink);
        if let Some(package) = packages.declaration_at(installation.span.start) {
            orphan::spawn(&context, std::slice::from_ref(&package.name), &mut sink);
        }
    }
    let context = DocumentContext {
        analysis,
        syntax,
        source,
    };
    let mut positioned = DiagnosticSink::default();
    document::naming_conventions(&context, &mut positioned);
    counted_loop::CountedLoops::spawn(&context, &mut positioned);
    sink.diagnostics
        .extend(positioned.diagnostics.into_iter().filter(|diagnostic| {
            source
                .bytes_for_range(diagnostic.finding.range)
                .is_some_and(selected)
        }));
    if live {
        return sink.diagnostics;
    }
    document::spawn(&context, &mut sink);
    let context = DocumentationContext {
        documentation,
        packages,
        syntax,
        source,
        knowledge,
    };
    documentation::spawn(&context, &mut sink);
    sink.diagnostics
}

/// Every diagnostic that depends on where the document at `uri` is: the
/// files it names that `sources` and the disk cannot provide.
pub fn collect_located(
    document: &DocumentSnapshot,
    uri: &Url,
    sources: &SourceResolver,
) -> Vec<M2Diagnostic> {
    let mut sink = DiagnosticSink::default();
    location::spawn(
        &LocationContext {
            document,
            uri,
            sources,
        },
        &mut sink,
    );
    sink.diagnostics
}

/// The orphan installations of a file without package declarations of its
/// own that the packages `owners` load from inside their sections, so that its
/// code runs as theirs.
pub fn collect_loaded_orphans(
    document: &DocumentSnapshot,
    owners: &[crate::object_registry::ObjectName],
) -> Vec<M2Diagnostic> {
    let mut sink = DiagnosticSink::default();
    if owners.is_empty()
        || document.properties().primary_code
        || !document.packages().declarations().is_empty()
    {
        return sink.diagnostics;
    }
    let analysis = document.analysis();
    let registry = document.object_registry();
    for installation in analysis.installations() {
        let knowledge = registry.at(installation.span.start);
        let context = InstallationContext {
            analysis,
            installation,
            knowledge: &knowledge,
        };
        orphan::spawn(&context, owners, &mut sink);
    }
    sink.diagnostics
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
        let builtins = ObjectRegistry::load(crate::catalog::test_corpus());
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

    /// The interpreter computes arithmetic and comparison on native values
    /// itself, so an installation for exactly those operands is never called;
    /// any other operand class, including ring elements, still dispatches.
    #[test]
    fn operator_installations_on_native_operands_have_no_effect() {
        let builtins = ObjectRegistry::load(crate::catalog::test_corpus());
        let findings = |installation: &str| {
            installation_findings(
                &builtins,
                &format!("T = new Type of BasicList\n{installation}\n"),
            )
        };
        let ignored = [catalog::INSTALL_NO_EFFECT.code];

        assert_eq!(findings("ZZ + QQ := (a, b) -> a"), ignored);
        assert_eq!(
            findings("installMethod(symbol *, RR, CC, (a, b) -> a)"),
            ignored
        );
        assert_eq!(findings("ZZ // ZZ := (a, b) -> a"), ignored);
        assert_eq!(findings("String == String := (a, b) -> true"), ignored);
        assert_eq!(findings("RRi ? T := (a, b) -> symbol <"), ignored);
        assert_eq!(findings("- ZZ := a -> a"), ignored);
        assert_eq!(findings("not Boolean := a -> a"), ignored);
        assert_eq!(findings("Boolean and Boolean := (a, b) -> a"), ignored);
        assert_eq!(findings("ZZ ** QQ := (a, b) -> a"), []);
        assert_eq!(findings("ZZ + T := (a, b) -> a"), []);
        assert_eq!(findings("T + T := (a, b) -> a"), []);
        assert_eq!(findings("Number + Number := (a, b) -> a"), []);
        assert_eq!(findings("CC ^ RRi := (a, b) -> a"), []);
    }

    /// Parallel assignment assigns each target by its own kind: symbols bind,
    /// parts store, and operator or method targets install, all in one statement.
    #[test]
    fn parallel_assignment_targets_are_assigned_component_wise() {
        let builtins = ObjectRegistry::load(crate::catalog::test_corpus());
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
        let builtins = ObjectRegistry::load(crate::catalog::test_corpus());
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

    /// The messages of the orphan installations `source` reports, against
    /// the real builtin catalog.
    fn orphans(source: &str) -> Vec<String> {
        let builtins = ObjectRegistry::load(crate::catalog::test_corpus());
        let document = DocumentSnapshot::from_text(source.to_string(), &builtins)
            .expect("fixture should parse");
        document
            .diagnostics()
            .iter()
            .filter(|reported| reported.rule.code == catalog::ORPHAN_INSTALLATION.code)
            .map(|reported| reported.finding.message.clone())
            .collect()
    }

    /// Whether the package fixture `body` reports an orphan installation.
    fn orphaned(body: &str) -> bool {
        !orphans(&format!("newPackage \"Pkg\"\n{body}\nendPackage \"Pkg\"\n")).is_empty()
    }

    /// Macaulay2 finds an installation only for arguments descending from each
    /// of its domain types, so owning the method function or one domain type
    /// keeps the method to callers of the package.
    #[test]
    fn package_installations_must_own_the_method_or_a_domain_type() {
        assert!(orphaned("net ZZ := n -> \"n\""));
        assert!(orphaned("ZZ + RR := (a, b) -> a"));
        assert!(orphaned(
            "installMethod(symbol *, Matrix, String, (m, s) -> m)"
        ));
        assert!(orphaned("toString(ZZ, QQ) := (a, b) -> \"\""));
        assert!(orphaned("- Matrix := m -> m"));

        assert!(!orphaned("T = new Type of List\nnet T := t -> \"t\""));
        assert!(!orphaned("T = new Type of List\nZZ * T := (n, t) -> t"));
        assert!(!orphaned(
            "T = new Type of List\ninstallMethod(symbol +, ZZ, T, (n, t) -> t)"
        ));
        assert!(!orphaned("f = method()\nf ZZ := n -> n"));
        assert!(!orphaned("f = method()\nf(ZZ, Matrix) := (n, m) -> m"));
        assert!(!orphaned("R = QQ[x]\npromote(ZZ, R) := (n, R) -> n"));
        assert!(
            !orphaned("net Unbound := u -> \"u\""),
            "a name nothing resolves may be defined where the package loads it from"
        );
    }

    /// Ownership follows the value: a package name bound to another package's
    /// type or method still installs on that type or method, and a codomain is
    /// never dispatched on.
    #[test]
    fn ownership_follows_the_value_not_the_name() {
        assert!(orphaned("Integer = ZZ\nnet Integer := n -> \"n\""));
        assert!(orphaned("T = new Type of List\nnet ZZ := T => n -> \"n\""));
        assert_eq!(
            notes_of(
                &catalog::ORPHAN_INSTALLATION,
                "newPackage \"Pkg\"\nInteger = ZZ\nnet Integer := n -> \"n\"\n",
            ),
            [[(
                1,
                "`Integer` is bound here to a value another package defines".to_string()
            )]]
        );
    }

    /// Code outside a package section runs in the user's own session, where
    /// changing builtin methods is the user's choice.
    #[test]
    fn only_package_code_is_held_to_the_orphan_rule() {
        assert_eq!(orphans("net ZZ := n -> \"n\"\n"), Vec::<String>::new());
        assert_eq!(
            orphans("newPackage \"Pkg\"\nendPackage \"Pkg\"\nnet ZZ := n -> \"n\"\n"),
            Vec::<String>::new()
        );
        let [message] = orphans("newPackage \"Pkg\"\nnet ZZ := n -> \"n\"\n")
            .try_into()
            .expect("one orphan installation");
        assert!(
            message.starts_with("`Pkg` installs `net` (Core) for `ZZ` (Core)"),
            "{message}"
        );
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
