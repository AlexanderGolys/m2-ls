//! Imported-package discovery and configured source-file resolution.

use crate::macaulay2::Macaulay2Conventions;
use crate::node_metadata::TypedSource;
use crate::telemetry::Timing;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::{env, iter, str, thread};

use m2_syn::nodes::Sequence;
use tower_lsp::lsp_types::{Location, Position, Range as TextRange, Url};

#[cfg(test)]
use crate::node_metadata::SyntaxParser;
use crate::node_metadata::{visit_source_nodes, SyntaxNode};
use crate::object_registry::ObjectName;
use crate::settings::SourceSettings;
use crate::source::SourceNavigation;

// Wrapper discovery must finish even when the runtime cannot start.
/// How long an `M2` hidden behind a wrapper may take to print its prefix before
/// discovery gives up on it.
const INSTALLATION_QUERY_TIMEOUT: Duration = Duration::from_secs(2);
/// How often the installation query checks whether `M2` has exited.
const INSTALLATION_QUERY_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// One package inclusion and the source position from which it takes effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageImport {
    /// The package imported.
    pub package: ObjectName,
    /// Where its names become visible: right after the import call.
    pub effective_from: Position,
}

/// One of the two arrangements of the files under a Macaulay2 prefix, the
/// values of its `Layout` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// `Layout#1`, every file under the prefix itself: an installation.
    Unified,
    /// `Layout#2`, the architecture-dependent files under the directory of
    /// `machine` and the others under `common`: a build tree (`usr-dist`).
    SeparateExec {
        /// The machine directory's name, `version#"machine"`.
        machine: OsString,
    },
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
            .join(Macaulay2Conventions::PKG)
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
    /// If neither exposes sources, query the executable for its prefix.
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
            (Some(direct), _) => direct,
            (None, _) => return Self::query_executable(executable),
        };
        let installation = Self::new(prefix, layout, build_directory);
        if installation
            .package_path()
            .any(|directory| directory.is_dir())
        {
            Some(installation)
        } else {
            Self::query_executable(executable)
        }
    }

    /// Ask an executable hidden behind a wrapper for its installation prefix.
    /// Skip user initialization and bound the wait so an unavailable runtime
    /// does not prevent the language server from starting.
    fn query_executable(executable: &Path) -> Option<Self> {
        let mut timing = Timing::start(
            "sources.query_installation",
            serde_json::json!({"executable": executable}),
        );
        let mut child = Command::new(executable)
            .args(Macaulay2Conventions::INSTALLATION_QUERY_ARGUMENTS)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() < INSTALLATION_QUERY_TIMEOUT => {
                    thread::sleep(INSTALLATION_QUERY_POLL_INTERVAL);
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    timing.error("installation query timed out or failed");
                    return None;
                }
            }
        }
        let output = child.wait_with_output().ok()?;
        if !output.status.success() {
            timing.error(format!("installation query exited with {}", output.status));
            return None;
        }
        let prefix = str::from_utf8(&output.stdout).ok()?.trim();
        let prefix = Path::new(prefix);
        prefix.is_absolute().then(|| Self::of_prefix(prefix))?
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

    /// The installation under `prefix` in `layout`, with the source directory
    /// its build directory records, if any.
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
            fs::read_to_string(build_directory.join(Macaulay2Conventions::SRC_DIR)).ok()?;
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
            .chain(iter::once(
                self.prefix
                    .join(self.layout.package(Macaulay2Conventions::CORE)),
            ))
    }

    /// The installation's `pkgpath`, its part of `path`: the package sources
    /// of the source directory ahead of the installed ones.
    fn package_path(&self) -> impl Iterator<Item = PathBuf> + '_ {
        self.source_directory
            .iter()
            .map(|directory| directory.join(Macaulay2Conventions::PACKAGES_DIR))
            .chain(iter::once(self.prefix.join(self.layout.packages())))
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
    /// The user's home directory, holding the application directory.
    pub home: Option<PathBuf>,
}

impl Macaulay2Environment {
    /// The environment of the running server. Only directories explicitly
    /// present in `PATH` are searched for an executable. Relative `PATH`
    /// entries name directories of the shell Macaulay2 would be started from,
    /// which the server cannot know, so they are left out.
    pub fn current() -> Self {
        let path = env::var_os("PATH").unwrap_or_default();
        #[cfg(windows)]
        let home = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME"));
        #[cfg(not(windows))]
        let home = env::var_os("HOME");
        Self {
            source_path_override: env::var_os(Macaulay2Conventions::SOURCE_PATH_VARIABLE),
            executable_search_path: env::split_paths(&path)
                .filter(|directory| directory.is_absolute())
                .collect(),
            home: home.map(PathBuf::from),
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
    pub fn expand_home(&self, path: &Path) -> PathBuf {
        match (path.strip_prefix("~"), self.home.as_deref()) {
            (Ok(relative), Some(home)) => home.join(relative),
            _ => path.to_path_buf(),
        }
    }

    /// Where the user's init file is, whether or not it exists: every
    /// Macaulay2 session started without `-q` runs it before loading its
    /// preloaded packages.
    pub fn init_file_path(&self) -> Option<PathBuf> {
        self.application_directory()
            .map(|directory| directory.join(Macaulay2Conventions::INITIALIZATION_FILE))
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
    /// Configured directories whose files implement the library, beside Core's.
    primary_code_directories: Vec<PathBuf>,
}

impl SourceResolver {
    /// The search lists of the Macaulay2 that runs as `M2`, found the way
    /// its documentation describes ("how Macaulay2 finds its files") and
    /// its startup code computes them: `corepath` from the
    /// installation, and `path` as `userpath` followed by the installation's
    /// package sources. Ahead of those come `configured_paths` and then
    /// `M2_LSP_SOURCE_PATH`, for directories added in `init.m2`; a configured
    /// executable or prefix stands for an installation the search cannot see
    /// (behind an `M2` wrapper script, say) and is searched before the found
    /// one. If the executable is a wrapper hiding its installation, query its
    /// `prefixDirectory` with user initialization disabled.
    pub fn discover(environment: &Macaulay2Environment, configured_paths: &[PathBuf]) -> Self {
        let _timing = Timing::start(
            "sources.discover",
            serde_json::json!({"configured_paths": configured_paths}),
        );
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
            primary_code_directories: Vec::new(),
        }
    }

    /// The resolver `sources` configures: the discovered search lists, unless
    /// discovery is disabled, and the configured primary code directories.
    pub fn configured(environment: &Macaulay2Environment, sources: &SourceSettings) -> Self {
        let discovered = if sources.enabled {
            Self::discover(environment, &sources.paths)
        } else {
            Self::default()
        };
        Self {
            primary_code_directories: sources
                .primary_code_paths
                .iter()
                .map(|directory| environment.expand_home(directory))
                .collect(),
            ..discovered
        }
    }

    /// Whether the file at `path` implements the library itself: it lies on
    /// `corepath` or in a configured primary code directory, as written or
    /// once symbolic links are resolved.
    pub fn is_primary_code_file(&self, path: &Path) -> bool {
        let resolved_path = fs::canonicalize(path).ok();
        self.core_path
            .iter()
            .chain(&self.primary_code_directories)
            .any(|directory| {
                path.starts_with(directory)
                    || resolved_path
                        .as_deref()
                        .zip(fs::canonicalize(directory).ok())
                        .is_some_and(|(path, directory)| path.starts_with(directory))
            })
    }

    /// `path` once Core is loaded: the directories `load` searches for a file
    /// name it does not find beside the loading file.
    pub fn path(&self) -> &[PathBuf] {
        &self.path
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
        let main_file = format!("{package_name}.{}", Macaulay2Conventions::SOURCE_EXTENSION);
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
                Some(extension) if extension == Macaulay2Conventions::SOURCE_EXTENSION => {
                    first_component.file_stem()?
                }
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

/// The function names whose string argument names a package to import.
fn is_package_import_trigger(name: &str) -> bool {
    Macaulay2Conventions::PACKAGE_IMPORT_FUNCTIONS.contains(&name)
}

/// The package name a string literal gives an import, `"Graphs"` in
/// `needsPackage "Graphs"`, without its delimiters.
pub fn package_source_string(node: SyntaxNode<'_>) -> Option<&str> {
    is_package_import_string(node).then(|| node.string_literal_inner_text())?
}

/// Whether `node` is the string naming the package of an import call or of a
/// `PackageImports`/`PackageExports` list.
pub fn is_package_import_string(node: SyntaxNode<'_>) -> bool {
    PackageLoadingOption::lists_package(node)
        || node.ancestors().any(|application| {
            package_import_argument(application).is_some_and(|argument| argument.id() == node.id())
        })
}

/// The string argument of the import call `application` with the package name
/// it holds.
fn package_import<'tree>(
    application: SyntaxNode<'tree>,
) -> Option<(SyntaxNode<'tree>, &'tree str)> {
    let argument = package_import_argument(application)?;
    Some((argument, argument.string_literal_inner_text()?))
}

/// The first argument of `application` when it calls an import function.
fn package_import_argument(application: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    first_call_argument(application, is_package_import_trigger)
}

/// The name string of `newPackage "P"` / `newPackage("P", …)`, which binds the
/// global symbol `P` to the new package wherever the call runs.
pub fn declared_package_name(application: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    first_call_argument(application, |name| {
        name == Macaulay2Conventions::PACKAGE_CONSTRUCTOR
    })
    .filter(|argument| {
        argument
            .string_literal_inner_text()
            .is_some_and(|name| !name.is_empty())
    })
}

/// A `newPackage` option listing packages the new package loads: right after
/// the call makes the package current, Macaulay2 runs `needsPackage` on each
/// of them, imports first, so the package's own code sees their exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageLoadingOption {
    /// `PackageImports`: loaded for the package's own code.
    Imports,
    /// `PackageExports`: loaded for the package's own code and, along with
    /// the package, into every session that loads it.
    Exports,
}

impl PackageLoadingOption {
    /// Every option, in the order Macaulay2 loads their packages.
    pub const ALL: [Self; 2] = [Self::Imports, Self::Exports];

    /// The option key as written in the `newPackage` call.
    pub fn key(self) -> &'static str {
        match self {
            Self::Imports => Macaulay2Conventions::PACKAGE_IMPORTS_OPTION,
            Self::Exports => Macaulay2Conventions::PACKAGE_EXPORTS_OPTION,
        }
    }

    /// The package-name strings `call` lists under the option, when `call` is
    /// a `newPackage` call. Macaulay2 accepts only a list as its value.
    pub fn package_names(self, call: SyntaxNode<'_>) -> Vec<SyntaxNode<'_>> {
        if first_call_argument(call, |name| {
            name == Macaulay2Conventions::PACKAGE_CONSTRUCTOR
        })
        .is_none()
        {
            return Vec::new();
        }
        let Some(arguments) = call.binary_parts().map(|parts| parts.right) else {
            return Vec::new();
        };
        arguments
            .collection_elements()
            .filter(|option| option.is_option_assignment())
            .filter(|option| {
                option
                    .binary_parts()
                    .map(|parts| parts.left)
                    .is_some_and(|key| key.text() == self.key())
            })
            .filter_map(|option| option.binary_parts().map(|parts| parts.right))
            .filter(|packages| packages.is_collection_expression())
            .flat_map(|packages| packages.collection_elements().collect::<Vec<_>>())
            .filter(|package| package.string_literal_inner_text().is_some())
            .collect()
    }

    /// Whether the string `node` names a package some option of an enclosing
    /// `newPackage` call lists.
    pub fn lists_package(node: SyntaxNode<'_>) -> bool {
        node.ancestors().any(|call| {
            Self::ALL.into_iter().any(|option| {
                option
                    .package_names(call)
                    .iter()
                    .any(|package| package.id() == node.id())
            })
        })
    }
}

/// The first argument of `callee ARG` / `callee(ARG, …)` when `callee` is
/// accepted by `is_trigger`.
pub fn first_call_argument(
    application: SyntaxNode<'_>,
    is_trigger: impl Fn(&str) -> bool,
) -> Option<SyntaxNode<'_>> {
    binary_expression_left_symbol(application).filter(|name| is_trigger(name))?;
    let argument = application.binary_parts().map(|parts| parts.right)?;
    let argument = if argument.is::<Sequence>() || argument.is_holder() {
        argument.collection_elements().next()?
    } else {
        argument
    };
    Some(argument)
}

/// The packages `text` imports, each from the position its import takes effect.
#[cfg(test)]
pub fn collect_imported_packages(text: &str) -> Vec<PackageImport> {
    use crate::source::DocumentSource;

    let Some(mut parser) = SyntaxParser::new() else {
        return Vec::new();
    };
    let source = DocumentSource::new(text.to_string());
    let Some(tree) = parser.parse_tree(text, None) else {
        return Vec::new();
    };
    let syntax = Some(tree.syntax());
    collect_imported_packages_in_tree(tree.root(text), syntax, &source)
}

/// Walk an already-parsed tree for package-import calls, reusing the caller's
/// parse rather than spinning up a fresh parser. The snapshot keeps its tree
/// around, so per-version import collection costs one walk, not a re-parse.
pub fn collect_imported_packages_in_tree(
    root: SyntaxNode<'_>,
    syntax: Option<&TypedSource>,
    source: &(impl SourceNavigation + ?Sized),
) -> Vec<PackageImport> {
    let mut packages = Vec::new();
    visit_source_nodes(root, syntax, |application| {
        let effective_from = source.range_for_node(application).end;
        if let Some((_, package_name)) = package_import(application) {
            packages.push(PackageImport {
                package: ObjectName::new(package_name),
                effective_from,
            });
        }
        if application.applied_symbol_name() == Some(Macaulay2Conventions::BEGIN_DOCUMENTATION) {
            packages.extend(
                Macaulay2Conventions::DOCUMENTATION_PACKAGES
                    .into_iter()
                    .map(|package| PackageImport {
                        package: ObjectName::new(package),
                        effective_from,
                    }),
            );
        }
        packages.extend(
            PackageLoadingOption::ALL
                .into_iter()
                .flat_map(|option| option.package_names(application))
                .filter_map(|package| package.string_literal_inner_text())
                .map(|package| PackageImport {
                    package: ObjectName::new(package),
                    effective_from,
                }),
        );
    });

    packages
}

/// The left symbol of an application `callee ARG` (`needsPackage "Pkg"`): the
/// callee name, when `node` is a `SPACE` application whose left operand is a
/// bare symbol.
fn binary_expression_left_symbol(node: SyntaxNode<'_>) -> Option<&str> {
    node.applied_symbol_name()
}

#[cfg(test)]
mod import_trigger_tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::{env, fs};

    use super::{
        collect_imported_packages, package_source_string, Macaulay2Conventions,
        Macaulay2Environment, ObjectName, SourceResolver, SyntaxParser,
    };

    /// An isolated fixture tree with canonical paths and automatic cleanup.
    struct SourceFixture {
        root: PathBuf,
    }

    impl SourceFixture {
        fn new() -> Self {
            static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
            loop {
                let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                let root =
                    env::temp_dir().join(format!("m2-lsp-sources-{}-{id}", std::process::id()));
                match fs::create_dir(&root) {
                    Ok(()) => {
                        return Self {
                            root: fs::canonicalize(root).expect("fixture root"),
                        }
                    }
                    Err(error)
                        if std::io::Error::kind(&error) == std::io::ErrorKind::AlreadyExists =>
                    {
                        continue;
                    }
                    Err(error) => panic!("cannot create source fixture: {error}"),
                }
            }
        }

        fn path(&self) -> &Path {
            &self.root
        }
    }

    impl Drop for SourceFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

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
        let mut parser = SyntaxParser::new().expect("Macaulay2 parser should load");
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

    #[test]
    fn source_discovery_without_an_installation_is_optional() {
        let environment = Macaulay2Environment::default();
        let resolver = SourceResolver::discover(&environment, &[]);

        assert!(environment.executable().is_none());
        assert!(resolver.resolve_package_file("Core").is_none());
        assert!(resolver.resolve_package_file("Graphs").is_none());
        assert!(resolver.package_location("Graphs").is_none());
    }

    #[test]
    fn configured_sources_work_without_an_executable() {
        let fixture = SourceFixture::new();
        let configured = fixture.path().join("configured sources");
        let override_path = fixture.path().join("override sources");
        write_tree(&configured, &["Graphs.m2"]);
        write_tree(&override_path, &["Graphs.m2", "JSON.m2"]);
        let environment = Macaulay2Environment {
            source_path_override: Some(env::join_paths([&override_path]).unwrap()),
            ..Macaulay2Environment::default()
        };
        let resolver = SourceResolver::discover(&environment, std::slice::from_ref(&configured));

        assert_eq!(
            resolver.resolve_package_file("Graphs"),
            Some(configured.join("Graphs.m2"))
        );
        assert_eq!(
            resolver.resolve_package_file("JSON"),
            Some(override_path.join("JSON.m2"))
        );
        assert!(resolver.resolve_package_file("Core").is_none());
        assert_eq!(
            resolver.package_location("Graphs").unwrap().uri,
            tower_lsp::lsp_types::Url::from_file_path(configured.join("Graphs.m2")).unwrap()
        );
    }

    #[test]
    fn executable_discovery_uses_the_platform_name_and_path_order() {
        let fixture = SourceFixture::new();
        let first = fixture.path().join("first/bin");
        let second = fixture.path().join("second/bin");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let executable = second.join(Macaulay2Conventions::EXECUTABLE);
        fs::write(&executable, "").unwrap();
        let environment = Macaulay2Environment {
            executable_search_path: vec![first.clone(), second],
            ..Macaulay2Environment::default()
        };
        assert_eq!(environment.executable(), Some(executable));
        let earlier = first.join(Macaulay2Conventions::EXECUTABLE);
        fs::write(&earlier, "").unwrap();
        assert_eq!(environment.executable(), Some(earlier));
    }

    #[test]
    fn an_unusable_executable_does_not_prevent_configured_source_discovery() {
        let fixture = SourceFixture::new();
        let executable = fixture.path().join(Macaulay2Conventions::EXECUTABLE);
        fs::write(&executable, "not an executable").unwrap();
        let sources = fixture.path().join("sources");
        write_tree(&sources, &["Graphs.m2"]);
        let environment = Macaulay2Environment {
            executable_search_path: vec![fixture.path().to_path_buf()],
            ..Macaulay2Environment::default()
        };
        let resolver = SourceResolver::discover(&environment, std::slice::from_ref(&sources));
        assert_eq!(
            resolver.resolve_package_file("Graphs"),
            Some(sources.join("Graphs.m2"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn source_resolver_discovers_a_wrapped_installation() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = SourceFixture::new();
        let root = fixture.path();
        let installed = root.join("installed");
        write_tree(
            &installed,
            &[
                "lib/Macaulay2/.keep",
                "share/Macaulay2/Core/Core.m2",
                "share/Macaulay2/Text.m2",
            ],
        );
        let executable = root.join("wrapper/bin/M2");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\ntest \"$1\" = -q || exit 1\nprintf '%s\\n' '{}'\n",
                installed.to_str().unwrap().replace('\'', "'\\''")
            ),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let environment = Macaulay2Environment {
            executable_search_path: vec![executable.parent().unwrap().to_path_buf()],
            ..Macaulay2Environment::default()
        };
        let resolver = SourceResolver::discover(&environment, &[]);
        assert_eq!(
            resolver.resolve_package_file("Text"),
            Some(installed.join("share/Macaulay2/Text.m2"))
        );
        assert_eq!(
            resolver.resolve_package_file("Core"),
            Some(installed.join("share/Macaulay2/Core/Core.m2"))
        );

        fs::write(&executable, "#!/bin/sh\nexit 1\n").unwrap();
        let resolver = SourceResolver::discover(&environment, &[]);
        assert_eq!(resolver.resolve_package_file("Text"), None);
    }

    /// Macaulay2 finds its files from the `M2` on `PATH` ("how Macaulay2 finds
    /// its files"): the directory of the executable is the `bin` of a prefix
    /// in one of its two layouts, and an executable linked from elsewhere
    /// leads to its target's prefix. `path` puts the application directory
    /// ahead of the installation, and Core loads from `corepath`.
    #[cfg(unix)]
    #[test]
    fn source_resolver_finds_sources_the_way_macaulay2_finds_its_files() {
        let fixture = SourceFixture::new();
        let root = fixture.path();
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
                home: Some(home.to_path_buf()),
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
                home: Some(root.to_path_buf()),
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
    }

    /// A build tree's prefix is the `usr-dist` of its build directory, which
    /// records its source directory; Macaulay2 loads Core and packages from
    /// the sources there ahead of the built copies.
    #[test]
    fn source_resolver_prefers_the_sources_a_build_tree_records() {
        let fixture = SourceFixture::new();
        let root = fixture.path();
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
        fs::write(
            build
                .join("usr-dist/x86_64-Linux/bin")
                .join(Macaulay2Conventions::EXECUTABLE),
            "",
        )
        .expect("fixture executable");
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
    }

    #[test]
    fn initialization_file_is_in_the_application_directory() {
        let home = Path::new("/home/user");
        let environment = Macaulay2Environment {
            home: Some(home.to_path_buf()),
            ..Macaulay2Environment::default()
        };

        assert_eq!(
            environment.init_file_path(),
            Some(
                home.join(Macaulay2Conventions::APPLICATION_DIRECTORY_SUFFIX)
                    .join(Macaulay2Conventions::INITIALIZATION_FILE)
            )
        );
    }
}
