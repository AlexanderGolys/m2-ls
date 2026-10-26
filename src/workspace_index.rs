//! A workspace-wide index of top-level (global) navigation facts across the
//! files the editor has open, every file they load (`load`/`needs`/`input`),
//! and every user package they import, transitively, including files the
//! editor has not opened.
//!
//! M2's top-level symbols are global once loaded, so a name-keyed index of every
//! file's global facts is a faithful model: a name used in one file and defined
//! or implemented at the top level of another resolves across the project.
//!
//! Packages are the exception. A global defined inside a `newPackage` section
//! lands in the package's private dictionary: the files of the package see it,
//! everyone else sees only the names the package exports. A file belongs to a
//! package by declaring it, or by being loaded from inside its section.

use dashmap::DashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use tower_lsp::lsp_types::{Location, Position, Range as TextRange, Url};

use crate::analysis::AssignmentFactKind;
use crate::capabilities::document_symbols::{collect_workspace_symbols, WorkspaceSourceSymbol};
use crate::document::DocumentSnapshot;
use crate::object_registry::{ObjectName, ObjectRegistry};
use crate::package_documentation::DocumentedSubject;
use crate::package_index::collect_loaded_files_in_tree;
use crate::semantic_token::{local_symbol_semantic_token, M2SemanticTokenType};
use crate::source::SourceNavigation;
use crate::source_package::{ExportAccess, PackageDeclaration, PackageExport};

#[derive(Debug, Clone)]
struct DefLocation {
    uri: Url,
    range: TextRange,
    semantic_token_type: M2SemanticTokenType,
    is_declaration: bool,
    /// The statement defining the name, as written, when it is more than
    /// the name.
    code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImplementationKind {
    Lambda,
    Method,
}

#[derive(Debug, Clone)]
struct ImplementationLocation {
    uri: Url,
    range: TextRange,
    kind: ImplementationKind,
    requires_declaration: bool,
}

/// One export of an indexed package.
#[derive(Debug, Clone)]
struct IndexedExport {
    /// The file declaring the package.
    file: Url,
    /// The exporting package.
    package: ObjectName,
    export: PackageExport,
}

/// One documentation page of an indexed file, filed under the name one of
/// its keys documents.
#[derive(Debug, Clone)]
struct IndexedPage {
    file: Url,
    /// The key documenting the name.
    key: DocumentedSubject,
    /// The page as Markdown.
    markdown: String,
}

impl IndexedPage {
    /// How directly the page's key documents its name: the name itself
    /// before an optional argument it is, before a method it heads.
    fn directness(&self) -> u8 {
        match self.key {
            DocumentedSubject::Name(_) => 0,
            DocumentedSubject::OptionalArgument { .. } => 1,
            DocumentedSubject::Method { .. } | DocumentedSubject::Expression(_) => 2,
        }
    }
}

/// Whether the index hides a package's unexported globals from the files
/// outside the package.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PackagePrivacy {
    /// Files outside a package see only its exports: the user's workspace.
    #[default]
    Enforced,
    /// Every global is visible: the library sources, whose corpus records
    /// already decide which names a document sees before the sources are
    /// asked where they are defined.
    Waived,
}

/// Who a workspace query answers for, deciding which package globals it sees
/// and which file's definitions its own analysis already knows.
#[derive(Debug, Clone, Copy)]
enum WorkspaceViewer<'a> {
    /// A document, whose own definitions its analysis answers for. It sees
    /// the private globals of the package it belongs to.
    Document(&'a Url),
    /// Code run in a package session, such as a TEST block or a
    /// documentation example. Analyzed on its own, it knows none of the
    /// definitions of the document holding it, and sees every package, its
    /// own included, only through the exports.
    PackageSession,
}

/// What the workspace says about a name besides where it is defined: its
/// documentation pages, which are global — every session that loads a package
/// sees its documentation — and the code defining it.
pub trait WorkspaceDocumentationKnowledge {
    /// The Markdown of the page documenting `name` most directly.
    fn documentation(&self, name: &str) -> Option<String>;

    /// The statement defining the global `name` that the document `exclude`
    /// sees in another file, as written.
    fn declaration_code(&self, name: &str, exclude: &Url) -> Option<String>;
}

pub trait WorkspaceSymbolKnowledge {
    fn matching_symbols(&self, query: &str) -> Vec<WorkspaceSourceSymbol>;
}

/// The method implementations of the workspace. `exclude` is the requesting
/// document: its analysis answers for its own implementations.
pub trait WorkspaceImplementationKnowledge {
    fn implementations(&self, name: &str, kind: ImplementationKind, exclude: &Url)
        -> Vec<Location>;
    fn has_method_declaration(&self, name: &str, exclude: &Url) -> bool;
}

impl<T: WorkspaceImplementationKnowledge + ?Sized> WorkspaceImplementationKnowledge for Arc<T> {
    fn implementations(
        &self,
        name: &str,
        kind: ImplementationKind,
        exclude: &Url,
    ) -> Vec<Location> {
        self.as_ref().implementations(name, kind, exclude)
    }

    fn has_method_declaration(&self, name: &str, exclude: &Url) -> bool {
        self.as_ref().has_method_declaration(name, exclude)
    }
}

impl<T: WorkspaceSymbolKnowledge + ?Sized> WorkspaceSymbolKnowledge for Arc<T> {
    fn matching_symbols(&self, query: &str) -> Vec<WorkspaceSourceSymbol> {
        self.as_ref().matching_symbols(query)
    }
}

/// The global definitions of the workspace. `exclude` is the requesting
/// document: its analysis answers for its own definitions.
pub trait WorkspaceDefinitionKnowledge {
    fn declarations(&self, name: &str, exclude: &Url) -> Vec<Location>;
    fn definitions(&self, name: &str, exclude: &Url) -> Vec<Location>;
    fn type_definitions(&self, name: &str, exclude: &Url) -> Vec<Location>;
    fn is_defined(&self, name: &str, exclude: &Url) -> bool;
    fn semantic_token_type(&self, name: &str, exclude: &Url) -> Option<M2SemanticTokenType>;
    /// Whether a package of the workspace exports `name` with
    /// `exportMutable`, so the sessions loading it may assign it.
    fn is_mutable_export(&self, name: &str) -> bool;
}

impl<T: WorkspaceDefinitionKnowledge + ?Sized> WorkspaceDefinitionKnowledge for Arc<T> {
    fn declarations(&self, name: &str, exclude: &Url) -> Vec<Location> {
        self.as_ref().declarations(name, exclude)
    }

    fn definitions(&self, name: &str, exclude: &Url) -> Vec<Location> {
        self.as_ref().definitions(name, exclude)
    }

    fn type_definitions(&self, name: &str, exclude: &Url) -> Vec<Location> {
        self.as_ref().type_definitions(name, exclude)
    }

    fn is_defined(&self, name: &str, exclude: &Url) -> bool {
        self.as_ref().is_defined(name, exclude)
    }

    fn semantic_token_type(&self, name: &str, exclude: &Url) -> Option<M2SemanticTokenType> {
        self.as_ref().semantic_token_type(name, exclude)
    }

    fn is_mutable_export(&self, name: &str) -> bool {
        self.as_ref().is_mutable_export(name)
    }
}

/// Global navigation index, keyed by symbol name, kept in sync with edits and
/// on-disk changes. Open documents are indexed from their live text; the files
/// they load and the user packages they import, from disk.
#[derive(Debug, Default)]
pub struct WorkspaceIndex {
    definitions_by_name: DashMap<ObjectName, Vec<DefLocation>>,
    implementations_by_name: DashMap<ObjectName, Vec<ImplementationLocation>>,
    method_declarations_by_name: DashMap<ObjectName, Vec<Location>>,
    pages_by_name: DashMap<ObjectName, Vec<IndexedPage>>,
    names_by_file: DashMap<Url, Vec<ObjectName>>,
    symbols_by_file: DashMap<Url, Vec<WorkspaceSourceSymbol>>,
    /// Every indexed file, mapped to the files it loads and the user packages
    /// it imports.
    loads_by_file: DashMap<Url, Vec<Url>>,
    /// The packages each indexed file declares.
    declared_packages_by_file: DashMap<Url, Vec<PackageDeclaration>>,
    /// The package a file belongs to because it is loaded from inside the
    /// package's section.
    package_of_loaded_file: DashMap<Url, ObjectName>,
    privacy: PackagePrivacy,
}

/// The workspace as the source a request is analyzed in sees it.
#[derive(Debug, Clone, Copy)]
pub enum WorkspaceView<'index> {
    /// A document of the workspace.
    Document(&'index WorkspaceIndex),
    /// Code run in a package session, such as a TEST block as `check` runs
    /// it or a documentation example as `installPackage` runs it.
    PackageSession(&'index WorkspaceIndex),
}

impl WorkspaceIndex {
    /// The index of one package's library sources.
    pub fn of_library_sources() -> Self {
        Self {
            privacy: PackagePrivacy::Waived,
            ..Self::default()
        }
    }

    /// Index `uri` from `text`, then every file it loads and every user
    /// package it imports, transitively, from disk. Files already indexed keep
    /// their facts, so an open document's live text is never clobbered by its
    /// saved copy.
    pub fn index_file(&self, uri: &Url, text: &str, knowledge_provider: &ObjectRegistry) {
        let loaded = self.index_source(uri, text, knowledge_provider);
        self.index_files_from_disk(loaded, knowledge_provider);
    }

    /// Index `files` from disk, then every file they load, transitively. Each
    /// wave of newly discovered files is parsed in parallel; files already
    /// indexed keep their facts.
    pub fn index_files_from_disk(&self, files: Vec<Url>, knowledge_provider: &ObjectRegistry) {
        let mut timing = crate::telemetry::Timing::start(
            "workspace.dependencies",
            serde_json::json!({"roots": files.len()}),
        );
        let mut processed = 0;
        let mut pending = files;
        while !pending.is_empty() {
            pending.sort();
            pending.dedup();
            let candidates = pending.len();
            pending.retain(|file| !self.is_indexed(file));
            crate::telemetry::event(
                serde_json::json!({"operation": "workspace.index_wave", "queued": pending.len(), "cache_hits": candidates - pending.len()}),
            );
            processed += pending.len();
            let workers = thread::available_parallelism()
                .map_or(1, usize::from)
                .min(pending.len());
            let wave = Mutex::new(pending.into_iter());
            let loaded = Mutex::new(Vec::new());
            thread::scope(|scope| {
                for _ in 0..workers {
                    thread::Builder::new()
                        .stack_size(crate::RUNTIME_THREAD_STACK_BYTES)
                        .spawn_scoped(scope, || loop {
                            // Taken in its own statement, so the lock is
                            // released before the file is parsed.
                            let next_file = wave.lock().expect("wave lock").next();
                            let Some(file) = next_file else {
                                break;
                            };
                            let Ok(text) = file
                                .to_file_path()
                                .and_then(|path| fs::read_to_string(path).map_err(drop))
                            else {
                                continue;
                            };
                            let file_loads = self.index_source(&file, &text, knowledge_provider);
                            loaded.lock().expect("loaded lock").extend(file_loads);
                        })
                        .expect("indexing worker starts");
                }
            });
            pending = loaded.into_inner().expect("loaded lock");
        }
        timing.record("processed_files", processed);
        timing.record("cached_files", self.loads_by_file.len());
    }

    pub fn is_indexed(&self, uri: &Url) -> bool {
        self.loads_by_file.contains_key(uri)
    }

    /// Replace the navigation facts recorded for `uri` with those parsed from
    /// `text`, returning the files it loads and the user packages it imports.
    fn index_source(&self, uri: &Url, text: &str, knowledge_provider: &ObjectRegistry) -> Vec<Url> {
        let _timing = crate::telemetry::Timing::start(
            "workspace.index_file",
            serde_json::json!({"uri": uri, "source_bytes": text.len()}),
        );
        self.remove_file(uri);
        let Some(snapshot) = DocumentSnapshot::from_text(text.to_string(), knowledge_provider)
        else {
            self.loads_by_file.insert(uri.clone(), Vec::new());
            return Vec::new();
        };
        self.index_snapshot(uri, &snapshot, knowledge_provider)
    }

    /// Records an existing document's facts without parsing or analyzing its source.
    pub fn index_snapshot(
        &self,
        uri: &Url,
        snapshot: &DocumentSnapshot,
        knowledge_provider: &ObjectRegistry,
    ) -> Vec<Url> {
        self.remove_file(uri);
        let declarations = snapshot.packages().declarations().to_vec();
        if !declarations.is_empty() {
            self.declared_packages_by_file
                .insert(uri.clone(), declarations);
        }
        let mut loads = self.record_loaded_files(uri, snapshot);
        loads.extend(imported_user_package_files(
            uri,
            snapshot,
            knowledge_provider,
        ));
        self.loads_by_file.insert(uri.clone(), loads.clone());
        let definitions = top_level_definitions(uri, snapshot);
        let implementations = top_level_implementations(snapshot);
        let method_declarations = top_level_method_declarations(snapshot);
        let pages = documentation_pages(uri, snapshot);
        let symbols = collect_workspace_symbols(snapshot, uri);
        if !symbols.is_empty() {
            self.symbols_by_file.insert(uri.clone(), symbols);
        }
        if definitions.is_empty()
            && implementations.is_empty()
            && method_declarations.is_empty()
            && pages.is_empty()
        {
            return loads;
        }
        let mut names = Vec::with_capacity(
            definitions.len() + implementations.len() + method_declarations.len() + pages.len(),
        );
        for (name, definition) in definitions {
            self.definitions_by_name
                .entry(name.clone())
                .or_default()
                .push(definition);
            names.push(name);
        }
        for (name, range, kind, requires_declaration) in implementations {
            let name = ObjectName::new(name);
            self.implementations_by_name
                .entry(name.clone())
                .or_default()
                .push(ImplementationLocation {
                    uri: uri.clone(),
                    range,
                    kind,
                    requires_declaration,
                });
            names.push(name);
        }
        for (name, range) in method_declarations {
            let name = ObjectName::new(name);
            self.method_declarations_by_name
                .entry(name.clone())
                .or_default()
                .push(Location::new(uri.clone(), range));
            names.push(name);
        }
        for (name, page) in pages {
            self.pages_by_name
                .entry(name.clone())
                .or_default()
                .push(page);
            names.push(name);
        }
        names.sort();
        names.dedup();
        self.names_by_file.insert(uri.clone(), names);
        loads
    }

    /// The existing files `snapshot` (the document `uri`) loads, resolved
    /// against the directory of `uri` as Macaulay2 resolves them against
    /// `currentFileDirectory`. A file loaded from inside a package's section
    /// is recorded as belonging to that package.
    fn record_loaded_files(&self, uri: &Url, snapshot: &DocumentSnapshot) -> Vec<Url> {
        let Some(directory) = file_directory(uri) else {
            return Vec::new();
        };
        let mut loaded_files = Vec::new();
        for load in collect_loaded_files_in_tree(snapshot.root_node(), snapshot.syntax()) {
            let path = directory.join(load.path);
            let Some(loaded) = path
                .is_file()
                .then(|| Url::from_file_path(path).ok())
                .flatten()
            else {
                continue;
            };
            if let Some(package) = self.package_at(uri, snapshot.position_for_node(load.call)) {
                self.package_of_loaded_file.insert(loaded.clone(), package);
            }
            loaded_files.push(loaded);
        }
        loaded_files
    }

    pub fn remove_file(&self, uri: &Url) {
        self.loads_by_file.remove(uri);
        self.symbols_by_file.remove(uri);
        self.declared_packages_by_file.remove(uri);
        let Some((_, names)) = self.names_by_file.remove(uri) else {
            return;
        };
        for name in names {
            let now_empty = if let Some(mut locations) = self.definitions_by_name.get_mut(&name) {
                locations.retain(|location| &location.uri != uri);
                locations.is_empty()
            } else {
                false
            };
            if now_empty {
                self.definitions_by_name.remove(&name);
            }
            let implementations_empty =
                if let Some(mut locations) = self.implementations_by_name.get_mut(&name) {
                    locations.retain(|location| &location.uri != uri);
                    locations.is_empty()
                } else {
                    false
                };
            if implementations_empty {
                self.implementations_by_name.remove(&name);
            }
            let declarations_empty =
                if let Some(mut declarations) = self.method_declarations_by_name.get_mut(&name) {
                    declarations.retain(|declaration| &declaration.uri != uri);
                    declarations.is_empty()
                } else {
                    false
                };
            if declarations_empty {
                self.method_declarations_by_name.remove(&name);
            }
            let pages_empty = if let Some(mut pages) = self.pages_by_name.get_mut(&name) {
                pages.retain(|page| &page.file != uri);
                pages.is_empty()
            } else {
                false
            };
            if pages_empty {
                self.pages_by_name.remove(&name);
            }
        }
    }

    /// Every indexed file: the open documents and the files they load.
    pub fn indexed_file_uris(&self) -> Vec<Url> {
        self.loads_by_file
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// The package whose private dictionary a global defined at `position`
    /// of `uri` lands in: the package whose section there contains it in a
    /// file declaring packages, the package loading the file otherwise.
    fn package_at(&self, uri: &Url, position: Position) -> Option<ObjectName> {
        match self.declared_packages_by_file.get(uri) {
            Some(declarations) => declarations
                .iter()
                .find(|declaration| declaration.contains(position))
                .map(|declaration| declaration.name.clone()),
            None => self
                .package_of_loaded_file
                .get(uri)
                .map(|package| package.value().clone()),
        }
    }

    /// Whether `uri` is one of the files of `package`: the file declaring
    /// it, or a file loaded from inside its section.
    fn belongs_to_package(&self, uri: &Url, package: &ObjectName) -> bool {
        self.declared_packages_by_file
            .get(uri)
            .is_some_and(|declarations| {
                declarations
                    .iter()
                    .any(|declaration| &declaration.name == package)
            })
            || self
                .package_of_loaded_file
                .get(uri)
                .is_some_and(|loaded_into| loaded_into.value() == package)
    }

    /// Every export of `name` by an indexed package, with the file declaring
    /// the package.
    fn exports_named(&self, name: &str) -> Vec<IndexedExport> {
        self.declared_packages_by_file
            .iter()
            .flat_map(|declarations| {
                let uri = declarations.key().clone();
                declarations
                    .value()
                    .iter()
                    .filter_map(|declaration| {
                        declaration.export_named(name).map(|export| IndexedExport {
                            file: uri.clone(),
                            package: declaration.name.clone(),
                            export: export.clone(),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Whether `viewer` sees the global `name` defined at `range` of `uri`.
    fn is_visible(
        &self,
        viewer: WorkspaceViewer<'_>,
        name: &str,
        uri: &Url,
        range: TextRange,
    ) -> bool {
        if self.privacy == PackagePrivacy::Waived {
            return true;
        }
        let Some(owner) = self.package_at(uri, range.start) else {
            return true;
        };
        // `newPackage` binds the package's own name in `PackageDictionary`,
        // which every session sees.
        if owner.name() == name {
            return true;
        }
        let is_member = match viewer {
            WorkspaceViewer::Document(requester) => self.belongs_to_package(requester, &owner),
            WorkspaceViewer::PackageSession => false,
        };
        is_member
            || self
                .exports_named(name)
                .iter()
                .any(|indexed| indexed.package == owner && indexed.export.definition.name() == name)
    }

    /// Whether `viewer`'s own analysis answers for the file `uri`.
    fn answers_for_itself(viewer: WorkspaceViewer<'_>, uri: &Url) -> bool {
        matches!(viewer, WorkspaceViewer::Document(requester) if requester == uri)
    }

    /// The definitions of `name` that `viewer` sees and `include` accepts. A
    /// name a package exports as a synonym resolves to the definitions of
    /// the global it stands for; an exported name without a global definition
    /// is a symbol created by the export itself, which then defines it.
    fn locations(
        &self,
        name: &str,
        viewer: WorkspaceViewer<'_>,
        include: impl Fn(&DefLocation) -> bool,
    ) -> Vec<Location> {
        let defined_in = |definition: &str, package: Option<&ObjectName>| {
            self.definitions_by_name
                .get(definition)
                .map(|locations| {
                    locations
                        .iter()
                        .filter(|location| !Self::answers_for_itself(viewer, &location.uri))
                        .filter(|location| include(location))
                        .filter(|location| match package {
                            Some(package) => {
                                self.package_at(&location.uri, location.range.start)
                                    .as_ref()
                                    == Some(package)
                            }
                            None => self.is_visible(viewer, name, &location.uri, location.range),
                        })
                        .map(|location| Location::new(location.uri.clone(), location.range))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let mut locations = defined_in(name, None);
        let exports = self.exports_named(name);
        for indexed in &exports {
            if indexed.export.definition.name() != name {
                locations.extend(defined_in(
                    indexed.export.definition.name(),
                    Some(&indexed.package),
                ));
            }
        }
        if locations.is_empty() {
            locations.extend(
                exports
                    .into_iter()
                    .filter(|indexed| !Self::answers_for_itself(viewer, &indexed.file))
                    .map(|indexed| Location::new(indexed.file, indexed.export.range)),
            );
        }
        locations
    }

    /// The definition `viewer` sees of the global `name`, or of the global
    /// a package exports `name` as a synonym of.
    fn visible_definition(&self, name: &str, viewer: WorkspaceViewer<'_>) -> Option<DefLocation> {
        let definition_of = |definition: &str| {
            self.definitions_by_name
                .get(definition)
                .and_then(|locations| {
                    locations
                        .iter()
                        .filter(|location| !Self::answers_for_itself(viewer, &location.uri))
                        .find(|location| {
                            self.is_visible(viewer, definition, &location.uri, location.range)
                        })
                        .cloned()
                })
        };
        definition_of(name).or_else(|| {
            self.exports_named(name)
                .iter()
                .find_map(|IndexedExport { export, .. }| {
                    (export.definition.name() != name)
                        .then(|| definition_of(export.definition.name()))
                        .flatten()
                })
        })
    }

    /// The token type of the global `name` as `viewer` sees it; an exported
    /// name defined nowhere is the symbol the export creates.
    fn semantic_token_type_for(
        &self,
        name: &str,
        viewer: WorkspaceViewer<'_>,
    ) -> Option<M2SemanticTokenType> {
        self.visible_definition(name, viewer)
            .map(|definition| definition.semantic_token_type)
            .or_else(|| {
                (!self.exports_named(name).is_empty()).then_some(M2SemanticTokenType::Symbol)
            })
    }

    fn implementations_for(
        &self,
        name: &str,
        kind: ImplementationKind,
        viewer: WorkspaceViewer<'_>,
    ) -> Vec<Location> {
        let method_declarations = self
            .method_declarations_by_name
            .get(name)
            .map(|declarations| declarations.clone())
            .unwrap_or_default();
        let mut locations =
            self.implementations_by_name
                .get(name)
                .map_or_else(Vec::new, |locations| {
                    locations
                        .iter()
                        .filter(|location| {
                            !Self::answers_for_itself(viewer, &location.uri)
                                && location.kind == kind
                                && self.is_visible(viewer, name, &location.uri, location.range)
                                && (!location.requires_declaration
                                    || method_declarations
                                        .iter()
                                        .any(|declaration| declaration.uri != location.uri))
                        })
                        .map(|location| Location {
                            uri: location.uri.clone(),
                            range: location.range,
                        })
                        .collect::<Vec<_>>()
                });
        locations.sort_by(|left, right| {
            left.uri
                .as_str()
                .cmp(right.uri.as_str())
                .then_with(|| left.range.start.cmp(&right.range.start))
        });
        locations
    }

    fn has_method_declaration_for(&self, name: &str, viewer: WorkspaceViewer<'_>) -> bool {
        self.method_declarations_by_name
            .get(name)
            .is_some_and(|declarations| {
                declarations.iter().any(|declaration| {
                    !Self::answers_for_itself(viewer, &declaration.uri)
                        && self.is_visible(viewer, name, &declaration.uri, declaration.range)
                })
            })
    }
}

impl WorkspaceDocumentationKnowledge for WorkspaceIndex {
    fn documentation(&self, name: &str) -> Option<String> {
        let pages = self.pages_by_name.get(name)?;
        pages
            .iter()
            .min_by(|left, right| {
                left.directness()
                    .cmp(&right.directness())
                    .then_with(|| left.file.as_str().cmp(right.file.as_str()))
            })
            .map(|page| page.markdown.clone())
    }

    fn declaration_code(&self, name: &str, exclude: &Url) -> Option<String> {
        self.visible_definition(name, WorkspaceViewer::Document(exclude))?
            .code
    }
}

impl WorkspaceDocumentationKnowledge for WorkspaceView<'_> {
    fn documentation(&self, name: &str) -> Option<String> {
        match self {
            Self::Document(index) | Self::PackageSession(index) => index.documentation(name),
        }
    }

    fn declaration_code(&self, name: &str, exclude: &Url) -> Option<String> {
        let (index, viewer) = self.viewer(exclude);
        index.visible_definition(name, viewer)?.code
    }
}

impl WorkspaceSymbolKnowledge for WorkspaceIndex {
    fn matching_symbols(&self, query: &str) -> Vec<WorkspaceSourceSymbol> {
        let query = query.to_lowercase();
        let mut symbols = self
            .symbols_by_file
            .iter()
            .flat_map(|symbols| symbols.value().clone())
            .filter(|symbol| symbol.name.name().to_lowercase().contains(&query))
            .collect::<Vec<_>>();
        symbols.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.location.uri.as_str().cmp(right.location.uri.as_str()))
                .then_with(|| left.location.range.start.cmp(&right.location.range.start))
        });
        symbols
    }
}

impl WorkspaceImplementationKnowledge for WorkspaceIndex {
    fn implementations(
        &self,
        name: &str,
        kind: ImplementationKind,
        exclude: &Url,
    ) -> Vec<Location> {
        self.implementations_for(name, kind, WorkspaceViewer::Document(exclude))
    }

    fn has_method_declaration(&self, name: &str, exclude: &Url) -> bool {
        self.has_method_declaration_for(name, WorkspaceViewer::Document(exclude))
    }
}

impl WorkspaceDefinitionKnowledge for WorkspaceIndex {
    fn declarations(&self, name: &str, exclude: &Url) -> Vec<Location> {
        self.locations(name, WorkspaceViewer::Document(exclude), |location| {
            location.is_declaration
        })
    }

    fn definitions(&self, name: &str, exclude: &Url) -> Vec<Location> {
        self.locations(name, WorkspaceViewer::Document(exclude), |_| true)
    }

    fn type_definitions(&self, name: &str, exclude: &Url) -> Vec<Location> {
        self.locations(name, WorkspaceViewer::Document(exclude), |location| {
            location.semantic_token_type == M2SemanticTokenType::Type
        })
    }

    fn is_defined(&self, name: &str, exclude: &Url) -> bool {
        !self.definitions(name, exclude).is_empty()
    }

    fn semantic_token_type(&self, name: &str, exclude: &Url) -> Option<M2SemanticTokenType> {
        self.semantic_token_type_for(name, WorkspaceViewer::Document(exclude))
    }

    fn is_mutable_export(&self, name: &str) -> bool {
        self.exports_named(name)
            .iter()
            .any(|indexed| indexed.export.access == ExportAccess::Mutable)
    }
}

impl WorkspaceView<'_> {
    /// Who the view answers for, given the document `exclude` asking.
    fn viewer<'a>(&'a self, exclude: &'a Url) -> (&'a WorkspaceIndex, WorkspaceViewer<'a>) {
        match self {
            Self::Document(index) => (index, WorkspaceViewer::Document(exclude)),
            Self::PackageSession(index) => (index, WorkspaceViewer::PackageSession),
        }
    }
}

/// For package-session code, `exclude` is the document holding it, whose
/// analysis does not answer for the session; the code sees that document like
/// any other file of its package, through the exports.
impl WorkspaceDefinitionKnowledge for WorkspaceView<'_> {
    fn declarations(&self, name: &str, exclude: &Url) -> Vec<Location> {
        let (index, viewer) = self.viewer(exclude);
        index.locations(name, viewer, |location| location.is_declaration)
    }

    fn definitions(&self, name: &str, exclude: &Url) -> Vec<Location> {
        let (index, viewer) = self.viewer(exclude);
        index.locations(name, viewer, |_| true)
    }

    fn type_definitions(&self, name: &str, exclude: &Url) -> Vec<Location> {
        let (index, viewer) = self.viewer(exclude);
        index.locations(name, viewer, |location| {
            location.semantic_token_type == M2SemanticTokenType::Type
        })
    }

    fn is_defined(&self, name: &str, exclude: &Url) -> bool {
        !self.definitions(name, exclude).is_empty()
    }

    fn semantic_token_type(&self, name: &str, exclude: &Url) -> Option<M2SemanticTokenType> {
        let (index, viewer) = self.viewer(exclude);
        index.semantic_token_type_for(name, viewer)
    }

    fn is_mutable_export(&self, name: &str) -> bool {
        match self {
            Self::Document(index) | Self::PackageSession(index) => index.is_mutable_export(name),
        }
    }
}

impl WorkspaceImplementationKnowledge for WorkspaceView<'_> {
    fn implementations(
        &self,
        name: &str,
        kind: ImplementationKind,
        exclude: &Url,
    ) -> Vec<Location> {
        let (index, viewer) = self.viewer(exclude);
        index.implementations_for(name, kind, viewer)
    }

    fn has_method_declaration(&self, name: &str, exclude: &Url) -> bool {
        let (index, viewer) = self.viewer(exclude);
        index.has_method_declaration_for(name, viewer)
    }
}

impl WorkspaceIndex {
    /// Every indexed installation of a method named `name`, whether or not its
    /// own file declares the method: for the library sources, the corpus
    /// already knows the name is a method function.
    pub fn method_installations(&self, name: &str) -> Vec<Location> {
        self.implementations_by_name
            .get(name)
            .map(|locations| {
                locations
                    .iter()
                    .filter(|location| location.kind == ImplementationKind::Method)
                    .map(|location| Location {
                        uri: location.uri.clone(),
                        range: location.range,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The directory holding the file `uri`.
fn file_directory(uri: &Url) -> Option<PathBuf> {
    uri.to_file_path()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
}

/// The main files of the user packages `snapshot` (the document `uri`)
/// imports: a package the corpus does not know, found as `P.m2` in the
/// directory of `uri`. Macaulay2 searches its `path`, which starts with the
/// session's current directory, and a document's own directory is where its
/// session runs.
fn imported_user_package_files(
    uri: &Url,
    snapshot: &DocumentSnapshot,
    knowledge_provider: &ObjectRegistry,
) -> Vec<Url> {
    let Some(directory) = file_directory(uri) else {
        return Vec::new();
    };
    snapshot
        .imported_packages()
        .iter()
        .filter(|import| {
            knowledge_provider
                .catalog_package_id(&import.package)
                .is_none()
        })
        .map(|import| directory.join(format!("{}.m2", import.package)))
        .filter(|path| path.is_file())
        .filter_map(|path| Url::from_file_path(path).ok())
        .collect()
}

/// Parse `text` and return its global-scope definitions as `(name, range)`,
/// where `range` is the definition site — the same range go-to-definition
/// returns for an in-file symbol.
/// The documentation pages of `snapshot` (the document `uri`), each filed
/// under every name its keys document.
fn documentation_pages(uri: &Url, snapshot: &DocumentSnapshot) -> Vec<(ObjectName, IndexedPage)> {
    snapshot
        .package_documentation()
        .nodes()
        .iter()
        .flat_map(|node| {
            node.keys.iter().filter_map(|key| {
                let name = key.subject.documented_name()?.clone();
                Some((
                    name,
                    IndexedPage {
                        file: uri.clone(),
                        key: key.subject.clone(),
                        markdown: node.markdown.clone(),
                    },
                ))
            })
        })
        .collect()
}

/// The global definitions of `snapshot`, the document `uri`, by name.
fn top_level_definitions(uri: &Url, snapshot: &DocumentSnapshot) -> Vec<(ObjectName, DefLocation)> {
    let knowledge = snapshot.object_registry();
    let analysis = snapshot.analysis();
    analysis
        .binding_states()
        .filter(|binding| binding.state.scope_idx == 0)
        .map(|binding| {
            let definition = DefLocation {
                uri: uri.clone(),
                range: binding.state.span,
                semantic_token_type: local_symbol_semantic_token(&binding, &knowledge).token_type,
                is_declaration: binding.state.span == binding.range,
                code: snapshot.declaration_code(binding).map(str::to_string),
            };
            (binding.name.clone(), definition)
        })
        .collect()
}

fn top_level_implementations(
    snapshot: &DocumentSnapshot,
) -> Vec<(String, TextRange, ImplementationKind, bool)> {
    let analysis = snapshot.analysis();
    let lambda_ranges = snapshot.lambda_value_ranges();
    let mut implementations = analysis
        .binding_states()
        .filter(|binding| binding.state.scope_idx == 0)
        .filter(|binding| {
            binding
                .state
                .value_range
                .is_some_and(|range| lambda_ranges.contains(&range))
        })
        .map(|binding| {
            (
                binding.name.name().to_string(),
                binding.state.span,
                ImplementationKind::Lambda,
                false,
            )
        })
        .collect::<Vec<_>>();

    implementations.extend(analysis.assignment_facts().iter().filter_map(|assignment| {
        if assignment.scope_idx != 0 {
            return None;
        }
        let AssignmentFactKind::MethodInstallation(id) = assignment.kind else {
            return None;
        };
        let installation = analysis.method_installation(id)?;
        if !installation.is_workspace_candidate() {
            return None;
        }
        Some((
            installation.method.head.name().name().to_string(),
            assignment.target_span,
            ImplementationKind::Method,
            !installation.takes_effect(),
        ))
    }));
    implementations.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.start.cmp(&right.1.start))
    });
    implementations.dedup();
    implementations
}

fn top_level_method_declarations(snapshot: &DocumentSnapshot) -> Vec<(String, TextRange)> {
    let analysis = snapshot.analysis();
    let mut declarations = analysis
        .binding_states()
        .filter(|binding| binding.state.scope_idx == 0)
        .filter(|binding| {
            analysis
                .function_for_binding(*binding)
                .is_some_and(|function| function.is_method_function())
        })
        .map(|binding| (binding.name.name().to_string(), binding.state.span))
        .collect::<Vec<_>>();
    declarations.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.start.cmp(&right.1.start))
    });
    declarations.dedup();
    declarations
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object_registry::ObjectRegistry;

    #[test]
    fn indexes_and_looks_up_cross_file_definitions() {
        let builtins = ObjectRegistry::default();
        let index = WorkspaceIndex::default();
        let defs = Url::parse("file:///defs.m2").expect("uri");
        let main = Url::parse("file:///main.m2").expect("uri");

        index.index_file(
            &defs,
            "myHelper = x -> x + 1\nmyHelper = x -> x + 2\nGreeting = new Type of HashTable\n",
            &builtins,
        );

        let found = index.definitions("myHelper", &main);
        assert_eq!(found.len(), 2, "every reassignment should be indexed");
        assert_eq!(found[0].uri, defs);
        assert_eq!(index.declarations("myHelper", &main).len(), 1);
        assert!(
            !index.definitions("Greeting", &main).is_empty(),
            "Greeting type should be indexed"
        );
        // The current document is excluded (its live analysis is authoritative).
        assert!(index.definitions("myHelper", &defs).is_empty());

        // Re-indexing replaces; removal clears.
        index.index_file(&defs, "renamed = 1\n", &builtins);
        assert!(index.definitions("myHelper", &main).is_empty());
        assert_eq!(index.definitions("renamed", &main).len(), 1);
        index.remove_file(&defs);
        assert!(index.definitions("renamed", &main).is_empty());
    }

    #[test]
    fn imported_package_knowledge_classifies_workspace_definitions() {
        let corpus = concat!(
            "{\"kind\":\"meta\",\"default_loaded\":[\"Core\"]}\n",
            "{\"kind\":\"type\",\"name\":\"Type\",",
            "\"package\":\"$Core$Core\",\"class\":\"$Core$Type\",",
            "\"parent\":\"$Core$Thing\",\"ancestors\":[\"$Core$Thing\"]}\n",
            "{\"kind\":\"type\",\"name\":\"MethodFunction\",",
            "\"package\":\"$Core$Core\",\"class\":\"$Core$Type\",",
            "\"parent\":\"$Core$Function\",\"ancestors\":[\"$Core$Function\",\"$Core$Thing\"]}\n",
            "{\"kind\":\"methodFunction\",\"name\":\"packageType\",",
            "\"package\":\"$Pkg$Pkg\",\"class\":\"$Core$MethodFunction\",",
            "\"methods\":[{\"domain\":[\"$Core$ZZ\"],\"typicalValue\":\"$Core$Type\"}]}\n",
        );
        let knowledge = ObjectRegistry::load(corpus);
        let index = WorkspaceIndex::default();
        let definitions = Url::parse("file:///definitions.m2").unwrap();
        let reference = Url::parse("file:///reference.m2").unwrap();

        index.index_file(
            &definitions,
            "needsPackage \"Pkg\"\nGreeting = packageType 1\n",
            &knowledge,
        );

        assert_eq!(
            index.semantic_token_type("Greeting", &reference),
            Some(M2SemanticTokenType::Type)
        );
    }

    #[test]
    fn a_package_shows_only_its_exports_outside_itself() {
        let builtins = ObjectRegistry::default();
        let index = WorkspaceIndex::default();
        let package = Url::parse("file:///Pkg.m2").expect("uri");
        let script = Url::parse("file:///script.m2").expect("uri");
        index.index_file(
            &package,
            concat!(
                "newPackage \"Pkg\"\n",
                "export {\"pub\", \"Syn\" => \"pub\", \"Flag\"}\n",
                "pub = x -> x\n",
                "priv = x -> x\n",
                "endPackage \"Pkg\"\n",
                "after = 1\n",
            ),
            &builtins,
        );

        assert_eq!(index.definitions("pub", &script).len(), 1);
        assert!(index.definitions("priv", &script).is_empty());
        assert!(!index.is_defined("priv", &script));
        assert_eq!(
            index.definitions("Syn", &script),
            index.definitions("pub", &script),
            "a synonym resolves to the global it stands for"
        );
        assert_eq!(
            index.definitions("Flag", &script)[0].range.start,
            pos!(1, 31),
            "an export without a definition is defined by the export"
        );
        assert_eq!(
            index.semantic_token_type("Flag", &script),
            Some(M2SemanticTokenType::Symbol)
        );
        assert_eq!(index.definitions("after", &script).len(), 1);

        let test_view = WorkspaceView::PackageSession(&index);
        assert_eq!(test_view.definitions("pub", &package).len(), 1);
        assert!(test_view.definitions("priv", &package).is_empty());
    }
}
