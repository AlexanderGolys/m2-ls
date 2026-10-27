//! In-document highlighting for resolved symbols and compound-statement words.

use m2_syn::nodes::{ExprFor as ForLoop, ExprLambda as LambdaExpression, ExprWhile as WhileLoop};

use crate::capabilities::navigation::{reference_ranges_resolved, unbound_reference_ranges};
use crate::document::DocumentSnapshot;
use crate::node_metadata::SyntaxNode;
use crate::object_registry::ObjectName;
use crate::source::SourceNavigation;
use crate::typesystem::TypeKnowledge;
use tower_lsp::lsp_types::{
    DocumentHighlight, DocumentHighlightKind, DocumentHighlightOptions, OneOf, Position,
};

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
    ranges.extend(
        document
            .root_node()
            .binding_references()
            .filter(|node| node.text() == name)
            .filter(|node| {
                document
                    .source_binding_at(name, document.position_for_node(*node))
                    .is_none_or(|binding| {
                        binding.state.presentation_kind
                            == tower_lsp::lsp_types::SymbolKind::NAMESPACE
                    })
            })
            .map(|node| document.range_for_node(node)),
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
    let references = analysis.indexed_ring_generator_references(
        document.root_node(),
        position,
        document,
        true,
    )?;
    let declarations = analysis.indexed_ring_generator_references(
        document.root_node(),
        position,
        document,
        false,
    )?;
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
    let semicolon = document
        .node_at_position_minimal(position)
        .filter(SyntaxNode::is_semicolon)?;
    let start = document.range_for_node(semicolon).start;
    let preceding_character = start.character.checked_sub(1)?;
    Some(Position::new(start.line, preceding_character))
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

fn control_transfer_highlights(
    document: &DocumentSnapshot,
    position: Position,
    knowledge: &(impl TypeKnowledge + ?Sized),
) -> Option<Vec<DocumentHighlight>> {
    let cursor_node = document.node_at_position_minimal(position)?;
    let transfer = enclosing_control_transfer(cursor_node)?;
    let keyword = transfer.child(0)?;
    if !keyword.contains(cursor_node) {
        return None;
    }
    let target = document
        .analysis()
        .control_transfer_target(transfer, document, knowledge)?;
    let owner = target.owner();

    let mut nodes = match target {
        crate::analysis::ControlTransferTarget::LoopCallback { callable, .. } => vec![callable],
        crate::analysis::ControlTransferTarget::ListLoop(loop_statement)
        | crate::analysis::ControlTransferTarget::DoLoop(loop_statement) => {
            statement_keyword_tokens(loop_statement)
        }
        crate::analysis::ControlTransferTarget::Function(function) => {
            function.operator_node().into_iter().collect()
        }
    };
    extend_control_transfer_nodes(&mut nodes, owner, document, knowledge);

    Some(highlights_for_nodes(nodes, document))
}

fn control_owner_highlights(
    document: &DocumentSnapshot,
    position: Position,
    knowledge: &(impl TypeKnowledge + ?Sized),
) -> Option<Vec<DocumentHighlight>> {
    let cursor = document.node_at_position_minimal(position)?;
    let mut current = cursor;
    loop {
        if current.is::<LambdaExpression>() {
            let operator = current.operator_node()?;
            if operator.contains(cursor) {
                let mut nodes = vec![operator];
                extend_control_transfer_nodes(&mut nodes, current, document, knowledge);
                return Some(highlights_for_nodes(nodes, document));
            }
            return None;
        }
        if current.is::<ForLoop>() || current.is::<WhileLoop>() {
            let mut nodes = statement_keyword_tokens(current);
            if nodes.iter().any(|keyword| keyword.contains(cursor)) {
                extend_control_transfer_nodes(&mut nodes, current, document, knowledge);
                return Some(highlights_for_nodes(nodes, document));
            }
            return None;
        }
        current = current.parent()?;
    }
}

fn extend_control_transfer_nodes<'tree>(
    nodes: &mut Vec<SyntaxNode<'tree>>,
    owner: SyntaxNode<'tree>,
    document: &'tree DocumentSnapshot,
    knowledge: &(impl TypeKnowledge + ?Sized),
) {
    document.analysis().for_each_control_transfer(
        document.root_node(),
        document.syntax(),
        document,
        knowledge,
        |statement, target| {
            if target.owner().id() == owner.id() {
                nodes.extend(statement.child(0));
            }
        },
    );
}

fn highlights_for_nodes(
    mut nodes: Vec<SyntaxNode<'_>>,
    document: &DocumentSnapshot,
) -> Vec<DocumentHighlight> {
    nodes.sort_by_key(|node| (node.start_byte(), node.end_byte()));
    nodes.dedup_by_key(|node| node.id());

    nodes
        .into_iter()
        .map(|node| DocumentHighlight {
            range: document.range_for_node(node),
            kind: Some(DocumentHighlightKind::TEXT),
        })
        .collect()
}

fn enclosing_control_transfer(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    node.enclosing_node(SyntaxNode::is_control_transfer)
}

/// Highlight the keyword sequence of the compound statement under the cursor:
/// resting on any one keyword lights up its siblings (`if`/`then`/`else`,
/// `for`/`in`/`list`/`do`, …). Triggers only when the cursor is on a keyword,
/// not when it is inside the statement's expressions.
fn keyword_sequence_highlights(
    document: &DocumentSnapshot,
    position: Position,
) -> Option<Vec<DocumentHighlight>> {
    let node = document.node_at_position_minimal(position)?;
    let statement = enclosing_keyword_statement(node)?;
    let keywords = statement_keyword_tokens(statement);

    if !keywords.iter().any(|keyword| keyword.contains(node)) {
        return None;
    }

    Some(
        keywords
            .into_iter()
            .map(|keyword| DocumentHighlight {
                range: document.range_for_node(keyword),
                kind: Some(DocumentHighlightKind::TEXT),
            })
            .collect(),
    )
}

fn enclosing_keyword_statement(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    node.enclosing_node(SyntaxNode::is_keyword_statement)
}

/// Collect the keyword tokens of a compound statement: its own leading keyword
/// plus the leading keyword of each direct clause child. Nested statements are
/// not descended into — they own their own sequence.
fn statement_keyword_tokens(statement: SyntaxNode<'_>) -> Vec<SyntaxNode<'_>> {
    if statement.is_keyword_statement() {
        let mut keywords = statement
            .children()
            .filter(SyntaxNode::is_keyword_token)
            .collect::<Vec<_>>();
        for child in statement.named_children() {
            if child.is_keyword_clause() {
                keywords.extend(child.children().filter(SyntaxNode::is_keyword_token));
            }
        }
        return keywords;
    }
    if statement.is_keyword_clause() {
        let Some(st) = enclosing_keyword_statement(statement) else {
            return Vec::new();
        };
        return statement_keyword_tokens(st);
    }
    Vec::new()
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
    fn highlights_an_unbound_name_inside_an_expression() {
        assert_eq!(highlighted_words("if a then b else c\n", 0, 3), vec!["a"]);
    }

    #[test]
    fn does_not_highlight_null_or_boolean_literals() {
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
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
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
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
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
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
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
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
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
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
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
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
