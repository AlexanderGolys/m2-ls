//! The init file a Macaulay2 session runs before any document: `init.m2` in
//! the application directory, or the file a setting or a directive selects
//! instead.
//!
//! Macaulay2 runs the init file as if it were imported on the first line of
//! every file the session runs: the packages it loads are loaded, and its
//! global definitions are known. The settings select the file every document
//! starts with (`sources.initFile`); a document's
//! `-- m2-ls: initFile …` comment selects another, or none, for that document
//! alone. Each selected file is read and analyzed once, and again whenever it
//! changes on disk.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use dashmap::DashMap;
use m2_syn::nodes::Symbol;
use m2_syn::Token;
use serde::Deserialize;
use tower_lsp::lsp_types::{Range as TextRange, Url};

use crate::diagnostics::suppression::{DIRECTIVE_MARKER, LINE_COMMENT};
use crate::document::DocumentSnapshot;
use crate::macaulay2::Macaulay2Conventions;
use crate::node_metadata::{matches_token, visit_source_nodes, SyntaxNode, TypedSource};
use crate::object_registry::{ObjectName, ObjectRegistry};
use crate::package_index::Macaulay2Environment;
use crate::source::SourceNavigation;

/// The directive command selecting the init file of a document.
const DIRECTIVE_COMMAND: &str = "initFile";

/// The directive argument starting a document without an init file.
const NO_INIT_FILE_ARGUMENT: &str = "none";

/// Which init file sessions start with: the value of `sources.initFile`, and
/// of a `-- m2-ls: initFile …` directive.
///
/// In JSON, `true` is [`Self::Discovered`], `false` [`Self::Disabled`], and a
/// string [`Self::Path`]. A directive writes `none` for [`Self::Disabled`] and
/// anything else for a path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum InitFileChoice {
    /// `init.m2` in the user's application directory, found along with the
    /// library sources: none while their discovery is disabled.
    #[default]
    Discovered,
    /// No init file.
    Disabled,
    /// The file at this path. A leading `~` is the home directory; a relative
    /// path is relative to the workspace folder for the setting, and to the
    /// directory of the document for a directive.
    Path(PathBuf),
}

impl<'de> Deserialize<'de> for InitFileChoice {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged, expecting = "`true`, `false`, or the path of a file")]
        enum Written {
            /// `true` or `false`.
            Enabled(bool),
            /// A string.
            Path(PathBuf),
        }
        match Written::deserialize(deserializer)? {
            Written::Enabled(true) => Ok(Self::Discovered),
            Written::Enabled(false) => Ok(Self::Disabled),
            Written::Path(path) if path.as_os_str().is_empty() => Err(serde::de::Error::custom(
                "empty path; `false` selects no init file",
            )),
            Written::Path(path) => Ok(Self::Path(path)),
        }
    }
}

/// A document's `-- m2-ls: initFile …` comment, selecting the init file its
/// session starts with in place of the configured one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitFileDirective {
    /// The file selected: [`InitFileChoice::Disabled`] or a path.
    pub choice: InitFileChoice,
    /// The comment.
    pub range: TextRange,
}

impl InitFileDirective {
    /// The first `initFile` directive comment under `root`: later ones are
    /// ignored.
    pub fn collect(
        root: SyntaxNode<'_>,
        source: &(impl SourceNavigation + ?Sized),
    ) -> Option<Self> {
        root.descendants()
            .filter(SyntaxNode::is_line_comment)
            .find_map(|comment| {
                let choice = Self::choice_in(comment.text())?;
                Some(Self {
                    choice,
                    range: source.range_for_node(comment),
                })
            })
    }

    /// The choice a comment written as `comment` makes, if it is an
    /// `initFile` directive with an argument.
    fn choice_in(comment: &str) -> Option<InitFileChoice> {
        let argument = comment
            .strip_prefix(LINE_COMMENT)?
            .trim_start()
            .strip_prefix(DIRECTIVE_MARKER)?
            .trim_start()
            .strip_prefix(DIRECTIVE_COMMAND)?
            .strip_prefix(char::is_whitespace)?
            .trim();
        match argument {
            "" => None,
            NO_INIT_FILE_ARGUMENT => Some(InitFileChoice::Disabled),
            path => Some(InitFileChoice::Path(PathBuf::from(path))),
        }
    }
}

/// Why an init file cannot be used: the file is not there, cannot be opened,
/// or is no text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitFileUnavailable {
    /// The file selected, as resolved.
    path: PathBuf,
    /// What reading it failed with.
    reason: io::ErrorKind,
}

impl fmt::Display for InitFileUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.path.display();
        match self.reason {
            io::ErrorKind::NotFound => write!(formatter, "there is no init file `{path}`"),
            io::ErrorKind::PermissionDenied => {
                write!(formatter, "the init file `{path}` may not be read")
            }
            io::ErrorKind::IsADirectory => {
                write!(formatter, "the init file `{path}` is a directory")
            }
            io::ErrorKind::InvalidData => {
                write!(formatter, "the init file `{path}` is not UTF-8 text")
            }
            reason => write!(
                formatter,
                "the init file `{path}` cannot be read ({reason})"
            ),
        }
    }
}

/// An init file as read and analyzed: what every session starting with it
/// knows before its first line.
pub struct InitFile {
    /// Where the file was read from.
    path: PathBuf,
    /// The file as a document of the workspace.
    uri: Url,
    /// The file analyzed as a document of its own, starting without an init
    /// file.
    snapshot: DocumentSnapshot,
    /// The packages it loads, which every session starting with it has
    /// loaded.
    packages: Vec<ObjectName>,
}

impl fmt::Debug for InitFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InitFile")
            .field("path", &self.path)
            .field("packages", &self.packages)
            .finish_non_exhaustive()
    }
}

impl InitFile {
    /// Read the file at `path` and analyze it against `registry`, without an
    /// init file of its own.
    pub fn read(path: &Path, registry: &ObjectRegistry) -> Result<Self, InitFileUnavailable> {
        let unavailable = |reason| InitFileUnavailable {
            path: path.to_path_buf(),
            reason,
        };
        let text =
            fs::read_to_string(path).map_err(|error| unavailable(io::Error::kind(&error)))?;
        let uri =
            Url::from_file_path(path).map_err(|()| unavailable(io::ErrorKind::InvalidInput))?;
        let snapshot = DocumentSnapshot::from_text(text, &registry.without_init_file())
            .expect("the Macaulay2 grammar parses any text");
        let mut packages = snapshot
            .imported_packages()
            .iter()
            .map(|import| import.package.clone())
            .collect::<Vec<_>>();
        packages.extend(preloaded_package_additions(
            snapshot.root_node(),
            snapshot.syntax(),
        ));
        Ok(Self {
            path: path.to_path_buf(),
            uri,
            snapshot,
            packages,
        })
    }

    /// Where the file was read from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file as a document of the workspace, which its diagnostics and
    /// definitions are published for.
    pub fn uri(&self) -> &Url {
        &self.uri
    }

    /// The file analyzed as a document of its own.
    pub fn snapshot(&self) -> &DocumentSnapshot {
        &self.snapshot
    }

    /// The packages every session starting with the file has loaded.
    pub fn packages(&self) -> &[ObjectName] {
        &self.packages
    }
}

/// The init file a session starts with, as the registry it is analyzed
/// against records it.
#[derive(Debug, Clone, Default)]
pub enum InitFileStart {
    /// No init file.
    #[default]
    None,
    /// The file as read: two starts are the same only with the same reading.
    Read(Arc<InitFile>),
    /// The selected file, which cannot be read: the session starts without
    /// one.
    Unavailable(InitFileUnavailable),
}

impl PartialEq for InitFileStart {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::None, Self::None) => true,
            (Self::Read(file), Self::Read(other)) => Arc::ptr_eq(file, other),
            (Self::Unavailable(problem), Self::Unavailable(other)) => problem == other,
            _ => false,
        }
    }
}

impl InitFileStart {
    /// The start of a session with the file at `path`, read against
    /// `registry`.
    fn read(path: &Path, registry: &ObjectRegistry) -> Self {
        match InitFile::read(path, registry) {
            Ok(file) => Self::Read(Arc::new(file)),
            Err(problem) => Self::Unavailable(problem),
        }
    }

    /// The init file the session starts with.
    pub fn file(&self) -> Option<&Arc<InitFile>> {
        match self {
            Self::Read(file) => Some(file),
            Self::None | Self::Unavailable(_) => None,
        }
    }

    /// Why the selected init file cannot be used.
    pub fn problem(&self) -> Option<&InitFileUnavailable> {
        match self {
            Self::Unavailable(problem) => Some(problem),
            Self::None | Self::Read(_) => None,
        }
    }
}

/// Where the init file every document starts with is, as the settings
/// select it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InitFileLocation {
    /// The discovered `init.m2`, which need not exist.
    Discovered(PathBuf),
    /// A configured file, which has to.
    Configured(PathBuf),
}

impl InitFileLocation {
    /// The file, whether or not it exists.
    pub fn path(&self) -> &Path {
        match self {
            Self::Discovered(path) | Self::Configured(path) => path,
        }
    }

    /// The start of sessions with the file here: none when a discovered file
    /// is not there.
    fn read(&self, registry: &ObjectRegistry) -> InitFileStart {
        match (self, InitFileStart::read(self.path(), registry)) {
            (Self::Discovered(_), InitFileStart::Unavailable(problem))
                if problem.reason == io::ErrorKind::NotFound =>
            {
                InitFileStart::None
            }
            (_, start) => start,
        }
    }
}

/// The settings' selection of the init file every document starts with.
#[derive(Debug, Clone, Default)]
struct ConfiguredInitFile {
    /// Where the settings place the file; none for no init file.
    location: Option<InitFileLocation>,
    /// What the documents start with, as the file there last read.
    start: InitFileStart,
}

/// The init files sessions start with, each read once and again only when it
/// changes: the one the settings select for every document, and those the
/// documents' directives select.
#[derive(Debug, Default)]
pub struct InitFiles {
    /// The file the settings select for the documents without a directive.
    configured: RwLock<ConfiguredInitFile>,
    /// The files directives select, by path.
    selected_by_directives: DashMap<PathBuf, InitFileStart>,
}

impl InitFiles {
    /// Where `choice` places the init file every document starts with. The
    /// discovered file is looked for only while `discovery` of the library
    /// sources is enabled; a relative path is relative to
    /// `workspace_folder`, and without one it names no file.
    pub fn locate(
        choice: &InitFileChoice,
        discovery: bool,
        workspace_folder: Option<&Path>,
        environment: &Macaulay2Environment,
    ) -> Result<Option<InitFileLocation>, InitFileUnavailable> {
        match choice {
            InitFileChoice::Discovered if discovery => Ok(environment
                .init_file_path()
                .map(InitFileLocation::Discovered)),
            InitFileChoice::Discovered | InitFileChoice::Disabled => Ok(None),
            InitFileChoice::Path(path) => {
                let path = environment.expand_home(path);
                match (path.is_absolute(), workspace_folder) {
                    (true, _) => Ok(Some(InitFileLocation::Configured(path))),
                    (false, Some(folder)) => {
                        Ok(Some(InitFileLocation::Configured(folder.join(path))))
                    }
                    (false, None) => Err(InitFileUnavailable {
                        path,
                        reason: io::ErrorKind::NotFound,
                    }),
                }
            }
        }
    }

    /// Make the file at `location` the one every document without a
    /// directive starts with, reading it against `registry` unless it is
    /// already. Returns the start it replaced, when it did.
    pub fn configure(
        &self,
        location: Option<InitFileLocation>,
        registry: &ObjectRegistry,
    ) -> Option<InitFileStart> {
        let mut configured = self.configured.write().expect("init file lock");
        if configured.location == location {
            return None;
        }
        let start = location
            .as_ref()
            .map_or(InitFileStart::None, |location| location.read(registry));
        let previous = std::mem::replace(&mut configured.start, start);
        configured.location = location;
        Some(previous)
    }

    /// Make the configured selection unreadable for `problem`, as when the
    /// configured path cannot be resolved. Returns the start it replaced,
    /// when it did.
    pub fn configure_unavailable(&self, problem: InitFileUnavailable) -> Option<InitFileStart> {
        let mut configured = self.configured.write().expect("init file lock");
        let start = InitFileStart::Unavailable(problem);
        if configured.location.is_none() && configured.start == start {
            return None;
        }
        configured.location = None;
        Some(std::mem::replace(&mut configured.start, start))
    }

    /// The start of the sessions of documents without a directive.
    pub fn configured(&self) -> InitFileStart {
        self.configured
            .read()
            .expect("init file lock")
            .start
            .clone()
    }

    /// Where the configured init file is, whether or not it exists.
    pub fn configured_location(&self) -> Option<InitFileLocation> {
        self.configured
            .read()
            .expect("init file lock")
            .location
            .clone()
    }

    /// The start of the session of the document at `uri`, whose directive,
    /// if any, is `directive`: the file the directive selects, read against
    /// `registry` the first time, or else the configured one. An init file
    /// never starts with itself.
    pub fn start_for(
        &self,
        uri: &Url,
        directive: Option<&InitFileDirective>,
        environment: &Macaulay2Environment,
        registry: &ObjectRegistry,
    ) -> InitFileStart {
        let document_path = uri.to_file_path().ok();
        let start = match directive.map(|directive| &directive.choice) {
            None | Some(InitFileChoice::Discovered) => self.configured(),
            Some(InitFileChoice::Disabled) => InitFileStart::None,
            Some(InitFileChoice::Path(path)) => {
                let path = environment.expand_home(path);
                let path = match document_path.as_deref().and_then(Path::parent) {
                    Some(directory) if path.is_relative() => directory.join(path),
                    _ => path,
                };
                self.selected_by_directives
                    .entry(path.clone())
                    .or_insert_with(|| InitFileStart::read(&path, registry))
                    .clone()
            }
        };
        let is_itself = start.file().is_some_and(|file| {
            file.uri() == uri
                || document_path
                    .as_deref()
                    .is_some_and(|path| is_same_file(path, file.path()))
        });
        if is_itself {
            InitFileStart::None
        } else {
            start
        }
    }

    /// Read the file at `path` again against `registry`, as it changed on
    /// disk, if a session may start with it. Returns whether it was.
    pub fn reload(&self, path: &Path, registry: &ObjectRegistry) -> bool {
        let mut reloaded = false;
        {
            let mut configured = self.configured.write().expect("init file lock");
            if let Some(location) = configured
                .location
                .clone()
                .filter(|location| location.path() == path)
            {
                configured.start = location.read(registry);
                reloaded = true;
            }
        }
        if let Some(mut start) = self.selected_by_directives.get_mut(path) {
            *start = InitFileStart::read(path, registry);
            reloaded = true;
        }
        reloaded
    }

    /// Every init file read: the configured one and those directives select.
    pub fn files(&self) -> Vec<Arc<InitFile>> {
        let mut files = self
            .configured()
            .file()
            .cloned()
            .into_iter()
            .collect::<Vec<_>>();
        for start in self.selected_by_directives.iter() {
            if let Some(file) = start.file() {
                if !files.iter().any(|known| known.path() == file.path()) {
                    files.push(Arc::clone(file));
                }
            }
        }
        files
    }
}

/// Whether `left` and `right` are the same file, as written or once symbolic
/// links are resolved.
fn is_same_file(left: &Path, right: &Path) -> bool {
    left == right
        || fs::canonicalize(left)
            .ok()
            .zip(fs::canonicalize(right).ok())
            .is_some_and(|(left, right)| left == right)
}

/// The packages the file under `root` adds to `Core#"preloaded packages"`.
/// Macaulay2 loads that list right after running the init file, so this
/// assignment is a way of loading packages that no import call shows.
/// Replacing the list can also drop default packages, but their dependencies
/// are unknown here, so the defaults stay: a name kept visible is never
/// wrongly reported unknown.
fn preloaded_package_additions(
    root: SyntaxNode<'_>,
    syntax: Option<&TypedSource>,
) -> Vec<ObjectName> {
    let mut packages = Vec::new();
    visit_source_nodes(root, syntax, |node| {
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
fn preloaded_packages_assignment(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let operator = node.binary_operator()?;
    if !(matches_token::<Token![=]>(operator) || matches_token::<Token![|=]>(operator)) {
        return None;
    }
    let target = node.binary_parts().map(|parts| parts.left)?;
    let (Some(table), Some(key)) = (
        target.binary_parts().map(|parts| parts.left),
        target.binary_parts().map(|parts| parts.right),
    ) else {
        return None;
    };
    let names_preloaded_packages = target.has_binary_operator::<Token![#]>()
        && table.is::<Symbol>()
        && table.text() == Macaulay2Conventions::CORE
        && key.string_literal_inner_text() == Some(Macaulay2Conventions::PRELOADED_PACKAGES_KEY);
    names_preloaded_packages.then(|| node.binary_parts().map(|parts| parts.right))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ObjectRegistry {
        ObjectRegistry::default()
    }

    /// A fresh directory under the system temporary directory, removed on
    /// drop.
    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("m2-ls-init-file-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("fixture directory");
            Self(fs::canonicalize(root).expect("fixture root"))
        }

        fn write(&self, name: &str, text: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, text).expect("fixture file");
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn additions(text: &str) -> Vec<String> {
        let snapshot =
            DocumentSnapshot::from_text(text.to_string(), &registry()).expect("snapshot");
        preloaded_package_additions(snapshot.root_node(), snapshot.syntax())
            .into_iter()
            .map(|package| package.name().to_string())
            .collect()
    }

    /// Packages added to `Core#"preloaded packages"` load right after the
    /// init file runs; only that list counts, and commented-out lines do
    /// nothing.
    #[test]
    fn preloaded_package_additions_come_from_the_preloaded_packages_list() {
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

    #[test]
    fn directives_select_no_init_file_or_a_path_with_spaces() {
        assert_eq!(
            InitFileDirective::choice_in("-- m2-ls: initFile none"),
            Some(InitFileChoice::Disabled)
        );
        assert_eq!(
            InitFileDirective::choice_in("--m2-ls:initFile  ~/my init.m2  "),
            Some(InitFileChoice::Path(PathBuf::from("~/my init.m2")))
        );
        assert_eq!(InitFileDirective::choice_in("-- m2-ls: initFile"), None);
        assert_eq!(
            InitFileDirective::choice_in("-- m2-ls: initFiles a.m2"),
            None
        );
        assert_eq!(InitFileDirective::choice_in("-- initFile a.m2"), None);
    }

    #[test]
    fn the_setting_reads_true_false_or_a_path() {
        let read = |value: serde_json::Value| serde_json::from_value::<InitFileChoice>(value);
        assert_eq!(
            read(serde_json::json!(true)).ok(),
            Some(InitFileChoice::Discovered)
        );
        assert_eq!(
            read(serde_json::json!(false)).ok(),
            Some(InitFileChoice::Disabled)
        );
        assert_eq!(
            read(serde_json::json!("~/init.m2")).ok(),
            Some(InitFileChoice::Path(PathBuf::from("~/init.m2")))
        );
        assert!(read(serde_json::json!("")).is_err());
        assert!(read(serde_json::json!(3)).is_err());
    }

    #[test]
    fn unreadable_files_say_why() {
        let fixture = Fixture::new("unreadable");
        let missing = fixture.0.join("missing.m2");
        let directory = fixture.0.join("directory.m2");
        fs::create_dir(&directory).expect("directory");
        let binary = fixture.0.join("binary.m2");
        fs::write(&binary, [0xff, 0xfe, 0x00]).expect("binary file");

        let problem = |path: &Path| InitFile::read(path, &registry()).expect_err("unreadable");
        assert!(problem(&missing)
            .to_string()
            .starts_with("there is no init file"));
        assert!(problem(&directory).to_string().ends_with("is a directory"));
        assert!(problem(&binary).to_string().ends_with("is not UTF-8 text"));
    }

    #[cfg(unix)]
    #[test]
    fn a_file_that_may_not_be_read_says_so() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new("permissions");
        let path = fixture.write("init.m2", "x = 1\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("permissions");
        // A privileged user reads the file anyway.
        if fs::read(&path).is_ok() {
            return;
        }
        let problem = InitFile::read(&path, &registry()).expect_err("unreadable");
        assert!(
            problem.to_string().ends_with("may not be read"),
            "{problem}"
        );
    }

    #[test]
    fn a_missing_discovered_file_is_no_problem_but_a_configured_one_is() {
        let fixture = Fixture::new("missing");
        let path = fixture.0.join("init.m2");
        let files = InitFiles::default();

        files.configure(
            Some(InitFileLocation::Discovered(path.clone())),
            &registry(),
        );
        assert_eq!(files.configured(), InitFileStart::None);

        files.configure(
            Some(InitFileLocation::Configured(path.clone())),
            &registry(),
        );
        assert!(files.configured().problem().is_some());

        fixture.write("init.m2", "needsPackage \"Graphs\"\n");
        assert!(files.reload(&path, &registry()));
        let file = files
            .configured()
            .file()
            .cloned()
            .expect("read once created");
        assert_eq!(file.packages(), [ObjectName::new("Graphs")]);
    }

    #[test]
    fn directives_select_files_beside_the_document_and_never_the_document_itself() {
        let fixture = Fixture::new("directives");
        let init = fixture.write("project-init.m2", "x = 1\n");
        let document = Url::from_file_path(fixture.0.join("a.m2")).expect("uri");
        let files = InitFiles::default();
        let environment = Macaulay2Environment::default();
        let directive = |choice| InitFileDirective {
            choice,
            range: TextRange::default(),
        };

        let start = files.start_for(
            &document,
            Some(&directive(InitFileChoice::Path(PathBuf::from(
                "project-init.m2",
            )))),
            &environment,
            &registry(),
        );
        assert_eq!(start.file().map(|file| file.path()), Some(init.as_path()));
        assert_eq!(
            files.start_for(
                &document,
                Some(&directive(InitFileChoice::Disabled)),
                &environment,
                &registry()
            ),
            InitFileStart::None
        );
        let itself = Url::from_file_path(&init).expect("uri");
        assert_eq!(
            files.start_for(
                &itself,
                Some(&directive(InitFileChoice::Path(init.clone()))),
                &environment,
                &registry()
            ),
            InitFileStart::None
        );
        let missing = files.start_for(
            &document,
            Some(&directive(InitFileChoice::Path(PathBuf::from("absent.m2")))),
            &environment,
            &registry(),
        );
        assert!(missing.problem().is_some());
    }
}
