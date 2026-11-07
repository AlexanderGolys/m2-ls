//! Macaulay2 language-server transport and standalone command-line tools.
//!
//! Editors start `m2-ls` over standard input and output. `m2-lint` reports
//! diagnostics without executing source; `m2-fmt` applies the same formatting
//! settings as the server. The [`cli`] module exposes their process entry point.
//!
//! Builtin metadata and documentation are loaded through [`catalog`]. An existing
//! local catalog selected by `M2_LS_CATALOG` enables offline analysis; otherwise
//! the tools download and verify the M2Book catalog before using its cached copy.
//!
//! ```
//! use m2_ls::catalog::CatalogDigest;
//! let first = CatalogDigest::of(b"catalog bytes");
//! assert_eq!(first, CatalogDigest::of(b"catalog bytes"));
//! ```

use std::backtrace::Backtrace;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::thread;

use dashmap::{DashMap, DashSet};
use tokio::{io, task};
use tower_lsp::{
    jsonrpc::Result,
    lsp_types::request::{
        GotoDeclarationParams, GotoDeclarationResponse, GotoImplementationParams,
        GotoImplementationResponse, GotoTypeDefinitionParams, GotoTypeDefinitionResponse,
    },
    lsp_types::{Range as TextRange, *},
    Client, LanguageServer, LspService, Server,
};

#[macro_use]
mod util;
mod analysis;
mod builtin_index;
mod capabilities;
pub mod catalog;
pub mod cli;
/// The optional client capabilities the server negotiates, such as refresh
/// requests, and acting on them only when the client supports them.
mod client_capabilities;
mod diagnostics;
mod document;
mod documentation;
mod file_load;
mod init_file;
mod library_index;
mod macaulay2;
mod meta;
mod node_metadata;
mod object_hierarchy;
mod object_registry;
mod package_documentation;
mod package_index;
mod record_lsp;
mod semantic_token;
mod session;
mod settings;
mod source;
mod source_package;
mod string_literal;
mod syntax;
mod telemetry;
#[cfg(test)]
mod test_support;
mod typesystem;
mod usage_log;
mod workspace_index;

use capabilities::code_actions::available_code_actions;
use capabilities::completion::{completion_options, completion_response};
use capabilities::document_highlight::{
    document_highlight_provider_capability, document_highlights,
};
use capabilities::document_symbols::collect_document_symbols;
use capabilities::file_operations::{file_operations_capability, FileMoves};
use capabilities::formatting::{
    document_formatting_provider_capability, document_formatting_text_edits,
    folding_range_provider_capability, folding_ranges,
};
use capabilities::hover::hover_response;
use capabilities::inlay_hints::{inlay_hint_provider_capability, inlay_hints_response};
use capabilities::inline_values::{inline_value_provider_capability, inline_values_response};
use capabilities::navigation::{
    document_link_request, document_links_response, global_reference_ranges,
    goto_declaration_response, goto_definition_response, goto_implementation_response,
    goto_type_definition_response, is_valid_m2_identifier, prepare_rename_range, reference_target,
    references_response, rename_edits, resolve_document_link, workspace_symbols_response,
    ReferenceTarget,
};
use capabilities::semantic_tokens::collect_semantic_tokens;
use capabilities::signature_help::signature_help_response;
use capabilities::type_hierarchy::{
    TypeHierarchyCapabilityService, TypeHierarchyContext, TYPE_HIERARCHY_METHOD,
};
use client_capabilities::{
    refresh_if_changed, ClientSupport, InlayHintRefresh, SemanticTokensAugmentSyntax,
    SemanticTokensRefresh, TypeHierarchyDynamicRegistration, WorkspaceRefresh,
};
use diagnostics::{publish_diagnostics, visible_diagnostics};
use document::{DocumentSnapshot, FileProperties};
use init_file::{InitFile, InitFileDirective, InitFileStart, InitFiles};
use library_index::{LibraryIndex, LibrarySources};
use package_index::{Macaulay2Environment, SourceResolver};
use session::{ClientInput, ExitNotificationService, SessionEnd};

use crate::catalog::CatalogSource;
use crate::diagnostics::DiagnosticCode;
use crate::object_hierarchy::{HierarchySnapshot, WorkspaceHierarchy};
use crate::object_registry::ObjectRegistry;
use crate::semantic_token::M2SemanticToken;
use crate::settings::{
    ParsedSettings, ProjectConfiguration, ServerSettings, SettingProblem, SettingsStore,
};
use crate::workspace_index::{
    WorkspaceDefinitionKnowledge, WorkspaceIndex, WorkspaceSymbolKnowledge, WorkspaceView,
};

/// The language server's state for one client session: the open documents, the
/// indexes built from them and the library, and the settings.
#[derive(Debug)]
struct Backend {
    /// The connection to the client, for notifications and server-side
    /// requests.
    client: Client,
    /// The builtin catalog with the default packages loaded, which every
    /// session starts from; init files are added per document.
    registry: OnceLock<ObjectRegistry>,
    /// Where the Macaulay2 installation, the user's application directory and
    /// the server's source override are.
    environment: Macaulay2Environment,
    /// The open documents, each as analyzed for its latest text.
    documents: DashMap<Url, DocumentSnapshot>,
    /// The client's version of each open document, which published diagnostics
    /// carry.
    document_versions: DashMap<Url, i32>,
    /// The global definitions of the open documents and of the files they load
    /// and the packages they import.
    workspace_index: Arc<WorkspaceIndex>,
    /// Serializes source mutations so a stale background refresh cannot restore
    /// definitions after a subsequent edit, deletion, or workspace change.
    workspace_updates: tokio::sync::Mutex<()>,
    /// The library sources navigation enters, replaced when `sources.*`
    /// changes.
    library: SettingsStore<Arc<LibraryIndex>>,
    /// The settings the client sent last.
    settings: SettingsStore<ServerSettings>,
    /// Explicit client settings, retained separately from project-file defaults.
    client_settings: SettingsStore<serde_json::Value>,
    /// Whether the client keeps its syntax highlighting under semantic tokens.
    semantic_tokens_augment_syntax: ClientSupport<SemanticTokensAugmentSyntax>,
    /// Whether the client registers the type hierarchy dynamically.
    type_hierarchy_dynamic_registration: ClientSupport<TypeHierarchyDynamicRegistration>,
    /// Whether the client accepts requests to refresh inlay hints.
    inlay_hint_refresh: ClientSupport<InlayHintRefresh>,
    /// Whether the client accepts requests to refresh semantic tokens.
    semantic_tokens_refresh: ClientSupport<SemanticTokensRefresh>,
    /// The init files the documents' sessions start with, which Macaulay2
    /// runs before any document. `registry` itself starts without one.
    init_files: InitFiles,
    /// The workspace folders the client has open: where a rename looks for
    /// the loads it has to edit. Closing one drops its files from the index.
    workspace_folders: DashSet<Url>,
}

impl Backend {
    /// Session state before initialization selects the optional catalog.
    fn new(client: Client) -> Self {
        let environment = Macaulay2Environment::current();
        let registry = ObjectRegistry::load("");
        let library = configured_library(&environment, &registry, &ServerSettings::default());
        let workspace_index = Arc::new(WorkspaceIndex::default());
        if !registry.has_catalog() {
            workspace_index.configure_fallback_sources(Some(library.source_resolver().clone()));
        }
        Backend {
            client,
            library: SettingsStore::new(library),
            registry: OnceLock::new(),
            environment,
            documents: DashMap::new(),
            document_versions: DashMap::new(),
            workspace_index,
            workspace_updates: tokio::sync::Mutex::new(()),
            settings: SettingsStore::default(),
            client_settings: SettingsStore::new(serde_json::Value::Null),
            semantic_tokens_augment_syntax: ClientSupport::default(),
            type_hierarchy_dynamic_registration: ClientSupport::default(),
            inlay_hint_refresh: ClientSupport::default(),
            semantic_tokens_refresh: ClientSupport::default(),
            init_files: InitFiles::default(),
            workspace_folders: DashSet::new(),
        }
    }

    /// Registry selected during initialization, before document analysis begins.
    fn registry(&self) -> &ObjectRegistry {
        self.registry.get_or_init(|| ObjectRegistry::load(""))
    }

    /// The open workspace folders on disk.
    fn workspace_folder_paths(&self) -> Vec<PathBuf> {
        self.workspace_folders
            .iter()
            .filter_map(|folder| folder.to_file_path().ok())
            .collect()
    }

    /// Global and selected workspace configuration overlaid by the last client settings.
    fn project_configuration(&self) -> ParsedSettings {
        let root = self.workspace_folder_paths().into_iter().min();
        ProjectConfiguration::load(root.as_deref(), &self.client_settings.snapshot())
    }

    /// Re-index files whose explicit loads name a changed path. Diagnostics
    /// are published after the resulting source contexts have been refreshed.
    async fn refresh_loaders_of(&self, path: &Path) {
        for loader in self.workspace_index.loaders_under(path) {
            let Some(document) = self.documents.get(&loader) else {
                self.reindex_from_disk(&loader);
                continue;
            };
            let loaded =
                self.workspace_index
                    .index_snapshot(&loader, document.value(), self.registry());
            drop(document);
            self.index_loaded_in_background(loaded).await;
        }
    }

    /// Refreshes all affected contexts before publishing each document once.
    /// Direct loaders need fresh load diagnostics even if their imported value
    /// metadata did not change, for example when a loaded file disappears.
    async fn refresh_context_diagnostics(&self, paths: &[&Path]) {
        self.index_loaded_in_background(Vec::new()).await;
        let mut changed = self
            .refresh_source_contexts()
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        for path in paths {
            changed.extend(self.workspace_index.loaders_under(path));
        }
        self.publish_context_diagnostics(changed.into_iter().collect(), None)
            .await;
    }

    /// Re-index the open documents and what they load, restoring the files
    /// they need after others left the index.
    async fn reindex_open_documents(&self) {
        let loaded = self
            .documents
            .iter()
            .flat_map(|document| {
                self.workspace_index.index_snapshot(
                    document.key(),
                    document.value(),
                    self.registry(),
                )
            })
            .collect();
        self.index_loaded_in_background(loaded).await;
    }

    /// Drop the closed files at or under `path` from the index.
    fn forget_closed_files_under(&self, path: &Path) -> Vec<Url> {
        let forgotten = self
            .workspace_index
            .files_under(path)
            .into_iter()
            .filter(|file| !self.documents.contains_key(file))
            .collect::<Vec<_>>();
        for file in &forgotten {
            self.workspace_index.forget_file(file);
        }
        forgotten
    }

    /// A file or directory appeared at `path`.
    async fn on_created(&self, path: &Path) {
        if self
            .workspace_folder_paths()
            .iter()
            .any(|root| path.starts_with(root))
        {
            self.index_loaded_in_background(self.workspace_index.sources_under(path))
                .await;
        }
        self.refresh_loaders_of(path).await;
        self.reload_init_file(path).await;
        self.refresh_context_diagnostics(&[path]).await;
    }

    /// The file or directory at `path` is gone.
    async fn on_deleted(&self, path: &Path) {
        self.forget_closed_files_under(path);
        self.refresh_loaders_of(path).await;
        self.reload_init_file(path).await;
        self.refresh_context_diagnostics(&[path]).await;
    }

    /// Files and directories moved: their closed files are indexed where
    /// they went, and every file loading something on either side re-indexed.
    async fn on_moved(&self, moves: &FileMoves) {
        let origins = moves.origins().map(Path::to_path_buf).collect::<Vec<_>>();
        let destinations = moves
            .destinations()
            .map(Path::to_path_buf)
            .collect::<Vec<_>>();
        let moved = origins
            .iter()
            .flat_map(|origin| self.forget_closed_files_under(origin))
            .filter_map(|file| moves.destination(&file))
            .filter(|file| !self.documents.contains_key(file))
            .collect();
        self.index_loaded_in_background(moved).await;
        for path in origins.iter().chain(&destinations) {
            self.refresh_loaders_of(path).await;
        }
        let paths = origins
            .iter()
            .chain(&destinations)
            .map(PathBuf::as_path)
            .collect::<Vec<_>>();
        self.refresh_context_diagnostics(&paths).await;
    }

    /// Index the file `uri` again from its saved text, or drop it from the
    /// index when it can no longer be read.
    fn reindex_from_disk(&self, uri: &Url) {
        let Ok(path) = uri.to_file_path() else {
            return;
        };
        match fs::read_to_string(&path) {
            Ok(text) => self
                .workspace_index
                .index_file(uri, &text, &self.workspace_registry()),
            Err(_) => self.workspace_index.remove_file(uri),
        }
    }

    /// Borrow the registry loaded once for this exact document version.
    fn scoped_index_for<'a>(&self, document: &'a DocumentSnapshot) -> &'a ObjectRegistry {
        document.object_registry()
    }

    /// Answer `request` from the open document `uri`; `None` when it is not
    /// open.
    fn with_document<R>(
        &self,
        uri: &Url,
        request: impl FnOnce(&DocumentSnapshot) -> R,
    ) -> Option<R> {
        let document = self.documents.get(uri)?;
        Some(request(document.value()))
    }

    /// Answer `request` from the open document `uri` and the packages it has
    /// loaded; `None` when it is not open.
    fn with_scoped_document<R>(
        &self,
        uri: &Url,
        request: impl FnOnce(&DocumentSnapshot, &ObjectRegistry) -> R,
    ) -> Option<R> {
        let document = self.documents.get(uri)?;
        let scoped = self.scoped_index_for(document.value());
        Some(request(document.value(), scoped))
    }

    /// The source analyzed at `position` of `document`, with the workspace as
    /// it sees it: the package-session code containing the position — a TEST
    /// block or a documentation example — analyzed as the session that runs
    /// it, or else the document itself.
    fn source_at<'a>(
        &'a self,
        document: &'a DocumentSnapshot,
        position: Position,
    ) -> (&'a DocumentSnapshot, WorkspaceView<'a>) {
        match document.package_session_at(position) {
            Some(session) => (
                &session.snapshot,
                WorkspaceView::PackageSession(&self.workspace_index),
            ),
            None => (document, WorkspaceView::Document(&self.workspace_index)),
        }
    }

    /// What the symbol at `position` of `document` refers to. In package-
    /// session code, a name the code binds itself lives in the code's own
    /// session, so it is local to the code; only the names it gets from the
    /// workspace are shared with other sources.
    fn reference_target_at(
        &self,
        document: &DocumentSnapshot,
        uri: &Url,
        position: Position,
    ) -> Option<ReferenceTarget> {
        let (source, workspace) = self.source_at(document, position);
        let target = reference_target(source, uri, position, &workspace)?;
        let bound_by_session =
            !std::ptr::eq(source, document) && source.binding_at_position(position).is_some();
        Some(if bound_by_session {
            ReferenceTarget::Local
        } else {
            target
        })
    }

    /// Run a request at `position` against the source analyzed there (see
    /// [`Self::source_at`]) and the packages it has loaded.
    fn with_source_at<R>(
        &self,
        uri: &Url,
        position: Position,
        request: impl FnOnce(&DocumentSnapshot, &ObjectRegistry, WorkspaceView) -> R,
    ) -> Option<R> {
        let lookup_timing =
            telemetry::Timing::start("document.lock", serde_json::json!({"uri": uri}));
        let document = self.documents.get(uri)?;
        drop(lookup_timing);
        let (source, workspace) = self.source_at(document.value(), position);
        Some(request(source, source.object_registry(), workspace))
    }

    /// Run a navigation request at `position` against the library sources
    /// indexed so far; while it wanted packages that were not indexed yet,
    /// index them off the session task and run it again.
    async fn navigate<R>(
        &self,
        uri: &Url,
        position: Position,
        request: impl Fn(&DocumentSnapshot, &ObjectRegistry, WorkspaceView, &LibrarySources) -> R,
    ) -> Option<R> {
        const MAX_INDEXING_STEPS: usize = 8;
        for step in 0..=MAX_INDEXING_STEPS {
            let library_index = self.library.snapshot();
            let library = library_index.sources();
            let response = self.with_source_at(uri, position, |document, knowledge, workspace| {
                request(document, knowledge, workspace, &library)
            });
            let wanted_packages = library.into_wanted_packages();
            if wanted_packages.is_empty() || step == MAX_INDEXING_STEPS {
                return response;
            }
            library_index.index_packages(wanted_packages).await;
        }
        unreachable!("navigation returns at its indexing limit")
    }

    /// Collect occurrences of a global symbol across every indexed file
    /// other than `exclude`. Prefers a live open buffer (so unsaved edits are
    /// reflected) and falls back to parsing the on-disk file. Used by both
    /// `references` and `rename` so the cross-file scan lives in one place.
    fn cross_file_global_references<'a>(
        &'a self,
        name: &'a str,
        exclude: &'a Url,
    ) -> impl Iterator<Item = (Url, TextRange)> + 'a {
        self.workspace_index
            .indexed_file_uris()
            .into_iter()
            .filter(move |file_uri| {
                file_uri != exclude
                    && self
                        .workspace_index
                        .shares_global_binding(name, exclude, file_uri)
            })
            .flat_map(move |file_uri| {
                let ranges = if let Some(open) = self.documents.get(&file_uri) {
                    global_reference_ranges(open.value(), name)
                } else if let Ok(path) = file_uri.to_file_path() {
                    fs::read_to_string(path)
                        .ok()
                        .and_then(|text| {
                            DocumentSnapshot::from_text(text, &self.workspace_registry())
                        })
                        .map(|snapshot| global_reference_ranges(&snapshot, name))
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                ranges
                    .into_iter()
                    .map(move |range| (file_uri.clone(), range))
            })
    }

    /// Materializes the common hierarchy from retained indexed and open analyses.
    fn workspace_hierarchy(&self) -> WorkspaceHierarchy {
        let mut snapshots = self.workspace_index.hierarchy_snapshots();
        for document in &self.documents {
            let mut snapshot = HierarchySnapshot::new(document.value(), document.key());
            snapshot.package_context = snapshots
                .get(document.key())
                .and_then(|indexed| indexed.package_context.clone());
            snapshots.insert(document.key().clone(), snapshot);
        }
        WorkspaceHierarchy::new(snapshots, self.registry())
    }

    /// Point library navigation at the sources `sources` configures.
    fn configure_library(&self, settings: &ServerSettings) {
        let library = configured_library(&self.environment, self.registry(), settings);
        self.workspace_index.configure_fallback_sources(
            (!self.registry().has_catalog()).then(|| library.source_resolver().clone()),
        );
        self.library.replace(library);
    }

    /// Analyze again each open document whose location implies other
    /// properties in the library now configured. Returns whether any was.
    fn imply_document_properties(&self) -> bool {
        let library = self.library.snapshot();
        let mut reanalyzed = false;
        for mut document in self.documents.iter_mut() {
            let implied_properties =
                FileProperties::implied_by_location(document.key(), library.source_resolver());
            let registry = self.document_session_registry(document.key(), document.value());
            reanalyzed |= document.imply_properties(implied_properties, &registry);
            let registry = self.document_session_registry(document.key(), document.value());
            reanalyzed |= document.restart_session(&registry);
        }
        reanalyzed
    }

    /// The registry the session of the document at `uri` with `properties`
    /// starts with: the base registry with the init file `directive`
    /// selects, or else the configured one. Library implementation code
    /// starts without one.
    fn session_registry(
        &self,
        uri: &Url,
        directive: Option<&InitFileDirective>,
        properties: FileProperties,
    ) -> ObjectRegistry {
        if properties.primary_code {
            return self.registry().clone();
        }
        let start = self
            .init_files
            .start_for(uri, directive, &self.environment, self.registry());
        self.registry().with_init_file(start)
    }

    /// The registry the session of `document`, at `uri`, starts with as the
    /// document now reads.
    fn document_session_registry(&self, uri: &Url, document: &DocumentSnapshot) -> ObjectRegistry {
        let registry =
            self.session_registry(uri, document.init_file_directive(), document.properties());
        if document.properties().primary_code {
            registry
        } else {
            self.workspace_index.context_registry(uri, &registry)
        }
    }

    /// The registry the files the documents load are analyzed with: they
    /// run in sessions starting with the configured init file.
    fn workspace_registry(&self) -> ObjectRegistry {
        self.registry().with_init_file(self.init_files.configured())
    }

    /// Start the session of each open document again where it should start
    /// with another init file, re-indexing it and republishing its
    /// diagnostics. Returns whether any was.
    async fn restart_sessions(&self) -> bool {
        let mut restarted = Vec::new();
        for mut document in self.documents.iter_mut() {
            let uri = document.key().clone();
            let registry = self.document_session_registry(&uri, document.value());
            if document.restart_session(&registry) {
                self.workspace_index
                    .index_snapshot(&uri, document.value(), self.registry());
                restarted.push(uri);
            }
        }
        for uri in &restarted {
            if let Some(document) = self.documents.get(uri) {
                self.publish_document_diagnostics(uri.clone(), document.value())
                    .await;
            }
        }
        !restarted.is_empty()
    }

    /// Select the init file the documents without a directive start with,
    /// as `sources` configures it, and start their sessions again with it.
    /// Returns whether it changed.
    async fn configure_init_file(&self, settings: &ServerSettings) -> bool {
        let previous_files = self.init_files.files();
        let previous = self.init_files.configured();
        let workspace_folder = self.workspace_folder_paths().into_iter().next();
        let replaced = match InitFiles::locate_settings(
            settings,
            workspace_folder.as_deref(),
            &self.environment,
        ) {
            Ok(location) => self.init_files.configure(location, self.registry()),
            Err(problem) => self.init_files.configure_unavailable(problem),
        };
        if replaced.is_none() {
            return false;
        }
        self.report_configured_init_file(&previous).await;
        self.sync_init_files(&previous_files).await;
        self.restart_sessions().await;
        true
    }

    /// Tell the user why the configured init file cannot be used, when it
    /// became unusable since `previous`.
    async fn report_configured_init_file(&self, previous: &InitFileStart) {
        let current = self.init_files.configured();
        if let Some(problem) = current.problem().filter(|_| current != *previous) {
            self.client
                .show_message(
                    MessageType::WARNING,
                    format!(
                        "m2-ls analyzes every document without an init file: {problem}. \
                         Set `initialization.files` to readable files, or to `[]`."
                    ),
                )
                .await;
        }
    }

    /// Read the init file at `path` again after it changed, if a session may
    /// start with it, and start the sessions starting with it again.
    async fn reload_init_file(&self, path: &Path) {
        let previous_files = self.init_files.files();
        let previous = self.init_files.configured();
        if !self.init_files.reload(path, self.registry()) {
            return;
        }
        self.report_configured_init_file(&previous).await;
        self.sync_init_files(&previous_files).await;
        if self.restart_sessions().await {
            let _ = refresh_if_changed(&self.inlay_hint_refresh, &self.client, true).await;
            let _ = refresh_if_changed(&self.semantic_tokens_refresh, &self.client, true).await;
        }
    }

    /// Bring the index and the client's diagnostics up to date with the
    /// init files read now, `previous` being those read before: a file no
    /// longer read leaves both, and each new reading replaces the old.
    async fn sync_init_files(&self, previous: &[Arc<InitFile>]) {
        let current = self.init_files.files();
        for gone in previous
            .iter()
            .filter(|file| !current.iter().any(|now| now.path() == file.path()))
        {
            if !self.documents.contains_key(gone.uri()) {
                self.workspace_index.forget_file(gone.uri());
                self.client
                    .publish_diagnostics(gone.uri().clone(), Vec::new(), None)
                    .await;
            }
        }
        for read in current
            .iter()
            .filter(|file| !previous.iter().any(|before| Arc::ptr_eq(before, file)))
        {
            self.adopt_init_file(read).await;
        }
    }

    /// Index the init file `start` reads and publish its diagnostics, if no
    /// session started with it before.
    async fn adopt_started_init_file(&self, start: &InitFileStart) {
        for init_file in start.files() {
            if !self.workspace_index.is_init_file(init_file.uri()) {
                self.adopt_init_file(init_file).await;
            }
        }
    }

    /// Index `init_file`, whose globals only the sessions starting with it
    /// see, and publish its diagnostics, unless it is open: its buffer then
    /// does both.
    async fn adopt_init_file(&self, init_file: &InitFile) {
        self.workspace_index.add_init_file(init_file.uri());
        if self.documents.contains_key(init_file.uri()) {
            return;
        }
        let loaded = self.workspace_index.index_snapshot(
            init_file.uri(),
            init_file.snapshot(),
            self.registry(),
        );
        self.index_loaded_in_background(loaded).await;
        self.publish_init_file_diagnostics(init_file).await;
    }

    /// Publish the diagnostics of `init_file` as a file of the workspace,
    /// not of the buffer that starts with it, unless it is open and
    /// publishes its own. Outside the workspace folders the file is not the
    /// user's project, so only what the grammar cannot parse is published.
    async fn publish_init_file_diagnostics(&self, init_file: &InitFile) {
        if self.documents.contains_key(init_file.uri()) {
            return;
        }
        let settings = self.settings.snapshot();
        let library = self.library.snapshot();
        let in_workspace = self
            .workspace_folder_paths()
            .iter()
            .any(|folder| init_file.path().starts_with(folder));
        let diagnostics = visible_diagnostics(
            init_file.snapshot(),
            init_file.uri(),
            &settings.diagnostics,
            library.source_resolver(),
        )
        .into_iter()
        .filter(|diagnostic| {
            in_workspace
                || DiagnosticCode::from_lsp(diagnostic)
                    .is_some_and(DiagnosticCode::is_parse_failure)
        })
        .collect::<Vec<_>>();
        usage_log::diagnostics(init_file.uri(), init_file.snapshot().text(), &diagnostics);
        self.client
            .publish_diagnostics(init_file.uri().clone(), diagnostics, None)
            .await;
    }

    /// Publish the diagnostics of every init file read.
    async fn publish_init_files_diagnostics(&self) {
        for init_file in self.init_files.files() {
            self.publish_init_file_diagnostics(&init_file).await;
        }
    }

    /// Ask the client to report changes to the configured init file
    /// wherever it is: the `.m2` watcher covers only the workspace.
    async fn watch_init_file(&self) {
        const METHOD: &str = "workspace/didChangeWatchedFiles";
        const ID: &str = "m2-ls-watch-init-file";
        let _ = self
            .client
            .unregister_capability(vec![Unregistration {
                id: ID.to_string(),
                method: METHOD.to_string(),
            }])
            .await;
        let Some(location) = self.init_files.configured_location() else {
            return;
        };
        let watchers = location
            .paths()
            .into_iter()
            .filter_map(|path| {
                let base = Url::from_directory_path(path.parent()?).ok()?;
                let name = path.file_name()?.to_str()?;
                Some(serde_json::json!({"globPattern": {"baseUri": base, "pattern": name}}))
            })
            .collect::<Vec<_>>();
        let _ = self
            .client
            .register_capability(vec![Registration {
                id: ID.to_string(),
                method: METHOD.to_string(),
                register_options: Some(serde_json::json!({
                    "watchers": watchers
                })),
            }])
            .await;
    }

    /// Publish the diagnostics of the open `document` at `uri` the settings
    /// allow, with its current version.
    async fn publish_document_diagnostics(&self, uri: Url, document: &DocumentSnapshot) {
        let settings = self.settings.snapshot();
        let version = self.document_versions.get(&uri).map(|version| *version);
        let library = self.library.snapshot();
        publish_diagnostics(
            &self.client,
            uri,
            document,
            &settings.diagnostics,
            library.source_resolver(),
            version,
        )
        .await;
    }

    /// Tell the user, not just the log, which settings were not read as
    /// given and why.
    async fn report_setting_problems(&self, problems: &[SettingProblem]) {
        if problems.is_empty() {
            return;
        }
        let listed = problems
            .iter()
            .map(SettingProblem::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        self.client
            .show_message(
                MessageType::WARNING,
                format!("m2-ls settings need attention: {listed}"),
            )
            .await;
    }

    /// Replace the settings with `settings`, then bring everything they decide
    /// up to date: the library sources and the init file, the published
    /// diagnostics, and, through `refresh_client`, inlay hints and semantic
    /// tokens.
    async fn apply_settings(
        &self,
        settings: ServerSettings,
        refresh_client: &(impl WorkspaceRefresh<InlayHintRefresh>
              + WorkspaceRefresh<SemanticTokensRefresh>
              + ?Sized),
    ) -> Result<()> {
        let previous = self.settings.replace(settings.clone());
        if previous.catalog != settings.catalog {
            self.client
                .log_message(MessageType::WARNING, "Changing catalog settings requires a server restart; set it in initializationOptions.")
                .await;
        }
        let mut reanalyzed = false;
        if previous.sources != settings.sources
            || previous.macaulay2 != settings.macaulay2
            || previous.initialization != settings.initialization
        {
            self.configure_library(&settings);
            reanalyzed = self.imply_document_properties();
            if self.configure_init_file(&settings).await {
                reanalyzed = true;
                self.watch_init_file().await;
            }
        }

        if previous.diagnostics != settings.diagnostics || reanalyzed {
            self.publish_init_files_diagnostics().await;
            let library = self.library.snapshot();
            let diagnostics = self
                .documents
                .iter()
                .map(|document| {
                    (
                        document.key().clone(),
                        visible_diagnostics(
                            document.value(),
                            document.key(),
                            &settings.diagnostics,
                            library.source_resolver(),
                        ),
                    )
                })
                .collect::<Vec<_>>();
            for (uri, diagnostics) in diagnostics {
                if let Some(document) = self.documents.get(&uri) {
                    usage_log::diagnostics(&uri, document.text(), &diagnostics);
                }
                self.client
                    .publish_diagnostics(uri, diagnostics, None)
                    .await;
            }
        }

        let inlay_hints = refresh_if_changed(
            &self.inlay_hint_refresh,
            refresh_client,
            previous.inlay_hints != settings.inlay_hints || reanalyzed,
        )
        .await;
        let semantic_tokens = refresh_if_changed(
            &self.semantic_tokens_refresh,
            refresh_client,
            previous.semantic_tokens != settings.semantic_tokens || reanalyzed,
        )
        .await;
        inlay_hints.and(semantic_tokens)
    }

    /// Analyze a newly opened document, starting its session with the init file
    /// it selects, index it with the files it loads, and publish its
    /// diagnostics.
    async fn on_open(&self, params: TextDocumentItem) {
        let _timing = telemetry::Timing::start(
            "document.open",
            serde_json::json!({"uri": params.uri, "source_bytes": params.text.len()}),
        );
        let uri = params.uri;
        let implied_properties =
            FileProperties::implied_by_location(&uri, self.library.snapshot().source_resolver());
        let Some(mut document) = DocumentSnapshot::from_text_with_implied_properties(
            params.text,
            &self.session_registry(&uri, None, implied_properties),
            implied_properties,
        ) else {
            return;
        };
        document.restart_session(&self.document_session_registry(&uri, &document));
        let started = document.object_registry().init_file().clone();
        let mut loaded = self
            .workspace_index
            .index_snapshot(&uri, &document, self.registry());
        loaded.extend(
            self.workspace_index
                .repository_files(&uri, &self.workspace_folder_paths()),
        );
        self.document_versions.insert(uri.clone(), params.version);
        self.documents.insert(uri.clone(), document);
        self.index_loaded_in_background(loaded).await;
        let refreshed = self.refresh_source_contexts();
        self.adopt_started_init_file(&started).await;
        if let Some(document) = self.documents.get(&uri) {
            self.publish_document_diagnostics(uri.clone(), document.value())
                .await;
        }
        self.publish_context_diagnostics(refreshed, Some(&uri))
            .await;
    }

    /// Apply the client's edits to the open document `uri` as of `version`,
    /// analyze it again, and publish its diagnostics.
    async fn on_change(
        &self,
        uri: Url,
        version: i32,
        changes: Vec<TextDocumentContentChangeEvent>,
    ) {
        let _timing = telemetry::Timing::start(
            "document.change",
            serde_json::json!({"uri": uri, "version": version, "changes": changes.len()}),
        );
        self.document_versions.insert(uri.clone(), version);
        let (started, loaded) = {
            let Some(mut document) = self.documents.get_mut(&uri) else {
                return;
            };
            let registry = self.document_session_registry(&uri, document.value());
            if document.apply_changes(&changes, &registry).is_none() {
                return;
            }
            // An edit may select another init file.
            let registry = self.document_session_registry(&uri, document.value());
            document.restart_session(&registry);
            let library = self.library.snapshot();
            let library_file = uri.to_file_path().ok().is_some_and(|path| {
                library
                    .source_resolver()
                    .package_of_source_file(&path)
                    .is_some()
            });
            let loaded = if !library_file || !self.registry().has_catalog() {
                self.workspace_index
                    .index_snapshot(&uri, document.value(), self.registry())
            } else {
                Vec::new()
            };
            (document.object_registry().init_file().clone(), loaded)
        };
        self.index_loaded_in_background(loaded).await;
        let refreshed = self.refresh_source_contexts();
        self.adopt_started_init_file(&started).await;

        if let Some(document) = self.documents.get(&uri) {
            self.publish_document_diagnostics(uri.clone(), document.value())
                .await;
        }
        self.publish_context_diagnostics(refreshed, Some(&uri))
            .await;
    }

    /// Refresh user-source dependencies on open/save, sharing the open
    /// document's facts. Installed sources are analyzed only when opened;
    /// their dependencies stay out of the workspace index.
    async fn index_loaded_in_background(&self, loaded: Vec<Url>) {
        let index = Arc::clone(&self.workspace_index);
        let registry = self.workspace_registry();
        let library = self.library.snapshot();
        let open = self
            .documents
            .iter()
            .map(|document| document.key().clone())
            .collect();
        task::spawn_blocking(move || {
            index.index_files_from_disk_where(loaded, &registry, |file| {
                !registry.has_catalog()
                    || file.to_file_path().is_ok_and(|path| {
                        library
                            .source_resolver()
                            .package_of_source_file(&path)
                            .is_none()
                    })
            });
            index.rebuild_import_contexts();
            index.refresh_closed_contexts(&registry, &open);
        })
        .await
        .expect("workspace indexing task panicked");
        let _ = refresh_if_changed(&self.semantic_tokens_refresh, &self.client, true).await;
    }

    /// Reanalyzes open files against definitions supplied by their loaders and
    /// sibling includes, preserving their own unsaved text and lexical scopes.
    fn refresh_source_contexts(&self) -> Vec<Url> {
        let mut refreshed = Vec::new();
        for mut document in self.documents.iter_mut() {
            let registry = self.document_session_registry(document.key(), document.value());
            if document.restart_session(&registry) {
                refreshed.push(document.key().clone());
                self.workspace_index.index_snapshot(
                    document.key(),
                    document.value(),
                    self.registry(),
                );
            }
        }
        refreshed
    }

    /// Publishes diagnostics after a source context changes, skipping a document
    /// whose current operation already published its own diagnostics.
    async fn publish_context_diagnostics(&self, files: Vec<Url>, exclude: Option<&Url>) {
        for uri in files {
            if Some(&uri) != exclude {
                if let Some(document) = self.documents.get(&uri) {
                    self.publish_document_diagnostics(uri.clone(), document.value())
                        .await;
                }
            }
        }
    }

    /// Analyze a saved document in full off the session task, keeping the
    /// result only if the document did not change meanwhile; a saved init file
    /// is read again.
    async fn on_save(&self, uri: Url) {
        let _timing = telemetry::Timing::start("document.save", serde_json::json!({"uri": uri}));
        self.library.snapshot().invalidate_saved_file(&uri);
        if let Ok(path) = uri.to_file_path() {
            self.reload_init_file(&path).await;
        }
        let Some(version) = self.document_versions.get(&uri).map(|version| *version) else {
            return;
        };
        let job = {
            let Some(document) = self.documents.get(&uri) else {
                return;
            };
            document.save_job(self.document_session_registry(&uri, document.value()))
        };
        let saved = task::spawn_blocking(job)
            .await
            .expect("save analysis task panicked");
        let loaded = {
            let Some(current) = self.document_versions.get(&uri) else {
                return;
            };
            if *current != version {
                return;
            }
            let dashmap::mapref::entry::Entry::Occupied(mut document) =
                self.documents.entry(uri.clone())
            else {
                return;
            };
            if document.get().text() != saved.text() {
                return;
            }
            let loaded = self
                .workspace_index
                .index_snapshot(&uri, &saved, self.registry());
            document.insert(saved);
            loaded
        };
        self.index_loaded_in_background(loaded).await;
        let refreshed = self.refresh_source_contexts();
        if let Some(document) = self.documents.get(&uri) {
            self.publish_document_diagnostics(uri.clone(), document.value())
                .await;
        }
        self.publish_context_diagnostics(refreshed, Some(&uri))
            .await;
    }
}

/// Preserve panic diagnostics alongside request and stage timings.
fn install_panic_logging() {
    panic::set_hook(Box::new(|panic_info| {
        telemetry::event(serde_json::json!({
            "operation": "panic", "message": panic_info.to_string(),
            "backtrace": Backtrace::force_capture().to_string(),
        }));
    }));
    telemetry::event(
        serde_json::json!({"operation": "startup", "version": env!("CARGO_PKG_VERSION")}),
    );
}

impl Backend {
    /// Runs an explicitly requested operation in a fresh M2 process.
    async fn execute_runtime_command(
        &self,
        params: ExecuteCommandParams,
    ) -> Result<serde_json::Value> {
        let invalid =
            |message: &str| tower_lsp::jsonrpc::Error::invalid_params(message.to_string());
        let argument = params
            .arguments
            .first()
            .ok_or_else(|| invalid("expected one object argument"))?;
        let uri: Url = serde_json::from_value(
            argument
                .get("uri")
                .cloned()
                .ok_or_else(|| invalid("uri is required"))?,
        )
        .map_err(|_| invalid("invalid uri"))?;
        let path = uri
            .to_file_path()
            .map_err(|_| invalid("uri must name a local file"))?;
        let directory = path
            .parent()
            .ok_or_else(|| invalid("file has no parent directory"))?;
        let quote = |text: &str| serde_json::to_string(text).expect("string serialization");
        let code = match params.command.as_str() {
            "m2-ls.executeCell" => argument
                .get("code")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| invalid("executeCell requires code containing the selected cell"))?
                .to_string(),
            "m2-ls.runTests" => {
                let package = argument
                    .get("package")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| invalid("runTests requires a package name"))?;
                format!(
                    "check(loadPackage({}, FileName => {}), Verbose => true)",
                    quote(package),
                    quote(&path.to_string_lossy())
                )
            }
            _ => format!("input {}", quote(&path.to_string_lossy())),
        };
        let child = tokio::process::Command::new("M2")
            .args(["--silent", "--no-readline", "--no-prompts", "--stop", "-e"])
            .arg(format!("{code}\nexit 0"))
            .current_dir(directory)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| invalid(&format!("cannot start M2: {error}")))?;
        let output = child
            .wait_with_output()
            .await
            .map_err(|error| invalid(&format!("M2 failed: {error}")))?;
        Ok(
            serde_json::json!({"success": output.status.success(), "exitCode": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout), "stderr": String::from_utf8_lossy(&output.stderr)}),
        )
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        #[allow(deprecated)]
        let folders = params
            .workspace_folders
            .clone()
            .map(|folders| folders.into_iter().map(|folder| folder.uri).collect())
            .or_else(|| params.root_uri.clone().map(|root| vec![root]))
            .unwrap_or_default();
        for folder in folders {
            self.workspace_folders.insert(folder);
        }
        self.semantic_tokens_augment_syntax
            .negotiate(&params.capabilities);
        self.type_hierarchy_dynamic_registration
            .negotiate(&params.capabilities);
        self.inlay_hint_refresh.negotiate(&params.capabilities);
        self.semantic_tokens_refresh.negotiate(&params.capabilities);

        let client_settings = params
            .initialization_options
            .unwrap_or(serde_json::Value::Null);
        self.client_settings.replace(client_settings.clone());
        let parsed = self.project_configuration();
        let corpus = if parsed.settings.catalog.enabled {
            let corpus = CatalogSource::load_configured(&parsed.settings.catalog);
            if !corpus.is_empty() {
                if let Ok(source @ CatalogSource::Cache(_)) =
                    CatalogSource::configured(&parsed.settings.catalog)
                {
                    thread::spawn(move || {
                        if let Err(error) = source.update() {
                            eprintln!("m2-ls: builtin catalog not updated: {error}");
                        }
                    });
                }
            }
            corpus
        } else {
            String::new()
        };
        let _ = self.registry.set(ObjectRegistry::load(&corpus));
        usage_log::record(
            "catalog",
            serde_json::json!({
                "packages": self.registry().package_names_with_prefix("", usize::MAX).len(),
                "loaded_objects": self.registry().len(),
                "available": self.registry().has_catalog(),
            }),
        );
        self.configure_library(&parsed.settings);
        self.settings.replace(parsed.settings);
        self.report_setting_problems(&parsed.problems).await;

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::INCREMENTAL),
                        save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                        ..Default::default()
                    },
                )),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![
                        WorkspaceHierarchy::COMMAND.to_string(),
                        "m2-ls.executeFile".into(),
                        "m2-ls.executeCell".into(),
                        "m2-ls.runTests".into(),
                    ],
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                references_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                })),
                document_formatting_provider: document_formatting_provider_capability(),
                folding_range_provider: folding_range_provider_capability(),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                completion_provider: Some(completion_options()),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    retrigger_characters: Some(vec![",".to_string()]),
                    work_done_progress_options: Default::default(),
                }),
                definition_provider: Some(OneOf::Left(true)),
                declaration_provider: Some(DeclarationCapability::Simple(true)),
                type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
                implementation_provider: Some(ImplementationProviderCapability::Simple(true)),
                document_link_provider: Some(DocumentLinkOptions {
                    resolve_provider: Some(true),
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                }),
                document_symbol_provider: Some(OneOf::Left(true)),
                document_highlight_provider: document_highlight_provider_capability(),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        code_action_kinds: Some(vec![
                            CodeActionKind::QUICKFIX,
                            CodeActionKind::REFACTOR_REWRITE,
                            CodeActionKind::SOURCE_FIX_ALL,
                        ]),
                        ..CodeActionOptions::default()
                    },
                )),
                inlay_hint_provider: inlay_hint_provider_capability(),
                inline_value_provider: inline_value_provider_capability(),
                workspace: Some(WorkspaceServerCapabilities {
                    workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                        supported: Some(true),
                        change_notifications: Some(OneOf::Left(true)),
                    }),
                    file_operations: Some(file_operations_capability()),
                }),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: M2SemanticToken::legend(),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: None,
                            work_done_progress_options: WorkDoneProgressOptions::default(),
                        },
                    ),
                ),
                ..Default::default()
            },
            server_info: Some(server_info()),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        // Notifications such as the init file's diagnostics may only follow
        // the initialization.
        self.configure_init_file(&self.settings.snapshot()).await;
        if self.type_hierarchy_dynamic_registration.is_supported()
            && self
                .client
                .register_capability(vec![Registration {
                    id: "m2-ls-type-hierarchy".to_string(),
                    method: TYPE_HIERARCHY_METHOD.to_string(),
                    register_options: Some(serde_json::json!({
                        "documentSelector": [
                            { "language": "macaulay2" }
                        ]
                    })),
                }])
                .await
                .is_ok()
        {
            self.client
                .log_message(MessageType::INFO, "Macaulay2 type hierarchy registered")
                .await;
        }

        // Watch the workspace for `.m2` changes made outside the editor so the
        // cross-file definition index stays fresh.
        let mut watchers = vec![
            serde_json::json!({"globPattern": "**/*.m2"}),
            serde_json::json!({"globPattern": format!("**/{}", ProjectConfiguration::FILE_NAME)}),
        ];
        if let Some(global) = ProjectConfiguration::global_path() {
            if let Some(directory) = global
                .parent()
                .and_then(|path| Url::from_directory_path(path).ok())
            {
                watchers.push(serde_json::json!({"globPattern": {
                    "baseUri": directory, "pattern": ProjectConfiguration::FILE_NAME
                }}));
            }
        }
        let _ = self
            .client
            .register_capability(vec![Registration {
                id: "m2-ls-watch-m2-files".to_string(),
                method: "workspace/didChangeWatchedFiles".to_string(),
                register_options: Some(serde_json::json!({
                    "watchers": watchers
                })),
            }])
            .await;
        self.watch_init_file().await;

        self.client
            .log_message(
                MessageType::INFO,
                format!(
                    "Macaulay2 LSP initialized with {} builtin symbols",
                    self.registry().len()
                ),
            )
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let _update = self.workspace_updates.lock().await;
        self.on_open(TextDocumentItem {
            uri: params.text_document.uri,
            language_id: "macaulay2".to_string(),
            version: params.text_document.version,
            text: params.text_document.text,
        })
        .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let _update = self.workspace_updates.lock().await;
        self.on_change(
            params.text_document.uri,
            params.text_document.version,
            params.content_changes,
        )
        .await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let _update = self.workspace_updates.lock().await;
        self.on_save(params.text_document.uri).await;
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        let _update = self.workspace_updates.lock().await;
        self.client_settings.replace(params.settings.clone());
        let parsed = self.project_configuration();
        self.report_setting_problems(&parsed.problems).await;
        if let Err(error) = self.apply_settings(parsed.settings, &self.client).await {
            self.client
                .log_message(
                    MessageType::WARNING,
                    format!("Failed to refresh Macaulay2 inlay hints or semantic tokens: {error}"),
                )
                .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        // Invalidate live queries and queued save results immediately; an
        // earlier update may still hold the index lock while awaiting work.
        self.document_versions.remove(&uri);
        self.documents.remove(&uri);
        let _update = self.workspace_updates.lock().await;
        // Re-index from disk so the workspace index reflects the saved file
        // rather than the last in-editor edit.
        self.reindex_from_disk(&uri);
        self.workspace_index.rebuild_import_contexts();
        self.publish_context_diagnostics(self.refresh_source_contexts(), None)
            .await;
        // A closed init file is a workspace file again.
        if let Some(init_file) = self
            .init_files
            .files()
            .into_iter()
            .find(|init_file| init_file.uri() == &uri)
        {
            self.publish_init_file_diagnostics(&init_file).await;
        }
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let _update = self.workspace_updates.lock().await;
        for change in params.changes {
            let Ok(path) = change.uri.to_file_path() else {
                continue;
            };
            if path
                .file_name()
                .is_some_and(|name| name == ProjectConfiguration::FILE_NAME)
            {
                let parsed = self.project_configuration();
                self.report_setting_problems(&parsed.problems).await;
                let _ = self.apply_settings(parsed.settings, &self.client).await;
                continue;
            }
            match change.typ {
                FileChangeType::CREATED => self.on_created(&path).await,
                FileChangeType::DELETED => self.on_deleted(&path).await,
                // Open documents are indexed from their live buffer; ignore
                // disk changes to them to avoid clobbering unsaved edits.
                // Files neither indexed nor loaded by an indexed file stay
                // out of the index; one an indexed file loads joins it once
                // it can be read.
                _ => {
                    if !self.documents.contains_key(&change.uri)
                        && (self.workspace_index.is_indexed(&change.uri)
                            || !self.workspace_index.loaders_under(&path).is_empty())
                    {
                        self.reindex_from_disk(&change.uri);
                    }
                    self.reload_init_file(&path).await;
                    self.refresh_context_diagnostics(&[&path]).await;
                }
            }
        }
    }

    async fn did_change_workspace_folders(&self, params: DidChangeWorkspaceFoldersParams) {
        let _update = self.workspace_updates.lock().await;
        let removed = params
            .event
            .removed
            .into_iter()
            .filter_map(|folder| self.workspace_folders.remove(&folder.uri))
            .filter_map(|folder| folder.to_file_path().ok())
            .collect::<Vec<_>>();
        for folder in params.event.added {
            self.workspace_folders.insert(folder.uri);
        }
        let parsed = self.project_configuration();
        self.report_setting_problems(&parsed.problems).await;
        let _ = self.apply_settings(parsed.settings, &self.client).await;
        // Relative initialization files belong to the first workspace folder,
        // and which init files are the user's project changed.
        if self.configure_init_file(&self.settings.snapshot()).await {
            self.watch_init_file().await;
        }
        self.publish_init_files_diagnostics().await;
        if removed.is_empty() {
            return;
        }
        for folder in &removed {
            self.forget_closed_files_under(folder);
        }
        self.reindex_open_documents().await;
    }

    async fn did_create_files(&self, params: CreateFilesParams) {
        let _update = self.workspace_updates.lock().await;
        for path in params
            .files
            .iter()
            .filter_map(|file| Url::parse(&file.uri).ok()?.to_file_path().ok())
        {
            self.on_created(&path).await;
        }
    }

    async fn did_delete_files(&self, params: DeleteFilesParams) {
        let _update = self.workspace_updates.lock().await;
        for path in params
            .files
            .iter()
            .filter_map(|file| Url::parse(&file.uri).ok()?.to_file_path().ok())
        {
            self.on_deleted(&path).await;
        }
    }

    async fn will_rename_files(&self, params: RenameFilesParams) -> Result<Option<WorkspaceEdit>> {
        let moves = params.files.into_iter().collect::<FileMoves>();
        let index = Arc::clone(&self.workspace_index);
        let folders = self.workspace_folder_paths();
        let edits = task::spawn_blocking(move || {
            let on_disk = moves.loaders_on_disk(&folders, |file| index.is_indexed(file));
            moves.load_edits(index.file_loads().into_iter().chain(on_disk))
        })
        .await
        .expect("rename edit task panicked");
        Ok((!edits.is_empty()).then(|| WorkspaceEdit {
            changes: Some(edits),
            ..WorkspaceEdit::default()
        }))
    }

    async fn did_rename_files(&self, params: RenameFilesParams) {
        let _update = self.workspace_updates.lock().await;
        let moves = params.files.into_iter().collect::<FileMoves>();
        self.on_moved(&moves).await;
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(self
            .with_source_at(uri, position, |document, knowledge, workspace| {
                hover_response(document, uri, position, &knowledge.at(position), &workspace)
            })
            .flatten())
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        Ok(self
            .with_source_at(uri, position, |document, knowledge, workspace| {
                let source_symbols = if self.registry().has_catalog() {
                    Vec::new()
                } else {
                    self.workspace_index
                        .matching_symbols("")
                        .into_iter()
                        .filter(|symbol| workspace.is_defined(symbol.name.name(), uri))
                        .collect()
                };
                completion_response(
                    document,
                    uri,
                    position,
                    &knowledge.at(position),
                    &source_symbols,
                )
            })
            .flatten())
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(self
            .with_source_at(uri, position, |document, knowledge, _| {
                signature_help_response(document, position, &knowledge.at(position))
            })
            .flatten())
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let augments_syntax_tokens = self.semantic_tokens_augment_syntax.is_supported();
        let settings = self.settings.snapshot().semantic_tokens;
        Ok(self.with_scoped_document(&uri, |document, knowledge| {
            let tokens = collect_semantic_tokens(
                document,
                knowledge,
                &self.workspace_index,
                &uri,
                &settings,
                augments_syntax_tokens,
            );
            SemanticTokensResult::Tokens(SemanticTokens {
                result_id: None,
                data: tokens,
            })
        }))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        Ok(self.with_document(&uri, |document| {
            DocumentSymbolResponse::Nested(collect_document_symbols(document))
        }))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        let settings = self.settings.snapshot();
        let position = params.range.start;
        Ok(self
            .with_source_at(uri, position, |document, _, _| {
                if document.properties().primary_code {
                    return None;
                }
                let mut action_settings = settings.diagnostics.clone();
                action_settings.emit_as.fix_action_only.clear();
                let mut diagnostics = visible_diagnostics(
                    document,
                    uri,
                    &action_settings,
                    self.library.snapshot().source_resolver(),
                );
                for diagnostic in params
                    .context
                    .diagnostics
                    .into_iter()
                    .filter(|diagnostic| settings.diagnostics.allows_lsp_diagnostic(diagnostic))
                {
                    if !diagnostics.contains(&diagnostic) {
                        diagnostics.push(diagnostic);
                    }
                }
                let mut actions = available_code_actions(
                    document,
                    uri,
                    position,
                    &diagnostics,
                    &settings.formatting,
                )?;
                if let Some(only) = &params.context.only {
                    actions.retain(|action| match action {
                        CodeActionOrCommand::CodeAction(action) => {
                            action.kind.as_ref().is_some_and(|kind| {
                                only.iter().any(|requested| {
                                    kind == requested
                                        || kind
                                            .as_str()
                                            .strip_prefix(requested.as_str())
                                            .is_some_and(|suffix| suffix.starts_with('.'))
                                })
                            })
                        }
                        CodeActionOrCommand::Command(_) => false,
                    });
                }
                (!actions.is_empty()).then_some(actions)
            })
            .flatten())
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = &params.text_document.uri;
        let options = self.settings.snapshot().inlay_hints;
        Ok(self.with_scoped_document(uri, |document, knowledge| {
            let mut hints = inlay_hints_response(document, params.range, &options, knowledge);
            for session in document.package_sessions() {
                hints.extend(inlay_hints_response(
                    &session.snapshot,
                    params.range,
                    &options,
                    session.snapshot.object_registry(),
                ));
            }
            hints
        }))
    }

    async fn inline_value(&self, params: InlineValueParams) -> Result<Option<Vec<InlineValue>>> {
        let uri = &params.text_document.uri;
        Ok(self.with_scoped_document(uri, |document, knowledge| {
            let mut values = inline_values_response(document, params.range, knowledge);
            for session in document.package_sessions() {
                values.extend(inline_values_response(
                    &session.snapshot,
                    params.range,
                    session.snapshot.object_registry(),
                ));
            }
            values
        }))
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(self
            .with_source_at(uri, position, |document, knowledge, _| {
                document_highlights(document, position, &knowledge.at(position))
            })
            .flatten())
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;

        // Resolve the target and the current file's references, then drop the
        // document guard before scanning other files (avoids holding a DashMap
        // ref across the cross-file pass).
        let (name, mut locations) = {
            let document = match self.documents.get(uri) {
                Some(document) => document,
                None => return Ok(None),
            };
            // An indexed generator `z_1` is its own variable, whose
            // references stay with the file that declares its ring.
            if let Some(ranges) = document.analysis().indexed_ring_generator_references(
                position,
                document.value(),
                include_declaration,
            ) {
                return Ok(Some(
                    ranges
                        .into_iter()
                        .map(|range| Location {
                            uri: uri.clone(),
                            range,
                        })
                        .collect(),
                ));
            }
            match self.reference_target_at(document.value(), uri, position) {
                None | Some(ReferenceTarget::Local) => {
                    // A local binding's references never leave its source.
                    let (source, _) = self.source_at(document.value(), position);
                    return Ok(Some(references_response(
                        source,
                        uri,
                        position,
                        include_declaration,
                    )));
                }
                Some(ReferenceTarget::Global(name)) => {
                    let locations = global_reference_ranges(document.value(), &name)
                        .into_iter()
                        .map(|range| Location {
                            uri: uri.clone(),
                            range,
                        })
                        .collect::<Vec<_>>();
                    (name, locations)
                }
            }
        };

        // Global symbol: collect uses from every other workspace file, preferring
        // a live buffer over the on-disk copy.
        locations.extend(
            self.cross_file_global_references(&name, uri)
                .map(|(uri, range)| Location { uri, range }),
        );

        Ok(Some(locations))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let uri = &params.text_document.uri;
        let position = params.position;
        Ok(self
            .with_source_at(uri, position, |document, _, _| {
                prepare_rename_range(document, position).map(PrepareRenameResponse::Range)
            })
            .flatten())
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let new_name = params.new_name.trim();
        if !is_valid_m2_identifier(new_name) {
            // Renaming to an invalid identifier would silently produce
            // unparsable code; refuse instead.
            return Ok(None);
        }

        // Resolve the target and the current file's edits first, then drop the
        // document guard before scanning other files (mirrors `references`).
        let (name, mut changes) = {
            let document = match self.documents.get(uri) {
                Some(document) => document,
                None => return Ok(None),
            };
            match self.reference_target_at(document.value(), uri, position) {
                None => return Ok(None),
                Some(ReferenceTarget::Local) => {
                    // A local binding renames only within its own source.
                    let (source, _) = self.source_at(document.value(), position);
                    return Ok(rename_edits(source, uri, position, new_name));
                }
                Some(ReferenceTarget::Global(name)) => {
                    let edits = global_reference_ranges(document.value(), &name)
                        .into_iter()
                        .map(|range| TextEdit {
                            range,
                            new_text: new_name.to_string(),
                        })
                        .collect::<Vec<_>>();
                    (name, HashMap::from([(uri.clone(), edits)]))
                }
            }
        };

        // Global symbol: rename its uses in every other workspace file too, so a
        // top-level rename never leaves stale references behind in the files that
        // import it.
        for (file_uri, range) in self.cross_file_global_references(&name, uri) {
            changes.entry(file_uri).or_default().push(TextEdit {
                range,
                new_text: new_name.to_string(),
            });
        }

        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }))
    }

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> Result<Option<serde_json::Value>> {
        if matches!(
            params.command.as_str(),
            "m2-ls.executeFile" | "m2-ls.executeCell" | "m2-ls.runTests"
        ) {
            return self.execute_runtime_command(params).await.map(Some);
        }
        if params.command != WorkspaceHierarchy::COMMAND {
            return Err(tower_lsp::jsonrpc::Error::invalid_params("unknown command"));
        }
        if !params.arguments.is_empty() {
            return Err(tower_lsp::jsonrpc::Error::invalid_params(
                "printHierarchies takes no arguments",
            ));
        }
        let report = self.workspace_hierarchy();
        self.client
            .log_message(MessageType::INFO, report.text.clone())
            .await;
        serde_json::to_value(report)
            .map(Some)
            .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())
    }

    async fn prepare_type_hierarchy(
        &self,
        params: TypeHierarchyPrepareParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let hierarchy = self.workspace_hierarchy().hierarchy;
        let library = self.library.snapshot();
        let context = TypeHierarchyContext::new(&hierarchy, library.source_resolver());
        Ok(self
            .with_source_at(&uri, position, |document, _, workspace| {
                context.prepare(document, &uri, position, &workspace)
            })
            .flatten())
    }

    async fn supertypes(
        &self,
        params: TypeHierarchySupertypesParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        let hierarchy = self.workspace_hierarchy().hierarchy;
        let library = self.library.snapshot();
        Ok(
            TypeHierarchyContext::new(&hierarchy, library.source_resolver())
                .supertypes(&params.item),
        )
    }

    async fn subtypes(
        &self,
        params: TypeHierarchySubtypesParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        let hierarchy = self.workspace_hierarchy().hierarchy;
        let library = self.library.snapshot();
        Ok(TypeHierarchyContext::new(&hierarchy, library.source_resolver()).subtypes(&params.item))
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;
        let settings = self.settings.snapshot();
        Ok(self.with_document(&uri, |document| {
            if document.properties().primary_code {
                return Vec::new();
            }
            document_formatting_text_edits(
                document,
                params.options.tab_size,
                params.options.insert_spaces,
                &settings.formatting,
            )
        }))
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        let uri = params.text_document.uri;
        Ok(self.with_document(&uri, |document| {
            folding_ranges(crate::source::SyntaxSource::syntax(document))
        }))
    }

    #[allow(deprecated)]
    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let query = params.query.trim();
        Ok(Some(workspace_symbols_response(
            query,
            &self.workspace_index,
        )))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(self
            .navigate(uri, position, |document, knowledge, workspace, library| {
                goto_definition_response(
                    document,
                    uri,
                    position,
                    &knowledge.at(position),
                    &workspace,
                    library,
                )
            })
            .await
            .flatten())
    }

    async fn goto_declaration(
        &self,
        params: GotoDeclarationParams,
    ) -> Result<Option<GotoDeclarationResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(self
            .navigate(uri, position, |document, knowledge, workspace, library| {
                goto_declaration_response(
                    document,
                    uri,
                    position,
                    &knowledge.at(position),
                    &workspace,
                    library,
                )
            })
            .await
            .flatten())
    }

    async fn goto_type_definition(
        &self,
        params: GotoTypeDefinitionParams,
    ) -> Result<Option<GotoTypeDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(self
            .navigate(uri, position, |document, knowledge, workspace, library| {
                goto_type_definition_response(
                    document,
                    uri,
                    position,
                    &knowledge.at(position),
                    &workspace,
                    library,
                )
            })
            .await
            .flatten())
    }

    async fn goto_implementation(
        &self,
        params: GotoImplementationParams,
    ) -> Result<Option<GotoImplementationResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(self
            .navigate(uri, position, |document, knowledge, workspace, library| {
                goto_implementation_response(
                    document,
                    position,
                    uri,
                    &knowledge.at(position),
                    &workspace,
                    library,
                )
            })
            .await
            .flatten())
    }

    async fn document_link(&self, params: DocumentLinkParams) -> Result<Option<Vec<DocumentLink>>> {
        let uri = params.text_document.uri;
        let library = self.library.snapshot();
        Ok(self.with_document(&uri, |document| {
            document_links_response(document, &uri, library.source_resolver())
        }))
    }

    async fn document_link_resolve(&self, link: DocumentLink) -> Result<DocumentLink> {
        let Some(request) = document_link_request(&link) else {
            return Ok(link);
        };
        let uri = request.text_document.uri;
        let position = request.position;
        let response = self
            .navigate(&uri, position, |document, knowledge, workspace, library| {
                goto_definition_response(
                    document,
                    &uri,
                    position,
                    &knowledge.at(position),
                    &workspace,
                    library,
                )
            })
            .await
            .flatten();
        Ok(response.map_or(link.clone(), |response| {
            resolve_document_link(link, response)
        }))
    }
}

/// The library of the Macaulay2 sources `sources` configures; with library
/// discovery disabled, a library without sources.
fn configured_library(
    environment: &Macaulay2Environment,
    registry: &ObjectRegistry,
    settings: &ServerSettings,
) -> Arc<LibraryIndex> {
    let source_resolver = SourceResolver::for_settings(environment, settings);
    Arc::new(LibraryIndex::new(source_resolver, registry.clone()))
}

/// The name and version the server reports to the client.
fn server_info() -> ServerInfo {
    ServerInfo {
        name: env!("CARGO_PKG_NAME").to_string(),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
    }
}

/// Stack reserved for every runtime thread. Lowering and analysis recurse once
/// per nesting level, and generated sources (e.g. polynomials with thousands of
/// terms in one left-associated chain) nest far beyond the default 2 MiB worker
/// stack. The reservation is virtual; pages are committed only when touched.
pub const RUNTIME_THREAD_STACK_BYTES: usize = 1 << 30;

/// Starts the stdio language server and runs until the protocol session ends.
///
/// Catalog failures are reported on stderr; the session continues with source analysis.
/// The completed protocol session terminates the process with its session status.
///
/// # Panics
///
/// Panics if the asynchronous runtime cannot be built or its server task panics.
///
/// # Examples
///
/// ```no_run
/// m2_ls::run_server();
/// ```
pub fn run_server() {
    telemetry::enable();
    usage_log::enable("m2-ls");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(RUNTIME_THREAD_STACK_BYTES)
        .enable_all()
        .build()
        .expect("tokio runtime builds");
    // The session runs on a runtime thread, never on the default-sized main thread.
    runtime
        .block_on(runtime.spawn(serve()))
        .expect("language server session task panicked");
}

/// Serve one client over stdio until it exits or disconnects.
/// Initialization settings select whether its builtin catalog is loaded.
async fn serve() {
    install_panic_logging();
    let session_end = SessionEnd::default();
    let stdin = ClientInput::new(io::stdin(), session_end.clone());
    let stdout = io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    let service = ExitNotificationService::new(
        TypeHierarchyCapabilityService::new(service),
        session_end.clone(),
    );
    tokio::select! {
        () = Server::new(stdin, stdout, socket).serve(service) => {}
        () = session_end.ended() => {}
    }
    telemetry::summary();
    // Returning from `main` would wait for tokio's blocking stdin reader, which
    // outlives the session; the process ends here instead.
    std::process::exit(session_end.exit_code());
}
