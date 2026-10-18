//! Imported-package discovery and configured source-file resolution.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use m2_syn::nodes::{Sequence, SourceFile, Symbol};
use m2_syn::Token;
use tower_lsp::lsp_types::{Location, Position, Range as TextRange, Url};

use crate::node_metadata::{matches_token, visit_source_nodes, M2Node, M2Parser};
use crate::object_registry::{ObjectName, ObjectRegistry};
use crate::source::SourceNavigation;
use crate::workspace_index::WorkspaceIndex;

/// One package inclusion and the source position from which it takes effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageImport {
    pub package: ObjectName,
    pub effective_from: Position,
}

/// Where Macaulay2 keeps its files, as its startup code defines it
/// (`startup.m2.in`, `system.m2`, `last.m2` and the end of `Core.m2`) from the
/// default directories of its `configure`. These are the values its `Layout`
/// table, `applicationDirectory` and `path` are built from; directories are
/// relative to the directory each one's documentation names.
pub struct Macaulay2Conventions;

impl Macaulay2Conventions {
    /// The command Macaulay2 is run by.
    pub const EXECUTABLE: &str = "M2";

    /// The directories Macaulay2 searches for its executable after those of
    /// `PATH`.
    pub const EXECUTABLE_FALLBACK_DIRECTORIES: [&str; 2] = ["/bin", "/usr/bin"];

    /// `Layout#1#"bin"`, the executables relative to a prefix; under
    /// `Layout#2` the same directory sits in the machine directory.
    pub const BIN: &str = "bin";

    /// `Layout#1#"packages"`, the package sources relative to a prefix: each
    /// package's main file `PKG.m2`, beside the directory `PKG/` of its other
    /// files (`Layout#1#"package"`).
    pub const PACKAGES: &str = "share/Macaulay2";

    /// `Layout#1#"packagelib"` without its package: the architecture-dependent
    /// package files, which with the package sources identify a layout.
    pub const PACKAGE_LIBRARIES: &str = "lib/Macaulay2";

    /// What `Layout#2` puts in place of `${prefix}`, holding the
    /// architecture-independent files; it puts the machine directory
    /// (`version#"machine"`) in place of `${exec_prefix}`.
    pub const ARCHITECTURE_INDEPENDENT: &str = "common";

    /// The prefix a build directory (`topBuilddir`) installs into.
    pub const BUILD_PREFIX: &str = "usr-dist";

    /// The file of a build directory whose first line names its source
    /// directory (`topSrcdir`), relative to the build directory.
    pub const SOURCE_DIRECTORY_RECORD: &str = "srcdir";

    /// Core's files relative to a source or build directory.
    pub const CORE_SOURCES: &str = "Macaulay2/m2";

    /// The package sources relative to a source directory.
    pub const PACKAGE_SOURCES: &str = "Macaulay2/packages";

    /// The package made of Macaulay2's basic files, which loads from
    /// `corepath` rather than from `path`.
    pub const CORE: &str = "Core";

    /// `applicationDirectorySuffix`, the user's application directory relative
    /// to `HOME`.
    #[cfg(not(target_os = "macos"))]
    pub const APPLICATION_DIRECTORY_SUFFIX: &str = ".Macaulay2";
    #[cfg(target_os = "macos")]
    pub const APPLICATION_DIRECTORY_SUFFIX: &str = "Library/Application Support/Macaulay2";

    /// The application directory's source directory, first on `path`.
    pub const APPLICATION_CODE: &str = "code";

    /// The prefix inside the application directory that `installPackage`
    /// installs into; it always has `Layout#1`.
    pub const APPLICATION_PREFIX: &str = "local";

    /// The initialization file in the application directory.
    pub const INITIALIZATION_FILE: &str = "init.m2";
}

/// One of the two arrangements of the files under a Macaulay2 prefix, the
/// values of its `Layout` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// `Layout#1`, every file under the prefix itself: an installation.
    Unified,
    /// `Layout#2`, the architecture-dependent files under the directory of
    /// `machine` and the others under `common`: a build tree (`usr-dist`).
    SeparateExec { machine: OsString },
}

impl Layout {
    /// The directory of the architecture-independent files, relative to the
    /// prefix.
    fn architecture_independent(&self) -> PathBuf {
        match self {
            Self::Unified => PathBuf::new(),
            Self::SeparateExec { .. } => {
                PathBuf::from(Macaulay2Conventions::ARCHITECTURE_INDEPENDENT)
            }
        }
    }

    /// The directory of the architecture-dependent files, relative to the
    /// prefix.
    fn architecture_dependent(&self) -> PathBuf {
        match self {
            Self::Unified => PathBuf::new(),
            Self::SeparateExec { machine } => PathBuf::from(machine),
        }
    }

    /// `"packages"`: the package sources, relative to the prefix.
    pub fn packages(&self) -> PathBuf {
        self.architecture_independent()
            .join(Macaulay2Conventions::PACKAGES)
    }

    /// `"package"`: the files of `package` other than its main file, relative
    /// to the prefix.
    pub fn package(&self, package: &str) -> PathBuf {
        self.packages().join(package)
    }

    /// `"packagelib"` without its package, relative to the prefix.
    fn package_libraries(&self) -> PathBuf {
        self.architecture_dependent()
            .join(Macaulay2Conventions::PACKAGE_LIBRARIES)
    }

    /// Whether `prefix` has files in this layout: both its package sources and
    /// its package libraries, as `detectCurrentLayout` tests.
    fn is_layout_of(&self, prefix: &Path) -> bool {
        prefix.join(self.packages()).is_dir() && prefix.join(self.package_libraries()).is_dir()
    }

    /// The layout of the files under `prefix`, detected as
    /// `detectCurrentLayout` detects it: `Layout#1` first, then `Layout#2`,
    /// whose machine is the directory beside `common` that holds it.
    pub fn of_prefix(prefix: &Path) -> Option<Self> {
        let separate_exec = || {
            fs::read_dir(prefix)
                .ok()?
                .flatten()
                .map(|entry| entry.file_name())
                .filter(|name| name != Macaulay2Conventions::ARCHITECTURE_INDEPENDENT)
                .map(|machine| Self::SeparateExec { machine })
                .find(|layout| layout.is_layout_of(prefix))
        };
        Some(Self::Unified)
            .filter(|layout| layout.is_layout_of(prefix))
            .or_else(separate_exec)
    }

    /// The prefix and layout of the executables directory `bin`, decided as
    /// Macaulay2's startup decides them: by which layout's `"bin"` ends the
    /// path, `Layout#2`'s first. Macaulay2 knows its machine from
    /// `version#"machine"`; here the directory above `bin` is that machine
    /// when the prefix above it has files in `Layout#2`.
    fn of_bin_directory(bin: &Path) -> Option<(PathBuf, Self)> {
        if bin.file_name()? != Macaulay2Conventions::BIN {
            return None;
        }
        let machine_directory = bin.parent()?;
        let separate_exec = machine_directory
            .parent()
            .zip(machine_directory.file_name())
            .map(|(prefix, machine)| {
                let machine = machine.to_os_string();
                (prefix.to_path_buf(), Self::SeparateExec { machine })
            })
            .filter(|(prefix, layout)| layout.is_layout_of(prefix));
        separate_exec.or_else(|| Some((machine_directory.to_path_buf(), Self::Unified)))
    }
}

/// A Macaulay2 installation as Macaulay2's startup finds it
/// (`initcurrentlayout`): a prefix with its layout, and, for a build tree, the
/// build and source directories it was made from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    /// `prefixDirectory`.
    prefix: PathBuf,
    /// `currentLayout`.
    layout: Layout,
    /// `topBuilddir`, the build directory whose `usr-dist` the prefix is.
    build_directory: Option<PathBuf>,
    /// `topSrcdir`, the source directory the build directory records.
    source_directory: Option<PathBuf>,
}

impl Installation {
    /// The installation that `executable` runs from, found as Macaulay2 finds
    /// its own ("how Macaulay2 finds its files"): with symbolic links in its
    /// directory resolved, that directory is the `"bin"` of a prefix. When
    /// `executable` itself is a symbolic link, the prefix of its target is
    /// taken instead if that one has package sources and the first does not.
    pub fn of_executable(executable: &Path) -> Option<Self> {
        let directory = fs::canonicalize(executable.parent()?).ok()?;
        let direct = Layout::of_bin_directory(&directory);
        let linked = fs::read_link(directory.join(executable.file_name()?))
            .ok()
            .and_then(|target| Layout::of_bin_directory(directory.join(target).parent()?));
        // Macaulay2 recognizes a build tree by either prefix, even the one it
        // does not take.
        let build_directory = [&direct, &linked]
            .into_iter()
            .flatten()
            .find_map(|(prefix, _)| Self::build_directory_of(prefix));
        let has_package_sources =
            |(prefix, layout): &(PathBuf, Layout)| prefix.join(layout.packages()).is_dir();
        let (prefix, layout) = match (direct, linked) {
            (direct, Some(linked))
                if has_package_sources(&linked)
                    && !direct.as_ref().is_some_and(has_package_sources) =>
            {
                linked
            }
            (direct, _) => direct?,
        };
        Some(Self::new(prefix, layout, build_directory))
    }

    /// The installation under `prefix`, in the layout its files have: what
    /// Macaulay2 started with `--prefix` uses.
    pub fn of_prefix(prefix: &Path) -> Option<Self> {
        let layout = Layout::of_prefix(prefix)?;
        Some(Self::new(
            prefix.to_path_buf(),
            layout,
            Self::build_directory_of(prefix),
        ))
    }

    /// The installation a configured path names: the one an `M2` executable
    /// runs from, or the one under a prefix.
    pub fn at(path: &Path) -> Option<Self> {
        if path.is_file() {
            Self::of_executable(path)
        } else {
            Self::of_prefix(path)
        }
    }

    fn new(prefix: PathBuf, layout: Layout, build_directory: Option<PathBuf>) -> Self {
        let source_directory = build_directory
            .as_deref()
            .and_then(Self::source_directory_of);
        Self {
            prefix,
            layout,
            build_directory,
            source_directory,
        }
    }

    /// `topBuilddir`: the directory whose `usr-dist` is `prefix`.
    fn build_directory_of(prefix: &Path) -> Option<PathBuf> {
        (prefix.file_name()? == Macaulay2Conventions::BUILD_PREFIX)
            .then(|| prefix.parent().map(Path::to_path_buf))?
    }

    /// `topSrcdir`: the directory named, relative to `build_directory`, by the
    /// first line of its source-directory record.
    fn source_directory_of(build_directory: &Path) -> Option<PathBuf> {
        let record =
            fs::read_to_string(build_directory.join(Macaulay2Conventions::SOURCE_DIRECTORY_RECORD))
                .ok()?;
        fs::canonicalize(build_directory.join(record.lines().next()?)).ok()
    }

    /// The installation's `corepath`, where Core's files load from: the Core
    /// sources of the source and build directories ahead of the installed
    /// Core package.
    fn core_path(&self) -> impl Iterator<Item = PathBuf> + '_ {
        self.source_directory
            .iter()
            .chain(&self.build_directory)
            .map(|directory| directory.join(Macaulay2Conventions::CORE_SOURCES))
            .chain(std::iter::once(
                self.prefix
                    .join(self.layout.package(Macaulay2Conventions::CORE)),
            ))
    }

    /// The installation's `pkgpath`, its part of `path`: the package sources
    /// of the source directory ahead of the installed ones.
    fn package_path(&self) -> impl Iterator<Item = PathBuf> + '_ {
        self.source_directory
            .iter()
            .map(|directory| directory.join(Macaulay2Conventions::PACKAGE_SOURCES))
            .chain(std::iter::once(self.prefix.join(self.layout.packages())))
    }
}

/// The environment that decides where Macaulay2's files are: what Macaulay2
/// itself reads to find them, plus the server's own source override.
#[derive(Debug, Clone, Default)]
pub struct Macaulay2Environment {
    /// `M2_LSP_SOURCE_PATH`: directories searched before Macaulay2's own.
    pub source_path_override: Option<OsString>,
    /// The directories searched for the `M2` executable, in order.
    pub executable_search_path: Vec<PathBuf>,
    /// `HOME`, which holds the user's application directory.
    pub home: Option<PathBuf>,
}

impl Macaulay2Environment {
    /// The environment of the running server. Its executable search path is
    /// Macaulay2's own: `PATH`, then the fallback directories. Relative `PATH`
    /// entries name directories of the shell Macaulay2 would be started from,
    /// which the server cannot know, so they are left out.
    pub fn current() -> Self {
        let path = env::var_os("PATH").unwrap_or_default();
        Self {
            source_path_override: env::var_os("M2_LSP_SOURCE_PATH"),
            executable_search_path: env::split_paths(&path)
                .chain(Macaulay2Conventions::EXECUTABLE_FALLBACK_DIRECTORIES.map(PathBuf::from))
                .filter(|directory| directory.is_absolute())
                .collect(),
            home: env::var_os("HOME").map(PathBuf::from),
        }
    }

    /// The `M2` executable that comes first on the executable search path.
    fn executable(&self) -> Option<PathBuf> {
        self.executable_search_path
            .iter()
            .map(|directory| directory.join(Macaulay2Conventions::EXECUTABLE))
            .find(|candidate| candidate.is_file())
    }

    /// The user's application directory (`applicationDirectory()`), where
    /// Macaulay2 keeps `init.m2` and locally installed packages.
    fn application_directory(&self) -> Option<PathBuf> {
        self.home
            .as_deref()
            .map(|home| home.join(Macaulay2Conventions::APPLICATION_DIRECTORY_SUFFIX))
    }

    /// `userpath`, which Macaulay2 puts ahead of the installation's part of
    /// `path`: the application directory's source directory, then the package
    /// sources of its local prefix.
    fn user_path(&self) -> Vec<PathBuf> {
        let Some(application_directory) = self.application_directory() else {
            return Vec::new();
        };
        vec![
            application_directory.join(Macaulay2Conventions::APPLICATION_CODE),
            application_directory
                .join(Macaulay2Conventions::APPLICATION_PREFIX)
                .join(Layout::Unified.packages()),
        ]
    }

    /// `path` with a leading `~` standing for `HOME`, as a shell reads it.
    fn expand_home(&self, path: &Path) -> PathBuf {
        match (path.strip_prefix("~"), self.home.as_deref()) {
            (Ok(relative), Some(home)) => home.join(relative),
            _ => path.to_path_buf(),
        }
    }

    /// The user's initialization file, which every Macaulay2 session started
    /// without `-q` runs before loading its preloaded packages.
    pub fn initialization_file(&self) -> Option<PathBuf> {
        self.application_directory()
            .map(|directory| directory.join(Macaulay2Conventions::INITIALIZATION_FILE))
            .filter(|file| file.is_file())
    }
}

/// The user's `init.m2`. It runs before any document, as if it were imported
/// on the first line of every file: its packages resolve like any other
/// import and its global definitions are known in every file.
#[derive(Debug)]
pub struct InitializationFile {
    uri: Url,
    text: String,
}

impl InitializationFile {
    /// The initialization file of `environment`, with the packages it loads
    /// registered in `registry` as imported from the start of every document.
    pub fn load(environment: &Macaulay2Environment, registry: &mut ObjectRegistry) -> Option<Self> {
        let path = environment.initialization_file()?;
        let text = fs::read_to_string(&path).ok()?;
        let uri = Url::from_file_path(&path).ok()?;
        collect_imported_packages(&text)
            .into_iter()
            .map(|import| import.package)
            .chain(preloaded_package_additions(&text))
            .for_each(|package| {
                registry.load_package(&package);
            });
        Some(Self { uri, text })
    }

    /// Index its global definitions into `workspace_index`, making them known
    /// in every file like those of a workspace file.
    pub fn index_into(&self, workspace_index: &WorkspaceIndex, registry: &ObjectRegistry) {
        workspace_index.index_file(&self.uri, &self.text, registry);
    }
}

/// Where Macaulay2's library sources are, as the two search lists Macaulay2
/// itself loads them through.
#[derive(Debug, Clone, Default)]
pub struct SourceResolver {
    /// `corepath`: the directories Core's files load from.
    core_path: Vec<PathBuf>,
    /// `path` once Core is loaded: the directories packages load from.
    path: Vec<PathBuf>,
}

impl SourceResolver {
    /// The search lists of the Macaulay2 that runs as `M2`, found without
    /// running it the way its documentation describes ("how Macaulay2 finds
    /// its files") and its startup code computes them: `corepath` from the
    /// installation, and `path` as `userpath` followed by the installation's
    /// package sources. Ahead of those come `configured_paths` and then
    /// `M2_LSP_SOURCE_PATH`, for directories added in `init.m2`; a configured
    /// executable or prefix stands for an installation the search cannot see
    /// (behind an `M2` wrapper script, say) and is searched before the found
    /// one.
    pub fn discover(environment: &Macaulay2Environment, configured_paths: &[PathBuf]) -> Self {
        let mut installations = Vec::new();
        let mut path = Vec::new();
        for configured in configured_paths
            .iter()
            .map(|configured| environment.expand_home(configured))
        {
            match Installation::at(&configured) {
                Some(installation) => installations.push(installation),
                None => path.push(configured),
            }
        }
        if let Some(paths) = environment.source_path_override.as_deref() {
            path.extend(env::split_paths(paths));
        }
        path.extend(environment.user_path());
        installations.extend(
            environment
                .executable()
                .and_then(|executable| Installation::of_executable(&executable)),
        );
        path.extend(installations.iter().flat_map(Installation::package_path));
        let core_path = installations.iter().flat_map(Installation::core_path);
        Self {
            core_path: Self::existing_directories(core_path),
            path: Self::existing_directories(path),
        }
    }

    /// The directories of `search_list` that exist, each once, in order: the
    /// directories Macaulay2 keeps of a search list it builds.
    fn existing_directories(search_list: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
        let mut directories = Vec::new();
        for directory in search_list {
            if directory.is_dir() && !directories.contains(&directory) {
                directories.push(directory);
            }
        }
        directories
    }

    /// A package's main file `PKG.m2`, the first on the search list it loads
    /// through: `corepath` for Core, `path` for every other package.
    pub fn resolve_package_file(&self, package_name: &str) -> Option<PathBuf> {
        let search_list = if package_name == Macaulay2Conventions::CORE {
            &self.core_path
        } else {
            &self.path
        };
        let main_file = format!("{package_name}.m2");
        search_list
            .iter()
            .map(|directory| directory.join(&main_file))
            .find(|candidate| candidate.is_file())
    }

    /// The package whose sources contain `path`: Core for a file on
    /// `corepath`; otherwise the package whose main file `PKG.m2` sits in a
    /// directory of `path` beside its directory `PKG/`, which holds the rest
    /// of its files.
    pub fn package_of_source_file(&self, path: &Path) -> Option<ObjectName> {
        if self
            .core_path
            .iter()
            .any(|directory| path.starts_with(directory))
        {
            return self
                .resolve_package_file(Macaulay2Conventions::CORE)
                .map(|_| ObjectName::new(Macaulay2Conventions::CORE));
        }
        self.path.iter().find_map(|root| {
            let first_component = path.strip_prefix(root).ok()?.components().next()?;
            let first_component = Path::new(first_component.as_os_str());
            let package_name = match first_component.extension() {
                Some(extension) if extension == "m2" => first_component.file_stem()?,
                Some(_) => return None,
                None => first_component.as_os_str(),
            };
            let package_name = package_name.to_str()?;
            self.resolve_package_file(package_name)
                .map(|_| ObjectName::new(package_name))
        })
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

/// The packages `init.m2` adds to `Core#"preloaded packages"`. Macaulay2 loads
/// that list right after running `init.m2`, so this assignment is a way of
/// loading packages that no import call shows. Replacing the list can also
/// drop default packages, but their dependencies are unknown here, so the
/// defaults stay: a name kept visible is never wrongly reported unknown.
fn preloaded_package_additions(text: &str) -> Vec<ObjectName> {
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
        && table.text() == Macaulay2Conventions::CORE
        && key.string_literal_inner_text() == Some(PRELOADED_PACKAGES_KEY);
    names_preloaded_packages.then(|| node.child_by_field_name("right"))?
}

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
    first_call_argument(application, is_package_import_trigger)
}

/// The function names whose string argument names a source file to run.
fn is_file_load_trigger(name: &str) -> bool {
    matches!(name, "load" | "needs" | "input")
}

/// The file paths a tree runs through `load`/`needs`/`input`, as written.
pub fn collect_loaded_files_in_tree<'tree>(
    root: M2Node<'tree>,
    syntax: Option<&SourceFile>,
) -> Vec<&'tree str> {
    let mut files = Vec::new();
    visit_source_nodes(root, syntax, |application| {
        if let Some(path) = first_call_argument(application, is_file_load_trigger)
            .and_then(|argument| argument.string_literal_inner_text())
        {
            files.push(path);
        }
    });
    files
}

/// The function that creates a package and binds its name as a global symbol.
const PACKAGE_CONSTRUCTOR: &str = "newPackage";

/// The name string of `newPackage "P"` / `newPackage("P", …)`, which binds the
/// global symbol `P` to the new package wherever the call runs.
pub fn declared_package_name(application: M2Node<'_>) -> Option<M2Node<'_>> {
    first_call_argument(application, |name| name == PACKAGE_CONSTRUCTOR).filter(|argument| {
        argument
            .string_literal_inner_text()
            .is_some_and(|name| !name.is_empty())
    })
}

/// The first argument of `callee ARG` / `callee(ARG, …)` when `callee` is
/// accepted by `is_trigger`.
fn first_call_argument(
    application: M2Node<'_>,
    is_trigger: impl Fn(&str) -> bool,
) -> Option<M2Node<'_>> {
    binary_expression_left_symbol(application).filter(|name| is_trigger(name))?;
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
    use std::path::PathBuf;
    use std::{env, fs};

    use super::{
        collect_imported_packages, package_source_string, preloaded_package_additions, M2Parser,
        Macaulay2Conventions, Macaulay2Environment, ObjectName, SourceResolver,
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

    /// Writes each file of `files` under `root`, creating its directories.
    fn write_tree(root: &std::path::Path, files: &[&str]) {
        for file in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().expect("fixture files have a directory"))
                .expect("fixture directory");
            fs::write(path, "").expect("fixture file");
        }
    }

    /// Macaulay2 finds its files from the `M2` on `PATH` ("how Macaulay2 finds
    /// its files"): the directory of the executable is the `bin` of a prefix
    /// in one of its two layouts, and an executable linked from elsewhere
    /// leads to its target's prefix. `path` puts the application directory
    /// ahead of the installation, and Core loads from `corepath`.
    #[cfg(unix)]
    #[test]
    fn source_resolver_finds_sources_the_way_macaulay2_finds_its_files() {
        let root = env::temp_dir().join(format!("m2-lsp-source-discovery-{}", std::process::id()));
        let installed = root.join("installed");
        write_tree(
            &installed,
            &[
                "bin/M2",
                "lib/Macaulay2/JSON/.keep",
                "share/Macaulay2/Core/Core.m2",
                "share/Macaulay2/Core/classes.m2",
                "share/Macaulay2/Graphs.m2",
            ],
        );
        let linked = root.join("linked-bin");
        fs::create_dir_all(&linked).expect("link directory");
        std::os::unix::fs::symlink(installed.join("bin/M2"), linked.join("M2"))
            .expect("link to the installed executable");

        let build_tree = root.join("usr-dist");
        write_tree(
            &build_tree,
            &[
                "x86_64-Linux/bin/M2",
                "x86_64-Linux/lib/Macaulay2/JSON/.keep",
                "common/share/Macaulay2/JSON.m2",
            ],
        );

        let home = root.join("home");
        let local_packages = home
            .join(Macaulay2Conventions::APPLICATION_DIRECTORY_SUFFIX)
            .join("local/share/Macaulay2");
        write_tree(&local_packages, &["Mine.m2", "Graphs.m2"]);

        let linked_installation = SourceResolver::discover(
            &Macaulay2Environment {
                source_path_override: None,
                executable_search_path: vec![linked.clone(), build_tree.join("x86_64-Linux/bin")],
                home: Some(home.clone()),
            },
            &[],
        );
        assert_eq!(
            linked_installation.resolve_package_file("Core"),
            Some(installed.join("share/Macaulay2/Core/Core.m2")),
            "the linked executable leads to its installed prefix, whose Core is on corepath"
        );
        assert_eq!(
            linked_installation.resolve_package_file("JSON"),
            None,
            "only the first M2 on PATH is the installation"
        );
        assert_eq!(
            linked_installation.resolve_package_file("Mine"),
            Some(local_packages.join("Mine.m2"))
        );
        assert_eq!(
            linked_installation.resolve_package_file("Graphs"),
            Some(local_packages.join("Graphs.m2")),
            "userpath comes ahead of the installation's packages"
        );
        assert_eq!(
            linked_installation
                .package_of_source_file(&installed.join("share/Macaulay2/Core/classes.m2")),
            Some(ObjectName::new("Core"))
        );

        let build_tree_installation = SourceResolver::discover(
            &Macaulay2Environment {
                source_path_override: None,
                executable_search_path: vec![build_tree.join("x86_64-Linux/bin")],
                home: None,
            },
            &[],
        );
        assert_eq!(
            build_tree_installation.resolve_package_file("JSON"),
            Some(build_tree.join("common/share/Macaulay2/JSON.m2")),
            "a bin in a machine directory beside `common` is Layout#2's"
        );

        let configured = SourceResolver::discover(
            &Macaulay2Environment {
                source_path_override: None,
                executable_search_path: Vec::new(),
                home: Some(root.clone()),
            },
            &[PathBuf::from("~/usr-dist"), installed.join("bin/M2")],
        );
        assert_eq!(
            configured.resolve_package_file("JSON"),
            Some(build_tree.join("common/share/Macaulay2/JSON.m2")),
            "a configured prefix under `~` has its layout detected from its files"
        );
        assert_eq!(
            configured.resolve_package_file("Core"),
            Some(installed.join("share/Macaulay2/Core/Core.m2")),
            "a configured executable leads to its installation"
        );
        assert_eq!(
            configured.package_of_source_file(&build_tree.join("common/share/Macaulay2/JSON.m2")),
            Some(ObjectName::new("JSON"))
        );
        assert_eq!(
            configured.package_of_source_file(&root.join("elsewhere.m2")),
            None
        );

        let _ = fs::remove_dir_all(root);
    }

    /// A build tree's prefix is the `usr-dist` of its build directory, which
    /// records its source directory; Macaulay2 loads Core and packages from
    /// the sources there ahead of the built copies.
    #[test]
    fn source_resolver_prefers_the_sources_a_build_tree_records() {
        let root = env::temp_dir().join(format!("m2-lsp-source-tree-{}", std::process::id()));
        let source = root.join("M2");
        let build = root.join("build");
        write_tree(
            &source,
            &[
                "Macaulay2/m2/Core.m2",
                "Macaulay2/m2/classes.m2",
                "Macaulay2/packages/Graphs.m2",
            ],
        );
        write_tree(
            &build,
            &[
                "usr-dist/x86_64-Linux/bin/M2",
                "usr-dist/x86_64-Linux/lib/Macaulay2/Graphs/.keep",
                "usr-dist/common/share/Macaulay2/Core/Core.m2",
                "usr-dist/common/share/Macaulay2/Graphs.m2",
            ],
        );
        fs::write(build.join("srcdir"), "../M2\n").expect("source directory record");

        let resolver = SourceResolver::discover(
            &Macaulay2Environment {
                source_path_override: None,
                executable_search_path: vec![build.join("usr-dist/x86_64-Linux/bin")],
                home: None,
            },
            &[],
        );
        let source = fs::canonicalize(&source).expect("source directory");
        assert_eq!(
            resolver.resolve_package_file("Core"),
            Some(source.join("Macaulay2/m2/Core.m2"))
        );
        assert_eq!(
            resolver.resolve_package_file("Graphs"),
            Some(source.join("Macaulay2/packages/Graphs.m2"))
        );
        assert_eq!(
            resolver.package_of_source_file(&source.join("Macaulay2/m2/classes.m2")),
            Some(ObjectName::new("Core"))
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn initialization_file_is_found_in_the_application_directory() {
        let home = env::temp_dir().join(format!("m2-lsp-init-file-{}", std::process::id()));
        let application_directory = home.join(Macaulay2Conventions::APPLICATION_DIRECTORY_SUFFIX);
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
