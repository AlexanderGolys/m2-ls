//! LSP encoding of semantic-token candidates registered in document analysis.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::analysis::BindingView;
use crate::document::{DocumentSnapshot, PackageSession};
use crate::documentation::DocumentationSnippet;
use crate::object_registry::ObjectRegistry;
use crate::package_documentation::{DocumentationMarkupKind, TextStyle};
use crate::semantic_token::{
    classify_source_semantic_token, local_symbol_semantic_token, M2SemanticToken,
    M2SemanticTokenModifier, M2SemanticTokenType, SemanticTokenKnowledge, SourceSemanticToken,
    SourceSemanticTokenContext,
};
use crate::settings::SemanticTokenSettings;
use crate::source::{ByteRange, DocumentSpan, SourceNavigation, SymbolMention};
use crate::source_package::{CodeFragment, ExportAccess};
use crate::workspace_index::{WorkspaceDefinitionKnowledge, WorkspaceIndex, WorkspaceView};
use tower_lsp::lsp_types::Range as TextRange;
use tower_lsp::lsp_types::*;

pub fn collect_semantic_tokens(
    document: &DocumentSnapshot,
    builtins: &ObjectRegistry,
    workspace_index: &WorkspaceIndex,
    uri: &Url,
    settings: &SemanticTokenSettings,
    augments_syntax_tokens: bool,
) -> Vec<SemanticToken> {
    let classifier = SemanticTokenClassifier {
        document,
        builtins,
        workspace_index: &WorkspaceView::Document(workspace_index),
        uri,
    };
    let session_workspace = WorkspaceView::PackageSession(workspace_index);
    let session_classifiers = document
        .package_sessions()
        .iter()
        .map(|session| SessionTokenClassifier {
            session,
            classifier: SemanticTokenClassifier {
                document: &session.snapshot,
                builtins: session.snapshot.object_registry(),
                workspace_index: &session_workspace,
                uri,
            },
        })
        .collect::<Vec<_>>();
    let mut string_spans = classifier.export_spans();
    string_spans.extend(classifier.documentation_spans());
    string_spans.sort_by_key(|span| span.source.bytes().start);
    let mut emitter = SemanticTokenEmitter::new(document, settings);

    for source_token in document.analysis().source_semantic_tokens() {
        if emitter.emit_embedded_tokens(
            source_token,
            &session_classifiers,
            &string_spans,
            augments_syntax_tokens,
        ) {
            continue;
        }
        let bytes = source_token.span.bytes();
        let first = string_spans.partition_point(|span| span.source.bytes().start < bytes.start);
        let markup = string_spans[first..]
            .iter()
            .take_while(|span| span.source.bytes().start == bytes.start)
            .find(|span| span.source.bytes() == bytes);
        if let Some(span) = markup {
            emitter.push(span.clone());
        } else if let Some(span) = classifier.semantic_span(
            source_token,
            augments_syntax_tokens && !emitter.has_context(&bytes),
        ) {
            emitter.push(span);
        }
    }

    emitter.finish()
}

/// The classifier of one package session's code, whose own analysis decides
/// the tokens of the code embedded in the document's strings.
struct SessionTokenClassifier<'a> {
    session: &'a PackageSession,
    classifier: SemanticTokenClassifier<'a, WorkspaceView<'a>>,
}

impl SessionTokenClassifier<'_> {
    /// The tokens of the session's code lying inside `container`, a byte
    /// range of the document, placed in the document. Its literals and
    /// operators are among them: syntax highlighting sees only the string
    /// holding the code.
    fn spans_inside(
        &self,
        document: &DocumentSnapshot,
        container: &ByteRange,
    ) -> Vec<SemanticSpan> {
        let inside = |fragment: &CodeFragment| {
            container.start <= fragment.bytes.start && fragment.bytes.end <= container.end
        };
        if !self.session.code.fragments.iter().any(inside) {
            return Vec::new();
        }
        self.session
            .snapshot
            .analysis()
            .source_semantic_tokens()
            .iter()
            .filter_map(|token| self.classifier.semantic_span(token, false))
            .filter_map(|span| {
                let bytes = document.bytes_for_range(span.source.range())?;
                (container.start <= bytes.start && bytes.end <= container.end).then(|| {
                    SemanticSpan {
                        source: document.span_for_bytes(bytes),
                        token: span.token,
                    }
                })
            })
            .collect()
    }
}

struct SemanticTokenEmitter<'a> {
    document: &'a DocumentSnapshot,
    settings: &'a SemanticTokenSettings,
    tokens: Vec<SemanticToken>,
    previous: Position,
    contexts: Vec<(ByteRange, M2SemanticTokenModifier)>,
}

impl<'a> SemanticTokenEmitter<'a> {
    fn new(document: &'a DocumentSnapshot, settings: &'a SemanticTokenSettings) -> Self {
        let documentation = document.package_documentation();
        let mut contexts = documentation
            .documentation_ranges()
            .iter()
            .chain(document.documentation_comment_ranges())
            .cloned()
            .map(|bytes| (bytes, M2SemanticTokenModifier::Documentation))
            .collect::<Vec<_>>();
        contexts.extend(
            documentation
                .example_ranges()
                .iter()
                .chain(document.documentation_example_ranges())
                .cloned()
                .map(|bytes| (bytes, M2SemanticTokenModifier::Example)),
        );
        contexts.extend(
            document
                .root_node()
                .descendants()
                .filter(|node| {
                    node.is_debug_expr()
                        && node
                            .children()
                            .any(|child| child.is::<m2_syn::Token![TEST]>())
                })
                .map(|node| {
                    (
                        node.start_byte()..node.end_byte(),
                        M2SemanticTokenModifier::Test,
                    )
                }),
        );
        Self {
            document,
            settings,
            tokens: Vec::new(),
            previous: pos!(),
            contexts,
        }
    }

    /// Emit a string or comment `source_token` that embeds code or markup:
    /// the code of package sessions (TEST blocks, documentation examples) as
    /// their own analysis classifies it, the `string_spans` inside it — the
    /// names export strings list, the code and SimpleDoc section keywords of
    /// documentation strings — and documentation snippets, and the rest of
    /// the container as the string or comment it is. Returns `false`,
    /// emitting nothing, when the container embeds nothing.
    fn emit_embedded_tokens(
        &mut self,
        source_token: &SourceSemanticToken,
        sessions: &[SessionTokenClassifier<'_>],
        string_spans: &[SemanticSpan],
        augments_syntax_tokens: bool,
    ) -> bool {
        let Some(base_type) = source_token.syntax_token_type.filter(|token_type| {
            matches!(
                token_type,
                M2SemanticTokenType::Comment | M2SemanticTokenType::String
            )
        }) else {
            return false;
        };
        let source_bytes = source_token.span.bytes();
        let inside =
            |(start, end): (usize, usize)| source_bytes.start <= start && end <= source_bytes.end;

        let mut spans = sessions
            .iter()
            .flat_map(|session| session.spans_inside(self.document, &source_bytes))
            .collect::<Vec<_>>();
        let first_inside =
            string_spans.partition_point(|span| span.source.bytes().start < source_bytes.start);
        spans.extend(
            string_spans[first_inside..]
                .iter()
                .take_while(|span| span.source.bytes().start < source_bytes.end)
                .filter(|span| span.source.bytes().end <= source_bytes.end)
                .cloned(),
        );
        spans.extend(
            self.document
                .documentation_snippets()
                .iter()
                .filter(|snippet| inside(snippet.byte_span()))
                .map(|snippet| documentation_snippet_semantic_span(self.document, snippet)),
        );
        let is_documentation = self.has_context(&source_bytes);
        let base_token = M2SemanticToken::new(base_type);
        if spans.is_empty() && !is_documentation {
            return false;
        }
        spans.sort_by_key(|span| {
            let bytes = span.source.bytes();
            (bytes.start, bytes.end)
        });

        let emit_base = !augments_syntax_tokens
            || base_type.emit_with_syntax_highlighting()
            || is_documentation;
        let mut boundaries = vec![source_bytes.start, source_bytes.end];
        for span in &spans {
            let bytes = span.source.bytes();
            boundaries.extend([bytes.start, bytes.end]);
        }
        boundaries.sort_unstable();
        boundaries.dedup();

        // Prefer the most specific span where markup nests or contains code.
        let mut active = BinaryHeap::new();
        let mut next_span = 0;
        let mut run: Option<(ByteRange, M2SemanticToken)> = None;
        let mut emitted = false;
        for segment in boundaries.windows(2) {
            let bytes = segment[0]..segment[1];
            while next_span < spans.len() && spans[next_span].source.bytes().start <= bytes.start {
                active.push(Reverse((spans[next_span].source.bytes().len(), next_span)));
                next_span += 1;
            }
            while active
                .peek()
                .is_some_and(|Reverse((_, index))| spans[*index].source.bytes().end <= bytes.start)
            {
                active.pop();
            }
            let token = active
                .peek()
                .map(|Reverse((_, index))| spans[*index].token)
                .or_else(|| emit_base.then_some(base_token));
            if let (Some((previous, previous_token)), Some(token)) = (&mut run, token) {
                if previous.end == bytes.start && *previous_token == token {
                    previous.end = bytes.end;
                    continue;
                }
            }
            if let Some((previous, token)) = run.take() {
                emitted |= self.push(SemanticSpan {
                    source: self.document.span_for_bytes(previous),
                    token,
                });
            }
            run = token.map(|token| (bytes, token));
        }
        if let Some((bytes, token)) = run {
            emitted |= self.push(SemanticSpan {
                source: self.document.span_for_bytes(bytes),
                token,
            });
        }

        emitted
    }

    fn has_context(&self, bytes: &ByteRange) -> bool {
        self.contexts
            .iter()
            .any(|(context, _)| context.start <= bytes.start && bytes.end <= context.end)
    }

    fn push(&mut self, span: SemanticSpan) -> bool {
        let bytes = span.source.bytes();
        let mut boundaries = vec![bytes.start, bytes.end];
        for (context, _) in &self.contexts {
            boundaries.extend(
                [context.start, context.end]
                    .into_iter()
                    .filter(|boundary| bytes.start < *boundary && *boundary < bytes.end),
            );
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        let mut emitted = false;
        for segment in boundaries.windows(2) {
            let bytes = segment[0]..segment[1];
            let token = self
                .contexts
                .iter()
                .filter(|(context, _)| context.start <= bytes.start && bytes.end <= context.end)
                .fold(span.token, |token, (_, modifier)| {
                    token.with_modifier(*modifier)
                });
            let source = self.document.span_for_bytes(bytes);
            emitted |= self.push_ranges(self.document.visible_ranges(&source), token);
        }
        emitted
    }

    /// Emit `token` over `ranges`, single-line ranges in document order.
    fn push_ranges(&mut self, ranges: Vec<TextRange>, token: M2SemanticToken) -> bool {
        let mut emitted = false;
        let (token_type, token_modifiers_bitset) = token.encode(self.settings);
        for range in ranges {
            let delta_line = range.start.line - self.previous.line;
            let delta_start = if delta_line == 0 {
                range.start.character - self.previous.character
            } else {
                range.start.character
            };
            self.tokens.push(SemanticToken {
                delta_line,
                delta_start,
                length: range.end.character - range.start.character,
                token_type,
                token_modifiers_bitset,
            });
            self.previous = range.start;
            emitted = true;
        }
        emitted
    }

    fn finish(self) -> Vec<SemanticToken> {
        self.tokens
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SemanticSpan {
    source: DocumentSpan,
    token: M2SemanticToken,
}

fn documentation_snippet_semantic_span(
    document: &DocumentSnapshot,
    snippet: &DocumentationSnippet,
) -> SemanticSpan {
    let (start_byte, end_byte) = snippet.byte_span();
    SemanticSpan {
        source: document.span_for_bytes(start_byte..end_byte),
        token: M2SemanticToken::new(M2SemanticTokenType::Property),
    }
}

struct SemanticTokenClassifier<'a, W: ?Sized> {
    document: &'a DocumentSnapshot,
    builtins: &'a ObjectRegistry,
    workspace_index: &'a W,
    uri: &'a Url,
}

impl<W> SemanticTokenClassifier<'_, W>
where
    W: WorkspaceDefinitionKnowledge + ?Sized,
{
    /// The token of what the name `mention` writes names: the binding it
    /// resolves to, an indexed object, or a workspace definition.
    fn mention_token(&self, mention: &SymbolMention) -> Option<M2SemanticToken> {
        let name = mention.name(self.document.text());
        let knowledge = self.builtins.at(mention.range().start);
        self.document
            .mention_symbol(mention)
            .map(|binding| local_symbol_semantic_token(&binding, &knowledge))
            .or_else(|| knowledge.semantic_token(name))
            .or_else(|| {
                self.workspace_index
                    .semantic_token_type(name, self.uri)
                    .map(M2SemanticToken::new)
            })
    }

    /// The names export strings list, as the objects their definitions
    /// name, including declarations that follow the export list and
    /// exported synonyms. A name defined nowhere is the symbol the export
    /// creates. A name `exportMutable` lists is marked mutable, which sets
    /// it apart even when it names a plain variable.
    fn export_spans(&self) -> Vec<SemanticSpan> {
        let mut spans = Vec::new();
        for export in self
            .document
            .packages()
            .declarations()
            .iter()
            .flat_map(|package| &package.exports)
        {
            let named = self
                .mention_token(&export.definition_mention)
                .unwrap_or_else(|| M2SemanticToken::new(M2SemanticTokenType::Symbol));
            let token = match export.access {
                ExportAccess::Mutable => named.with_modifier(M2SemanticTokenModifier::Mutable),
                _ => match named.within_string() {
                    Some(token) => token,
                    None => continue,
                },
            };
            spans.push(SemanticSpan {
                source: export.definition_mention.span().clone(),
                token,
            });
            if export.name != export.definition {
                spans.push(SemanticSpan {
                    source: export.name_mention.span().clone(),
                    token,
                });
            }
        }
        spans
    }

    /// The marked spans of the document's documentation strings: section
    /// keywords, and the tokens of the code they embed, its names as what
    /// they name. A name naming nothing known stays part of the string.
    fn documentation_spans(&self) -> Vec<SemanticSpan> {
        self.document
            .package_documentation()
            .markup()
            .iter()
            .filter_map(|markup| {
                let source = self.document.span_for_bytes(markup.bytes.clone());
                let token = match markup.kind {
                    DocumentationMarkupKind::SectionKeyword => {
                        M2SemanticToken::new(M2SemanticTokenType::Annotation)
                    }
                    DocumentationMarkupKind::TextMarkup => {
                        M2SemanticToken::new(M2SemanticTokenType::Macro)
                    }
                    DocumentationMarkupKind::LinkConstructor => {
                        M2SemanticToken::new(M2SemanticTokenType::Annotation)
                    }
                    DocumentationMarkupKind::OptionName => M2SemanticToken::option_name(
                        &self.document.text()[markup.bytes.clone()],
                        &self.builtins.at(source.range().start),
                    ),
                    DocumentationMarkupKind::Styled(style) => {
                        M2SemanticToken::new(M2SemanticTokenType::String).with_modifier(match style
                        {
                            TextStyle::Strong => M2SemanticTokenModifier::Strong,
                            TextStyle::Emphasis => M2SemanticTokenModifier::Emphasis,
                            TextStyle::Monospace => M2SemanticTokenModifier::Monospace,
                            TextStyle::Link => M2SemanticTokenModifier::Link,
                            TextStyle::Math => M2SemanticTokenModifier::Math,
                        })
                    }
                    DocumentationMarkupKind::Name => self
                        .mention_token(&SymbolMention::new(source.clone()))?
                        .within_string()?,
                    DocumentationMarkupKind::ItemName => {
                        M2SemanticToken::new(M2SemanticTokenType::Parameter)
                    }
                    DocumentationMarkupKind::Syntax(token_type) => M2SemanticToken::new(token_type),
                };
                Some(SemanticSpan { source, token })
            })
            .collect()
    }

    /// The token `source_token` of the classifier's document is emitted as.
    fn semantic_span(
        &self,
        source_token: &SourceSemanticToken,
        augments_syntax_tokens: bool,
    ) -> Option<SemanticSpan> {
        let position = source_token.span.range().start;
        let source_text = &self.document.text()[source_token.span.bytes()];
        let binding = source_token
            .is_symbol
            .then(|| {
                self.document
                    .source_symbol_at(source_text, position)
                    .map(|symbol| (symbol, position == symbol.range.start))
            })
            .flatten();
        let emit_syntax = !augments_syntax_tokens
            || source_token
                .syntax_token_type
                .is_some_and(M2SemanticTokenType::emit_with_syntax_highlighting);
        let names_local = binding
            .as_ref()
            .is_some_and(|(binding, _)| binding.is_local());
        self.classify(source_token, binding, emit_syntax)
            .map(|token| {
                if source_token.is_symbol
                    && !names_local
                    && self.is_mutable_global(source_text, position)
                {
                    token.with_modifier(M2SemanticTokenModifier::Mutable)
                } else {
                    token
                }
            })
            .map(|token| SemanticSpan {
                source: source_token.span.clone(),
                token,
            })
    }

    /// Whether the global `name`, written at `position`, is one some package
    /// exports with `exportMutable`: the document's own, one of the
    /// workspace, or an indexed one.
    fn is_mutable_global(&self, name: &str, position: Position) -> bool {
        self.document
            .packages()
            .declarations()
            .iter()
            .flat_map(|package| &package.exports)
            .any(|export| export.access == ExportAccess::Mutable && export.name.name() == name)
            || self.workspace_index.is_mutable_export(name)
            || self.builtins.at(position).is_mutable_global(name)
    }

    fn classify(
        &self,
        source_token: &SourceSemanticToken,
        binding: Option<(BindingView<'_>, bool)>,
        emit_syntax: bool,
    ) -> Option<M2SemanticToken> {
        let source_text = &self.document.text()[source_token.span.bytes()];
        let position = source_token.span.range().start;
        let knowledge = self.builtins.at(position);
        let is_bound = binding.is_some();
        let token = classify_source_semantic_token(
            SourceSemanticTokenContext {
                source_text,
                source_token,
                binding: binding.as_ref().map(|(binding, _)| binding),
                is_declaration: binding.is_some_and(|(_, is_declaration)| is_declaration),
                workspace_token_type: (!source_token.is_condition_value)
                    .then(|| {
                        self.workspace_index
                            .semantic_token_type(source_text, self.uri)
                    })
                    .flatten(),
                emit_syntax,
            },
            &knowledge,
        );
        if !source_token.is_condition_value {
            return token;
        }
        match token {
            Some(token)
                if matches!(
                    token.token_type,
                    M2SemanticTokenType::Function | M2SemanticTokenType::Method
                ) =>
            {
                Some(if is_bound {
                    M2SemanticToken::new(M2SemanticTokenType::Variable)
                } else {
                    M2SemanticToken::unresolved()
                })
            }
            None if source_token.is_expression_symbol => Some(M2SemanticToken::unresolved()),
            token => token,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::DocumentSnapshot;
    use crate::meta::BindingRole;
    use crate::object_registry::ObjectRegistry;
    use crate::semantic_token::{
        local_symbol_semantic_token, M2SemanticToken, M2SemanticTokenModifier,
        M2SemanticTokenModifiers, M2SemanticTokenType,
    };
    use crate::workspace_index::WorkspaceIndex;
    use strum::IntoEnumIterator;

    const OPTION_MODIFIER: u32 = M2SemanticTokenModifier::Option.bit();
    const FILE_MODIFIER: u32 = M2SemanticTokenModifier::File.bit();
    const DECLARATION_MODIFIER: u32 = M2SemanticTokenModifier::Declaration.bit();
    const DEFAULT_LIBRARY_MODIFIER: u32 = M2SemanticTokenModifier::DefaultLibrary.bit();
    const MUTABLE_MODIFIER: u32 = M2SemanticTokenModifier::Mutable.bit();
    const UNRESOLVED_MODIFIER: u32 = M2SemanticTokenModifier::Unresolved.bit();

    fn document(text: &str, builtins: &ObjectRegistry) -> DocumentSnapshot {
        DocumentSnapshot::from_text(text.to_string(), builtins).expect("fixture should parse")
    }

    /// `symbol` and `list` are spelled the same as the grammar's *named* node
    /// kinds for identifiers and list expressions, so a kind-string-only match
    /// classified them as expressions and let a builtin lookup recolor them.
    /// They must read as keywords wherever they appear, exactly like `global`.
    #[test]
    fn keywords_colliding_with_node_kinds_stay_keywords() {
        let text = concat!(
            "protect symbol clearAll\n",
            "protect global clearAll\n",
            "f(symbol foo)\n",
            "a = symbol b\n",
            "squares = for i to 3 list i^2\n",
        );
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        // `augments_syntax_tokens` is off so plain keywords are emitted too;
        // the modifier tokens below are emitted either way.
        let tokens = collect_tokens(&document, &builtins, false);

        for (line, keyword, expected) in [
            (0, "symbol", M2SemanticTokenType::Modifier),
            (1, "global", M2SemanticTokenType::Modifier),
            (2, "symbol", M2SemanticTokenType::Modifier),
            (3, "symbol", M2SemanticTokenType::Modifier),
            (4, "list", M2SemanticTokenType::Keyword),
        ] {
            let source_line = text.lines().nth(line).expect("fixture line");
            let character = source_line.find(keyword).expect("keyword in fixture") as u32;
            assert_eq!(
                token_type_at(&tokens, line as u32, character),
                Some(expected as u32),
                "`{keyword}` on line {line} must classify as {expected:?}"
            );
        }
    }

    /// Collect tokens for a single isolated document — no other workspace files,
    /// so the cross-file classification step contributes nothing.
    fn collect_tokens(
        document: &DocumentSnapshot,
        builtins: &ObjectRegistry,
        augments_syntax_tokens: bool,
    ) -> Vec<SemanticToken> {
        let workspace_index = WorkspaceIndex::default();
        let uri = Url::parse("file:///fixture.m2").expect("valid fixture uri");
        collect_semantic_tokens(
            document,
            builtins,
            &workspace_index,
            &uri,
            &SemanticTokenSettings::default(),
            augments_syntax_tokens,
        )
    }

    fn token_at(tokens: &[SemanticToken], line: u32, character: u32) -> Option<&SemanticToken> {
        let mut token_line = 0;
        let mut token_start = 0;
        for token in tokens {
            if token.delta_line == 0 {
                token_start += token.delta_start;
            } else {
                token_line += token.delta_line;
                token_start = token.delta_start;
            }
            if token_line == line
                && character >= token_start
                && character < token_start + token.length
            {
                return Some(token);
            }
        }
        None
    }

    fn token_type_at(tokens: &[SemanticToken], line: u32, character: u32) -> Option<u32> {
        token_at(tokens, line, character).map(|token| token.token_type)
    }

    #[test]
    fn multi_line_string_token_is_split_per_line() {
        // A raw string spans two lines. The LSP protocol forbids a token from
        // crossing a line boundary, so it must be emitted as one token per line.
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let text = "x := ///alpha\nbeta///\n";
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        let line_lengths = text
            .match_indices('\n')
            .map(|(byte, _)| document.position_for_byte(byte).character)
            .chain(std::iter::once(
                document.position_for_byte(text.len()).character,
            ))
            .collect::<Vec<_>>();

        // Decode the delta-encoded stream to absolute positions and assert every
        // token fits within its own source line.
        let mut line = 0u32;
        let mut start = 0u32;
        for token in &tokens {
            if token.delta_line > 0 {
                line += token.delta_line;
                start = token.delta_start;
            } else {
                start += token.delta_start;
            }
            assert!(
                start + token.length <= line_lengths[line as usize],
                "token at line {line} col {start} len {} runs past the {}-wide line",
                token.length,
                line_lengths[line as usize]
            );
        }

        let string_tokens = tokens
            .iter()
            .filter(|token| token.token_type == M2SemanticTokenType::String as u32)
            .count();
        assert!(
            string_tokens >= 2,
            "the two-line raw string must yield a token per line, got {string_tokens}"
        );
    }

    #[test]
    fn cross_file_type_reference_is_highlighted_as_a_type() {
        // An M2 type defined at the top level of another workspace file
        // highlights as a type where it is referenced, even though it is neither
        // a local binding nor a builtin in the referencing file.
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let workspace_index = WorkspaceIndex::default();
        let defs_uri = Url::parse("file:///defs.m2").expect("valid uri");
        workspace_index.index_file(&defs_uri, "TokenStream = new Type of List\n", &builtins);

        let main_uri = Url::parse("file:///main.m2").expect("valid uri");
        let text = "TokenStream\n";
        let document = document(text, &builtins);
        let tokens = collect_semantic_tokens(
            &document,
            &builtins,
            &workspace_index,
            &main_uri,
            &SemanticTokenSettings::default(),
            false,
        );

        let type_token = M2SemanticTokenType::Type as u32;
        assert!(
            tokens.iter().any(|token| token.token_type == type_token),
            "expected a type token for the cross-file type reference, got {tokens:?}"
        );
    }

    #[test]
    fn cross_file_lookup_excludes_the_current_file() {
        // The current file's own definitions come from its live analysis, not the
        // workspace index, so a self-reference must not be sourced cross-file.
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let workspace_index = WorkspaceIndex::default();
        let main_uri = Url::parse("file:///main.m2").expect("valid uri");
        workspace_index.index_file(&main_uri, "TokenStream = new Type of List\n", &builtins);
        // Excluding main.m2 leaves no other definition, so nothing is contributed.
        assert!(workspace_index
            .semantic_token_type("TokenStream", &main_uri)
            .is_none());
    }

    #[test]
    fn semantic_tokens_classify_parameter_body_references_as_parameters() {
        let text = "f := x -> x";
        let builtins = ObjectRegistry::default();
        let document = document(text, &builtins);

        let tokens = collect_tokens(&document, &builtins, false);

        assert_eq!(
            tokens
                .iter()
                .map(|token| token.token_type)
                .collect::<Vec<_>>(),
            vec![
                M2SemanticTokenType::Function as u32,
                M2SemanticTokenType::Operator as u32,
                M2SemanticTokenType::Parameter as u32,
                M2SemanticTokenType::Operator as u32,
                M2SemanticTokenType::Parameter as u32,
            ]
        );
        assert_eq!(
            tokens[2].token_modifiers_bitset & DECLARATION_MODIFIER,
            DECLARATION_MODIFIER
        );
        assert_eq!(tokens[4].token_modifiers_bitset & DECLARATION_MODIFIER, 0);
    }

    #[test]
    fn local_copy_of_a_parameter_remains_a_variable() {
        let text = "\
matchingMacroClose = (src, bodyStart, outerName) -> (
    nestedNames := {};
    k := bodyStart;
)";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        assert_eq!(
            token_type_at(&tokens, 2, 4),
            Some(M2SemanticTokenType::Variable as u32)
        );
    }

    #[test]
    fn typed_parameter_references_remain_parameters() {
        let text = "f ZZ := x -> x";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);

        let tokens = collect_tokens(&document, &builtins, false);

        assert_eq!(
            tokens
                .iter()
                .map(|token| token.token_type)
                .collect::<Vec<_>>(),
            vec![
                M2SemanticTokenType::DomainType as u32,
                M2SemanticTokenType::Operator as u32,
                M2SemanticTokenType::Parameter as u32,
                M2SemanticTokenType::Operator as u32,
                M2SemanticTokenType::Parameter as u32,
            ]
        );
        assert_eq!(
            tokens[2].token_modifiers_bitset & DECLARATION_MODIFIER,
            DECLARATION_MODIFIER
        );
        assert_eq!(tokens[4].token_modifiers_bitset & DECLARATION_MODIFIER, 0);
    }

    #[test]
    fn semantic_tokens_include_recognized_syntax_tokens() {
        let text = "-- hi\nif x then 42 + 1 else \"no\"\nlocal y";
        let builtins = ObjectRegistry::default();

        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        assert_eq!(
            tokens
                .iter()
                .map(|token| token.token_type)
                .collect::<Vec<_>>(),
            vec![
                M2SemanticTokenType::Comment as u32,
                M2SemanticTokenType::Keyword as u32,
                M2SemanticTokenType::Variable as u32,
                M2SemanticTokenType::Keyword as u32,
                M2SemanticTokenType::Number as u32,
                M2SemanticTokenType::Operator as u32,
                M2SemanticTokenType::Number as u32,
                M2SemanticTokenType::Keyword as u32,
                M2SemanticTokenType::String as u32,
                M2SemanticTokenType::Modifier as u32,
                // `y` in `local y` — quoted, so a symbol rather than a value.
                M2SemanticTokenType::Symbol as u32,
            ]
        );
    }

    #[test]
    fn semantic_tokens_color_wikilinks_as_properties() {
        let text = "x := 1\n-- use [[x]] and [[ideal]]\ny := 2\n";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        let local = token_at(&tokens, 1, 9).expect("local documentation reference is tokenized");
        assert_eq!(local.token_type, M2SemanticTokenType::Property as u32);
        assert_eq!(
            local.token_modifiers_bitset,
            M2SemanticTokenModifier::Documentation.bit()
        );

        let builtin =
            token_at(&tokens, 1, 19).expect("builtin documentation reference is tokenized");
        assert_eq!(builtin.token_type, M2SemanticTokenType::Property as u32);
        assert_eq!(
            builtin.token_modifiers_bitset,
            M2SemanticTokenModifier::Documentation.bit()
        );

        assert_eq!(
            token_at(&tokens, 1, 8).map(|token| token.token_type),
            Some(M2SemanticTokenType::Comment as u32),
            "the wiki-link delimiter remains comment-colored"
        );
    }

    #[test]
    fn semantic_tokens_keep_wikilinks_when_augmenting_syntax() {
        let text = "x := 1\n-- use [[x]]\ny := 2\n";
        let builtins = ObjectRegistry::default();
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, true);

        assert_eq!(
            token_at(&tokens, 1, 9).map(|token| token.token_type),
            Some(M2SemanticTokenType::Property as u32)
        );
        let comment = token_at(&tokens, 1, 8).expect("documentation comment is highlighted");
        assert_eq!(comment.token_type, M2SemanticTokenType::Comment as u32);
        assert_eq!(
            comment.token_modifiers_bitset,
            M2SemanticTokenModifier::Documentation.bit()
        );
    }

    #[test]
    fn semantic_tokens_use_one_property_color_for_complete_comment_code() {
        let text = concat!(
            "-- inspect `instance(t, Comment)`\n",
            "Comment = new Type of HashTable\n",
            "t := 1\n",
            "instance := x -> x\n",
        );
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, true);
        let line = text.lines().next().unwrap();

        assert_eq!(
            token_type_at(&tokens, 0, line.find("instance").unwrap() as u32),
            Some(M2SemanticTokenType::Property as u32)
        );
        assert_eq!(
            token_type_at(&tokens, 0, line.find("(t").unwrap() as u32 + 1),
            Some(M2SemanticTokenType::Property as u32)
        );
        assert_eq!(
            token_type_at(&tokens, 0, line.find("Comment").unwrap() as u32),
            Some(M2SemanticTokenType::Property as u32)
        );
    }

    #[test]
    fn comment_code_does_not_create_document_bindings() {
        let text = "-- example `ghost := x -> x`\ndocumented := 1\nghost\n";
        let builtins = ObjectRegistry::default();
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, true);
        let comment_line = text.lines().next().unwrap();

        assert_eq!(
            token_type_at(&tokens, 0, comment_line.find("ghost").unwrap() as u32),
            Some(M2SemanticTokenType::Property as u32)
        );
        assert_eq!(
            token_type_at(&tokens, 2, 0),
            Some(M2SemanticTokenType::Variable as u32),
            "the isolated snippet assignment must not bind the real document symbol"
        );
    }

    #[test]
    fn comment_code_uses_one_property_color_when_augmenting() {
        let text = "-- example `if true then 1 + 2 else \"x\"`\ndocumented := 1\n";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, true);
        let line = text.lines().next().unwrap();

        for fragment in ["if", "true", "then", "1", "+", "2", "else", "\"x\""] {
            assert_eq!(
                token_type_at(&tokens, 0, line.find(fragment).unwrap() as u32),
                Some(M2SemanticTokenType::Property as u32)
            );
        }
    }

    #[test]
    fn semantic_tokens_classify_binding_qualifiers_as_modifiers() {
        // Each line is a quote: the qualifier is a modifier keyword and the name
        // it quotes is produced as a `Symbol` rather than evaluated.
        let text = "global x\nlocal y\nsymbol z\nthreadLocal w\nthreadVariable q";
        let builtins = ObjectRegistry::default();

        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        assert_eq!(
            tokens
                .iter()
                .map(|token| token.token_type)
                .collect::<Vec<_>>(),
            [[
                M2SemanticTokenType::Modifier as u32,
                M2SemanticTokenType::Symbol as u32,
            ]; 5]
                .concat()
        );
    }

    #[test]
    fn quoted_operators_and_punctuation_are_symbols_too() {
        // Anything can follow a quote, not only names: operators, punctuation,
        // and even a lone opening bracket. M2 hands them all back as symbols —
        // `class(symbol +)` is `Keyword` — so the rule keys on the quote's
        // operand rather than on that operand being an identifier.
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        for quoted in ["+", "==", ",", "(", "{", "#", ".."] {
            let text = format!("a = symbol {quoted}\n");
            let document = document(&text, &builtins);
            let tokens = collect_tokens(&document, &builtins, true);

            assert_eq!(
                token_type_at(&tokens, 0, "a = symbol ".len() as u32),
                Some(M2SemanticTokenType::Symbol as u32),
                "`symbol {quoted}` must classify its operand as a symbol"
            );
        }
    }

    #[test]
    fn quoted_names_are_symbols_whatever_they_are_bound_to() {
        // A quote produces the name as a `Symbol` instead of evaluating it, so
        // the quoted occurrence must outrank the binding it would otherwise
        // resolve to — here a local function.
        let text = concat!(
            "f = x -> x\n",
            "a = symbol f\n",
            "b = global f\n",
            "c = local f\n",
            "d = threadVariable f\n",
            "e = threadLocal f\n",
        );
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, true);

        assert_eq!(
            token_type_at(&tokens, 0, 0),
            Some(M2SemanticTokenType::Function as u32),
            "the binding itself is still a function"
        );
        for line in 1..=5u32 {
            let source_line = text.lines().nth(line as usize).expect("fixture line");
            let character = source_line.rfind('f').expect("quoted name") as u32;
            assert_eq!(
                token_type_at(&tokens, line, character),
                Some(M2SemanticTokenType::Symbol as u32),
                "the quoted name on line {line} must classify as a symbol"
            );
        }
    }

    #[test]
    fn semantic_tokens_classify_debug_keywords() {
        let text = "step 1\nfinish 2";
        let builtins = ObjectRegistry::default();
        let document = document(text, &builtins);

        let tokens = collect_tokens(&document, &builtins, false);

        assert_eq!(
            tokens
                .iter()
                .map(|token| token.token_type)
                .collect::<Vec<_>>(),
            vec![
                M2SemanticTokenType::Keyword as u32,
                M2SemanticTokenType::Number as u32,
                M2SemanticTokenType::Keyword as u32,
                M2SemanticTokenType::Number as u32,
            ]
        );
    }

    #[test]
    fn semantic_tokens_do_not_classify_booleans_as_keywords() {
        let text = "if true then false else true";
        let builtins = ObjectRegistry::default();

        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        assert_eq!(
            tokens
                .iter()
                .map(|token| token.token_type)
                .collect::<Vec<_>>(),
            vec![
                M2SemanticTokenType::Keyword as u32,
                M2SemanticTokenType::Keyword as u32,
                M2SemanticTokenType::Keyword as u32,
            ]
        );
    }

    #[test]
    fn semantic_tokens_do_not_guess_string_roles_from_callable_names() {
        let text = "match(\"a+\", s)\nreplace(\"a+\", \"b\", s)\nseparate(\"a+\", s)";
        let builtins = ObjectRegistry::default();

        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        assert_eq!(
            tokens
                .iter()
                .map(|token| token.token_type)
                .filter(|token_type| *token_type == M2SemanticTokenType::String as u32)
                .collect::<Vec<_>>(),
            vec![
                M2SemanticTokenType::String as u32,
                M2SemanticTokenType::String as u32,
                M2SemanticTokenType::String as u32,
                M2SemanticTokenType::String as u32,
            ]
        );
    }

    #[test]
    fn semantic_tokens_classify_package_argument_strings_as_namespaces() {
        let text = concat!(
            "loadPackage \"Graphs\"\n",
            "installPackage(\"Normaliz\")\n",
            "uninstallPackage \"Foo\"\n",
            "needsPackage \"Core\"\n",
            "export \"thing\"\n",
            "endPackage \"Pkg\"\n",
            "newPackage(\"Pkg\")\n",
            "importFrom(\"Pkg\")\n",
            "exportFrom {\"Pkg\"}\n",
            "print \"ordinary\""
        );
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));

        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        assert_eq!(
            tokens
                .iter()
                .map(|token| token.token_type)
                .filter(|token_type| {
                    *token_type == M2SemanticTokenType::Namespace as u32
                        || *token_type == M2SemanticTokenType::String as u32
                })
                .collect::<Vec<_>>(),
            vec![
                M2SemanticTokenType::Namespace as u32, // loadPackage "Graphs"
                M2SemanticTokenType::Namespace as u32, // installPackage("Normaliz")
                M2SemanticTokenType::Namespace as u32, // uninstallPackage "Foo"
                M2SemanticTokenType::Namespace as u32, // needsPackage "Core"
                M2SemanticTokenType::String as u32,    // export "thing" — a symbol name
                M2SemanticTokenType::Namespace as u32, // endPackage "Pkg"
                M2SemanticTokenType::Namespace as u32, // newPackage("Pkg")
                M2SemanticTokenType::Namespace as u32, // importFrom("Pkg") — first arg
                M2SemanticTokenType::String as u32,    // exportFrom {"Pkg"} — list element
                M2SemanticTokenType::String as u32,    // print "ordinary"
            ]
        );
    }

    #[test]
    fn new_package_binds_its_name_as_a_package() {
        // `newPackage "P"` binds the global `P` to the package it creates, even
        // from inside a function, so later references resolve as namespaces.
        let text = concat!(
            "newPackage(\"Pkg\", Version => \"0.1\")\n",
            "Pkg\n",
            "create = () -> newPackage \"Inner\"\n",
            "Inner\n",
        );
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, true);

        for line in [1, 3] {
            let token = token_at(&tokens, line, 0).expect("package reference is highlighted");
            assert_eq!(token.token_type, M2SemanticTokenType::Namespace as u32);
            assert_eq!(token.token_modifiers_bitset & UNRESOLVED_MODIFIER, 0);
        }
    }

    #[test]
    fn cross_file_package_reference_is_highlighted_as_a_namespace() {
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let workspace_index = WorkspaceIndex::default();
        let package_uri = Url::parse("file:///Pkg.m2").expect("valid uri");
        workspace_index.index_file(&package_uri, "newPackage \"Pkg\"\n", &builtins);

        let main_uri = Url::parse("file:///main.m2").expect("valid uri");
        let document = document("Pkg\n", &builtins);
        let tokens = collect_semantic_tokens(
            &document,
            &builtins,
            &workspace_index,
            &main_uri,
            &SemanticTokenSettings::default(),
            false,
        );

        assert_eq!(
            token_type_at(&tokens, 0, 0),
            Some(M2SemanticTokenType::Namespace as u32)
        );
    }

    #[test]
    fn namespace_argument_selection_skips_muted_parts_but_not_empty_slots() {
        let text = concat!(
            "loadPackage(ignored;\"AfterMuted\")\n",
            "loadPackage(,\"AfterNull\")\n",
            "loadPackage(\"Muted\";)\n",
        );
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        assert_eq!(
            tokens
                .iter()
                .map(|token| token.token_type)
                .filter(|token_type| {
                    *token_type == M2SemanticTokenType::Namespace as u32
                        || *token_type == M2SemanticTokenType::String as u32
                })
                .collect::<Vec<_>>(),
            vec![
                M2SemanticTokenType::Namespace as u32,
                M2SemanticTokenType::String as u32,
                M2SemanticTokenType::String as u32,
            ]
        );
    }

    #[test]
    fn semantic_tokens_keep_exported_symbol_names_as_strings() {
        // `export`/`exportMutable` arguments name symbols defined in this package,
        // not modules. For `importFrom`/`exportFrom` only the first argument (the
        // package) is a namespace; the imported/exported symbol names are not.
        let text = concat!(
            "export {\"installMacro\", \"expandSource\"}\n",
            "exportMutable {\"state\"}\n",
            "importFrom(\"Core\", {\"first\", \"second\"})\n",
            "exportFrom(\"Pkg\", \"only\")\n",
        );
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));

        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        assert_eq!(
            tokens
                .iter()
                .map(|token| token.token_type)
                .filter(|token_type| {
                    *token_type == M2SemanticTokenType::Namespace as u32
                        || *token_type == M2SemanticTokenType::String as u32
                })
                .collect::<Vec<_>>(),
            vec![
                M2SemanticTokenType::String as u32,    // "installMacro"
                M2SemanticTokenType::String as u32,    // "expandSource"
                M2SemanticTokenType::String as u32,    // "state"
                M2SemanticTokenType::Namespace as u32, // importFrom "Core" — the package
                M2SemanticTokenType::String as u32,    // "first" — imported symbol
                M2SemanticTokenType::String as u32,    // "second" — imported symbol
                M2SemanticTokenType::Namespace as u32, // exportFrom "Pkg" — the package
                M2SemanticTokenType::String as u32,    // "only" — exported symbol
            ]
        );
    }

    #[test]
    fn globals_exported_mutable_are_marked_mutable_wherever_they_are_used() {
        let text = concat!(
            "newPackage \"Pkg\"\n",
            "exportMutable {\"counter\"}\n",
            "counter = 1\n",
            "bump = () -> counter = counter + 1\n",
            "shadow = counter -> counter\n",
            "debugLevel = 2\n",
            "protectedName\n",
        );
        let builtins = ObjectRegistry::load(concat!(
            "{\"kind\":\"symbol\",\"name\":\"debugLevel\",\"protected\":false}\n",
            "{\"kind\":\"symbol\",\"name\":\"protectedName\"}\n",
        ));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);
        let is_mutable = |line: u32, column: u32| {
            token_at(&tokens, line, column)
                .unwrap_or_else(|| panic!("a token at {line}:{column}"))
                .token_modifiers_bitset
                & MUTABLE_MODIFIER
                != 0
        };

        assert!(is_mutable(2, 0), "its definition");
        assert!(is_mutable(3, 13), "an assignment in a function");
        assert!(is_mutable(3, 23), "a use");
        assert!(!is_mutable(4, 9), "a parameter of the same name is local");
        assert!(!is_mutable(4, 20), "a use of that parameter");
        assert!(is_mutable(5, 0), "an unprotected library global");
        assert!(!is_mutable(6, 0), "a protected library global");

        let index = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let indexed = DocumentSnapshot::from_text("debugLevel = 1\nideal 1\n".into(), &index)
            .expect("fixture should parse");
        let tokens = collect_tokens(&indexed, &index, false);
        let modifiers = |line| token_at(&tokens, line, 0).unwrap().token_modifiers_bitset;
        assert_ne!(
            modifiers(0) & MUTABLE_MODIFIER,
            0,
            "Core's debugLevel is mutable"
        );
        assert_eq!(
            modifiers(1) & MUTABLE_MODIFIER,
            0,
            "Core's ideal is protected"
        );
    }

    #[test]
    fn export_strings_use_the_roles_of_forward_definitions_and_synonyms() {
        let text = concat!(
            "newPackage \"Pkg\"\n",
            "export {\"f\", \"Alias\" => \"g\", \"marker\"}\n",
            "exportMutable {\"counter\", \"hook\"}\n",
            "f = x -> x\n",
            "g = method()\n",
            "counter = 1\n",
            "hook = x -> x\n",
            "f 1\n",
        );
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        for augments_syntax in [false, true] {
            let tokens = collect_tokens(&document, &builtins, augments_syntax);
            for (line, name, role) in [
                (1, "f", M2SemanticTokenType::Function),
                (1, "Alias", M2SemanticTokenType::Method),
                (1, "g", M2SemanticTokenType::Method),
                (1, "marker", M2SemanticTokenType::Symbol),
            ] {
                let text_line = text.lines().nth(line).unwrap();
                let column = text_line.find(&format!("\"{name}\"")).unwrap() as u32 + 1;
                let token = token_at(&tokens, line as u32, column).unwrap();
                assert_eq!(
                    token.token_type, role as u32,
                    "{name}, augments_syntax={augments_syntax}"
                );
                assert_eq!(token.length, name.len() as u32);
                assert_eq!(token.token_modifiers_bitset & DECLARATION_MODIFIER, 0);
            }
            // `exportMutable` names read as what they name, marked mutable.
            for (name, role) in [
                ("counter", M2SemanticTokenType::Variable),
                ("hook", M2SemanticTokenType::Function),
            ] {
                let column = text.lines().nth(2).unwrap().find(name).unwrap() as u32;
                let token = token_at(&tokens, 2, column).unwrap();
                assert_eq!(token.token_type, role as u32, "{name}");
                assert_ne!(token.token_modifiers_bitset & MUTABLE_MODIFIER, 0, "{name}");
            }
            assert_eq!(
                token_type_at(&tokens, 7, 0),
                Some(M2SemanticTokenType::Function as u32)
            );
        }
    }

    #[test]
    fn exported_functions_defined_in_loaded_files_receive_function_tokens() {
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let text = "newPackage \"Pkg\"\nexport {\"helper\"}\n";
        let document = document(text, &builtins);
        let workspace = WorkspaceIndex::default();
        workspace.index_file(
            &Url::parse("file:///helper.m2").unwrap(),
            "helper = x -> x\n",
            &builtins,
        );
        let tokens = collect_semantic_tokens(
            &document,
            &builtins,
            &workspace,
            &Url::parse("file:///Pkg.m2").unwrap(),
            &SemanticTokenSettings::default(),
            true,
        );
        let token = token_at(&tokens, 1, 9).unwrap();
        assert_eq!(token.token_type, M2SemanticTokenType::Function as u32);
        assert_eq!(token.length, 6);
    }

    #[test]
    fn semantic_tokens_classify_string_hash_keys_as_properties() {
        // The string on the left of `=>` is a hash key (property); the value on
        // the right stays an ordinary string.
        let text = concat!(
            "h = new HashTable from {\n",
            "    \"Quote\" => \"symbol\",\n",
            "    \"GlobalQuote\" => \"global\"\n",
            "}\n",
        );
        let builtins = ObjectRegistry::default();
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        assert_eq!(
            tokens
                .iter()
                .map(|token| token.token_type)
                .filter(|token_type| {
                    *token_type == M2SemanticTokenType::Property as u32
                        || *token_type == M2SemanticTokenType::String as u32
                })
                .collect::<Vec<_>>(),
            vec![
                M2SemanticTokenType::Property as u32, // "Quote"
                M2SemanticTokenType::String as u32,   // "symbol"
                M2SemanticTokenType::Property as u32, // "GlobalQuote"
                M2SemanticTokenType::String as u32,   // "global"
            ]
        );
    }

    #[test]
    fn semantic_tokens_classify_lookup_string_keys_as_properties() {
        // A string on the right of `#` / `#?` is a literal key (property). A
        // symbol key (`h#k`) is evaluated, so it stays an ordinary reference.
        let text = "a = h#\"first\"\nb = h#?\"second\"\nc = \"plain\"\n";
        let builtins = ObjectRegistry::default();
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        assert_eq!(
            tokens
                .iter()
                .map(|token| token.token_type)
                .filter(|token_type| {
                    *token_type == M2SemanticTokenType::Property as u32
                        || *token_type == M2SemanticTokenType::String as u32
                })
                .collect::<Vec<_>>(),
            vec![
                M2SemanticTokenType::Property as u32, // h#"first"
                M2SemanticTokenType::Property as u32, // h#?"second"
                M2SemanticTokenType::String as u32,   // "plain"
            ]
        );
    }

    #[test]
    fn semantic_tokens_classify_dot_access_keys_as_properties() {
        // `name` is also a global variable, yet the quoted global key in `R.name`
        // and `R.?name` must still win as a property over any other role.
        let text = "name = 5\nx = R.name\ny = R.?name\n";
        let builtins = ObjectRegistry::default();
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        let property_count = tokens
            .iter()
            .filter(|token| token.token_type == M2SemanticTokenType::Property as u32)
            .count();
        // Exactly the two dot-access keys, not the `name = 5` binding on line 1.
        assert_eq!(property_count, 2);
    }

    #[test]
    fn string_valued_locals_remain_variables() {
        let text = "s := 1\nt := toString s\nt\n";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);

        let tokens = collect_tokens(&document, &builtins, true);

        assert_eq!(
            tokens
                .iter()
                .filter(|token| token.token_type == M2SemanticTokenType::String as u32)
                .count(),
            0,
            "ordinary locals should not be recolored as string literals from inferred type"
        );
    }

    #[test]
    fn commands_are_functions_with_the_command_modifier() {
        let text = "saveClearAll := clearAll\nclearAll = new Command from { () -> () }\nprotect symbol clearAll";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, true);

        assert_eq!(
            tokens
                .iter()
                .filter(|token| token.token_modifiers_bitset
                    & M2SemanticTokenModifier::Command.bit()
                    != 0)
                .count(),
            3,
            "direct, aliased, and locally rebound Command values are commands"
        );
        let quoted = token_at(&tokens, 2, 15).expect("the quoted clearAll is highlighted");
        assert_eq!(
            quoted.token_type,
            M2SemanticTokenType::Symbol as u32,
            "`symbol clearAll` names the symbol, so the Command binding does not apply"
        );
        let original = token_at(&tokens, 0, 16).expect("indexed clearAll is highlighted");
        assert_eq!(original.token_type, M2SemanticTokenType::Function as u32);
        assert_eq!(
            original.token_modifiers_bitset,
            M2SemanticTokenModifier::Command.bit()
        );
        let protect = token_at(&tokens, 2, 0).expect("protect is highlighted");
        assert_eq!(protect.token_type, M2SemanticTokenType::Function as u32);
        assert_eq!(protect.token_modifiers_bitset, DEFAULT_LIBRARY_MODIFIER);
    }

    #[test]
    fn manipulators_are_functions_with_the_command_modifier() {
        let text = "endl";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, true);
        let token = token_at(&tokens, 0, 0).expect("endl should have a semantic token");

        assert_eq!(token.token_type, M2SemanticTokenType::Function as u32);
        assert_eq!(
            token.token_modifiers_bitset,
            M2SemanticTokenModifier::Command.bit()
        );
    }

    #[test]
    fn scripted_functors_are_functions() {
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let token = builtins
            .get_semantic_token("HH")
            .expect("HH should have builtin metadata");

        assert_eq!(token.token_type, M2SemanticTokenType::Function);
    }

    #[test]
    fn semantic_tokens_preserve_the_file_modifier_without_provenance() {
        let text = "stdio";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, true);
        let token = token_at(&tokens, 0, 0).expect("stdio should have a semantic token");

        assert_eq!(token.token_type, M2SemanticTokenType::Variable as u32);
        assert_eq!(token.token_modifiers_bitset & FILE_MODIFIER, FILE_MODIFIER);
        assert_eq!(token.token_modifiers_bitset, FILE_MODIFIER);
    }

    #[test]
    fn indexed_and_local_noncompiled_values_have_no_provenance_modifier() {
        let text = "true\nlocalValue := true\nlocalValue";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, true);

        let indexed = token_at(&tokens, 0, 0).expect("indexed true should be highlighted");
        assert_eq!(indexed.token_type, M2SemanticTokenType::Variable as u32);
        assert_eq!(indexed.token_modifiers_bitset, 0);

        let declaration = token_at(&tokens, 1, 0).expect("local declaration should be highlighted");
        assert_eq!(declaration.token_modifiers_bitset, DECLARATION_MODIFIER);
        let reference = token_at(&tokens, 2, 0).expect("local reference should be highlighted");
        assert_eq!(reference.token_modifiers_bitset, 0);
    }

    #[test]
    fn imported_index_tokens_use_scoped_role_without_core_provenance() {
        let corpus = concat!(
            "{\"kind\":\"meta\",\"default_loaded\":[\"Core\"]}\n",
            "{\"kind\":\"type\",\"name\":\"MethodFunction\",",
            "\"package\":\"$Core$Core\",\"class\":\"$Core$Type\",",
            "\"parent\":\"$Core$Function\",\"ancestors\":[\"$Core$Function\",\"$Core$Thing\"]}\n",
            "{\"kind\":\"methodFunction\",\"name\":\"pkgFn\",",
            "\"package\":\"$Pkg$Pkg\",\"class\":\"$Core$MethodFunction\",",
            "\"methods\":[{\"domain\":[],\"typicalValue\":null}]}\n",
        );
        let provider = ObjectRegistry::load(corpus);
        let text = "needsPackage \"Pkg\"\npkgFn";
        let document =
            DocumentSnapshot::from_text(text.to_string(), &provider).expect("fixture should parse");
        let workspace_index = WorkspaceIndex::default();
        let uri = Url::parse("file:///fixture.m2").expect("valid fixture uri");

        let tokens = collect_semantic_tokens(
            &document,
            document.object_registry(),
            &workspace_index,
            &uri,
            &SemanticTokenSettings::default(),
            true,
        );
        let token = token_at(&tokens, 1, 0).expect("imported pkgFn should be highlighted");
        assert_eq!(token.token_type, M2SemanticTokenType::Method as u32);
        assert_eq!(token.token_modifiers_bitset & DEFAULT_LIBRARY_MODIFIER, 0);

        let document = DocumentSnapshot::from_text("pkgFn".to_string(), &provider)
            .expect("unimported fixture should parse");
        let tokens = collect_semantic_tokens(
            &document,
            document.object_registry(),
            &workspace_index,
            &uri,
            &SemanticTokenSettings::default(),
            true,
        );
        let token = token_at(&tokens, 0, 0).expect("unassigned pkgFn should be highlighted");
        assert_eq!(token.token_type, M2SemanticTokenType::Variable as u32);
        assert_eq!(token.token_modifiers_bitset, UNRESOLVED_MODIFIER);
    }

    #[test]
    fn parameter_references_use_parameter_semantic_token_type() {
        let symbol = crate::meta::Meta {
            symbol_kind: Some(SymbolKind::VARIABLE),
            binding_role: Some(BindingRole::Parameter),
            ..crate::meta::Meta::default()
        };
        let builtins = ObjectRegistry::default();

        assert_eq!(
            local_symbol_semantic_token(&symbol, &builtins).token_type,
            M2SemanticTokenType::Parameter
        );
    }

    #[test]
    fn builtin_type_tokens_do_not_use_custom_type_modifier() {
        assert_eq!(
            M2SemanticToken::new(M2SemanticTokenType::Type).modifiers,
            M2SemanticTokenModifiers::default()
        );
    }

    #[test]
    fn builtin_function_role_does_not_bake_in_provenance_modifier() {
        assert_eq!(
            M2SemanticToken::new(M2SemanticTokenType::Function).modifiers,
            M2SemanticTokenModifiers::default()
        );
    }

    #[test]
    fn builtin_constructor_like_names_do_not_emit_constructor_modifier() {
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let token = builtins
            .get_semantic_token("toString")
            .expect("toString should have builtin metadata");

        assert_eq!(token.token_type, M2SemanticTokenType::Method);
        assert!(!token.modifiers.contains(M2SemanticTokenModifier::File));
    }

    #[test]
    fn option_keys_classify_by_protected_symbol() {
        // The key of a `k => v` pair is classified by whether it is a protected
        // symbol: `Strategy` (a protected class-`Symbol` builtin) is a nominal
        // enum member, while `myKey` (an unprotected user name) is a field. The
        // value `7` is not a symbol, so it is not classified here.
        let text = "f(x, Strategy => 4, myKey => 7)";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        assert_eq!(
            token_type_at(&tokens, 0, text.find("Strategy").unwrap() as u32),
            Some(M2SemanticTokenType::Symbol as u32)
        );
        assert_eq!(
            token_type_at(&tokens, 0, text.find("myKey").unwrap() as u32),
            Some(M2SemanticTokenType::Property as u32)
        );
    }

    #[test]
    fn option_keys_keep_only_the_option_modifier() {
        let text = "f(x, Strategy => 4, custom => 7)";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        let builtin = token_at(
            &tokens,
            0,
            text.find("Strategy").expect("fixture contains Strategy") as u32,
        )
        .expect("builtin option key is highlighted");
        assert_eq!(builtin.token_modifiers_bitset, OPTION_MODIFIER);

        let custom = token_at(
            &tokens,
            0,
            text.find("custom").expect("fixture contains custom") as u32,
        )
        .expect("custom option key is highlighted");
        assert_eq!(custom.token_modifiers_bitset, OPTION_MODIFIER);
    }

    #[test]
    fn local_types_use_type_tokens_and_binding_sites_are_declarations() {
        let text = "TokenStream = new Type of List\nTokenStream\n";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        let declaration = token_at(&tokens, 0, 0).expect("type declaration is highlighted");
        assert_eq!(declaration.token_type, M2SemanticTokenType::Type as u32);
        assert_eq!(
            declaration.token_modifiers_bitset & DECLARATION_MODIFIER,
            DECLARATION_MODIFIER
        );

        let reference = token_at(&tokens, 1, 0).expect("type reference is highlighted");
        assert_eq!(reference.token_type, M2SemanticTokenType::Type as u32);
        assert_eq!(reference.token_modifiers_bitset & DECLARATION_MODIFIER, 0);
    }

    #[test]
    fn semantic_tokens_never_emit_zero_length_entries() {
        let text = "f ZZ := x -> x";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        assert!(tokens.iter().all(|token| token.length > 0));
    }

    #[test]
    fn algebraic_values_use_general_standard_roles_without_type_modifiers() {
        let text = concat!(
            "R = QQ[x,y]\n",
            "I = ideal(x^2,y)\n",
            "Q = R/I\n",
            "M = Q^2\n",
            "x\n",
        );
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        for (line, token_type) in [
            (0, M2SemanticTokenType::Type),
            (1, M2SemanticTokenType::Variable),
            (2, M2SemanticTokenType::Type),
            (3, M2SemanticTokenType::Variable),
            (4, M2SemanticTokenType::Variable),
        ] {
            let token = token_at(&tokens, line, 0)
                .unwrap_or_else(|| panic!("line {line} should carry an algebraic token"));
            assert_eq!(token.token_type, token_type as u32, "line {line}");
            assert_eq!(
                token.token_modifiers_bitset & !DECLARATION_MODIFIER,
                if line == 4 {
                    M2SemanticTokenModifier::Algebraic.bit()
                } else {
                    0
                }
            );
        }
    }

    #[test]
    fn ring_generators_have_the_algebraic_modifier_on_declarations_and_references() {
        let text = "R = RR[x, y]\nx\ny\nz = x\nz\nx = 7\nx\n";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);
        let generator = M2SemanticTokenModifier::Algebraic.bit();

        for (line, column) in [(0, 7), (0, 10), (1, 0), (2, 0), (3, 4)] {
            let token = token_at(&tokens, line, column).expect("generator is highlighted");
            assert_eq!(
                token.token_modifiers_bitset & generator,
                generator,
                "{line}:{column}"
            );
        }
        for (line, column) in [(0, 0), (3, 0), (4, 0), (6, 0)] {
            let token = token_at(&tokens, line, column).expect("value is highlighted");
            assert_eq!(
                token.token_modifiers_bitset & generator,
                0,
                "{line}:{column}"
            );
        }
    }

    #[test]
    fn indexed_generator_tokens_cover_only_registered_entries() {
        let text = "R = QQ[x_0 .. x_2]\nx_1\nx\nx_3\nx_0 + x_2\n";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);
        let generator = M2SemanticTokenModifier::Algebraic.bit();

        for (line, column) in [(0, 7), (0, 14), (1, 0), (4, 0), (4, 6)] {
            let token = token_at(&tokens, line, column).expect("generator is highlighted");
            assert_eq!(
                token.token_modifiers_bitset & generator,
                generator,
                "{line}:{column}"
            );
            assert_eq!(token.length, 3, "the whole indexed variable is highlighted");
        }
        for line in [2, 3] {
            let token = token_at(&tokens, line, 0).expect("table is highlighted");
            assert_eq!(token.token_modifiers_bitset & generator, 0);
        }
        let standard_tokens = collect_semantic_tokens(
            &document,
            &builtins,
            &WorkspaceIndex::default(),
            &Url::parse("file:///fixture.m2").unwrap(),
            &SemanticTokenSettings {
                standard_only: true,
            },
            false,
        );
        // Under `standardOnly` an algebraic variable is a variable.
        assert!(standard_tokens
            .iter()
            .all(|token| token.token_modifiers_bitset & generator == 0));
        assert_eq!(
            token_at(&standard_tokens, 1, 0).map(|token| token.token_type),
            Some(M2SemanticTokenType::Variable as u32)
        );
    }

    #[test]
    fn document_options_and_values_are_documentation_with_standard_types() {
        use crate::package_documentation::DocumentOption;

        let builtins = ObjectRegistry::default();
        for option in DocumentOption::iter() {
            let text = format!("document {{ {option} => {{\"value\"}} }}\n");
            let document = document(&text, &builtins);
            for augments_syntax in [false, true] {
                let tokens = collect_tokens(&document, &builtins, augments_syntax);
                let tag = token_at(&tokens, 0, "document { ".len() as u32).unwrap();
                assert_eq!(
                    tag.token_type,
                    M2SemanticTokenType::Annotation as u32,
                    "{option}"
                );
                assert_eq!(
                    tag.token_modifiers_bitset,
                    M2SemanticTokenModifier::Documentation.bit(),
                    "{option}"
                );
                let value = token_at(&tokens, 0, text.find("value").unwrap() as u32).unwrap();
                assert_eq!(value.token_type, M2SemanticTokenType::String as u32);
                assert_eq!(
                    value.token_modifiers_bitset,
                    M2SemanticTokenModifier::Documentation.bit()
                );
            }
        }
    }

    #[test]
    fn every_simple_doc_section_is_a_documentation_annotation() {
        use crate::package_documentation::SimpleDocKeyword;

        let text = concat!(
            "doc ///\nNode\n",
            "  Key\n    page\n  Headline\n    title\n",
            "  Usage\n    f x\n  Inputs\n    x:ZZ\n  Outputs\n    :ZZ\n",
            "  Consequences\n    Item\n      result\n",
            "  Description\n    Text\n      prose\n    Example\n      1\n",
            "    CannedExample\n      2\n    Tree\n      page\n    Pre\n      text\n",
            "    Code\n      PARA \"text\"\n",
            "  Synopsis\n    Heading\n      title\n    BaseFunction\n      f\n",
            "  Acknowledgement\n    thanks\n  Contributors\n    authors\n",
            "  References\n    reference\n  Citation\n    citation\n",
            "  Caveat\n    warning\n  SeeAlso\n    page\n  Subnodes\n    page\n",
            "  SourceCode\n    f\n  ExampleFiles\n    data.txt\n///\n",
        );
        let builtins = ObjectRegistry::default();
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, true);
        let expected = M2SemanticTokenModifier::Documentation.bit();
        for keyword in SimpleDocKeyword::iter() {
            let markup = document
                .package_documentation()
                .markup()
                .iter()
                .find(|markup| {
                    matches!(markup.kind, DocumentationMarkupKind::SectionKeyword)
                        && text[markup.bytes.clone()] == keyword.to_string()
                })
                .unwrap_or_else(|| panic!("missing section {keyword}"));
            let position = document.position_for_byte(markup.bytes.start);
            let token = token_at(&tokens, position.line, position.character).unwrap();
            assert_eq!(
                token.token_type,
                M2SemanticTokenType::Annotation as u32,
                "{keyword}"
            );
            assert_eq!(
                token.token_modifiers_bitset & expected,
                expected,
                "{keyword}"
            );
        }
    }

    #[test]
    fn nested_hypertext_and_tex_values_are_text_in_their_specific_styles() {
        let text = concat!(
            "document {Key => \"page\", BOLD {\"bold\", ITALIC \"italic\"},\n",
            "  ///plain {\\tt typed}///, \"escaped {\\\\bf strong}\"}\n",
        );
        let builtins = ObjectRegistry::default();
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, true);
        let markup = M2SemanticTokenModifier::Documentation.bit();
        for (needle, style) in [
            ("bold", M2SemanticTokenModifier::Strong),
            ("italic", M2SemanticTokenModifier::Emphasis),
            ("typed", M2SemanticTokenModifier::Monospace),
            ("strong", M2SemanticTokenModifier::Strong),
        ] {
            let position = document.position_for_byte(text.find(needle).unwrap());
            let token = token_at(&tokens, position.line, position.character).unwrap();
            assert_eq!(
                token.token_type,
                M2SemanticTokenType::String as u32,
                "{needle}"
            );
            assert_eq!(
                token.token_modifiers_bitset,
                markup | style.bit(),
                "{needle}"
            );
        }
    }

    #[test]
    fn hypertext_examples_inside_simple_doc_keep_code_types_and_context() {
        let text = "doc ///\nKey\n  page\nDescription\n  Text\n    @EXAMPLE \"303\"@\n///\n";
        let builtins = ObjectRegistry::default();
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, true);
        let position = document.position_for_byte(text.find("303").unwrap());
        let token = token_at(&tokens, position.line, position.character).unwrap();
        assert_eq!(token.token_type, M2SemanticTokenType::Number as u32);
        assert_eq!(
            token.token_modifiers_bitset,
            M2SemanticTokenModifier::Documentation.bit() | M2SemanticTokenModifier::Example.bit()
        );
    }

    #[test]
    fn documentation_context_reaches_examples_comments_and_tests_without_leaking() {
        let text = concat!(
            "-- attached documentation\n",
            "f = x -> x\n",
            "\n",
            "doc ///\n",
            "Key\n",
            "  f\n",
            "Description\n",
            "  Example\n",
            "    123\n",
            "  CannedExample\n",
            "    456\n",
            "///\n",
            "document {Key => f, EXAMPLE \"789\"}\n",
            "TEST ///101///\n",
            "outside = 202\n",
        );
        let builtins = ObjectRegistry::default();
        let document = document(text, &builtins);
        for augments_syntax in [false, true] {
            let tokens = collect_tokens(&document, &builtins, augments_syntax);
            let documentation = M2SemanticTokenModifier::Documentation.bit();
            let example = M2SemanticTokenModifier::Example.bit();
            let test = M2SemanticTokenModifier::Test.bit();
            assert_eq!(
                token_at(&tokens, 0, 0).unwrap().token_modifiers_bitset,
                documentation
            );
            for (line, column) in [
                (8, 4),
                (10, 4),
                (
                    12,
                    text.lines().nth(12).unwrap().find("789").unwrap() as u32,
                ),
            ] {
                let token = token_at(&tokens, line, column)
                    .unwrap_or_else(|| panic!("missing token at {line}:{column}"));
                assert_eq!(
                    token.token_type,
                    M2SemanticTokenType::Number as u32,
                    "{line}"
                );
                assert_eq!(
                    token.token_modifiers_bitset,
                    documentation | example,
                    "{line}"
                );
            }
            let token = token_at(&tokens, 13, 8).unwrap();
            assert_eq!(token.token_type, M2SemanticTokenType::Number as u32);
            assert_eq!(token.token_modifiers_bitset, test);
            for line in [1, 14] {
                let token = token_at(&tokens, line, 0).unwrap();
                assert_eq!(
                    token.token_modifiers_bitset & (documentation | example | test),
                    0
                );
            }
        }
    }

    #[test]
    fn algebraic_type_names_are_type_tokens() {
        let text = "PolynomialRing\nIdeal\nRingElement\n";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        for line in 0..=2 {
            let token = token_at(&tokens, line, 0).expect("type name should be highlighted");
            assert_eq!(token.token_type, M2SemanticTokenType::Type as u32);
            assert_eq!(token.token_modifiers_bitset, 0);
        }
    }

    #[test]
    fn legend_lists_every_variant_in_declaration_order() {
        let legend = M2SemanticToken::legend();

        assert_eq!(
            legend.token_types,
            M2SemanticTokenType::iter()
                .map(M2SemanticTokenType::lsp_type)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            legend.token_modifiers,
            M2SemanticTokenModifier::iter()
                .map(M2SemanticTokenModifier::lsp_modifier)
                .collect::<Vec<_>>()
        );
        for (index, token_type) in M2SemanticTokenType::iter().enumerate() {
            assert_eq!(token_type as usize, index, "{token_type:?}");
        }
        for (index, modifier) in M2SemanticTokenModifier::iter().enumerate() {
            assert_eq!(modifier.bit(), 1 << index, "{modifier:?}");
        }
    }

    #[test]
    fn standard_only_sends_only_standard_modifiers() {
        let standard_only = SemanticTokenSettings {
            standard_only: true,
        };
        let legend = M2SemanticToken::legend();
        let token = M2SemanticToken::new(M2SemanticTokenType::Function)
            .with_modifier(M2SemanticTokenModifier::Command)
            .with_modifier(M2SemanticTokenModifier::DefaultLibrary)
            .with_modifier(M2SemanticTokenModifier::Unresolved);

        assert_eq!(
            token.encode(&SemanticTokenSettings::default()),
            (
                M2SemanticTokenType::Function as u32,
                DEFAULT_LIBRARY_MODIFIER
                    | UNRESOLVED_MODIFIER
                    | M2SemanticTokenModifier::Command.bit()
            )
        );
        let (token_type, modifiers) = token.encode(&standard_only);
        assert_eq!(
            legend.token_types[token_type as usize],
            SemanticTokenType::FUNCTION
        );
        assert_eq!(
            modifiers, DEFAULT_LIBRARY_MODIFIER,
            "command and unresolved are not standard"
        );
        let (symbol_type, _) =
            M2SemanticToken::new(M2SemanticTokenType::Symbol).encode(&standard_only);
        assert_eq!(
            legend.token_types[symbol_type as usize],
            SemanticTokenType::ENUM_MEMBER
        );
        for token_type in M2SemanticTokenType::iter() {
            assert_eq!(
                M2SemanticToken::new(token_type).encode(&standard_only).0,
                token_type as u32
            );
        }
    }

    #[test]
    fn only_primary_core_compiled_functions_are_default_library() {
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let mut compiled_functions = 0;

        for record in builtins.records_by_precedence() {
            let name = record.name.name();
            let token = builtins
                .get_semantic_token(name)
                .unwrap_or_else(|| panic!("{name} should have semantic metadata"));
            let is_core_compiled_function = record.class.name() == "CompiledFunction"
                && builtins
                    .object(&record.package)
                    .is_some_and(|package| package.name.name() == "Core");

            if is_core_compiled_function {
                compiled_functions += 1;
                assert_eq!(token.token_type, M2SemanticTokenType::Function);
                assert_eq!(
                    token.modifiers,
                    M2SemanticTokenModifiers::default()
                        .with(M2SemanticTokenModifier::DefaultLibrary),
                    "{name}"
                );
            } else {
                assert!(
                    !token
                        .modifiers
                        .contains(M2SemanticTokenModifier::DefaultLibrary),
                    "{name}"
                );
            }
        }

        assert!(compiled_functions > 0);
    }

    #[test]
    fn library_callables_are_classified_by_runtime_class() {
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let command = M2SemanticToken::new(M2SemanticTokenType::Function)
            .with_modifier(M2SemanticTokenModifier::Command);
        for (name, expected) in [
            ("source", M2SemanticToken::new(M2SemanticTokenType::Method)),
            ("flatten", M2SemanticToken::new(M2SemanticTokenType::Method)),
            ("gcd", M2SemanticToken::new(M2SemanticTokenType::Method)),
            ("det", M2SemanticToken::new(M2SemanticTokenType::Method)),
            ("print", M2SemanticToken::new(M2SemanticTokenType::Function)),
            ("lines", M2SemanticToken::new(M2SemanticTokenType::Function)),
            ("id", M2SemanticToken::new(M2SemanticTokenType::Function)),
            ("Ext", M2SemanticToken::new(M2SemanticTokenType::Function)),
            ("exit", command),
            ("endl", command),
        ] {
            assert_eq!(builtins.get_semantic_token(name), Some(expected), "{name}");
        }
    }

    #[test]
    fn method_installation_types_emit_type_parameters() {
        let text = "Ring Element := x -> x";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));

        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);
        let token_types: Vec<u32> = tokens.iter().map(|t| t.token_type).collect();

        let type_param = M2SemanticTokenType::DomainType as u32;
        assert!(
            token_types.contains(&type_param),
            "Ring in method installation should be TypeParameter, got {:?}",
            token_types
        );
    }

    #[test]
    fn explicit_method_codomain_outranks_option_field_classification() {
        let text = "\
p = method(TypicalValue => List)
p(ZZ) := Array => x -> [x]
";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));

        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        assert_eq!(
            token_type_at(&tokens, 1, 9),
            Some(M2SemanticTokenType::Annotation as u32),
            "the explicit Array codomain is an annotation, not an option field"
        );
        assert_eq!(
            token_type_at(&tokens, 1, 2),
            Some(M2SemanticTokenType::DomainType as u32),
            "the domain stays a type parameter"
        );
    }
}
