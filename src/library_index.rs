//! Navigation facts for the Macaulay2 library sources: Core and the packages
//! the source resolver finds, each indexed from disk the first time navigation
//! needs it.
//!
//! Indexing a package parses every file it loads, which takes seconds for Core,
//! so it never runs inside a request. A request navigates against
//! [`LibrarySources`], which answers from the packages already indexed and
//! records the ones it wanted; the server indexes those off the session task
//! and navigates again.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::OnceCell;
use tokio::task::{self, JoinSet};
use tower_lsp::lsp_types::{Location, Url};

use crate::object_registry::{ObjectName, ObjectRegistry};
use crate::package_index::SourceResolver;
use crate::workspace_index::WorkspaceIndex;

/// The file beside Core's main file listing the files it `needs`, in order.
const LOAD_SEQUENCE: &str = "loadsequence";

/// The navigation index of one package's sources; `None` when the configured
/// source roots do not contain the package.
type PackageSources = Option<Arc<WorkspaceIndex>>;

#[derive(Debug)]
pub struct LibraryIndex {
    source_resolver: SourceResolver,
    registry: ObjectRegistry,
    packages: DashMap<ObjectName, Arc<OnceCell<PackageSources>>>,
}

/// One request's view of the library: the packages indexed so far, and the
/// ones the request looked for before they were.
#[derive(Debug)]
pub struct LibrarySources<'library> {
    library: &'library LibraryIndex,
    wanted_packages: RefCell<Vec<ObjectName>>,
}

impl LibraryIndex {
    pub fn new(source_resolver: SourceResolver, registry: ObjectRegistry) -> Self {
        Self {
            source_resolver,
            registry,
            packages: DashMap::new(),
        }
    }

    pub fn source_resolver(&self) -> &SourceResolver {
        &self.source_resolver
    }

    pub fn sources(&self) -> LibrarySources<'_> {
        LibrarySources {
            library: self,
            wanted_packages: RefCell::default(),
        }
    }

    /// Index each of `packages` that is not indexed yet, concurrently, on the
    /// blocking pool. Concurrent requests for one package share a single
    /// indexing run.
    pub async fn index_packages(&self, packages: Vec<ObjectName>) {
        let mut indexing = JoinSet::new();
        for package in packages {
            let cell = Arc::clone(self.packages.entry(package.clone()).or_default().value());
            let source_resolver = self.source_resolver.clone();
            let registry = self.registry.clone();
            indexing.spawn(async move {
                cell.get_or_init(|| async move {
                    task::spawn_blocking(move || {
                        index_package_sources(&package, &source_resolver, &registry)
                    })
                    .await
                    .expect("library indexing task panicked")
                })
                .await;
            });
        }
        while let Some(indexed) = indexing.join_next().await {
            indexed.expect("library indexing task panicked");
        }
    }

    fn indexed_package(&self, package: &ObjectName) -> Option<PackageSources> {
        self.packages.get(package)?.get().cloned()
    }
}

impl LibrarySources<'_> {
    /// The sources of `package` when they are indexed; otherwise records the
    /// package as wanted and answers `None` for now.
    pub fn package_sources(&self, package: &ObjectName) -> Option<Arc<WorkspaceIndex>> {
        match self.library.indexed_package(package) {
            Some(sources) => sources,
            None => {
                self.wanted_packages.borrow_mut().push(package.clone());
                None
            }
        }
    }

    pub fn source_resolver(&self) -> &SourceResolver {
        self.library.source_resolver()
    }

    /// The package whose sources contain `document`, when it is a library file.
    pub fn package_of_file(&self, document: &Url) -> Option<ObjectName> {
        let path = document.to_file_path().ok()?;
        self.library.source_resolver.package_of_source_file(&path)
    }

    /// The first non-empty result of `query` over the sources of the package
    /// `document` belongs to, then of `record_package`, the package that
    /// exports the name. A package's own definitions shadow those it imports.
    pub fn locations(
        &self,
        document: &Url,
        record_package: Option<ObjectName>,
        query: impl Fn(&WorkspaceIndex) -> Vec<Location>,
    ) -> Vec<Location> {
        self.package_of_file(document)
            .into_iter()
            .chain(record_package)
            .filter_map(|package| self.package_sources(&package))
            .map(|sources| query(&sources))
            .find(|locations| !locations.is_empty())
            .unwrap_or_default()
    }

    /// Every installation of the method `name` outside `document`, in the
    /// sources of the package `document` belongs to and of `packages`.
    pub fn method_installations(
        &self,
        document: &Url,
        packages: Vec<ObjectName>,
        name: &str,
    ) -> Vec<Location> {
        let mut packages = self
            .package_of_file(document)
            .into_iter()
            .chain(packages)
            .collect::<Vec<_>>();
        packages.sort();
        packages.dedup();
        packages
            .iter()
            .filter_map(|package| self.package_sources(package))
            .flat_map(|sources| sources.method_installations(name))
            .filter(|location| &location.uri != document)
            .collect()
    }

    pub fn into_wanted_packages(self) -> Vec<ObjectName> {
        let mut packages = self.wanted_packages.into_inner();
        packages.sort();
        packages.dedup();
        packages
    }
}

/// Index the files `package` loads, starting from its main file. Core's main
/// file also `needs` every file its `loadsequence` lists.
fn index_package_sources(
    package: &ObjectName,
    source_resolver: &SourceResolver,
    registry: &ObjectRegistry,
) -> PackageSources {
    let main_file = source_resolver.resolve_package_file(package.name())?;
    let files = std::iter::once(main_file.clone())
        .chain(load_sequence_files(&main_file))
        .filter_map(|path| Url::from_file_path(path).ok())
        .collect();
    let sources = WorkspaceIndex::default();
    sources.index_files_from_disk(files, registry);
    Some(Arc::new(sources))
}

/// The files listed in the `loadsequence` beside `main_file`, selected the way
/// Core selects them: each line's leading `name.m2`.
fn load_sequence_files(main_file: &Path) -> Vec<PathBuf> {
    let Some(directory) = main_file.parent() else {
        return Vec::new();
    };
    let Ok(sequence) = fs::read_to_string(directory.join(LOAD_SEQUENCE)) else {
        return Vec::new();
    };
    sequence
        .lines()
        .filter_map(|line| {
            let stem_length = line
                .find(|character: char| !(character.is_alphanumeric() || character == '_'))
                .unwrap_or(line.len());
            (stem_length > 0 && line[stem_length..].starts_with(".m2"))
                .then(|| directory.join(&line[..stem_length + ".m2".len()]))
        })
        .collect()
}
