//! Imported-package discovery and configured source-file resolution.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use m2_syn::nodes::{Sequence, SourceFile, Symbol};
use m2_syn::Token;
use tower_lsp::lsp_types::{Location, Position, Range as TextRange, Url};

use crate::node_metadata::{matches_token, visit_source_nodes, M2Node, M2Parser};
use crate::object_registry::ObjectName;
use crate::source::SourceNavigation;

/// One package inclusion and the source position from which it takes effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageImport {
    pub package: ObjectName,
    pub effective_from: Position,
}

/// The executable name Macaulay2 is run by.
const M2_EXECUTABLE: &str = "M2";

/// Where an installed Macaulay2 keeps its packages, Core included, relative to
/// its prefix (Macaulay2's `Layout#1`).
const INSTALLED_PACKAGES: &str = "share/Macaulay2";

/// Where a Macaulay2 build tree (`usr-dist`) keeps its packages, relative to its
/// prefix (Macaulay2's `Layout#2`); its `bin` sits in a machine directory.
const BUILD_TREE_PACKAGES: &str = "common/share/Macaulay2";

/// The user's application directory relative to `HOME`, Macaulay2's default
/// `applicationDirectorySuffix`.
#[cfg(not(target_os = "macos"))]
const APPLICATION_DIRECTORY: &str = ".Macaulay2";
#[cfg(target_os = "macos")]
const APPLICATION_DIRECTORY: &str = "Library/Application Support/Macaulay2";

/// Where `installPackage` puts the user's packages, inside the application
/// directory.
const APPLICATION_PACKAGES: &str = "local/share/Macaulay2";

/// The application directory's own source directory, which Macaulay2 also
/// searches.
const APPLICATION_CODE: &str = "code";

/// The initialization file in the application directory.
const INITIALIZATION_FILE: &str = "init.m2";

/// The environment that decides where Macaulay2's files are: what Macaulay2
/// itself reads to find them, plus the server's own source override.
#[derive(Debug, Clone, Default)]
pub struct Macaulay2Environment {
    /// `M2_LSP_SOURCE_PATH`: directories searched before Macaulay2's own.
    pub source_path_override: Option<OsString>,
    /// `PATH`, searched for the `M2` executable.
    pub executable_search_path: Option<OsString>,
    /// `HOME`, which holds the user's application directory.
    pub home: Option<PathBuf>,
}

impl Macaulay2Environment {
    /// The environment of the running server.
    pub fn current() -> Self {
        Self {
            source_path_override: env::var_os("M2_LSP_SOURCE_PATH"),
            executable_search_path: env::var_os("PATH"),
            home: env::var_os("HOME").map(PathBuf::from),
        }
    }

    /// The user's application directory, where Macaulay2 keeps `init.m2` and
    /// locally installed packages.
    fn application_directory(&self) -> Option<PathBuf> {
        self.home
            .as_deref()
            .map(|home| home.join(APPLICATION_DIRECTORY))
    }

    /// The user's initialization file, which every Macaulay2 session started
    /// without `-q` runs before loading its preloaded packages.
    pub fn initialization_file(&self) -> Option<PathBuf> {
        self.application_directory()
            .map(|directory| directory.join(INITIALIZATION_FILE))
            .filter(|file| file.is_file())
    }
}

#[derive(Debug, Clone)]
pub struct SourceResolver {
    roots: Vec<PathBuf>,
}

impl SourceResolver {
    /// The directories Macaulay2 loads Core and packages from, found the way
    /// its documentation describes ("finding the Macaulay2 files") without
    /// running it: the `M2` on `PATH`, symbolic links followed, sits in the
    /// `bin` directory of its prefix, and the user's application directory
    /// holds locally installed packages. `M2_LSP_SOURCE_PATH` comes first, for
    /// directories added in `init.m2` or installations the search cannot see.
    pub fn discover(environment: &Macaulay2Environment) -> Self {
        let mut roots = environment
            .source_path_override
            .as_deref()
            .map(|paths| env::split_paths(paths).collect::<Vec<_>>())
            .unwrap_or_default();
        if let Some(search_path) = environment.executable_search_path.as_deref() {
            roots.extend(installation_package_directories(search_path));
        }
        if let Some(application_directory) = environment.application_directory() {
            roots.extend(
                [
                    application_directory.join(APPLICATION_PACKAGES),
                    application_directory.join(APPLICATION_CODE),
                ]
                .into_iter()
                .filter(|directory| directory.is_dir()),
            );
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

    /// A package's main file: `PKG.m2` beside the package's own directory, or
    /// inside it for Core, which Macaulay2 loads from its package directory
    /// (`Core/Core.m2`).
    pub fn resolve_package_file(&self, package_name: &str) -> Option<PathBuf> {
        let main_file = format!("{package_name}.m2");
        self.resolve_source_file(&main_file)
            .or_else(|| self.resolve_source_file(&format!("{package_name}/{main_file}")))
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

/// The package directories of the Macaulay2 installation whose `M2` comes first
/// on `search_path`. An installed prefix has `bin` directly under it; a build
/// tree has it under a machine directory; whichever has its packages
/// directory is the installation.
fn installation_package_directories(search_path: &OsStr) -> Vec<PathBuf> {
    let Some(executable) = env::split_paths(search_path)
        .map(|directory| directory.join(M2_EXECUTABLE))
        .find(|candidate| candidate.is_file())
    else {
        return Vec::new();
    };
    let Some(bin) = fs::canonicalize(&executable)
        .ok()
        .and_then(|executable| executable.parent().map(Path::to_path_buf))
    else {
        return Vec::new();
    };
    let installed = bin.parent().map(|prefix| prefix.join(INSTALLED_PACKAGES));
    let build_tree = bin
        .parent()
        .and_then(Path::parent)
        .map(|prefix| prefix.join(BUILD_TREE_PACKAGES));
    [installed, build_tree]
        .into_iter()
        .flatten()
        .filter(|directory| directory.is_dir())
        .collect()
}

/// The packages `init.m2` adds to `Core#"preloaded packages"`. Macaulay2 loads
/// that list right after running `init.m2`, so this assignment is a way of
/// loading packages that no import call shows. Replacing the list can also
/// drop default packages, but their dependencies are unknown here, so the
/// defaults stay: a name kept visible is never wrongly reported unknown.
pub fn preloaded_package_additions(text: &str) -> Vec<ObjectName> {
    let Some(mut parser) = M2Parser::new() else {
        return Vec::new();
    };
    let Some(tree) = parser.parse_tree(text, None) else {
        return Vec::new();
    };
    let syntax = tree.typed_source_file(text);
    let mut packages = Vec::new();
    visit_source_nodes(tree.root(text), syntax.as_ref(), |node| {
        if let Some(listed) = preloaded_packages_assignment(node) {
            packages.extend(
                listed
                    .descendants()
                    .filter(|literal| literal.is_string_literal())
                    .filter_map(|literal| literal.string_literal_inner_text())
                    .map(ObjectName::new),
            );
        }
    });
    packages
}

/// The right side of `Core#"preloaded packages" = …` or `|= …`.
fn preloaded_packages_assignment(node: M2Node<'_>) -> Option<M2Node<'_>> {
    let operator = node.binary_operator()?;
    if !(matches_token::<Token![=]>(operator) || matches_token::<Token![|=]>(operator)) {
        return None;
    }
    let target = node.child_by_field_name("left")?;
    let (Some(table), Some(key)) = (
        target.child_by_field_name("left"),
        target.child_by_field_name("right"),
    ) else {
        return None;
    };
    let names_preloaded_packages = target.has_binary_operator::<Token![#]>()
        && table.is::<Symbol>()
        && table.text() == CORE_PACKAGE
        && key.string_literal_inner_text() == Some(PRELOADED_PACKAGES_KEY);
    names_preloaded_packages.then(|| node.child_by_field_name("right"))?
}

/// The package that holds the preloaded-package list.
const CORE_PACKAGE: &str = "Core";

/// The key of the list of packages Macaulay2 loads at startup, in `Core`.
const PRELOADED_PACKAGES_KEY: &str = "preloaded packages";

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

/// The packages `text` imports, each from the position its import takes effect.
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

    use super::{
        collect_imported_packages, package_source_string, preloaded_package_additions, M2Parser,
        Macaulay2Environment, SourceResolver,
    };

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

    /// Macaulay2 finds its files from the `M2` on `PATH`, following symbolic
    /// links to the `bin` of its prefix ("finding the Macaulay2 files"); the
    /// server mirrors that for both of Macaulay2's layouts and adds the user's
    /// application directory, without running Macaulay2.
    #[cfg(unix)]
    #[test]
    fn source_resolver_finds_sources_the_way_macaulay2_finds_its_files() {
        let root = env::temp_dir().join(format!("m2-lsp-source-discovery-{}", std::process::id()));
        let installed = root.join("installed");
        fs::create_dir_all(installed.join("bin")).expect("installed bin");
        fs::create_dir_all(installed.join("share/Macaulay2/Core")).expect("installed packages");
        fs::write(installed.join("bin/M2"), "").expect("installed executable");
        fs::write(installed.join("share/Macaulay2/Core/Core.m2"), "").expect("Core.m2");
        fs::write(installed.join("share/Macaulay2/Core/classes.m2"), "").expect("Core source");
        let linked = root.join("linked-bin");
        fs::create_dir_all(&linked).expect("link directory");
        std::os::unix::fs::symlink(installed.join("bin/M2"), linked.join("M2"))
            .expect("link to the installed executable");

        let build_tree = root.join("usr-dist");
        fs::create_dir_all(build_tree.join("x86_64-Linux/bin")).expect("build-tree bin");
        fs::create_dir_all(build_tree.join("common/share/Macaulay2")).expect("build-tree packages");
        fs::write(build_tree.join("x86_64-Linux/bin/M2"), "").expect("build-tree executable");
        fs::write(build_tree.join("common/share/Macaulay2/JSON.m2"), "").expect("JSON.m2");

        let home = root.join("home");
        fs::create_dir_all(
            home.join(super::APPLICATION_DIRECTORY)
                .join("local/share/Macaulay2"),
        )
        .expect("application packages");
        let local_package = home
            .join(super::APPLICATION_DIRECTORY)
            .join("local/share/Macaulay2/Mine.m2");
        fs::write(&local_package, "").expect("Mine.m2");

        let search_path = |directories: &[&std::path::Path]| {
            Some(env::join_paths(directories).expect("search path"))
        };
        let linked_installation = SourceResolver::discover(&Macaulay2Environment {
            source_path_override: None,
            executable_search_path: search_path(&[&linked, &build_tree.join("x86_64-Linux/bin")]),
            home: Some(home.clone()),
        });
        assert_eq!(
            linked_installation.resolve_source_file("Core/classes.m2"),
            Some(installed.join("share/Macaulay2/Core/classes.m2")),
            "the linked executable leads to its installed prefix"
        );
        assert_eq!(
            linked_installation.resolve_package_file("Core"),
            Some(installed.join("share/Macaulay2/Core/Core.m2")),
            "Core's main file sits in its package directory"
        );
        assert_eq!(
            linked_installation.resolve_package_file("JSON"),
            None,
            "only the first M2 on PATH is the installation"
        );
        assert_eq!(
            linked_installation.resolve_package_file("Mine"),
            Some(local_package.clone())
        );

        let build_tree_installation = SourceResolver::discover(&Macaulay2Environment {
            source_path_override: None,
            executable_search_path: search_path(&[&build_tree.join("x86_64-Linux/bin")]),
            home: None,
        });
        assert_eq!(
            build_tree_installation.resolve_package_file("JSON"),
            Some(build_tree.join("common/share/Macaulay2/JSON.m2"))
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn initialization_file_is_found_in_the_application_directory() {
        let home = env::temp_dir().join(format!("m2-lsp-init-file-{}", std::process::id()));
        let application_directory = home.join(super::APPLICATION_DIRECTORY);
        fs::create_dir_all(&application_directory).expect("application directory");
        let environment = Macaulay2Environment {
            home: Some(home.clone()),
            ..Macaulay2Environment::default()
        };

        assert_eq!(environment.initialization_file(), None);
        fs::write(
            application_directory.join("init.m2"),
            "needsPackage \"Graphs\"\n",
        )
        .expect("init.m2");
        assert_eq!(
            environment.initialization_file(),
            Some(application_directory.join("init.m2"))
        );

        let _ = fs::remove_dir_all(home);
    }

    /// Packages added to `Core#"preloaded packages"` load right after `init.m2`
    /// runs; only that list counts, and commented-out lines do nothing.
    #[test]
    fn preloaded_package_additions_come_from_the_preloaded_packages_list() {
        let additions = |text: &str| {
            preloaded_package_additions(text)
                .into_iter()
                .map(|package| package.name().to_string())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            additions(concat!(
                "Core#\"preloaded packages\" |= {\"Normaliz\", \"Polyhedra\"}\n",
                "Core#\"preloaded packages\" = {\"Schubert2\"}\n",
            )),
            ["Normaliz", "Polyhedra", "Schubert2"]
        );
        assert_eq!(
            additions(concat!(
                "-- Core#\"preloaded packages\" |= {\"StateTables\"}\n",
                "favorites = {\"NotAPackage\"}\n",
                "Core#\"other key\" = {\"NotAPackage\"}\n",
            )),
            Vec::<String>::new()
        );
    }
}
