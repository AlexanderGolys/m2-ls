//! Shared Macaulay2 analysis, command-line tools, and language server.

use std::backtrace::Backtrace;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::panic;
use std::sync::Arc;

use dashmap::DashMap;
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
pub mod cli;
mod client_capabilities;
mod diagnostics;
mod document;
mod documentation;
mod library_index;
mod macaulay2;
mod meta;
mod node_metadata;
mod object_registry;
mod package_documentation;
mod package_index;
mod record_lsp;
mod semantic_token;
mod session;
mod settings;
mod source;
mod source_package;
mod telemetry;
#[cfg(test)]
mod test_support;
mod typesystem;
mod workspace_index;

use capabilities::code_actions::available_code_actions;
use capabilities::completion::{completion_options, completion_response};
use capabilities::document_highlight::{
    document_highlight_provider_capability, document_highlights,
};
use capabilities::document_symbols::collect_document_symbols;
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
use document::DocumentSnapshot;
use library_index::{LibraryIndex, LibrarySources};
use package_index::{InitializationFile, Macaulay2Environment, SourceResolver};
use session::{ClientInput, ExitNotificationService, SessionEnd};

use crate::object_registry::ObjectRegistry;
use crate::semantic_token::M2SemanticToken;
use crate::settings::{ServerSettings, SettingProblem, SettingsStore, SourceSettings};
use crate::workspace_index::{WorkspaceIndex, WorkspaceView};

#[derive(Debug)]
struct Backend {
    client: Client,
    registry: ObjectRegistry,
    environment: Macaulay2Environment,
    documents: DashMap<Url, DocumentSnapshot>,
    document_versions: DashMap<Url, i32>,
    workspace_index: Arc<WorkspaceIndex>,
    library: SettingsStore<Arc<LibraryIndex>>,
    settings: SettingsStore<ServerSettings>,
    semantic_tokens_augment_syntax: ClientSupport<SemanticTokensAugmentSyntax>,
    type_hierarchy_dynamic_registration: ClientSupport<TypeHierarchyDynamicRegistration>,
    inlay_hint_refresh: ClientSupport<InlayHintRefresh>,
    semantic_tokens_refresh: ClientSupport<SemanticTokensRefresh>,
    /// The user's `init.m2`, which Macaulay2 runs before any document.
    initialization_file: Option<InitializationFile>,
}

impl Backend {
    fn new(client: Client) -> Self {
        let environment = Macaulay2Environment::current();
        let mut registry = ObjectRegistry::load(include_str!("./data/m2-index.jsonl"));
        let initialization_file = InitializationFile::load(&environment, &mut registry);
        let library = configured_library(&environment, &registry, &SourceSettings::default());
        Backend {
            client,
            library: SettingsStore::new(library),
            registry,
            environment,
            documents: DashMap::new(),
            document_versions: DashMap::new(),
            workspace_index: Arc::new(WorkspaceIndex::default()),
            settings: SettingsStore::default(),
            semantic_tokens_augment_syntax: ClientSupport::default(),
            type_hierarchy_dynamic_registration: ClientSupport::default(),
            inlay_hint_refresh: ClientSupport::default(),
            semantic_tokens_refresh: ClientSupport::default(),
            initialization_file,
        }
    }

    fn reindex_from_disk(&self, uri: &Url) {
        let Ok(path) = uri.to_file_path() else {
            return;
        };
        match fs::read_to_string(&path) {
            Ok(text) => self.workspace_index.index_file(uri, &text, &self.registry),
            Err(_) => self.workspace_index.remove_file(uri),
        }
    }

    /// Borrow the registry loaded once for this exact document version.
    fn scoped_index_for<'a>(&self, document: &'a DocumentSnapshot) -> &'a ObjectRegistry {
        document.object_registry()
    }

    fn with_document<R>(
        &self,
        uri: &Url,
        request: impl FnOnce(&DocumentSnapshot) -> R,
    ) -> Option<R> {
        let document = self.documents.get(uri)?;
        Some(request(document.value()))
    }

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
            .filter(move |file_uri| file_uri != exclude)
            .flat_map(move |file_uri| {
                let ranges = if let Some(open) = self.documents.get(&file_uri) {
                    global_reference_ranges(open.value(), name)
                } else if let Ok(path) = file_uri.to_file_path() {
                    fs::read_to_string(path)
                        .ok()
                        .and_then(|text| DocumentSnapshot::from_text(text, &self.registry))
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

    fn type_hierarchy_context<'a>(
        &'a self,
        library: &'a LibraryIndex,
    ) -> TypeHierarchyContext<'a, ObjectRegistry> {
        TypeHierarchyContext::new(&self.registry, library.source_resolver())
    }

    /// Point library navigation at the sources `sources` configures.
    fn configure_library(&self, sources: &SourceSettings) {
        self.library.replace(configured_library(
            &self.environment,
            &self.registry,
            sources,
        ));
    }

    async fn publish_document_diagnostics(&self, uri: Url, document: &DocumentSnapshot) {
        let settings = self.settings.snapshot();
        let version = self.document_versions.get(&uri).map(|version| *version);
        publish_diagnostics(&self.client, uri, document, &settings.diagnostics, version).await;
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

    async fn apply_settings(
        &self,
        settings: ServerSettings,
        refresh_client: &(impl WorkspaceRefresh<InlayHintRefresh>
              + WorkspaceRefresh<SemanticTokensRefresh>
              + ?Sized),
    ) -> Result<()> {
        let previous = self.settings.replace(settings.clone());
        if previous.sources != settings.sources {
            self.configure_library(&settings.sources);
        }

        if previous.diagnostics != settings.diagnostics {
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
                        ),
                    )
                })
                .collect::<Vec<_>>();
            for (uri, diagnostics) in diagnostics {
                self.client
                    .publish_diagnostics(uri, diagnostics, None)
                    .await;
            }
        }

        let inlay_hints = refresh_if_changed(
            &self.inlay_hint_refresh,
            refresh_client,
            previous.inlay_hints != settings.inlay_hints,
        )
        .await;
        let semantic_tokens = refresh_if_changed(
            &self.semantic_tokens_refresh,
            refresh_client,
            previous.semantic_tokens != settings.semantic_tokens,
        )
        .await;
        inlay_hints.and(semantic_tokens)
    }

    async fn on_open(&self, params: TextDocumentItem) {
        let _timing = telemetry::Timing::start(
            "document.open",
            serde_json::json!({"uri": params.uri, "source_bytes": params.text.len()}),
        );
        let Some(document) = DocumentSnapshot::from_text(params.text, &self.registry) else {
            return;
        };
        let uri = params.uri;
        let loaded = self
            .workspace_index
            .index_snapshot(&uri, &document, &self.registry);
        self.document_versions.insert(uri.clone(), params.version);
        self.documents.insert(uri.clone(), document);
        self.index_loaded_in_background(loaded).await;
        if let Some(document) = self.documents.get(&uri) {
            self.publish_document_diagnostics(uri, document.value())
                .await;
        }
    }

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
        {
            let Some(mut document) = self.documents.get_mut(&uri) else {
                return;
            };
            if document.apply_changes(&changes, &self.registry).is_none() {
                return;
            }
            let library = self.library.snapshot();
            let library_file = uri.to_file_path().ok().is_some_and(|path| {
                library
                    .source_resolver()
                    .package_of_source_file(&path)
                    .is_some()
            });
            if !library_file {
                self.workspace_index
                    .index_snapshot(&uri, document.value(), &self.registry);
            }
        }

        if let Some(document) = self.documents.get(&uri) {
            self.publish_document_diagnostics(uri, document.value())
                .await;
        }
    }

    /// Refresh transitive disk dependencies on open/save, sharing the open document's facts.
    async fn index_loaded_in_background(&self, loaded: Vec<Url>) {
        let index = Arc::clone(&self.workspace_index);
        let registry = self.registry.clone();
        task::spawn_blocking(move || index.index_files_from_disk(loaded, &registry))
            .await
            .expect("workspace indexing task panicked");
    }

    async fn on_save(&self, uri: Url) {
        let _timing = telemetry::Timing::start("document.save", serde_json::json!({"uri": uri}));
        self.library.snapshot().invalidate_saved_file(&uri);
        let Some(version) = self.document_versions.get(&uri).map(|version| *version) else {
            return;
        };
        let job = {
            let Some(document) = self.documents.get(&uri) else {
                return;
            };
            document.save_job(self.registry.clone())
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
                .index_snapshot(&uri, &saved, &self.registry);
            document.insert(saved);
            loaded
        };
        self.index_loaded_in_background(loaded).await;
        if let Some(document) = self.documents.get(&uri) {
            self.publish_document_diagnostics(uri, document.value())
                .await;
        }
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

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        if let Some(initialization_file) = &self.initialization_file {
            initialization_file.index_into(&self.workspace_index, &self.registry);
        }
        self.semantic_tokens_augment_syntax
            .negotiate(&params.capabilities);
        self.type_hierarchy_dynamic_registration
            .negotiate(&params.capabilities);
        self.inlay_hint_refresh.negotiate(&params.capabilities);
        self.semantic_tokens_refresh.negotiate(&params.capabilities);

        if let Some(options) = params.initialization_options.as_ref() {
            let parsed = ServerSettings::from_value(options);
            self.configure_library(&parsed.settings.sources);
            self.settings.replace(parsed.settings);
            self.report_setting_problems(&parsed.problems).await;
        }

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
        let _ = self
            .client
            .register_capability(vec![Registration {
                id: "m2-ls-watch-m2-files".to_string(),
                method: "workspace/didChangeWatchedFiles".to_string(),
                register_options: Some(serde_json::json!({
                    "watchers": [{ "globPattern": "**/*.m2" }]
                })),
            }])
            .await;

        self.client
            .log_message(
                MessageType::INFO,
                format!(
                    "Macaulay2 LSP initialized with {} builtin symbols",
                    self.registry.len()
                ),
            )
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.on_open(TextDocumentItem {
            uri: params.text_document.uri,
            language_id: "macaulay2".to_string(),
            version: params.text_document.version,
            text: params.text_document.text,
        })
        .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        self.on_change(
            params.text_document.uri,
            params.text_document.version,
            params.content_changes,
        )
        .await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        self.on_save(params.text_document.uri).await;
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        let parsed = ServerSettings::from_value(&params.settings);
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
        self.documents.remove(&uri);
        self.document_versions.remove(&uri);
        // Re-index from disk so the workspace index reflects the saved file
        // rather than the last in-editor edit.
        self.reindex_from_disk(&uri);
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        for change in params.changes {
            // Open documents are indexed from their live buffer; ignore disk
            // events for them to avoid clobbering unsaved edits. Files neither
            // open nor loaded by an open file stay out of the index.
            if self.documents.contains_key(&change.uri)
                || !self.workspace_index.is_indexed(&change.uri)
            {
                continue;
            }
            if change.typ == FileChangeType::DELETED {
                self.workspace_index.remove_file(&change.uri);
            } else {
                self.reindex_from_disk(&change.uri);
            }
        }
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
            .with_source_at(uri, position, |document, knowledge, _| {
                completion_response(document, position, &knowledge.at(position))
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
                let mut diagnostics = visible_diagnostics(document, uri, &settings.diagnostics);
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
                document.root_node(),
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

    async fn prepare_type_hierarchy(
        &self,
        params: TypeHierarchyPrepareParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(self
            .with_source_at(&uri, position, |document, knowledge, _| {
                self.type_hierarchy_context(&self.library.snapshot())
                    .prepare(document, &knowledge.at(position), &uri, position)
            })
            .flatten())
    }

    async fn supertypes(
        &self,
        params: TypeHierarchySupertypesParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        Ok(self
            .type_hierarchy_context(&self.library.snapshot())
            .supertypes(&params.item))
    }

    async fn subtypes(
        &self,
        params: TypeHierarchySubtypesParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        Ok(self
            .type_hierarchy_context(&self.library.snapshot())
            .subtypes(&params.item))
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;
        let settings = self.settings.snapshot();
        Ok(self.with_document(&uri, |document| {
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
        Ok(self.with_document(&uri, |document| folding_ranges(document.root_node())))
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
        Ok(self.with_document(&uri, |document| document_links_response(document, &uri)))
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
/// navigation disabled, a library without sources.
fn configured_library(
    environment: &Macaulay2Environment,
    registry: &ObjectRegistry,
    sources: &SourceSettings,
) -> Arc<LibraryIndex> {
    let source_resolver = if sources.enabled {
        SourceResolver::discover(environment, &sources.paths)
    } else {
        SourceResolver::default()
    };
    Arc::new(LibraryIndex::new(source_resolver, registry.clone()))
}

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

/// Start the stdio language server.
pub fn run_server() {
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
