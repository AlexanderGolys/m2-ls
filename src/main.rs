//! Macaulay2's stdio language-server entry point and protocol wiring.

use std::backtrace::Backtrace;
use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::panic;
use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;
use tokio::{io, task};
use tower_lsp::jsonrpc::{Error, Result};
use tower_lsp::lsp_types::request::{
    GotoDeclarationParams, GotoDeclarationResponse, GotoImplementationParams,
    GotoImplementationResponse, GotoTypeDefinitionParams, GotoTypeDefinitionResponse,
};
use tower_lsp::lsp_types::Range as TextRange;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

#[macro_use]
mod util;
mod analysis;
mod builtin_index;
mod capabilities;
mod client_capabilities;
mod diagnostic_registry;
mod document;
mod documentation;
mod documentation_site;
mod meta;
mod node_metadata;
mod object_registry;
mod package_index;
mod record_lsp;
mod semantic_token;
mod settings;
mod source;
#[cfg(test)]
mod test_support;
mod typesystem;
mod workspace_index;

use capabilities::code_actions::available_code_actions;
use capabilities::completion::{completion_options, completion_response};
use capabilities::diagnostics::{publish_diagnostics, visible_diagnostics};
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
    TypeHierarchyDynamicRegistration, WorkspaceRefresh,
};
use diagnostic_registry::DiagnosticPolicy;
use document::DocumentSnapshot;
use documentation_site::{build_documentation_book, generate_documentation_book};
use package_index::SourceResolver;

use crate::object_registry::ObjectRegistry;
use crate::semantic_token::{LEGEND_MODIFIERS, LEGEND_TYPES};
use crate::settings::{ServerSettings, SettingsStore};
use crate::workspace_index::WorkspaceIndex;

#[derive(Debug)]
struct Backend {
    client: Client,
    registry: ObjectRegistry,
    source_resolver: SourceResolver,
    documents: DashMap<Url, DocumentSnapshot>,
    workspace_index: Arc<WorkspaceIndex>,
    settings: SettingsStore<ServerSettings>,
    semantic_tokens_augment_syntax: ClientSupport<SemanticTokensAugmentSyntax>,
    type_hierarchy_dynamic_registration: ClientSupport<TypeHierarchyDynamicRegistration>,
    inlay_hint_refresh: ClientSupport<InlayHintRefresh>,
}

impl Backend {
    fn new(client: Client) -> Self {
        let registry = ObjectRegistry::load(include_str!("./data/m2-index.jsonl"));
        Backend {
            client,
            registry,
            source_resolver: SourceResolver::from_environment(),
            documents: DashMap::new(),
            workspace_index: Arc::new(WorkspaceIndex::default()),
            settings: SettingsStore::default(),
            semantic_tokens_augment_syntax: ClientSupport::default(),
            type_hierarchy_dynamic_registration: ClientSupport::default(),
            inlay_hint_refresh: ClientSupport::default(),
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

    /// Collect occurrences of a global symbol across every workspace file
    /// other than `exclude`. Prefers a live open buffer (so unsaved edits are
    /// reflected) and falls back to parsing the on-disk file. Used by both
    /// `references` and `rename` so the cross-file scan lives in one place.
    fn cross_file_global_references<'a>(
        &'a self,
        name: &'a str,
        exclude: &'a Url,
    ) -> impl Iterator<Item = (Url, TextRange)> + 'a {
        self.workspace_index
            .workspace_file_uris()
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

    fn type_hierarchy_context(&self) -> TypeHierarchyContext<'_, ObjectRegistry> {
        TypeHierarchyContext::new(&self.registry, &self.source_resolver)
    }

    async fn publish_document_diagnostics(&self, uri: Url, document: &DocumentSnapshot) {
        let settings = self.settings.snapshot();
        publish_diagnostics(&self.client, uri, document, settings.diagnostics()).await;
    }

    async fn apply_settings(
        &self,
        settings: ServerSettings,
        refresh_client: &(impl WorkspaceRefresh<InlayHintRefresh> + ?Sized),
    ) -> Result<()> {
        let previous = self.settings.replace(settings.clone());

        if previous.diagnostics() != settings.diagnostics() {
            let diagnostics = self
                .documents
                .iter()
                .map(|document| {
                    (
                        document.key().clone(),
                        visible_diagnostics(document.value(), settings.diagnostics()),
                    )
                })
                .collect::<Vec<_>>();
            for (uri, diagnostics) in diagnostics {
                self.client
                    .publish_diagnostics(uri, diagnostics, None)
                    .await;
            }
        }

        refresh_if_changed(
            &self.inlay_hint_refresh,
            refresh_client,
            previous.inlay_hints() != settings.inlay_hints(),
        )
        .await
    }

    async fn on_open(&self, params: TextDocumentItem) {
        let Some(document) = DocumentSnapshot::from_text(params.text, &self.registry) else {
            return;
        };
        let uri = params.uri;
        self.workspace_index
            .index_file(&uri, document.text(), &self.registry);
        self.documents.insert(uri.clone(), document);
        if let Some(document) = self.documents.get(&uri) {
            self.publish_document_diagnostics(uri, document.value())
                .await;
        }
    }

    async fn on_change(&self, uri: Url, changes: Vec<TextDocumentContentChangeEvent>) {
        if let Some(mut document) = self.documents.get_mut(&uri) {
            if document.apply_changes(&changes, &self.registry).is_none() {
                return;
            }
            self.workspace_index
                .index_file(&uri, document.text(), &self.registry);
            self.publish_document_diagnostics(uri, document.value())
                .await;
        }
    }
}

fn append_debug_log(message: &str) {
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/m2-ls.log")
    else {
        return;
    };
    let _ = writeln!(file, "{message}");
}

/// Install the crash hook: a panic always logs its backtrace to
/// `/tmp/m2-ls.log` (crash diagnostics are worth the write). Routine chatter
/// is opt-in via the `M2_LS_LOG` environment variable.
fn install_panic_logging() {
    panic::set_hook(Box::new(|panic_info| {
        let backtrace = Backtrace::force_capture();
        append_debug_log(&format!("panic: {panic_info}\n{backtrace}"));
    }));
    if std::env::var_os("M2_LS_LOG").is_some() {
        append_debug_log("m2-ls starting");
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        self.workspace_index.set_roots(workspace_roots(&params));
        // Index every `.m2` file without blocking the async runtime.
        let index = Arc::clone(&self.workspace_index);
        let knowledge = self.registry.clone();
        task::spawn_blocking(move || index.scan(&knowledge))
            .await
            .map_err(|_| Error::internal_error())?;
        self.semantic_tokens_augment_syntax
            .negotiate(&params.capabilities);
        self.type_hierarchy_dynamic_registration
            .negotiate(&params.capabilities);
        self.inlay_hint_refresh.negotiate(&params.capabilities);

        if let Some(options) = params.initialization_options.as_ref() {
            match ServerSettings::from_value(options) {
                Ok(settings) => {
                    self.settings.replace(settings);
                }
                Err(error) => {
                    self.client
                        .log_message(
                            MessageType::WARNING,
                            format!("Ignoring invalid Macaulay2 LSP settings: {error}"),
                        )
                        .await;
                }
            }
        }

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
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
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                inlay_hint_provider: inlay_hint_provider_capability(),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: SemanticTokensLegend {
                                token_types: LEGEND_TYPES.into(),
                                token_modifiers: LEGEND_MODIFIERS.into(),
                            },
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
        self.on_change(params.text_document.uri, params.content_changes)
            .await;
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        let settings = match ServerSettings::from_value(&params.settings) {
            Ok(settings) => settings,
            Err(error) => {
                self.client
                    .log_message(
                        MessageType::WARNING,
                        format!("Ignoring invalid Macaulay2 LSP settings: {error}"),
                    )
                    .await;
                return;
            }
        };
        if let Err(error) = self.apply_settings(settings, &self.client).await {
            self.client
                .log_message(
                    MessageType::WARNING,
                    format!("Failed to refresh Macaulay2 inlay hints: {error}"),
                )
                .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.documents.remove(&uri);
        // Re-index from disk so the workspace index reflects the saved file
        // rather than the last in-editor edit.
        self.reindex_from_disk(&uri);
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        for change in params.changes {
            // Open documents are indexed from their live buffer; ignore disk
            // events for them to avoid clobbering unsaved edits.
            if self.documents.contains_key(&change.uri) {
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
            .with_scoped_document(uri, |document, knowledge| {
                hover_response(document, position, &knowledge.at(position))
            })
            .flatten())
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        Ok(self
            .with_scoped_document(uri, |document, knowledge| {
                completion_response(document, position, &knowledge.at(position))
            })
            .flatten())
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(self
            .with_scoped_document(uri, |document, knowledge| {
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
        Ok(self.with_scoped_document(&uri, |document, knowledge| {
            let tokens = collect_semantic_tokens(
                document,
                knowledge,
                &self.workspace_index,
                &uri,
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
        Ok(self
            .with_document(uri, |document| {
                let diagnostics = if params.context.diagnostics.is_empty() {
                    visible_diagnostics(document, settings.diagnostics())
                } else {
                    params
                        .context
                        .diagnostics
                        .into_iter()
                        .filter(|diagnostic| {
                            settings.diagnostics().allows_lsp_diagnostic(diagnostic)
                        })
                        .collect()
                };
                available_code_actions(document, uri, params.range.start, &diagnostics)
            })
            .flatten())
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = &params.text_document.uri;
        let options = self.settings.snapshot().inlay_hint_options();
        Ok(self.with_scoped_document(uri, |document, knowledge| {
            inlay_hints_response(document, params.range, options, knowledge)
        }))
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(self
            .with_scoped_document(uri, |document, knowledge| {
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
            match reference_target(document.value(), position, &self.workspace_index) {
                None | Some(ReferenceTarget::Local) => {
                    // A local binding's references never leave the document.
                    return Ok(Some(references_response(
                        document.value(),
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
            .with_document(uri, |document| {
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
            match reference_target(document.value(), position, &self.workspace_index) {
                None => return Ok(None),
                Some(ReferenceTarget::Local) => {
                    // A local binding renames only within its own document.
                    return Ok(rename_edits(document.value(), uri, position, new_name));
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
            .with_scoped_document(&uri, |document, knowledge| {
                self.type_hierarchy_context().prepare(
                    document,
                    &knowledge.at(position),
                    &uri,
                    position,
                )
            })
            .flatten())
    }

    async fn supertypes(
        &self,
        params: TypeHierarchySupertypesParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        Ok(self.type_hierarchy_context().supertypes(&params.item))
    }

    async fn subtypes(
        &self,
        params: TypeHierarchySubtypesParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        Ok(self.type_hierarchy_context().subtypes(&params.item))
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;
        let settings = self.settings.snapshot();
        Ok(self.with_document(&uri, |document| {
            document_formatting_text_edits(
                document,
                params.options.tab_size,
                params.options.insert_spaces,
                settings.formatting(),
            )
        }))
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        let uri = params.text_document.uri;
        Ok(self.with_document(&uri, |document| folding_ranges(document.text())))
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
            .with_scoped_document(uri, |document, knowledge| {
                goto_definition_response(
                    document,
                    uri,
                    position,
                    &knowledge.at(position),
                    &self.source_resolver,
                    &self.workspace_index,
                )
            })
            .flatten())
    }

    async fn goto_declaration(
        &self,
        params: GotoDeclarationParams,
    ) -> Result<Option<GotoDeclarationResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(self
            .with_scoped_document(uri, |document, knowledge| {
                goto_declaration_response(
                    document,
                    uri,
                    position,
                    &knowledge.at(position),
                    &self.source_resolver,
                    &self.workspace_index,
                )
            })
            .flatten())
    }

    async fn goto_type_definition(
        &self,
        params: GotoTypeDefinitionParams,
    ) -> Result<Option<GotoTypeDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(self
            .with_scoped_document(uri, |document, knowledge| {
                goto_type_definition_response(
                    document,
                    uri,
                    position,
                    &knowledge.at(position),
                    &self.workspace_index,
                    &self.source_resolver,
                )
            })
            .flatten())
    }

    async fn goto_implementation(
        &self,
        params: GotoImplementationParams,
    ) -> Result<Option<GotoImplementationResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(self
            .with_scoped_document(uri, |document, knowledge| {
                goto_implementation_response(
                    document,
                    position,
                    uri,
                    &knowledge.at(position),
                    &self.workspace_index,
                )
            })
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
            .with_scoped_document(&uri, |document, knowledge| {
                goto_definition_response(
                    document,
                    &uri,
                    position,
                    &knowledge.at(position),
                    &self.source_resolver,
                    &self.workspace_index,
                )
            })
            .flatten();
        Ok(response.map_or(link.clone(), |response| {
            resolve_document_link(link, response)
        }))
    }
}

/// The project roots to index, preferring `workspaceFolders` and falling back
/// to the (deprecated) `rootUri` single-folder field that older clients send.
fn workspace_roots(params: &InitializeParams) -> Vec<PathBuf> {
    if let Some(folders) = &params.workspace_folders {
        let roots: Vec<PathBuf> = folders
            .iter()
            .filter_map(|folder| folder.uri.to_file_path().ok())
            .collect();
        if !roots.is_empty() {
            return roots;
        }
    }
    #[allow(deprecated)]
    params
        .root_uri
        .as_ref()
        .and_then(|uri| uri.to_file_path().ok())
        .into_iter()
        .collect()
}

fn server_info() -> ServerInfo {
    ServerInfo {
        name: env!("CARGO_PKG_NAME").to_string(),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
    }
}

fn run_documentation_command() -> Option<std::result::Result<(), String>> {
    let mut arguments = env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("docs")) {
        return None;
    }
    let result = (|| {
        let input = arguments.next().map(PathBuf::from).ok_or_else(|| {
            "usage: m2-ls docs <source-or-directory> [--output PATH] [--no-build]".to_string()
        })?;
        let mut output = PathBuf::from("target/m2-doc");
        let mut build = true;
        while let Some(argument) = arguments.next() {
            if argument == "--output" {
                output = arguments
                    .next()
                    .map(PathBuf::from)
                    .ok_or_else(|| "--output requires a path".to_string())?;
            } else if argument == "--no-build" {
                build = false;
            } else {
                return Err(format!(
                    "unrecognized documentation option: {}",
                    argument.to_string_lossy()
                ));
            }
        }
        let registry = ObjectRegistry::load(include_str!("./data/m2-index.jsonl"));
        let generated = generate_documentation_book(&input, &output, &registry)
            .map_err(|error| error.to_string())?;
        if build {
            build_documentation_book(&output).map_err(|error| error.to_string())?;
        }
        println!(
            "generated {} documentation pages in {}",
            generated.pages,
            output.display()
        );
        Ok(())
    })();
    Some(result)
}

#[tokio::main]
async fn main() {
    if let Some(result) = run_documentation_command() {
        if let Err(error) = result {
            eprintln!("m2-ls docs: {error}");
            std::process::exit(2);
        }
        return;
    }
    install_panic_logging();
    let stdin = io::stdin();
    let stdout = io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket)
        .serve(TypeHierarchyCapabilityService::new(service))
        .await;
}
