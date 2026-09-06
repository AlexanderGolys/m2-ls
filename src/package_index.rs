//! Imported-package discovery and configured source-file resolution.

use std::env;
use std::path::{Path, PathBuf};

use m2_syn::nodes::{Sequence, SourceFile, Symbol};
use tower_lsp::lsp_types::{Location, Position, Range as TextRange, Url};

#[cfg(test)]
use crate::node_metadata::M2Parser;
use crate::node_metadata::{visit_source_nodes, M2Node};
use crate::object_registry::ObjectName;
use crate::source::SourceNavigation;

/// One package inclusion and the source position from which it takes effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageImport {
    pub package: ObjectName,
    pub effective_from: Position,
}

#[derive(Debug, Clone)]
pub struct SourceResolver {
    roots: Vec<PathBuf>,
}

impl SourceResolver {
    /// Source roots are configuration-driven only: the LSP never invokes M2 to
    /// discover them (no runtime dependency on M2 existing, nor on how it is
    /// launched). Sourced from `M2_LSP_SOURCE_PATH`; package-source jumps simply
    /// degrade to nothing when it is unset.
    pub fn from_environment() -> Self {
        let mut roots = Vec::new();
        if let Some(paths) = env::var_os("M2_LSP_SOURCE_PATH") {
            roots.extend(env::split_paths(&paths));
        }
        Self::new(roots)
    }

    pub fn new(roots: Vec<PathBuf>) -> Self {
        let mut deduped = Vec::new();
        for root in roots {
            Self::push_root(&mut deduped, root.clone());
            if root.file_name().is_some_and(|name| name == "packages") {
                if let Some(parent) = root.parent() {
                    Self::push_root(&mut deduped, parent.to_path_buf());
                }
            }
        }
        SourceResolver { roots: deduped }
    }

    fn push_root(roots: &mut Vec<PathBuf>, root: PathBuf) {
        if !roots.iter().any(|existing| existing == &root) {
            roots.push(root);
        }
    }

    pub fn resolve_source_file(&self, source_file: &str) -> Option<PathBuf> {
        let source = Path::new(source_file);
        if source.is_absolute() && source.exists() {
            return Some(source.to_path_buf());
        }

        self.roots
            .iter()
            .map(|root| root.join(source))
            .find(|candidate| candidate.exists())
    }

    pub fn resolve_package_file(&self, package_name: &str) -> Option<PathBuf> {
        let source_file = format!("{package_name}.m2");
        self.resolve_source_file(&source_file)
    }

    /// The start of a package source file, when configured source roots contain
    /// it. Exact object-definition positions require separate typed provenance
    /// and are not guessed here.
    pub fn package_location(&self, package_name: &str) -> Option<Location> {
        let path = self.resolve_package_file(package_name)?;
        let uri = Url::from_file_path(path).ok()?;
        let position = pos!();
        Some(Location {
            uri,
            range: TextRange::new(position, position),
        })
    }
}

/// The function names whose string argument names a package to import.
fn is_package_import_trigger(name: &str) -> bool {
    matches!(
        name,
        "needsPackage" | "loadPackage" | "debug" | "importFrom"
    )
}

pub fn package_source_string(node: M2Node<'_>) -> Option<&str> {
    is_package_import_string(node).then(|| node.string_literal_inner_text())?
}

pub fn is_package_import_string(node: M2Node<'_>) -> bool {
    node.ancestors()
        .find_map(|application| {
            let argument = package_import_argument(application)?;
            (argument.id() == node.id()).then_some(())
        })
        .is_some()
}

fn package_import<'tree>(application: M2Node<'tree>) -> Option<(M2Node<'tree>, &'tree str)> {
    let argument = package_import_argument(application)?;
    Some((argument, argument.string_literal_inner_text()?))
}

fn package_import_argument(application: M2Node<'_>) -> Option<M2Node<'_>> {
    binary_expression_left_symbol(application).filter(|name| is_package_import_trigger(name))?;
    let argument = application.child_by_field_name("right")?;
    let argument = if argument.is::<Sequence>() || argument.is_holder() {
        argument.collection_elements().next()?
    } else {
        argument
    };
    Some(argument)
}

#[cfg(test)]
pub fn collect_imported_packages(text: &str) -> Vec<PackageImport> {
    use crate::source::DocumentSource;

    let Some(mut parser) = M2Parser::new() else {
        return Vec::new();
    };
    let source = DocumentSource::new(text.to_string());
    let Some(tree) = parser.parse_tree(text, None) else {
        return Vec::new();
    };
    let syntax = tree.typed_source_file(text);
    collect_imported_packages_in_tree(tree.root(text), syntax.as_ref(), &source)
}

/// Walk an already-parsed tree for package-import calls, reusing the caller's
/// parse rather than spinning up a fresh parser. The snapshot keeps its tree
/// around, so per-version import collection costs one walk, not a re-parse.
pub fn collect_imported_packages_in_tree(
    root: M2Node<'_>,
    syntax: Option<&SourceFile>,
    source: &(impl SourceNavigation + ?Sized),
) -> Vec<PackageImport> {
    let mut packages = Vec::new();
    visit_source_nodes(root, syntax, |application| {
        if let Some((_, package_name)) = package_import(application) {
            packages.push(PackageImport {
                package: ObjectName::new(package_name),
                effective_from: source.range_for_node(application).end,
            });
        }
    });

    packages
}

/// The left symbol of an application `callee ARG` (`needsPackage "Pkg"`): the
/// callee name, when `node` is a `SPACE` application whose left operand is a
/// bare symbol.
fn binary_expression_left_symbol(node: M2Node<'_>) -> Option<&str> {
    if !node.is_space_application() {
        return None;
    }

    let left = node.child_by_field_name("left")?;
    if !left.is::<Symbol>() {
        return None;
    }

    Some(left.text())
}

#[cfg(test)]
mod import_trigger_tests {
    use std::{env, fs};

    use super::{collect_imported_packages, package_source_string, M2Parser, SourceResolver};

    fn imported_names(text: &str) -> Vec<String> {
        collect_imported_packages(text)
            .into_iter()
            .map(|import| import.package.name().to_string())
            .collect()
    }

    #[test]
    fn import_from_string_form_adds_the_package() {
        let pkgs = imported_names(r#"importFrom("FooPkg", {"barSym", "bazSym"})"#);
        assert_eq!(pkgs, vec!["FooPkg".to_string()]);
    }

    #[test]
    fn import_from_does_not_capture_symbol_name_strings() {
        // The second-argument symbol strings must NOT be treated as packages.
        let pkgs = imported_names(r#"importFrom("FooPkg", "barSym")"#);
        assert_eq!(pkgs, vec!["FooPkg".to_string()]);
    }

    #[test]
    fn import_from_package_object_form_adds_nothing() {
        // `importFrom_Core {...}` / `importFrom(Core, ...)` take a Package object,
        // not a string — no package name to detect.
        let pkgs = imported_names("importFrom_Core {\"raw\"}");
        assert!(
            pkgs.is_empty(),
            "Package-object form must add nothing, got {pkgs:?}"
        );
    }

    #[test]
    fn existing_triggers_still_detected() {
        let pkgs = imported_names("needsPackage \"A\"\nloadPackage \"B\"\ndebug \"C\"");
        assert_eq!(
            pkgs,
            vec!["A".to_string(), "B".to_string(), "C".to_string()]
        );
    }

    #[test]
    fn import_argument_selection_skips_muted_parts_but_not_empty_slots() {
        let pkgs = imported_names(
            "loadPackage(ignored;\"AfterMuted\")\nloadPackage(,\"AfterNull\")\nloadPackage(\"Muted\";)\n",
        );
        assert_eq!(pkgs, vec!["AfterMuted".to_string()]);
    }

    #[test]
    fn package_source_string_detects_import_like_calls() {
        let text =
            "needsPackage \"Graphs\"\nloadPackage(\"Normaliz\", Reload => true)\ndebug \"Core\"";
        let mut parser = M2Parser::new().expect("Macaulay2 parser should load");
        let root = parser.parse(text).expect("fixture should parse");
        let packages = root
            .descendants()
            .filter(|node| node.is_string_literal())
            .filter_map(package_source_string)
            .collect::<Vec<_>>();

        assert_eq!(packages, vec!["Graphs", "Normaliz", "Core"]);
    }

    #[test]
    fn repeated_imports_remain_source_ordered_registration_events() {
        let text = "needsPackage \"Graphs\"\nloadPackage(\"Normaliz\")\nneedsPackage \"Graphs\"";

        assert_eq!(
            imported_names(text),
            vec![
                "Graphs".to_string(),
                "Normaliz".to_string(),
                "Graphs".to_string()
            ]
        );
    }

    #[test]
    fn source_resolver_finds_package_and_doc_files_from_configured_roots() {
        let root = env::temp_dir().join(format!("m2-lsp-source-resolver-{}", std::process::id()));
        let packages = root.join("Macaulay2").join("packages");
        let docs = packages.join("Macaulay2Doc");
        let core = root.join("Macaulay2").join("m2");
        fs::create_dir_all(&docs).expect("test docs dir should be created");
        fs::create_dir_all(&core).expect("test core dir should be created");
        fs::write(packages.join("Graphs.m2"), "").expect("package fixture should write");
        fs::write(docs.join("operators.m2"), "").expect("doc fixture should write");
        fs::write(core.join("option.m2"), "").expect("core fixture should write");

        let resolver = SourceResolver::new(vec![packages.clone()]);

        assert_eq!(
            resolver.resolve_package_file("Graphs"),
            Some(packages.join("Graphs.m2"))
        );
        assert_eq!(
            resolver.resolve_source_file("Macaulay2Doc/operators.m2"),
            Some(docs.join("operators.m2"))
        );
        assert_eq!(
            resolver.resolve_source_file("m2/option.m2"),
            Some(core.join("option.m2"))
        );

        let _ = fs::remove_dir_all(root);
    }
}
