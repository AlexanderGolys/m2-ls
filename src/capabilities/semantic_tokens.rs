//! LSP encoding of semantic-token candidates registered in document analysis.

use tower_lsp::lsp_types::*;

use crate::analysis::BindingView;
use crate::document::DocumentSnapshot;
use crate::documentation::DocumentationSnippet;
use crate::object_registry::ObjectRegistry;
use crate::semantic_token::{
    classify_source_semantic_token, M2SemanticToken, M2SemanticTokenType, SourceSemanticToken,
    SourceSemanticTokenContext,
};
use crate::source::{DocumentSpan, SourceNavigation};
use crate::workspace_index::WorkspaceDefinitionKnowledge;

pub fn collect_semantic_tokens(
    document: &DocumentSnapshot,
    builtins: &ObjectRegistry,
    workspace_index: &(impl WorkspaceDefinitionKnowledge + ?Sized),
    uri: &Url,
    augments_syntax_tokens: bool,
) -> Vec<SemanticToken> {
    let classifier = SemanticTokenClassifier {
        document,
        builtins,
        workspace_index,
        uri,
    };
    let mut emitter = SemanticTokenEmitter::new(document);

    for source_token in document.analysis().source_semantic_tokens() {
        if emitter.emit_documentation_container_tokens(source_token, augments_syntax_tokens) {
            continue;
        }

        let position = source_token.span.range().start;
        let source_text = &document.text()[source_token.span.bytes()];
        let binding = source_token
            .is_symbol
            .then(|| {
                document
                    .source_symbol_at(source_text, position)
                    .map(|symbol| (symbol, position == symbol.range.start))
            })
            .flatten();
        let emit_syntax = !augments_syntax_tokens
            || source_token
                .syntax_token_type
                .is_some_and(M2SemanticTokenType::emit_with_syntax_highlighting);

        if let Some(token) = classifier.classify(source_token, binding, emit_syntax) {
            emitter.push(SemanticSpan {
                source: source_token.span.clone(),
                token,
            });
        }
    }

    emitter.finish()
}

struct SemanticTokenEmitter<'a> {
    document: &'a DocumentSnapshot,
    tokens: Vec<SemanticToken>,
    previous: Position,
}

impl<'a> SemanticTokenEmitter<'a> {
    fn new(document: &'a DocumentSnapshot) -> Self {
        Self {
            document,
            tokens: Vec::new(),
            previous: pos!(),
        }
    }

    fn emit_documentation_container_tokens(
        &mut self,
        source_token: &SourceSemanticToken,
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

        let mut spans = self
            .document
            .documentation_snippets()
            .iter()
            .filter(|snippet| {
                let (start, end) = snippet.byte_span();
                let source_bytes = source_token.span.bytes();
                source_bytes.start <= start && end <= source_bytes.end
            })
            .map(|snippet| documentation_snippet_semantic_span(self.document, snippet))
            .collect::<Vec<_>>();
        if spans.is_empty() {
            return false;
        }
        spans.sort_by_key(|span| {
            let bytes = span.source.bytes();
            (bytes.start, bytes.end)
        });

        let emit_base = !augments_syntax_tokens || base_type.emit_with_syntax_highlighting();
        let source_bytes = source_token.span.bytes();
        let mut cursor = source_bytes.start;
        let mut emitted = false;

        for span in spans {
            let span_bytes = span.source.bytes();
            if span_bytes.start < cursor {
                continue;
            }
            if emit_base && cursor < span_bytes.start {
                emitted |= self.push(SemanticSpan {
                    source: self.document.span_for_bytes(cursor..span_bytes.start),
                    token: M2SemanticToken::new(base_type),
                });
            }
            cursor = span_bytes.end;
            emitted |= self.push(span);
        }

        if emitted && emit_base && cursor < source_bytes.end {
            emitted |= self.push(SemanticSpan {
                source: self.document.span_for_bytes(cursor..source_bytes.end),
                token: M2SemanticToken::new(base_type),
            });
        }

        emitted
    }

    fn push(&mut self, span: SemanticSpan) -> bool {
        let mut emitted = false;
        for range in self.document.visible_ranges(&span.source) {
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
                token_type: span.token.token_type as u32,
                token_modifiers_bitset: span.token.modifiers.bits(),
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
                Some(M2SemanticToken::new(if is_bound {
                    M2SemanticTokenType::Variable
                } else {
                    M2SemanticTokenType::EnumMember
                }))
            }
            None if source_token.is_expression_symbol => {
                Some(M2SemanticToken::new(M2SemanticTokenType::EnumMember))
            }
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
        local_symbol_semantic_token, M2SemanticToken, M2SemanticTokenModifier, M2SemanticTokenType,
        LEGEND_MODIFIERS,
    };
    use crate::workspace_index::WorkspaceIndex;

    const OPTION_MODIFIER: u32 = M2SemanticTokenModifier::Option.bit();
    const COMMAND_MODIFIER: u32 = M2SemanticTokenModifier::Command.bit();
    const FILE_MODIFIER: u32 = M2SemanticTokenModifier::File.bit();
    const DECLARATION_MODIFIER: u32 = M2SemanticTokenModifier::Declaration.bit();
    const BUILTIN_MODIFIER: u32 = M2SemanticTokenModifier::Builtin.bit();

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
    fn cross_file_class_reference_is_highlighted_as_a_class() {
        // An M2 class defined at the top level of another workspace file
        // highlights as CLASS where it is referenced, even though it is neither
        // a local binding nor a builtin in the referencing file.
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let workspace_index = WorkspaceIndex::default();
        let defs_uri = Url::parse("file:///defs.m2").expect("valid uri");
        workspace_index.index_file(&defs_uri, "TokenStream = new Type of List\n", &builtins);

        let main_uri = Url::parse("file:///main.m2").expect("valid uri");
        let text = "TokenStream\n";
        let document = document(text, &builtins);
        let tokens =
            collect_semantic_tokens(&document, &builtins, &workspace_index, &main_uri, false);

        let class_token = M2SemanticTokenType::Class as u32;
        assert!(
            tokens.iter().any(|token| token.token_type == class_token),
            "expected a CLASS token for the cross-file class reference, got {tokens:?}"
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
                M2SemanticTokenType::TypeParameter as u32,
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
                M2SemanticTokenType::EnumMember as u32,
                M2SemanticTokenType::Keyword as u32,
                M2SemanticTokenType::Number as u32,
                M2SemanticTokenType::Operator as u32,
                M2SemanticTokenType::Number as u32,
                M2SemanticTokenType::Keyword as u32,
                M2SemanticTokenType::String as u32,
                M2SemanticTokenType::Modifier as u32,
                // `y` in `local y` — quoted, so a symbol rather than a value.
                M2SemanticTokenType::EnumMember as u32,
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
        assert_eq!(local.token_modifiers_bitset, 0);

        let builtin =
            token_at(&tokens, 1, 19).expect("builtin documentation reference is tokenized");
        assert_eq!(builtin.token_type, M2SemanticTokenType::Property as u32);
        assert_eq!(builtin.token_modifiers_bitset, 0);

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
        assert!(
            token_at(&tokens, 1, 8).is_none(),
            "syntax highlighting owns the surrounding comment"
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
            Some(M2SemanticTokenType::EnumMember as u32),
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
                M2SemanticTokenType::EnumMember as u32,
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
                Some(M2SemanticTokenType::EnumMember as u32),
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
                Some(M2SemanticTokenType::EnumMember as u32),
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
    fn commands_keep_the_command_modifier_without_provenance() {
        let text = "saveClearAll := clearAll\nclearAll = new Command from { () -> () }\nprotect symbol clearAll";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, true);

        assert_eq!(
            tokens
                .iter()
                .filter(|token| {
                    token.token_type == M2SemanticTokenType::Operator as u32
                        && token.token_modifiers_bitset & COMMAND_MODIFIER == COMMAND_MODIFIER
                })
                .count(),
            0,
            "Command values should no longer use operator+command"
        );
        assert_eq!(
            tokens
                .iter()
                .filter(|token| {
                    token.token_type == M2SemanticTokenType::Function as u32
                        && token.token_modifiers_bitset & COMMAND_MODIFIER == COMMAND_MODIFIER
                })
                .count(),
            3,
            "direct, aliased, and locally rebound Command values stay function+command"
        );
        let quoted = token_at(&tokens, 2, 15).expect("the quoted clearAll is highlighted");
        assert_eq!(
            quoted.token_type,
            M2SemanticTokenType::EnumMember as u32,
            "`symbol clearAll` names the symbol, so the Command binding does not apply"
        );
        let original = token_at(&tokens, 0, 16).expect("indexed clearAll is highlighted");
        assert_eq!(original.token_type, M2SemanticTokenType::Function as u32);
        assert_eq!(original.token_modifiers_bitset, COMMAND_MODIFIER);
        let protect = token_at(&tokens, 2, 0).expect("protect is highlighted");
        assert_eq!(protect.token_type, M2SemanticTokenType::Function as u32);
        assert_eq!(protect.token_modifiers_bitset, BUILTIN_MODIFIER);
    }

    #[test]
    fn semantic_tokens_merge_manipulators_into_command_modifier() {
        let text = "endl";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, true);
        let token = token_at(&tokens, 0, 0).expect("endl should have a semantic token");

        assert_eq!(token.token_type, M2SemanticTokenType::Operator as u32);
        assert_eq!(
            token.token_modifiers_bitset & COMMAND_MODIFIER,
            COMMAND_MODIFIER,
            "M2 Manipulator values share the command palette role"
        );
        assert_eq!(token.token_modifiers_bitset, COMMAND_MODIFIER);
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
            true,
        );
        let token = token_at(&tokens, 1, 0).expect("imported pkgFn should be highlighted");
        assert_eq!(token.token_type, M2SemanticTokenType::Method as u32);
        assert_eq!(token.token_modifiers_bitset & BUILTIN_MODIFIER, 0);

        let document = DocumentSnapshot::from_text("pkgFn".to_string(), &provider)
            .expect("unimported fixture should parse");
        let tokens = collect_semantic_tokens(
            &document,
            document.object_registry(),
            &workspace_index,
            &uri,
            true,
        );
        let token = token_at(&tokens, 0, 0).expect("unassigned pkgFn should be highlighted");
        assert_eq!(token.token_type, M2SemanticTokenType::EnumMember as u32);
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
            M2SemanticToken::new(M2SemanticTokenType::Type)
                .modifiers
                .bits(),
            0
        );
    }

    #[test]
    fn builtin_class_tokens_do_not_use_custom_type_modifier() {
        assert_eq!(
            M2SemanticToken::new(M2SemanticTokenType::Class)
                .modifiers
                .bits(),
            0
        );
    }

    #[test]
    fn builtin_function_role_does_not_bake_in_provenance_modifier() {
        assert_eq!(
            M2SemanticToken::new(M2SemanticTokenType::Function)
                .modifiers
                .bits(),
            0
        );
    }

    #[test]
    fn builtin_constructor_like_names_do_not_emit_constructor_modifier() {
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let token = builtins
            .get_semantic_token("toString")
            .expect("toString should have builtin metadata");

        assert_eq!(token.token_type, M2SemanticTokenType::Method);
        assert_eq!(
            token.modifiers.bits() & (COMMAND_MODIFIER | FILE_MODIFIER),
            0
        );
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
            Some(M2SemanticTokenType::EnumMember as u32)
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
    fn local_classes_use_class_tokens_and_binding_sites_are_declarations() {
        let text = "TokenStream = new Type of List\nTokenStream\n";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        let declaration = token_at(&tokens, 0, 0).expect("class declaration is highlighted");
        assert_eq!(declaration.token_type, M2SemanticTokenType::Class as u32);
        assert_eq!(
            declaration.token_modifiers_bitset & DECLARATION_MODIFIER,
            DECLARATION_MODIFIER
        );

        let reference = token_at(&tokens, 1, 0).expect("class reference is highlighted");
        assert_eq!(reference.token_type, M2SemanticTokenType::Class as u32);
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
    fn algebraic_values_use_general_standard_roles_without_class_modifiers() {
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
            assert_eq!(token.token_modifiers_bitset & !DECLARATION_MODIFIER, 0);
        }
    }

    #[test]
    fn algebraic_class_names_remain_standard_class_tokens() {
        let text = "PolynomialRing\nIdeal\nRingElement\n";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);

        for line in 0..=2 {
            let token = token_at(&tokens, line, 0).expect("class name should be highlighted");
            assert_eq!(token.token_type, M2SemanticTokenType::Class as u32);
            assert_eq!(token.token_modifiers_bitset, 0);
        }
    }

    #[test]
    fn semantic_token_modifier_bits_match_legend_order() {
        assert_eq!(
            LEGEND_MODIFIERS,
            &[
                SemanticTokenModifier::new("option"),
                SemanticTokenModifier::new("command"),
                SemanticTokenModifier::new("file"),
                SemanticTokenModifier::DECLARATION,
                SemanticTokenModifier::new("builtin"),
            ]
        );
        assert_eq!(OPTION_MODIFIER, 1 << 0);
        assert_eq!(COMMAND_MODIFIER, 1 << 1);
        assert_eq!(FILE_MODIFIER, 1 << 2);
        assert_eq!(DECLARATION_MODIFIER, 1 << 3);
        assert_eq!(BUILTIN_MODIFIER, 1 << 4);
    }

    #[test]
    fn only_primary_core_compiled_functions_are_builtin() {
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
                assert_eq!(token.modifiers.bits(), BUILTIN_MODIFIER, "{name}");
            } else {
                assert_eq!(token.modifiers.bits() & BUILTIN_MODIFIER, 0, "{name}");
            }
        }

        assert!(compiled_functions > 0);
    }

    #[test]
    fn method_installation_types_emit_type_parameters() {
        let text = "Ring Element := x -> x";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));

        let document = document(text, &builtins);
        let tokens = collect_tokens(&document, &builtins, false);
        let token_types: Vec<u32> = tokens.iter().map(|t| t.token_type).collect();

        let type_param = M2SemanticTokenType::TypeParameter as u32;
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
            Some(M2SemanticTokenType::TypeParameter as u32),
            "the explicit Array codomain is a type parameter, not an option field"
        );
    }
}
