//! Versioned document snapshots that combine source text, parse tree, and
//! analysis for LSP requests.

use crate::analysis::{Analysis, BindingView, FunctionInfo, MethodInstallation};
use crate::diagnostics::suppression::{DiagnosticSuppressions, DIRECTIVE_MARKER, LINE_COMMENT};
use crate::diagnostics::{self, M2Diagnostic};
use crate::documentation::{
    collect_documentation, DocumentationBlock, DocumentationIndex, DocumentationSnippet,
};
use crate::file_load::FileLoad;
use crate::init_file::InitFileDirective;
use crate::node_metadata::TypedSource;
use crate::node_metadata::{SyntaxNode, SyntaxParser, SyntaxTree};
use crate::object_registry::{ObjectName, ObjectRegistry};
use crate::package_documentation::PackageDocumentation;
use crate::package_index::{collect_imported_packages_in_tree, PackageImport, SourceResolver};
use crate::source::SourcePoint as Point;
use crate::source::{ByteRange, DocumentSource, SourceNavigation, SymbolMention};
use crate::source_package::{PackageSessionCode, SourcePackages};
use m2_syn::nodes::ExprLambda as LambdaExpression;
use std::ops::BitOr;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use tower_lsp::lsp_types::{Position, Range as TextRange, TextDocumentContentChangeEvent, Url};

/// One immutable source, syntax, and semantic-analysis snapshot served to LSP
/// requests.
#[derive(Debug)]
pub struct DocumentSnapshot {
    /// The properties the file's location implies, whatever its text declares.
    implied_properties: FileProperties,
    /// The implied properties with those the text declares.
    properties: FileProperties,
    /// The text, with the positions of the parse tree it is analyzed from.
    source: DocumentSource,
    /// The parse tree of the text, kept for incremental reparsing.
    tree: SyntaxTree,
    /// Bindings, scopes, inferred types and method installations of the text.
    analysis: Analysis,
    /// The findings of the analysis and of the package sessions, before
    /// settings and suppressions filter them.
    diagnostics: Vec<M2Diagnostic>,
    /// The packages loaded in the session: the provider's, with those the
    /// document imports, each from where its import takes effect.
    object_registry: ObjectRegistry,
    /// The documentation comments and the bindings they document.
    documentation: DocumentationIndex,
    /// Names mentioned outside code, in documentation links and export lists,
    /// sorted by position.
    symbol_mentions: Vec<SymbolMention>,
    /// Package names in strings that load packages, such as `"Graphs"` in
    /// `needsPackage "Graphs"`.
    package_mentions: Vec<SymbolMention>,
    /// The packages the document imports, each from where its import takes
    /// effect.
    imported_packages: Vec<PackageImport>,
    /// The `load`, `needs` and `input` calls of the document.
    file_loads: Vec<FileLoad>,
    /// The packages the document declares with `newPackage`, with their exports
    /// and TEST blocks.
    packages: SourcePackages,
    /// The package documentation the document writes with `doc` and `document`.
    package_documentation: PackageDocumentation,
    /// The diagnostics the document's `disable` directives hide.
    diagnostic_suppressions: DiagnosticSuppressions,
    /// The comment selecting the init file the document starts with.
    init_file_directive: Option<InitFileDirective>,
    /// The TEST blocks and documentation examples, each analyzed as the session
    /// that runs it, analyzed on first use.
    package_sessions: OnceLock<Vec<PackageSession>>,
    /// The package sessions of the previous snapshot, whose analyses unchanged
    /// sessions reuse.
    previous_sessions: Mutex<Vec<PackageSession>>,
    /// The packages a package session starts with: the provider's, without an
    /// init file.
    session_knowledge: ObjectRegistry,
}

/// File-wide analysis properties, declared in source comments or implied by
/// where the file is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileProperties {
    /// Implementation code starts without the session's preloaded packages.
    pub primary_code: bool,
}

impl FileProperties {
    /// The directive command declaring a file primary code.
    const PRIMARY_CODE_COMMAND: &str = "primaryCode";

    /// The properties declared by the directive comments under `root`.
    fn collect(root: SyntaxNode<'_>) -> Self {
        Self {
            primary_code: root
                .descendants()
                .filter(SyntaxNode::is_line_comment)
                .any(|comment| {
                    comment
                        .text()
                        .strip_prefix(LINE_COMMENT)
                        .and_then(|body| body.trim_start().strip_prefix(DIRECTIVE_MARKER))
                        .is_some_and(|body| body.trim() == Self::PRIMARY_CODE_COMMAND)
                }),
        }
    }

    /// The properties the location of `uri` implies: primary code for the
    /// files `sources` takes for implementing the library.
    pub fn implied_by_location(uri: &Url, sources: &SourceResolver) -> Self {
        Self {
            primary_code: uri
                .to_file_path()
                .is_ok_and(|path| sources.is_primary_code_file(&path)),
        }
    }

    /// The comment declaring a file primary code, as a quick fix writes it.
    pub fn primary_code_directive() -> String {
        format!(
            "{LINE_COMMENT} {DIRECTIVE_MARKER} {}",
            Self::PRIMARY_CODE_COMMAND
        )
    }
}

/// Every property either side has.
impl BitOr for FileProperties {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self {
            primary_code: self.primary_code || other.primary_code,
        }
    }
}

/// Code run in a separate session that loaded its package — a TEST block,
/// as `check` runs it, or the examples of a documentation node, as
/// `installPackage` runs them — analyzed as that session: the code alone,
/// seeing the package only through its exports. Its snapshot shares the
/// document's LSP positions, so whatever it answers for the code holds in the
/// document as is.
#[derive(Debug, Clone)]
pub struct PackageSession {
    /// The code and the package whose exports it sees.
    pub code: PackageSessionCode,
    /// The packages the session loads along with the package.
    pub exported_packages: Vec<ObjectName>,
    /// The code analyzed on its own.
    pub snapshot: Arc<DocumentSnapshot>,
}

impl SourceNavigation for DocumentSnapshot {
    fn source(&self) -> &DocumentSource {
        &self.source
    }
}

/// The common first step of every reference / highlight / rename request: the
/// source occurrence under the cursor together with its scope-aware binding.
/// The occurrence may be a CST symbol or a [`SymbolMention`].
/// Resolved once per request and threaded through downstream collection.
#[derive(Debug, Clone, Copy)]
pub struct TargetSymbol<'a> {
    /// The name as written.
    pub name: &'a str,
    /// Where the occurrence is.
    pub range: TextRange,
    /// The binding the occurrence refers to.
    pub symbol: BindingView<'a>,
}

impl DocumentSnapshot {
    /// The snapshot of `text` analyzed against `knowledge_provider`, for a file
    /// whose location implies nothing; `None` when the grammar cannot be
    /// loaded or parsing fails.
    pub fn from_text(text: String, knowledge_provider: &ObjectRegistry) -> Option<Self> {
        Self::from_text_with_implied_properties(text, knowledge_provider, FileProperties::default())
    }

    /// The snapshot of `text` in a file whose location implies
    /// `implied_properties`.
    pub fn from_text_with_implied_properties(
        text: String,
        knowledge_provider: &ObjectRegistry,
        implied_properties: FileProperties,
    ) -> Option<Self> {
        let source = DocumentSource::new(text);
        let parsing = crate::telemetry::Timing::start(
            "document.parse",
            serde_json::json!({"source_bytes": source.text().len(), "incremental": false}),
        );
        let tree = SyntaxParser::new()?.parse_tree(source.text(), None)?;
        drop(parsing);
        Some(Self::analyze(
            source,
            tree,
            knowledge_provider,
            Vec::new(),
            true,
            implied_properties,
        ))
    }

    /// A saved library file needed for navigation. Documentation examples and
    /// TEST sessions belong to the opened document, not the navigation cache.
    pub(crate) fn from_text_for_navigation(
        text: String,
        knowledge: &ObjectRegistry,
    ) -> Option<Self> {
        let source = DocumentSource::new(text);
        let parsing = crate::telemetry::Timing::start(
            "document.parse",
            serde_json::json!({"source_bytes": source.text().len(), "incremental": false}),
        );
        let tree = SyntaxParser::new()?.parse_tree(source.text(), None)?;
        drop(parsing);
        Some(Self::analyze(
            source,
            tree,
            knowledge,
            Vec::new(),
            false,
            FileProperties::default(),
        ))
    }

    /// Analyze `source`, parsed as `tree`, against the packages
    /// `knowledge_provider` has loaded plus those the source imports, then
    /// the code of each of its package sessions against the same packages
    /// plus the session's, reusing the analysis of the unchanged
    /// `previous_sessions`. The file is analyzed with `implied_properties`
    /// and those its directive comments declare.
    fn analyze(
        source: DocumentSource,
        tree: SyntaxTree,
        knowledge_provider: &ObjectRegistry,
        previous_sessions: Vec<PackageSession>,
        full: bool,
        implied_properties: FileProperties,
    ) -> Self {
        let mut source = source;
        source.register_syntax(tree.snapshot());
        let typed_ast = Some(tree.syntax());
        let root = tree.root(source.text());
        let properties = implied_properties | FileProperties::collect(root);
        let imported_packages = collect_imported_packages_in_tree(root, typed_ast, &source);
        let file_loads = FileLoad::collect(root, typed_ast, &source);
        let baseline = if properties.primary_code {
            knowledge_provider.without_prelude()
        } else {
            knowledge_provider.clone()
        };
        let knowledge = baseline.with_imports(&imported_packages);
        let analysis_timing = crate::telemetry::Timing::start(
            "document.analysis",
            serde_json::json!({"source_bytes": source.text().len(), "full": full}),
        );
        let analysis = Analysis::new_with_knowledge(root, typed_ast, &source, &knowledge);
        drop(analysis_timing);
        let documentation_timing = crate::telemetry::Timing::start(
            "document.documentation",
            serde_json::json!({"source_bytes": source.text().len()}),
        );
        let packages = SourcePackages::collect(root, &source);
        let package_documentation = PackageDocumentation::collect(root, &source, &packages);
        let diagnostic_suppressions = DiagnosticSuppressions::collect(root, &source);
        let init_file_directive = InitFileDirective::collect(root, &source);
        drop(documentation_timing);
        let diagnostics_timing = crate::telemetry::Timing::start(
            "document.diagnostics",
            serde_json::json!({"source_bytes": source.text().len()}),
        );
        let diagnostics = diagnostics::collect(
            diagnostics::DiagnosticInputs {
                primary_code: properties.primary_code,
                analysis: &analysis,
                root,
                syntax: typed_ast,
                source: &source,
                knowledge: &knowledge,
                packages: &packages,
                documentation: &package_documentation,
            },
            !full,
        );
        drop(diagnostics_timing);
        let documentation_timing = crate::telemetry::Timing::start(
            "document.attached_documentation",
            serde_json::json!({"source_bytes": source.text().len()}),
        );
        let mut documentation = collect_documentation(&source, root, &analysis);
        documentation.extend_blocks(package_documentation.documentation_blocks(&analysis));
        let mut symbol_mentions = documentation
            .references()
            .iter()
            .chain(
                packages
                    .declarations()
                    .iter()
                    .flat_map(|declaration| declaration.export_mentions()),
            )
            .cloned()
            .chain(package_documentation.mentions())
            .collect::<Vec<_>>();
        symbol_mentions.sort_by_key(SymbolMention::byte_span);
        symbol_mentions.dedup_by_key(|mention| mention.byte_span());
        let namespace_strings = analysis
            .source_semantic_tokens()
            .iter()
            .filter(|token| {
                matches!(
                    token.source_role,
                    Some(crate::semantic_token::SourceSemanticRole::NamespaceArgument)
                )
            })
            .map(|token| token.span.bytes())
            .collect::<std::collections::HashSet<_>>();
        let package_mentions = root
            .descendants()
            .filter(|node| {
                node.is_string_literal()
                    && namespace_strings.contains(&(node.start_byte()..node.end_byte()))
            })
            .filter_map(|node| node.string_literal_inner_bytes())
            .map(|bytes| SymbolMention::new(source.span_for_bytes(bytes)))
            .collect();
        drop(documentation_timing);
        let mut snapshot = Self {
            implied_properties,
            properties,
            source,
            tree,
            analysis,
            diagnostics,
            object_registry: knowledge,
            documentation,
            symbol_mentions,
            package_mentions,
            imported_packages,
            file_loads,
            packages,
            package_documentation,
            diagnostic_suppressions,
            init_file_directive,
            package_sessions: OnceLock::new(),
            previous_sessions: Mutex::new(previous_sessions),
            // `check` and `installPackage` run the code with `-q`, which
            // skips the init file.
            session_knowledge: knowledge_provider.without_init_file(),
        };
        if full {
            let _timing = crate::telemetry::Timing::start(
                "document.package_sessions",
                serde_json::json!({"source_bytes": snapshot.text().len()}),
            );
            let session_diagnostics = snapshot
                .package_sessions()
                .iter()
                .flat_map(|session| session.snapshot.diagnostics.iter().cloned())
                .collect::<Vec<_>>();
            snapshot.diagnostics.extend(session_diagnostics);
        }
        snapshot
    }

    /// Prepares save-time work using retained syntax, without holding a document lock.
    pub fn save_job(&self, knowledge: ObjectRegistry) -> impl FnOnce() -> Self + Send + 'static {
        let source = self.source.clone();
        let tree = self.tree.clone();
        let previous = self.package_sessions.get().cloned().unwrap_or_else(|| {
            self.previous_sessions
                .lock()
                .expect("session cache lock")
                .clone()
        });
        let implied_properties = self.implied_properties;
        move || Self::analyze(source, tree, &knowledge, previous, true, implied_properties)
    }

    /// Analyze the document again if its location now implies other
    /// properties, as when the configured primary code directories change.
    /// Returns whether it was analyzed again.
    pub fn imply_properties(
        &mut self,
        implied_properties: FileProperties,
        knowledge_provider: &ObjectRegistry,
    ) -> bool {
        if implied_properties == self.implied_properties {
            return false;
        }
        let previous_sessions = self.take_sessions();
        *self = Self::analyze(
            self.source.clone(),
            self.tree.clone(),
            knowledge_provider,
            previous_sessions,
            true,
            implied_properties,
        );
        true
    }

    /// Analyze the document again if `knowledge_provider` starts its
    /// session with another init file than the one it was analyzed with, as
    /// when its directive or the settings select another, or the file
    /// changed. Returns whether it was analyzed again.
    pub fn restart_session(&mut self, knowledge_provider: &ObjectRegistry) -> bool {
        if knowledge_provider.init_file() == self.object_registry.init_file() {
            return false;
        }
        let previous_sessions = self.take_sessions();
        *self = Self::analyze(
            self.source.clone(),
            self.tree.clone(),
            knowledge_provider,
            previous_sessions,
            true,
            self.implied_properties,
        );
        true
    }

    /// Take the package sessions out of the snapshot for reuse by its
    /// successor: those analyzed, or else those it inherited.
    fn take_sessions(&mut self) -> Vec<PackageSession> {
        std::mem::take(&mut self.package_sessions)
            .into_inner()
            .unwrap_or_else(|| {
                std::mem::take(
                    self.previous_sessions
                        .get_mut()
                        .expect("session cache lock"),
                )
            })
    }

    /// Apply the client's `changes` in order and analyze the result, reparsing
    /// incrementally and re-checking only from the first changed cell on,
    /// unless the file properties changed. `None` when a change range lies
    /// outside the text or parsing fails.
    pub fn apply_changes(
        &mut self,
        changes: &[TextDocumentContentChangeEvent],
        knowledge_provider: &ObjectRegistry,
    ) -> Option<()> {
        let mut source = DocumentSource::new(self.text().to_owned());
        for change in changes {
            if let Some(range) = change.range {
                let bytes = source.bytes_for_range(range)?;
                source.replace_range(bytes, &change.text);
            } else {
                source = DocumentSource::new(change.text.clone());
            }
        }
        if source.text() == self.text() {
            return Some(());
        }
        let parsing = crate::telemetry::Timing::start(
            "document.parse",
            serde_json::json!({"source_bytes": source.text().len(), "incremental": true}),
        );
        let tree = SyntaxParser::new()?.parse_tree(source.text(), Some(&self.tree))?;
        drop(parsing);
        let implied_properties = self.implied_properties;
        if implied_properties | FileProperties::collect(tree.root(source.text())) != self.properties
        {
            *self = Self::analyze(
                source,
                tree,
                knowledge_provider,
                Vec::new(),
                true,
                implied_properties,
            );
            return Some(());
        }
        let unchanged_prefix = source
            .text()
            .bytes()
            .zip(self.text().bytes())
            .take_while(|(a, b)| a == b)
            .count();
        let cutoff = tree
            .changed_ranges()
            .first()
            .map_or(unchanged_prefix, |range| range.start);
        let retained = self
            .diagnostics
            .iter()
            .filter(|diagnostic| {
                diagnostic.rule.code != diagnostics::catalog::UNUSED_BINDING.code
                    && std::iter::once(diagnostic.finding.range)
                        .chain(diagnostic.finding.notes.iter().map(|note| note.range))
                        .all(|range| {
                            self.bytes_for_range(range)
                                .is_some_and(|bytes| bytes.end <= cutoff)
                        })
            })
            .cloned()
            .collect::<Vec<_>>();
        let previous_sessions = self.take_sessions();
        *self = Self::analyze(
            source,
            tree,
            knowledge_provider,
            previous_sessions,
            false,
            implied_properties,
        );
        self.diagnostics.extend(retained);
        Some(())
    }

    /// The text analyzed.
    pub fn text(&self) -> &str {
        SourceNavigation::text(self)
    }

    /// The packages loaded in the document's session, its imports included, and
    /// the init file it started with.
    pub fn object_registry(&self) -> &ObjectRegistry {
        &self.object_registry
    }

    /// The file properties analysis used: those implied by the location with
    /// those the directives declare.
    pub fn properties(&self) -> FileProperties {
        self.properties
    }

    /// The semantic analysis of the text.
    pub fn analysis(&self) -> &Analysis {
        &self.analysis
    }

    /// The typed syntax tree of the text.
    pub fn syntax(&self) -> Option<&TypedSource> {
        Some(self.tree.syntax())
    }

    /// Every finding of the analysis and the package sessions, before settings
    /// and suppressions filter them.
    pub fn diagnostics(&self) -> &[M2Diagnostic] {
        &self.diagnostics
    }

    /// What the document's directive comments silence.
    /// The comment selecting the init file the document starts with.
    pub fn init_file_directive(&self) -> Option<&InitFileDirective> {
        self.init_file_directive.as_ref()
    }

    /// The diagnostics the document's `disable` directives hide.
    pub fn diagnostic_suppressions(&self) -> &DiagnosticSuppressions {
        &self.diagnostic_suppressions
    }

    /// The packages the document imports, each from where it takes effect.
    pub fn imported_packages(&self) -> &[PackageImport] {
        &self.imported_packages
    }

    /// The files the document runs with `load`/`needs`/`input`, in source
    /// order.
    pub fn file_loads(&self) -> &[FileLoad] {
        &self.file_loads
    }

    /// The load whose file name contains `position`.
    pub fn file_load_at(&self, position: Position) -> Option<&FileLoad> {
        self.file_loads
            .iter()
            .find(|load| load.names_file_at(position))
    }

    /// The packages the document declares.
    pub fn packages(&self) -> &SourcePackages {
        &self.packages
    }

    /// The package documentation the document writes.
    pub fn package_documentation(&self) -> &PackageDocumentation {
        &self.package_documentation
    }

    /// The code the document runs in package sessions — its TEST blocks and
    /// its documentation examples — each analyzed on its own.
    pub fn package_sessions(&self) -> &[PackageSession] {
        self.package_sessions.get_or_init(|| {
            let code = self
                .packages
                .tests()
                .iter()
                .chain(self.package_documentation.example_sessions())
                .collect::<Vec<_>>();
            let previous =
                std::mem::take(&mut *self.previous_sessions.lock().expect("session cache lock"));
            PackageSession::analyze_all(
                &code,
                &self.packages,
                &self.source,
                &self.session_knowledge,
                previous,
            )
        })
    }

    /// The package session whose code contains `position`, which answers for
    /// it in place of the document.
    pub fn package_session_at(&self, position: Position) -> Option<&PackageSession> {
        if !self
            .packages
            .tests()
            .iter()
            .chain(self.package_documentation.example_sessions())
            .any(|code| code.contains(position))
        {
            return None;
        }
        self.package_sessions()
            .iter()
            .find(|session| session.code.contains(position))
    }

    /// The binding of the name at `position`, in code or in a mention.
    pub fn binding_at_position(&self, position: Position) -> Option<BindingView<'_>> {
        if let Some(mention) = self.symbol_mention_at(position) {
            return self.mention_symbol(&mention);
        }
        let node = self.reference_node_at_position(position)?;
        self.source_binding_at(node.text(), position)
    }

    /// The binding `name` resolves to at `position`, if the session sees it
    /// there.
    pub fn source_binding_at(&self, name: &str, position: Position) -> Option<BindingView<'_>> {
        self.analysis
            .visible_source_binding_at(name, position, &self.object_registry.at(position))
    }

    /// The binding a later assignment of `name` gives it, for a use at
    /// `position` before it, as in a function body run only after the
    /// assignment.
    pub fn future_assignment_binding_at(
        &self,
        name: &str,
        position: Position,
    ) -> Option<BindingView<'_>> {
        self.analysis
            .future_assignment_binding_at(name, position)
            .filter(|binding| {
                Analysis::source_binding_is_visible(*binding, &self.object_registry.at(position))
            })
    }

    /// The binding `name` resolves to at `position`, as
    /// [`Self::source_binding_at`].
    pub fn source_symbol_at(&self, name: &str, position: Position) -> Option<BindingView<'_>> {
        self.source_binding_at(name, position)
    }

    /// The name at `position` with its range and binding: what references,
    /// highlights and rename start from.
    pub fn target_symbol_at(&self, position: Position) -> Option<TargetSymbol<'_>> {
        let mention = self.symbol_mention_at(position);
        let (name, range) = if let Some(mention) = mention.as_ref() {
            (mention.name(self.text()), mention.range())
        } else {
            let node = self.reference_node_at_position(position)?;
            (node.text(), self.range_for_node(node))
        };
        let symbol = if let Some(mention) = mention {
            self.mention_symbol(&mention)?
        } else {
            self.source_symbol_at(name, position)?
        };
        Some(TargetSymbol {
            name,
            range,
            symbol,
        })
    }

    /// Every symbol the document names outside of code, in source order:
    /// documentation links and the names its packages export.
    pub fn symbol_mentions(&self) -> &[SymbolMention] {
        &self.symbol_mentions
    }

    /// Package names in string arguments, kept separate from renameable symbols.
    pub fn package_mentions(&self) -> &[SymbolMention] {
        &self.package_mentions
    }

    /// Comment spans accepted as attached documentation.
    pub fn documentation_comment_ranges(&self) -> &[ByteRange] {
        self.documentation.comment_ranges()
    }

    /// Code examples in attached Markdown documentation.
    pub fn documentation_example_ranges(&self) -> &[ByteRange] {
        self.documentation.example_ranges()
    }

    /// The code snippets of the documentation comments, analyzed with the
    /// document.
    pub fn documentation_snippets(&self) -> &[DocumentationSnippet] {
        self.documentation.snippets()
    }

    #[cfg(test)]
    pub fn documentation_blocks(&self) -> &[DocumentationBlock] {
        self.documentation.blocks()
    }

    /// The statement declaring the state `binding` is in, as written: the
    /// code a hover shows for it. `None` when the declaration is the name
    /// alone, as for a parameter.
    pub fn declaration_code(&self, binding: BindingView<'_>) -> Option<&str> {
        if binding.state.definition_range == binding.state.span {
            return None;
        }
        self.statement_code_at(binding.state.span.start)
    }

    /// The statement installing the method `installation`, as written.
    pub fn installation_code(&self, installation: &MethodInstallation) -> Option<&str> {
        self.statement_code_at(installation.span.start)
    }

    /// The code of the statement at `position`: its cell without the
    /// comments around it, which document it rather than being its code.
    fn statement_code_at(&self, position: Position) -> Option<&str> {
        let cell = self
            .node_at_position_minimal(position)?
            .enclosing_node(|node| node.is_source_cell())?;
        let mut code = cell.named_children().filter(|child| !child.is_comment());
        let first = code.next()?;
        let end = code.last().unwrap_or(first).end_byte();
        Some(&self.text()[first.start_byte()..end])
    }

    /// The documentation comment attached to `binding`.
    pub fn documentation_for_binding(
        &self,
        binding: BindingView<'_>,
    ) -> Option<&DocumentationBlock> {
        self.documentation.for_binding(binding)
    }

    /// The documentation comment attached to the method installation
    /// `installation`.
    pub fn documentation_for_installation(
        &self,
        installation: crate::analysis::MethodInstallationId,
    ) -> Option<&DocumentationBlock> {
        self.documentation.for_installation(installation)
    }

    /// The mention outside code containing `position`.
    pub fn symbol_mention_at(&self, position: Position) -> Option<SymbolMention> {
        self.symbol_mentions
            .iter()
            .find(|mention| mention.contains(position))
            .cloned()
    }

    /// The binding `mention` refers to: the one its name resolves to at its
    /// position, or else the global the name is assigned to anywhere, since a
    /// mention, unlike code, may precede the definition it names.
    pub fn mention_symbol(&self, mention: &SymbolMention) -> Option<BindingView<'_>> {
        self.analysis
            .mention_symbol_at(mention.name(self.text()), mention.range().start)
            .filter(|binding| {
                Analysis::source_binding_is_visible(
                    *binding,
                    &self.object_registry.at(mention.range().start),
                )
            })
    }

    /// The name at `position`, in code or in a mention, with its range.
    pub fn symbol_occurrence_at(&self, position: Position) -> Option<(&str, TextRange)> {
        if let Some(mention) = self.symbol_mention_at(position) {
            return Some((mention.name(self.text()), mention.range()));
        }
        let node = self.reference_node_at_position(position)?;
        Some((node.text(), self.range_for_node(node)))
    }

    /// The function the name at `position` is bound to.
    pub fn callable_at_position(&self, position: Position) -> Option<&FunctionInfo> {
        let binding = self.binding_at_position(position)?;
        self.analysis.function_for_binding(binding)
    }

    /// The ranges of the values that are functions, `x -> …`, possibly
    /// parenthesized.
    pub fn lambda_value_ranges(&self) -> Vec<TextRange> {
        self.root_node()
            .descendants()
            .filter(|node| {
                let mut value = *node;
                while value.is_holder() {
                    let Some(inner) = value.final_value_child() else {
                        return false;
                    };
                    value = inner;
                }
                value.is::<LambdaExpression>()
            })
            .map(|node| self.range_for_node(node))
            .collect()
    }

    /// The parse tree of the text.
    pub(crate) fn syntax_tree(&self) -> &SyntaxTree {
        &self.tree
    }

    /// The root of the parse tree.
    pub fn root_node(&self) -> SyntaxNode<'_> {
        self.tree.root(self.text())
    }

    /// The smallest syntax node at `position`.
    pub fn node_at_position_minimal(&self, position: Position) -> Option<SyntaxNode<'_>> {
        let point = self.point_for_position(position)?;
        self.root_node().descendant_for_point_range(point, point)
    }

    /// The symbol under the cursor that refers to a binding by name; `None`
    /// on a member key, which names the symbol itself.
    pub fn reference_node_at_position(&self, position: Position) -> Option<SyntaxNode<'_>> {
        let point = self.point_for_position(position)?;
        let root = self.root_node();
        // When the cursor sits on the boundary between the anonymous SPACE
        // application operator (which the grammar emits zero-width) and an
        // adjacent symbol — e.g. the trailing `M` in an application `(f x) M`,
        // where SPACE sits exactly at `M`'s start column — a zero-width lookup
        // lands on the operator. Widening the lookup to the character under the
        // cursor lands on the symbol; the exact-point lookup is tried first so
        // ordinary mid-token positions are unaffected.
        let next = Point::new(point.row, point.column + 1);
        let starts = [
            root.descendant_for_point_range(point, point),
            root.descendant_for_point_range(point, next),
        ];
        for start in starts.into_iter().flatten() {
            let mut node = start;
            loop {
                if node.is_symbol_like() {
                    return (!node.is_member_key()).then_some(node);
                }
                match node.parent() {
                    Some(parent) => node = parent,
                    None => break,
                }
            }
        }
        None
    }
}

impl PackageSession {
    /// The sessions running `code`, declared in `source` with `packages`,
    /// each analyzed alone against `knowledge_provider` plus the packages its
    /// package exports along. A session whose code, positions, and packages
    /// match one of `previous` keeps that analysis; the others are analyzed in
    /// parallel.
    fn analyze_all(
        code: &[&PackageSessionCode],
        packages: &SourcePackages,
        source: &DocumentSource,
        knowledge_provider: &ObjectRegistry,
        mut previous: Vec<PackageSession>,
    ) -> Vec<PackageSession> {
        let mut sessions = Vec::with_capacity(code.len());
        let mut pending = Vec::new();
        for (ordinal, session_code) in code.iter().copied().enumerate() {
            let isolated_source = session_code.isolated_source(source.text());
            let exported_packages = session_code
                .package
                .as_ref()
                .map(|package| {
                    packages
                        .declarations()
                        .iter()
                        .find(|declaration| declaration.name == *package)
                        .expect("session code belongs to a package the document declares")
                        .exported_packages
                        .clone()
                })
                .unwrap_or_default();
            match previous.iter().position(|session| {
                session.code.package == session_code.package
                    && session.exported_packages == exported_packages
                    && session.snapshot.text() == isolated_source
            }) {
                Some(unchanged) => {
                    let mut kept = previous.swap_remove(unchanged);
                    kept.code = session_code.clone();
                    sessions.push((ordinal, kept));
                }
                None => pending.push((ordinal, session_code, isolated_source, exported_packages)),
            }
        }

        let workers = thread::available_parallelism()
            .map_or(1, usize::from)
            .min(pending.len());
        let queue = Mutex::new(pending.into_iter());
        let analyzed = Mutex::new(Vec::new());
        thread::scope(|scope| {
            for _ in 0..workers {
                thread::Builder::new()
                    .stack_size(crate::RUNTIME_THREAD_STACK_BYTES)
                    .spawn_scoped(scope, || loop {
                        // Taken in its own statement, so the lock is released
                        // before the code is analyzed.
                        let next_code = queue.lock().expect("queue lock").next();
                        let Some((ordinal, session_code, isolated_source, exported_packages)) =
                            next_code
                        else {
                            break;
                        };
                        if let Some(session) = Self::analyze(
                            session_code,
                            isolated_source,
                            exported_packages,
                            knowledge_provider,
                        ) {
                            analyzed
                                .lock()
                                .expect("analyzed lock")
                                .push((ordinal, session));
                        }
                    })
                    .expect("package session analysis worker starts");
            }
        });
        sessions.extend(analyzed.into_inner().expect("analyzed lock"));
        sessions.sort_by_key(|(ordinal, _)| *ordinal);
        sessions.into_iter().map(|(_, session)| session).collect()
    }

    /// `code`, isolated as `isolated_source`, analyzed against
    /// `knowledge_provider` plus `exported_packages`.
    fn analyze(
        code: &PackageSessionCode,
        isolated_source: String,
        exported_packages: Vec<ObjectName>,
        knowledge_provider: &ObjectRegistry,
    ) -> Option<Self> {
        let session_imports = exported_packages
            .iter()
            .map(|exported| PackageImport {
                package: exported.clone(),
                effective_from: pos!(),
            })
            .collect::<Vec<_>>();
        let snapshot = DocumentSnapshot::from_text(
            isolated_source,
            &knowledge_provider.with_imports(&session_imports),
        )?;
        Some(Self {
            code: code.clone(),
            exported_packages,
            snapshot: Arc::new(snapshot),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_first_pass_builds_scope_and_binding_graphs() {
        let document = DocumentSnapshot::from_text(
            "outer = 1\nlocalValue := 2\nf = x -> x\nfor i from 0 to 1 do i\n".to_string(),
            &ObjectRegistry::default(),
        )
        .expect("the document should parse");

        assert_eq!(document.analysis.binding_states().count(), 5);
        assert_eq!(document.analysis.scope_count(), 5);
    }

    #[test]
    fn typed_first_pass_handles_new_installations() {
        let source = concat!(
            "M = new Type of BasicList\n",
            "new Type of BasicList from Function := (target, parent, convert) -> hashTable {}\n",
            "new M from (ZZ, ZZ) := (target, left, right) -> {left, right}\n",
            "new M := target -> {}\n",
        );
        let document = DocumentSnapshot::from_text(source.to_string(), &ObjectRegistry::default())
            .expect("the document should parse");

        assert_eq!(document.analysis.scope_count(), 4);
    }

    #[test]
    fn document_keeps_working_when_typed_syntax_rejects_incomplete_source() {
        let document =
            DocumentSnapshot::from_text("value = (\n".to_string(), &ObjectRegistry::default())
                .expect("Tree-sitter should retain an incomplete document");

        assert!(document
            .analysis
            .bindings()
            .any(|binding| binding.name.name() == "value"));
        assert_eq!(document.text(), "value = (\n");
    }

    #[test]
    fn incremental_changes_rebuild_analysis() {
        let mut document =
            DocumentSnapshot::from_text("value = 1\n".to_string(), &ObjectRegistry::default())
                .expect("the document should parse");
        document
            .apply_changes(
                &[TextDocumentContentChangeEvent {
                    range: Some(TextRange::new(Position::new(0, 8), Position::new(0, 9))),
                    range_length: None,
                    text: "\"text\"".to_string(),
                }],
                &ObjectRegistry::default(),
            )
            .expect("the incremental change should parse");

        let binding = document
            .analysis
            .binding_states()
            .find(|binding| binding.name.name() == "value")
            .expect("the edited binding should be analyzed");
        assert_eq!(
            binding.state.inferred_type,
            Some(crate::typesystem::InferredType::exact("String"))
        );
    }

    #[test]
    fn package_tests_follow_edits_inside_and_around_them() {
        let text = "newPackage \"Pkg\"\nTEST ///\nfirst = 1\n///\nendPackage \"Pkg\"\n";
        let mut document =
            DocumentSnapshot::from_text(text.to_string(), &ObjectRegistry::default())
                .expect("the document should parse");
        let edit = |line, character, text: &str| TextDocumentContentChangeEvent {
            range: Some(TextRange::new(
                Position::new(line, character),
                Position::new(line, character),
            )),
            range_length: None,
            text: text.to_string(),
        };

        document
            .apply_changes(&[edit(4, 17, "\nafter = 2")], &ObjectRegistry::default())
            .expect("an edit after the TEST block should parse");
        assert_eq!(
            document.package_sessions()[0].snapshot.text().trim(),
            "first = 1"
        );

        document
            .apply_changes(&[edit(0, 0, "before = 0\n")], &ObjectRegistry::default())
            .expect("an edit before the TEST block should parse");
        assert_eq!(
            document.package_sessions()[0].code.fragments[0].range.start,
            Position::new(2, 8)
        );

        document
            .apply_changes(&[edit(3, 0, "second = 2\n")], &ObjectRegistry::default())
            .expect("an edit inside the TEST block should parse");
        let test = &document.package_sessions()[0];
        assert!(test
            .snapshot
            .analysis()
            .bindings()
            .any(|binding| binding.name.name() == "second"));
    }
}

#[cfg(test)]
mod snapshot_scheduling_tests {
    use super::*;

    #[test]
    fn batched_changes_and_save_share_unmodified_cell_syntax() {
        let knowledge = ObjectRegistry::default();
        let mut document =
            DocumentSnapshot::from_text("x = 1\ny = 2\n".into(), &knowledge).unwrap();
        let before = document.tree.snapshot().clone();
        document
            .apply_changes(
                &[
                    TextDocumentContentChangeEvent {
                        range: Some(TextRange::new(pos!(0, 4), pos!(0, 5))),
                        range_length: None,
                        text: String::new(),
                    },
                    TextDocumentContentChangeEvent {
                        range: Some(TextRange::new(pos!(0, 4), pos!(0, 4))),
                        range_length: None,
                        text: "1234".into(),
                    },
                ],
                &knowledge,
            )
            .unwrap();
        assert_eq!(document.text(), "x = 1234\ny = 2\n");
        assert_eq!(
            document.tree.snapshot().cells()[1].id(),
            before.cells()[1].id()
        );
        let ids = document
            .tree
            .snapshot()
            .cells()
            .iter()
            .map(|cell| cell.id())
            .collect::<Vec<_>>();
        let saved = document.save_job(knowledge)();
        assert_eq!(
            saved
                .tree
                .snapshot()
                .cells()
                .iter()
                .map(|cell| cell.id())
                .collect::<Vec<_>>(),
            ids
        );
    }

    #[test]
    fn save_refreshes_document_checks_while_input_keeps_local_checks() {
        let knowledge = ObjectRegistry::default();
        let mut document = DocumentSnapshot::from_text("value = 1\n".into(), &knowledge).unwrap();
        document
            .apply_changes(
                &[TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "f = x -> (unused := x; x)\n".into(),
                }],
                &knowledge,
            )
            .unwrap();
        assert!(!document
            .diagnostics()
            .iter()
            .any(|d| d.rule.code == diagnostics::catalog::UNUSED_BINDING.code));
        let saved = document.save_job(knowledge)();
        assert!(saved
            .diagnostics()
            .iter()
            .any(|d| d.finding.message == "Unused variable unused"));
    }

    #[test]
    fn live_updates_recheck_everything_after_the_edit() {
        let knowledge = ObjectRegistry::load(crate::catalog::test_corpus());
        let mut document = DocumentSnapshot::from_text(
            "-- points\np = method()\np ZZ := n -> n + 1\n".into(),
            &knowledge,
        )
        .unwrap();
        let codomain_lines = |document: &DocumentSnapshot| {
            document
                .diagnostics()
                .iter()
                .filter(|d| d.rule.code == diagnostics::catalog::INSTALL_CODOMAIN_MISSING.code)
                .map(|d| d.finding.range.start.line)
                .collect::<Vec<_>>()
        };
        assert_eq!(codomain_lines(&document), vec![2]);

        document
            .apply_changes(
                &[TextDocumentContentChangeEvent {
                    range: Some(TextRange::new(pos!(0, 9), pos!(0, 9))),
                    range_length: None,
                    text: " in the plane\n-- and space".into(),
                }],
                &knowledge,
            )
            .unwrap();
        assert_eq!(
            codomain_lines(&document),
            vec![3],
            "an edit above re-checks the code below it"
        );
    }

    #[test]
    fn live_updates_keep_local_findings_in_the_unchanged_prefix() {
        let knowledge = ObjectRegistry::default();
        let mut document =
            DocumentSnapshot::from_text("x = (1 + )\ny = 2\n".into(), &knowledge).unwrap();
        document
            .apply_changes(
                &[TextDocumentContentChangeEvent {
                    range: Some(TextRange::new(pos!(1, 4), pos!(1, 5))),
                    range_length: None,
                    text: "3".into(),
                }],
                &knowledge,
            )
            .unwrap();
        assert!(document
            .diagnostics()
            .iter()
            .any(|d| d.rule.code == diagnostics::catalog::SYNTAX_ERROR.code
                && d.finding.range.start.line == 0));
    }
}
