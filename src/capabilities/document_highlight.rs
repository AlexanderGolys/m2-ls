//! In-document highlighting for resolved symbols and compound-statement words.

use m2_syn::nodes::{Expr, ExprControl, ExprLambda};
use m2_syn::{Lex, Span, Spanned, Token, TokenStream, TokenTree};

use crate::analysis::{ControlTransfer, ControlTransferTarget, LoopStatement};
use crate::capabilities::navigation::{reference_ranges_resolved, unbound_reference_ranges};
use crate::document::DocumentSnapshot;
use crate::object_registry::ObjectName;
use crate::source::{SourceNavigation, SyntaxSource};
use crate::syntax::{BindingReferences, StatementKeywords};
use crate::typesystem::TypeKnowledge;
use tower_lsp::lsp_types::{
    DocumentHighlight, DocumentHighlightKind, DocumentHighlightOptions, OneOf, Position,
};

/// Advertises document-highlight support in the server initialization response.
pub fn document_highlight_provider_capability() -> Option<OneOf<bool, DocumentHighlightOptions>> {
    Some(OneOf::Left(true))
}

/// Highlight occurrences related to the cursor. A symbol under the cursor lights
/// up all its in-file references; a keyword lights up its compound statement's
/// keyword sequence; a semicolon highlights as the character preceding it.
pub fn document_highlights(
    document: &DocumentSnapshot,
    position: Position,
    builtins: &(impl TypeKnowledge + ?Sized),
) -> Option<Vec<DocumentHighlight>> {
    let position = position_preceding_semicolon(document, position).unwrap_or(position);
    if document
        .symbol_occurrence_at(position)
        .is_some_and(|(name, _)| matches!(name, "null" | "true" | "false"))
    {
        return None;
    }
    if let Some(highlights) = quoted_name_highlights(document, position) {
        return Some(highlights);
    }
    if let Some(highlights) = package_reference_highlights(document, position, builtins) {
        return Some(highlights);
    }
    if let Some(highlights) = indexed_ring_generator_highlights(document, position) {
        return Some(highlights);
    }
    if let Some(highlights) = symbol_reference_highlights(document, position) {
        return Some(highlights);
    }
    if let Some(highlights) = unbound_symbol_highlights(document, position, builtins) {
        return Some(highlights);
    }
    if let Some(highlights) = control_transfer_highlights(document, position, builtins) {
        return Some(highlights);
    }
    if let Some(highlights) = control_owner_highlights(document, position, builtins) {
        return Some(highlights);
    }
    keyword_sequence_highlights(document, position)
}

/// A quoted name denotes the `Symbol` object of its spelling, so the cursor on
/// one lights up the quotes of the same name rather than any binding of it.
fn quoted_name_highlights(
    document: &DocumentSnapshot,
    position: Position,
) -> Option<Vec<DocumentHighlight>> {
    let quoted = document.quoted_names();
    let (name, _) = quoted
        .iter()
        .find(|(_, range)| range.start <= position && position <= range.end)?;
    Some(
        quoted
            .iter()
            .filter(|(quoted_name, _)| quoted_name == name)
            .map(|(_, range)| DocumentHighlight {
                range: *range,
                kind: Some(DocumentHighlightKind::READ),
            })
            .collect(),
    )
}

/// Package-name arguments refer to packages even before they are loaded.
/// Ordinary strings and locally shadowed symbols do not share that identity.
fn package_reference_highlights(
    document: &DocumentSnapshot,
    position: Position,
    knowledge: &(impl TypeKnowledge + ?Sized),
) -> Option<Vec<DocumentHighlight>> {
    let name = document
        .package_mentions()
        .iter()
        .find(|mention| mention.contains(position))
        .map(|mention| mention.name(document.text()))
        .or_else(|| {
            let (name, _) = document.symbol_occurrence_at(position)?;
            let is_package = document.source_binding_at(name, position).map_or_else(
                || {
                    document
                        .package_mentions()
                        .iter()
                        .any(|mention| mention.name(document.text()) == name)
                        || knowledge
                            .get_record(&ObjectName::new(name))
                            .is_some_and(|record| {
                                knowledge.has_type_role(
                                    &record.class,
                                    crate::typesystem::TypeRole::Package,
                                )
                            })
                },
                |binding| {
                    binding.state.presentation_kind == tower_lsp::lsp_types::SymbolKind::NAMESPACE
                },
            );
            is_package.then_some(name)
        })?;
    let mut ranges = document
        .package_mentions()
        .iter()
        .filter(|mention| mention.name(document.text()) == name)
        .map(|mention| mention.range())
        .collect::<Vec<_>>();
    let mut references = BindingReferences::default();
    document.syntax().visit(&mut references);
    ranges.extend(
        references
            .0
            .into_iter()
            .filter(|symbol| symbol.text() == name)
            .filter_map(|symbol| document.range_for_syntax(symbol))
            .filter(|range| {
                document
                    .source_binding_at(name, range.start)
                    .is_none_or(|binding| {
                        binding.state.presentation_kind
                            == tower_lsp::lsp_types::SymbolKind::NAMESPACE
                    })
            }),
    );
    ranges.extend(
        document
            .symbol_mentions()
            .iter()
            .filter(|mention| mention.name(document.text()) == name)
            .filter(|mention| {
                document.mention_symbol(mention).is_none_or(|binding| {
                    binding.state.presentation_kind == tower_lsp::lsp_types::SymbolKind::NAMESPACE
                })
            })
            .map(|mention| mention.range()),
    );
    ranges.sort_by_key(|range| (range.start, range.end));
    ranges.dedup();
    (!ranges.is_empty()).then(|| {
        ranges
            .into_iter()
            .map(|range| {
                let declaration = document
                    .source_binding_at(name, range.start)
                    .is_some_and(|binding| binding.range == range);
                DocumentHighlight {
                    range,
                    kind: Some(if declaration {
                        DocumentHighlightKind::WRITE
                    } else {
                        DocumentHighlightKind::READ
                    }),
                }
            })
            .collect()
    })
}

/// Highlight the generator an indexed access such as `z_1` selects: its
/// declaration in the ring constructor, written to, and every access to the
/// same index, read.
fn indexed_ring_generator_highlights(
    document: &DocumentSnapshot,
    position: Position,
) -> Option<Vec<DocumentHighlight>> {
    let analysis = document.analysis();
    let references = analysis.indexed_ring_generator_references(position, document, true)?;
    let declarations = analysis.indexed_ring_generator_references(position, document, false)?;
    Some(
        references
            .into_iter()
            .map(|range| DocumentHighlight {
                range,
                kind: Some(if declarations.contains(&range) {
                    DocumentHighlightKind::READ
                } else {
                    DocumentHighlightKind::WRITE
                }),
            })
            .collect(),
    )
}

/// The position of the character before the semicolon under the cursor, so
/// resting on the `;` of `x := y;` highlights what resting on `y` would.
fn position_preceding_semicolon(
    document: &DocumentSnapshot,
    position: Position,
) -> Option<Position> {
    /// Whether `stream` holds a semicolon token starting at the document
    /// byte `byte`.
    fn has_semicolon_at(stream: TokenStream, document: &DocumentSnapshot, byte: usize) -> bool {
        stream.into_iter().any(|token| match token {
            TokenTree::Group(group) => has_semicolon_at(group.stream().clone(), document, byte),
            TokenTree::Punct(punct) if punct.text() == <Token![;]>::SPELLING => document
                .syntax_bytes(&punct)
                .is_some_and(|bytes| bytes.start == byte),
            _ => false,
        })
    }
    let byte = document.byte_for_position(position)?;
    let start = document
        .syntax()
        .cells_at(byte)
        .any(|cell| has_semicolon_at(cell.to_token_stream(), document, byte))
        .then_some(byte)?;
    Some(document.position_for_byte(start.checked_sub(1)?))
}

/// Highlight an otherwise unresolved symbol by spelling alone, without
/// requiring a source binding or an index record. Grammar/index keywords are
/// excluded, and locally bound occurrences with the same spelling are a
/// different symbol, so they stay out of this fallback set.
fn unbound_symbol_highlights(
    document: &DocumentSnapshot,
    position: Position,
    builtins: &(impl TypeKnowledge + ?Sized),
) -> Option<Vec<DocumentHighlight>> {
    let (name, _) = document.symbol_occurrence_at(position)?;
    if document.source_binding_at(name, position).is_some() {
        return None;
    }

    if builtins
        .get_record(&ObjectName::new(name))
        .is_some_and(|record| builtins.is_subtype(&record.class, &ObjectName::new("Keyword")))
    {
        return None;
    }

    let ranges = unbound_reference_ranges(document, name);
    (!ranges.is_empty()).then(|| {
        ranges
            .into_iter()
            .map(|range| DocumentHighlight {
                range,
                kind: Some(DocumentHighlightKind::READ),
            })
            .collect()
    })
}

/// Highlight every in-file occurrence of the symbol under the cursor — the same
/// scope-aware set a references request resolves. `None` when the cursor is not
/// on a resolvable symbol, so the caller falls back to keyword highlighting.
fn symbol_reference_highlights(
    document: &DocumentSnapshot,
    position: Position,
) -> Option<Vec<DocumentHighlight>> {
    let target = document.target_symbol_at(position)?;
    let declaration = target.symbol.range;
    let ranges = reference_ranges_resolved(target, document, true);
    if ranges.is_empty() {
        return None;
    }

    Some(
        ranges
            .into_iter()
            .map(|range| {
                // The binding site is a write; every other occurrence reads it.
                let kind = if range == declaration {
                    DocumentHighlightKind::WRITE
                } else {
                    DocumentHighlightKind::READ
                };
                DocumentHighlight {
                    range,
                    kind: Some(kind),
                }
            })
            .collect(),
    )
}

/// The control transfers of the cells at the cursor, with the construct
/// receiving each, and the cursor byte.
fn control_transfers_at<'a>(
    document: &'a DocumentSnapshot,
    position: Position,
    knowledge: &(impl TypeKnowledge + ?Sized),
) -> Option<(Vec<ControlTransfer<'a>>, usize)> {
    let byte = document.byte_for_position(position)?;
    let transfers = document.analysis().control_transfers(
        document.syntax().cells_at(byte),
        document,
        knowledge,
    );
    Some((transfers, byte))
}

/// Highlight a `return`, `break`, or `continue` under the cursor with the
/// construct receiving it and that construct's other transfers.
fn control_transfer_highlights(
    document: &DocumentSnapshot,
    position: Position,
    knowledge: &(impl TypeKnowledge + ?Sized),
) -> Option<Vec<DocumentHighlight>> {
    let (transfers, byte) = control_transfers_at(document, position, knowledge)?;
    let transfer = transfers
        .iter()
        .rev()
        .find(|transfer| spans_byte(document, transfer_keyword(transfer.transfer), byte))?;
    let target = transfer.target?;
    let mut keywords = match target {
        ControlTransferTarget::LoopCallback { callable, .. } => {
            callable.try_span().into_iter().collect()
        }
        ControlTransferTarget::ListLoop(statement) | ControlTransferTarget::DoLoop(statement) => {
            match statement {
                LoopStatement::For(node) => node.keywords(),
                LoopStatement::While(node) => node.keywords(),
            }
        }
        ControlTransferTarget::Function(function) => {
            lambda_operator(function).into_iter().collect()
        }
    };
    keywords.extend(owned_transfer_keywords(
        document,
        &transfers,
        target.owner(),
    ));
    Some(highlights_for_spans(keywords, document))
}

/// Highlight the arrow of a function or the keywords of a loop under the
/// cursor with the transfers it receives.
fn control_owner_highlights(
    document: &DocumentSnapshot,
    position: Position,
    knowledge: &(impl TypeKnowledge + ?Sized),
) -> Option<Vec<DocumentHighlight>> {
    let enclosing = document.enclosing_expressions(position);
    let owner = enclosing
        .iter()
        .rev()
        .find(|expression| matches!(expression, Expr::Lambda(_) | Expr::For(_) | Expr::While(_)))?;
    let (transfers, byte) = control_transfers_at(document, position, knowledge)?;
    let mut keywords = match owner {
        Expr::Lambda(function) => lambda_operator(function).into_iter().collect(),
        _ => owner.keywords(),
    };
    if !keywords
        .iter()
        .any(|keyword| spans_byte(document, keyword, byte))
    {
        return None;
    }
    keywords.extend(owned_transfer_keywords(document, &transfers, *owner));
    Some(highlights_for_spans(keywords, document))
}

/// The keywords of the transfers `owner` receives.
fn owned_transfer_keywords(
    document: &DocumentSnapshot,
    transfers: &[ControlTransfer<'_>],
    owner: &(impl Spanned + ?Sized),
) -> Vec<Span> {
    let owner = document.syntax_bytes(owner);
    transfers
        .iter()
        .filter(|transfer| {
            transfer.target.is_some_and(|target| {
                owner.is_some() && document.syntax_bytes(target.owner()) == owner
            })
        })
        .filter_map(|transfer| transfer_keyword(transfer.transfer).try_span())
        .collect()
}

/// The keyword of a `return`, `break`, or `continue`.
fn transfer_keyword(transfer: &ExprControl) -> &dyn Spanned {
    match transfer {
        ExprControl::Return(node) => &node.return_token,
        ExprControl::Break(node) => &node.break_token,
        ExprControl::Continue(node) => &node.continue_token,
    }
}

/// The `->` of a function.
fn lambda_operator(function: &ExprLambda) -> Option<Span> {
    match function {
        ExprLambda::Variadic(node) => node.operator.try_span(),
        ExprLambda::List(node) => node.operator.try_span(),
    }
}

/// Whether `syntax` covers the document byte `byte`.
fn spans_byte(document: &DocumentSnapshot, syntax: &(impl Spanned + ?Sized), byte: usize) -> bool {
    document
        .syntax_bytes(syntax)
        .is_some_and(|bytes| bytes.start <= byte && byte < bytes.end)
}

/// Text highlights of `spans`, in source order without repeats.
fn highlights_for_spans(spans: Vec<Span>, document: &DocumentSnapshot) -> Vec<DocumentHighlight> {
    let mut ranges = spans
        .iter()
        .filter_map(|span| document.syntax_bytes(span))
        .map(|bytes| (bytes.start, bytes.end))
        .collect::<Vec<_>>();
    ranges.sort_unstable();
    ranges.dedup();
    ranges
        .into_iter()
        .map(|(start, end)| DocumentHighlight {
            range: document.range_for_bytes(start..end),
            kind: Some(DocumentHighlightKind::TEXT),
        })
        .collect()
}

/// Highlight the keyword sequence of the compound statement under the cursor:
/// resting on any one keyword lights up its siblings (`if`/`then`/`else`,
/// `for`/`in`/`list`/`do`, …). Triggers only when the cursor is on a keyword,
/// not when it is inside the statement's expressions.
fn keyword_sequence_highlights(
    document: &DocumentSnapshot,
    position: Position,
) -> Option<Vec<DocumentHighlight>> {
    let byte = document.byte_for_position(position)?;
    let keywords = document
        .enclosing_expressions(position)
        .into_iter()
        .rev()
        .map(StatementKeywords::keywords)
        .find(|keywords| !keywords.is_empty())?;
    keywords
        .iter()
        .any(|keyword| spans_byte(document, keyword, byte))
        .then(|| highlights_for_spans(keywords, document))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object_registry::ObjectRegistry;

    fn document(text: &str) -> DocumentSnapshot {
        DocumentSnapshot::from_text(text.to_string(), &ObjectRegistry::default())
            .expect("fixture should parse")
    }

    fn highlighted_words(text: &str, line: u32, character: u32) -> Vec<String> {
        let document = document(text);
        document_highlights(&document, pos!(line, character), &ObjectRegistry::default())
            .unwrap_or_default()
            .into_iter()
            .map(|highlight| {
                let range = highlight.range;
                let line_text = text.lines().nth(range.start.line as usize).unwrap_or("");
                line_text[range.start.character as usize..range.end.character as usize].to_string()
            })
            .collect()
    }

    fn highlight_kinds(text: &str, line: u32, character: u32) -> Vec<DocumentHighlightKind> {
        let document = document(text);
        document_highlights(&document, pos!(line, character), &ObjectRegistry::default())
            .unwrap_or_default()
            .into_iter()
            .map(|highlight| highlight.kind.expect("symbol highlights carry a kind"))
            .collect()
    }

    #[test]
    fn highlights_if_then_else_keywords_from_any_keyword() {
        let text = "if a then b else c\n";
        for character in [0, 5, 12] {
            assert_eq!(
                highlighted_words(text, 0, character),
                vec!["if", "then", "else"],
                "cursor at column {character} should highlight the if-keyword sequence",
            );
        }
    }

    #[test]
    fn highlights_for_in_list_keywords() {
        assert_eq!(
            highlighted_words("for i in L list f i\n", 0, 1),
            vec!["for", "in", "list"]
        );
    }

    #[test]
    fn highlights_for_from_to_do_keywords() {
        assert_eq!(
            highlighted_words("for i from 1 to 10 do g i\n", 0, 7),
            vec!["for", "from", "to", "do"]
        );
    }

    #[test]
    fn highlights_new_of_from_keywords_from_any_keyword() {
        let text = "new T of S from x\n";
        for character in [0, 6, 11] {
            assert_eq!(
                highlighted_words(text, 0, character),
                vec!["new", "of", "from"],
                "cursor at column {character} should highlight the new-keyword sequence",
            );
        }
        assert_eq!(
            highlighted_words("new T from x\n", 0, 7),
            vec!["new", "from"]
        );
    }

    #[test]
    fn highlights_an_unbound_name_inside_an_expression() {
        assert_eq!(highlighted_words("if a then b else c\n", 0, 3), vec!["a"]);
    }

    #[test]
    fn does_not_highlight_null_or_boolean_literals() {
        let builtins = ObjectRegistry::load(crate::catalog::test_corpus());
        for literal in ["null", "true", "false"] {
            let text = format!("{literal}\n{literal}\n");
            let document = document(&text);
            assert!(
                document_highlights(&document, pos!(), &builtins).is_none(),
                "{literal} should not produce document highlights"
            );
        }
    }

    #[test]
    fn highlights_all_in_file_references_of_the_symbol_under_cursor() {
        // `x` is bound by `:=` then used twice; resting on any occurrence lights
        // up all three.
        let text = "x := 1\ny := x + x\n";
        for (line, character) in [(0, 0), (1, 5), (1, 9)] {
            assert_eq!(
                highlighted_words(text, line, character),
                vec!["x", "x", "x"],
                "occurrence at {line}:{character} should highlight every use of x",
            );
        }
    }

    #[test]
    fn highlights_wikilink_documentation_mentions_with_code_references() {
        let text = "x := 1\n-- use [[x]]\ny := x\n";
        assert_eq!(highlighted_words(text, 1, 9), vec!["x", "x", "x"],);
    }

    #[test]
    fn package_strings_highlight_package_references_without_strings_or_shadowed_locals() {
        let builtins = ObjectRegistry::load(crate::catalog::test_corpus());
        let text = concat!(
            "needsPackage \"Graphs\"\n",
            "loadPackage \"Graphs\"\n",
            "Graphs\n",
            "ordinary = \"Graphs\"\n",
            "f = Graphs -> Graphs\n",
        );
        let document = DocumentSnapshot::from_text(text.into(), &builtins).unwrap();
        for (line, character) in [(0, 14), (1, 13), (2, 0)] {
            let highlights =
                document_highlights(&document, pos!(line, character), &builtins).unwrap();
            assert_eq!(
                highlights
                    .iter()
                    .map(|highlight| highlight.range.start.line)
                    .collect::<Vec<_>>(),
                vec![0, 1, 2]
            );
            for highlight in highlights {
                let bytes = document.bytes_for_range(highlight.range).unwrap();
                assert_eq!(&text[bytes], "Graphs");
                assert_eq!(highlight.kind, Some(DocumentHighlightKind::READ));
            }
        }
    }

    #[test]
    fn package_declaration_is_a_write_and_package_strings_use_utf16_ranges() {
        let builtins = ObjectRegistry::load(crate::catalog::test_corpus());
        let text = "newPackage \"Pkg\"\nneedsPackage \"Pkg\"\nPkg\n";
        let document = DocumentSnapshot::from_text(text.into(), &builtins).unwrap();
        let highlights = document_highlights(&document, pos!(1, 14), &builtins).unwrap();
        assert_eq!(highlights.len(), 3);
        assert_eq!(highlights[0].kind, Some(DocumentHighlightKind::WRITE));
        assert_eq!(highlights[1].kind, Some(DocumentHighlightKind::READ));

        let text = "prefix = \"😀\"; needsPackage \"ΠPkg\"\nloadPackage ///ΠPkg///\n";
        let document = DocumentSnapshot::from_text(text.into(), &builtins).unwrap();
        let position = document.position_for_byte(text.find("ΠPkg").unwrap());
        let highlights = document_highlights(&document, position, &builtins).unwrap();
        assert_eq!(highlights.len(), 2);
        for highlight in highlights {
            assert_eq!(
                &text[document.bytes_for_range(highlight.range).unwrap()],
                "ΠPkg"
            );
        }
    }

    #[test]
    fn highlights_wikilink_mentions_of_later_bindings() {
        let text = "-- use [[x]]\nx := 1\nx\n";
        assert_eq!(highlighted_words(text, 0, 9), vec!["x", "x", "x"]);
    }

    #[test]
    fn highlights_unshadowed_builtin_names_but_excludes_keywords() {
        let builtins = ObjectRegistry::load(crate::catalog::test_corpus());
        let text = "ideal I\n-- call [[ideal]] again\nx := ideal J\nif true then ideal K\n";
        let source_document = document(text);
        let words = document_highlights(&source_document, pos!(0, 1), &builtins)
            .expect("ordinary builtin should resolve")
            .into_iter()
            .map(|highlight| {
                let range = highlight.range;
                text.lines().nth(range.start.line as usize).unwrap()
                    [range.start.character as usize..range.end.character as usize]
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(words, vec!["ideal", "ideal", "ideal", "ideal"]);

        let keyword_document = document("local x\nlocal y\n");
        assert!(
            document_highlights(&keyword_document, pos!(0, 1), &builtins).is_none(),
            "keyword-class builtin records must not trigger symbol highlighting"
        );
    }

    #[test]
    fn builtin_highlights_do_not_cross_a_local_shadow() {
        let builtins = ObjectRegistry::load(crate::catalog::test_corpus());
        let text = "ideal I\nf := ideal -> (ideal + 1)\nideal J\n";
        let document = document(text);
        let highlights = document_highlights(&document, pos!(0, 1), &builtins)
            .expect("builtin occurrence should resolve");

        assert_eq!(
            highlights
                .into_iter()
                .map(|highlight| highlight.range.start)
                .collect::<Vec<_>>(),
            vec![pos!(), pos!(2, 0)]
        );
    }

    #[test]
    fn highlights_repeated_unbound_names_without_an_index_record() {
        let text = "futureName x\n-- see [[futureName]]\ny := futureName\n";
        let document = document(text);
        let highlights = document_highlights(&document, pos!(0, 1), &ObjectRegistry::default())
            .expect("an unresolved non-keyword name is still highlightable");

        assert_eq!(
            highlights
                .into_iter()
                .map(|highlight| highlight.range.start)
                .collect::<Vec<_>>(),
            vec![pos!(), pos!(1, 9), pos!(2, 5)]
        );
    }

    #[test]
    fn distinguishes_the_binding_write_from_use_reads() {
        // Declaration is a WRITE; the two uses are READs.
        assert_eq!(
            highlight_kinds("x := 1\ny := x + x\n", 0, 0),
            vec![
                DocumentHighlightKind::WRITE,
                DocumentHighlightKind::READ,
                DocumentHighlightKind::READ,
            ],
        );
    }

    #[test]
    fn loop_variables_highlight_by_spelling_without_a_binding() {
        assert_eq!(
            highlighted_words("for i in L do f i\n", 0, 4),
            vec!["i", "i"]
        );
    }

    #[test]
    fn nested_if_highlights_only_the_innermost_statement() {
        // `if a then if b then c` — cursor on the inner `if` lights up only the
        // inner sequence, not the outer.
        let text = "if a then if b then c\n";
        assert_eq!(highlighted_words(text, 0, 10), vec!["if", "then"]);
    }

    #[test]
    fn return_highlights_the_function_arrow_and_sibling_returns() {
        let text = "f := x -> if x then return x else return 0\n";
        assert_eq!(
            highlighted_words(text, 0, 23),
            vec!["->", "return", "return"]
        );
    }

    #[test]
    fn break_and_continue_highlight_their_owning_loop() {
        let text = "while a do if b then break else continue\n";
        assert_eq!(
            highlighted_words(text, 0, 22),
            vec!["while", "do", "break", "continue"]
        );
        assert_eq!(
            highlighted_words(text, 0, 33),
            vec!["while", "do", "break", "continue"]
        );
    }

    #[test]
    fn nested_control_transfers_stay_with_the_innermost_owner() {
        let text = "while a do (while b do break; continue)\n";
        assert_eq!(highlighted_words(text, 0, 24), vec!["while", "do", "break"]);
    }

    #[test]
    fn break_highlights_its_apply_callback() {
        let text = "apply(0..3, i -> if i then break i else break)\n";
        let builtins = ObjectRegistry::load(crate::catalog::test_corpus());
        let document =
            DocumentSnapshot::from_text(text.to_string(), &builtins).expect("fixture should parse");
        let cursor = text.find("break").expect("fixture should contain break") as u32;
        let words = document_highlights(&document, pos!(0, cursor), &builtins)
            .expect("break should resolve to the apply callback")
            .into_iter()
            .map(|highlight| {
                text[highlight.range.start.character as usize
                    ..highlight.range.end.character as usize]
                    .to_string()
            })
            .collect::<Vec<_>>();

        assert_eq!(words, vec!["apply", "break", "break"]);
    }

    #[test]
    fn semicolon_highlights_as_its_preceding_character() {
        assert_eq!(highlighted_words("x := 1\ny := x;\n", 1, 6), vec!["x", "x"]);
        assert!(highlighted_words("ownStatement := false;\n", 0, 21).is_empty());
    }
}
