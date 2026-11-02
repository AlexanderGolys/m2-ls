//! Demand-driven navigation into installed library sources.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as AsyncMutex;
use tokio::task;
use tower_lsp::lsp_types::{Location, Url};

use crate::document::DocumentSnapshot;
use crate::object_registry::{ObjectName, ObjectRegistry};
use crate::package_index::SourceResolver;
use crate::settings::{LibraryIndexing, LibrarySettings};
use crate::workspace_index::WorkspaceIndex;

/// Installed package file listing the source files to index.
const LOAD_SEQUENCE: &str = "loadsequence";
/// Bound on unresolved package names retained to suppress repeated lookups.
const MAX_MISSING_PACKAGES: usize = 128;

/// When library sources are indexed and how much of them, as the
/// `library.*` settings say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LibraryLimits {
    /// When package sources are indexed.
    indexing: LibraryIndexing,
    /// Maximum number of library packages retained in the source cache.
    max_packages: usize,
    /// Maximum source-file count indexed for one library package.
    max_files: usize,
    /// Maximum cumulative source bytes indexed for one library package.
    max_bytes: u64,
}

impl From<&LibrarySettings> for LibraryLimits {
    fn from(settings: &LibrarySettings) -> Self {
        Self {
            indexing: settings.indexing,
            max_packages: settings.max_packages.max(1) as usize,
            max_files: settings.max_files_per_package as usize,
            max_bytes: u64::from(settings.max_megabytes_per_package) * 1024 * 1024,
        }
    }
}

impl Default for LibraryLimits {
    fn default() -> Self {
        Self::from(&LibrarySettings::default())
    }
}

/// Indexed files and synchronized progress for one lazily loaded library package.
#[derive(Debug)]
struct PackageSources {
    /// Semantic index of library files already analyzed for this package.
    sources: WorkspaceIndex,
    /// Mutable traversal progress for incremental library indexing.
    progress: Mutex<PackageProgress>,
    /// Logical access time used to evict the least recently requested package.
    last_used: AtomicU64,
    /// How much of the package may be indexed.
    limits: LibraryLimits,
}

/// Pending files, visited locations, and resource usage for library indexing.
#[derive(Debug, Default)]
struct PackageProgress {
    /// Source files waiting to be indexed.
    pending: VecDeque<Url>,
    /// Source locations already scheduled or indexed.
    visited: HashSet<Url>,
    /// Total source bytes consumed by this package's indexing pass.
    source_bytes: u64,
}

/// Bounded lazy index of library package sources supplementing the builtin catalog.
#[derive(Debug)]
pub struct LibraryIndex {
    /// Package and file lookup paths for resolving source locations.
    source_resolver: SourceResolver,
    /// Object and binding registry used to resolve semantic facts.
    registry: ObjectRegistry,
    /// Package declarations and imports visible to this operation.
    packages: Mutex<HashMap<ObjectName, Arc<PackageSources>>>,
    /// Known unresolved packages cached to avoid repeated lookups.
    missing_packages: Mutex<HashSet<ObjectName>>,
    /// Monotonic logical clock used to record cache access order.
    clock: AtomicU64,
    /// Async exclusion preventing concurrent library indexing passes.
    indexing: AsyncMutex<()>,
    /// When sources are indexed, and how much of them.
    limits: LibraryLimits,
}

/// A navigation request's view of the cached sources and the packages for
/// which it needs another file. Finding a location never schedules more work.
#[derive(Debug)]
pub struct LibrarySources<'library> {
    /// Library index borrowed for this request.
    library: &'library LibraryIndex,
    /// Packages requested by the current source lookup session.
    wanted_packages: RefCell<Vec<ObjectName>>,
}

impl LibraryIndex {
    /// Creates an empty lazy library index using the supplied resolver and builtin registry.
    #[cfg(test)]
    pub fn new(source_resolver: SourceResolver, registry: ObjectRegistry) -> Self {
        Self::with_limits(source_resolver, registry, LibraryLimits::default())
    }

    /// An empty library index indexing as much as `limits` allows.
    pub fn with_limits(
        source_resolver: SourceResolver,
        registry: ObjectRegistry,
        limits: LibraryLimits,
    ) -> Self {
        Self {
            source_resolver,
            registry,
            packages: Mutex::default(),
            missing_packages: Mutex::default(),
            clock: AtomicU64::new(0),
            indexing: AsyncMutex::new(()),
            limits,
        }
    }

    /// Package and file lookup configuration used by this index.
    pub fn source_resolver(&self) -> &SourceResolver {
        &self.source_resolver
    }

    /// Starts a source-lookup session recording which library packages are needed.
    pub fn sources(&self) -> LibrarySources<'_> {
        LibrarySources {
            library: self,
            wanted_packages: RefCell::default(),
        }
    }

    /// Advance each requested package by one file, sharing a single worker
    /// across requests. Dependencies stay queued until navigation needs them.
    pub async fn index_packages(&self, packages: Vec<ObjectName>) {
        if self.limits.indexing == LibraryIndexing::Off {
            return;
        }
        let queue_timing = crate::telemetry::Timing::start(
            "library.queue",
            serde_json::json!({"packages": packages.iter().map(ObjectName::name).collect::<Vec<_>>()}),
        );
        let _indexing = self.indexing.lock().await;
        drop(queue_timing);
        for name in packages {
            let Some(package) = self.package(&name, true) else {
                continue;
            };
            let registry = self.registry.clone();
            task::spawn_blocking(move || package.index_next_file(&registry))
                .await
                .expect("library indexing task panicked");
        }
    }

    /// Under `library.indexing = "loaded"`, index every file of `packages`,
    /// as far as the limits allow, a file per package at a time so requests
    /// can interleave. Otherwise nothing: files are indexed as navigation
    /// needs them.
    pub async fn index_loaded_packages(&self, packages: Vec<ObjectName>) {
        if self.limits.indexing != LibraryIndexing::Loaded {
            return;
        }
        let packages = packages
            .into_iter()
            .take(self.limits.max_packages)
            .collect::<Vec<_>>();
        loop {
            let pending = packages
                .iter()
                .filter(|name| {
                    self.package(name, true)
                        .is_some_and(|package| package.has_pending_files())
                })
                .cloned()
                .collect::<Vec<_>>();
            if pending.is_empty() {
                return;
            }
            self.index_packages(pending).await;
        }
    }

    /// Shared library facts follow saves, independently of live-buffer edits.
    pub fn invalidate_saved_file(&self, uri: &Url) {
        self.missing_packages
            .lock()
            .expect("missing packages lock")
            .clear();
        let mut packages = self.packages.lock().expect("library cache lock");
        let before = packages.len();
        packages.retain(|_, package| {
            !package
                .progress
                .lock()
                .expect("package progress lock")
                .visited
                .contains(uri)
        });
        if before != packages.len() {
            crate::telemetry::event(
                serde_json::json!({"operation": "library.invalidate_save", "uri": uri, "packages": before - packages.len()}),
            );
        }
    }

    /// Obtains the cached sources of a package, optionally creating its indexing state.
    fn package(&self, name: &ObjectName, create: bool) -> Option<Arc<PackageSources>> {
        let mut packages = self.packages.lock().expect("library cache lock");
        let tick = self.clock.fetch_add(1, Ordering::Relaxed);
        if let Some(package) = packages.get(name) {
            package.last_used.store(tick, Ordering::Relaxed);
            return Some(Arc::clone(package));
        }
        if !create {
            return None;
        }
        let Some(main_file) = self.source_resolver.resolve_package_file(name.name()) else {
            let mut missing = self.missing_packages.lock().expect("missing packages lock");
            if missing.len() >= MAX_MISSING_PACKAGES {
                missing.clear();
            }
            missing.insert(name.clone());
            return None;
        };
        if packages.len() >= self.limits.max_packages {
            if let Some(oldest) = packages
                .iter()
                .min_by_key(|(_, package)| package.last_used.load(Ordering::Relaxed))
                .map(|(name, _)| name.clone())
            {
                packages.remove(&oldest);
                crate::telemetry::event(
                    serde_json::json!({"operation": "library.evict", "package": oldest.name(), "cached_packages": packages.len()}),
                );
            }
        }
        let pending = std::iter::once(main_file.clone())
            .chain(load_sequence_files(&main_file))
            .take(self.limits.max_files)
            .filter_map(|path| Url::from_file_path(path).ok())
            .collect();
        let package = Arc::new(PackageSources {
            sources: WorkspaceIndex::of_library_sources(),
            progress: Mutex::new(PackageProgress {
                pending,
                ..PackageProgress::default()
            }),
            last_used: AtomicU64::new(tick),
            limits: self.limits,
        });
        packages.insert(name.clone(), Arc::clone(&package));
        Some(package)
    }
}

impl PackageSources {
    /// Whether any cached library package still has files waiting to be indexed.
    fn has_pending_files(&self) -> bool {
        !self
            .progress
            .lock()
            .expect("package progress lock")
            .pending
            .is_empty()
    }

    /// Indexes one queued library file while respecting package resource limits.
    fn index_next_file(&self, registry: &ObjectRegistry) {
        loop {
            let (uri, remaining) = {
                let mut progress = self.progress.lock().expect("package progress lock");
                if progress.visited.len() >= self.limits.max_files {
                    progress.pending.clear();
                    crate::telemetry::event(
                        serde_json::json!({"operation": "library.limit", "limit": "files", "files": progress.visited.len()}),
                    );
                    return;
                }
                let Some(uri) = progress.pending.pop_front() else {
                    return;
                };
                if !progress.visited.insert(uri.clone()) {
                    continue;
                }
                (
                    uri,
                    self.limits.max_bytes.saturating_sub(progress.source_bytes),
                )
            };
            let mut timing = crate::telemetry::Timing::start(
                "library.index_file",
                serde_json::json!({"uri": uri}),
            );
            let Ok(path) = uri.to_file_path() else {
                continue;
            };
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    timing.error(error.to_string());
                    continue;
                }
            };
            if metadata.len() > remaining {
                timing.record("skipped", "source_byte_limit");
                continue;
            }
            let text = match fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) => {
                    timing.error(error.to_string());
                    continue;
                }
            };
            if text.len() as u64 > remaining {
                continue;
            }
            timing.record("source_bytes", text.len());
            self.progress
                .lock()
                .expect("package progress lock")
                .source_bytes += text.len() as u64;
            // No package/cache lock is held while analysis runs on the blocking pool.
            let Some(snapshot) = DocumentSnapshot::from_text_for_navigation(text, registry) else {
                continue;
            };
            let loaded = self.sources.index_snapshot(&uri, &snapshot, registry);
            let mut progress = self.progress.lock().expect("package progress lock");
            progress.pending.extend(loaded);
            let remaining_files = self.limits.max_files.saturating_sub(progress.visited.len());
            progress.pending.truncate(remaining_files);
            timing.record("pending_files", progress.pending.len());
            timing.record("cached_files", progress.visited.len());
            timing.record("cached_source_bytes", progress.source_bytes);
            return;
        }
    }
}

impl LibrarySources<'_> {
    /// Package and file lookup configuration used by this index.
    pub fn source_resolver(&self) -> &SourceResolver {
        self.library.source_resolver()
    }

    /// Package owning the source location, when a containing package can be resolved.
    pub fn package_of_file(&self, document: &Url) -> Option<ObjectName> {
        self.library
            .source_resolver
            .package_of_source_file(&document.to_file_path().ok()?)
    }

    /// Source locations matching the requested object and navigation role.
    pub fn locations(
        &self,
        document: &Url,
        record_package: Option<ObjectName>,
        query: impl Fn(&WorkspaceIndex) -> Vec<Location>,
    ) -> Vec<Location> {
        for name in self
            .package_of_file(document)
            .into_iter()
            .chain(record_package)
        {
            if let Some(package) = self.library.package(&name, false) {
                let locations = query(&package.sources);
                if !locations.is_empty() {
                    return locations;
                }
                if !package.has_pending_files() {
                    continue;
                }
            }
            if !self
                .library
                .missing_packages
                .lock()
                .expect("missing packages lock")
                .contains(&name)
            {
                self.wanted_packages.borrow_mut().push(name);
            }
        }
        Vec::new()
    }

    /// Source installations matching the requested callable and method domain.
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
        let mut locations = Vec::new();
        for package_name in packages {
            if let Some(package) = self.library.package(&package_name, false) {
                locations.extend(package.sources.method_installations(name));
                if !package.has_pending_files() {
                    continue;
                }
            }
            if !self
                .library
                .missing_packages
                .lock()
                .expect("missing packages lock")
                .contains(&package_name)
            {
                self.wanted_packages.borrow_mut().push(package_name);
            }
        }
        locations.retain(|location| &location.uri != document);
        locations
    }

    /// Finishes the lookup session and returns the packages requested by it.
    pub fn into_wanted_packages(self) -> Vec<ObjectName> {
        let mut packages = self.wanted_packages.into_inner();
        packages.sort();
        packages.dedup();
        packages
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_index::Macaulay2Environment;
    use crate::workspace_index::WorkspaceDefinitionKnowledge;

    struct LibraryFixture {
        root: PathBuf,
        index: LibraryIndex,
    }

    impl LibraryFixture {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "m2-ls-library-cache-{label}-{}",
                std::process::id()
            ));
            fs::create_dir_all(root.join("lib/Macaulay2")).unwrap();
            fs::create_dir_all(root.join("share/Macaulay2/Pkg")).unwrap();
            let resolver = SourceResolver::discover(
                &Macaulay2Environment::default(),
                std::slice::from_ref(&root),
            );
            Self {
                root,
                index: LibraryIndex::new(resolver, ObjectRegistry::default()),
            }
        }

        fn source(&self, path: &str) -> PathBuf {
            self.root.join("share/Macaulay2").join(path)
        }
    }

    impl Drop for LibraryFixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[tokio::test]
    async fn navigation_stops_after_the_matching_file_and_cache_changes_only_on_save() {
        let fixture = LibraryFixture::new("incremental");
        let main = fixture.source("Pkg.m2");
        fs::write(&main, "alpha = 1\nload \"./Pkg/dependency.m2\"\n").unwrap();
        fs::write(fixture.source("Pkg/dependency.m2"), "beta = 2\n").unwrap();
        let name = ObjectName::new("Pkg");
        fixture.index.index_packages(vec![name.clone()]).await;
        let document = Url::parse("file:///consumer.m2").unwrap();
        let sources = fixture.index.sources();
        assert_eq!(
            sources
                .locations(&document, Some(name.clone()), |index| index
                    .definitions("alpha", &document))
                .len(),
            1
        );
        assert!(sources.into_wanted_packages().is_empty());
        let package = fixture.index.package(&name, false).unwrap();
        assert_eq!(package.progress.lock().unwrap().visited.len(), 1);
        assert!(package.has_pending_files());

        fs::write(&main, "gamma = 3\n").unwrap();
        assert_eq!(package.sources.definitions("alpha", &document).len(), 1);
        assert!(package.sources.definitions("gamma", &document).is_empty());
        fixture
            .index
            .invalidate_saved_file(&Url::from_file_path(&main).unwrap());
        assert!(fixture.index.package(&name, false).is_none());
        fixture.index.index_packages(vec![name.clone()]).await;
        assert_eq!(
            fixture
                .index
                .package(&name, false)
                .unwrap()
                .sources
                .definitions("gamma", &document)
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn oversized_sources_are_skipped_and_the_package_cache_evicts_old_entries() {
        let fixture = LibraryFixture::new("bounded");
        let limits = LibraryLimits::default();
        fs::write(
            fixture.source("Oversized.m2"),
            vec![b' '; limits.max_bytes as usize + 1],
        )
        .unwrap();
        let oversized = ObjectName::new("Oversized");
        fixture.index.index_packages(vec![oversized.clone()]).await;
        let package = fixture.index.package(&oversized, false).unwrap();
        assert_eq!(package.progress.lock().unwrap().source_bytes, 0);
        assert!(!package.has_pending_files());
        for number in 0..limits.max_packages {
            let name = format!("Pkg{number}");
            fs::write(fixture.source(&format!("{name}.m2")), "value = 1\n").unwrap();
            fixture
                .index
                .index_packages(vec![ObjectName::new(name)])
                .await;
        }
        assert_eq!(
            fixture.index.packages.lock().unwrap().len(),
            limits.max_packages
        );
        assert!(fixture.index.package(&oversized, false).is_none());
    }

    #[tokio::test]
    async fn indexing_can_be_off_or_index_whole_loaded_packages() {
        let fixture = LibraryFixture::new("modes");
        fs::write(fixture.source("Whole.m2"), "value = 1\n").unwrap();
        let whole = ObjectName::new("Whole");
        let index = |indexing| {
            LibraryIndex::with_limits(
                fixture.index.source_resolver().clone(),
                ObjectRegistry::default(),
                LibraryLimits {
                    indexing,
                    ..LibraryLimits::default()
                },
            )
        };

        let off = index(LibraryIndexing::Off);
        off.index_packages(vec![whole.clone()]).await;
        off.index_loaded_packages(vec![whole.clone()]).await;
        assert!(off.package(&whole, false).is_none());

        let on_demand = index(LibraryIndexing::OnDemand);
        on_demand.index_loaded_packages(vec![whole.clone()]).await;
        assert!(on_demand.package(&whole, false).is_none());

        let loaded = index(LibraryIndexing::Loaded);
        loaded.index_loaded_packages(vec![whole.clone()]).await;
        let package = loaded.package(&whole, false).unwrap();
        assert!(!package.has_pending_files());
        assert_eq!(package.progress.lock().unwrap().visited.len(), 1);
    }
}
