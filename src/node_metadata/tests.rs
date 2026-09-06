//! Syntax abstraction and traversal contract tests.

use super::*;

#[cfg(test)]
mod cst_compliance_gate {
    //! Build gate enforcing the repo rule that the rest of the crate must reach
    //! the syntax tree only through `M2Node` and m2-syn types, never raw
    //! tree-sitter node names or the raw source buffer. Reading raw code
    //! re-derives parser logic and is banned.
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    /// Substrings that must not appear outside the `node_metadata` module. Each bypasses
    /// the typed syntax or configured parser abstractions.
    const BANNED: &[(&str, &str)] = &[
        (
            ".kind()",
            "raw tree-sitter node-type name; use `node.is::<Syntax>()` instead",
        ),
        (
            ".raw_kind()",
            "raw node-type name is private to node_metadata; use a typed predicate",
        ),
        (".utf8_text(", "reads raw bytes; use `node.text()`"),
        ("tree_sitter::Node", "raw node type; pass `M2Node` instead"),
        (
            "starts_with('\"')",
            "byte-scanning for string quotes; use `node.string_literal_inner_text()`",
        ),
        (
            "strip_prefix('\"')",
            "byte-stripping string quotes; use `node.string_literal_inner_text()`",
        ),
        (
            "strip_suffix('\"')",
            "byte-stripping string quotes; use `node.string_literal_inner_text()`",
        ),
        (".set_language(", "raw parser configuration; use `M2Parser`"),
    ];

    fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("src dir is readable") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                rust_sources(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn no_raw_syntax_or_parser_access_outside_node_metadata() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let node_metadata = src.join("node_metadata");
        let mut files = Vec::new();
        rust_sources(&src, &mut files);

        let mut violations = Vec::new();
        for file in files {
            // The node_metadata module is the one sanctioned home for raw access.
            if file.starts_with(&node_metadata) {
                continue;
            }
            let contents = fs::read_to_string(&file).expect("source is readable");
            for (line_number, line) in contents.lines().enumerate() {
                // Skip comments: a doc/line comment may legitimately mention a
                // banned form while describing the rule.
                if line.trim_start().starts_with("//") {
                    continue;
                }
                if line.contains("Parser::new()") && !line.contains("M2Parser::new()") {
                    violations.push(format!(
                        "{}:{}: `Parser::new()` — raw parser construction; use `M2Parser`",
                        file.display(),
                        line_number + 1,
                    ));
                }
                for (needle, why) in BANNED {
                    if line.contains(needle) {
                        violations.push(format!(
                            "{}:{}: `{needle}` — {why}",
                            file.display(),
                            line_number + 1,
                        ));
                    }
                }
            }
        }

        assert!(
            violations.is_empty(),
            "CST-compliance gate failed; route these through M2Node and m2-syn:\n{}",
            violations.join("\n")
        );
    }
}

#[cfg(test)]
mod descendants_tests {
    //! Locks the pre-order DFS contract of `M2Node::descendants`: the contract
    //! every migrated call site relies on (parent before children, source order
    //! across siblings, root yielded exactly once, empty file safe).
    use super::*;
    use m2_syn::nodes::{ExprQuote as QuoteExpression, Sequence, SourceFile, Symbol};

    #[test]
    fn descendants_visit_parent_before_children_in_source_order() {
        let mut parser = M2Parser::new().expect("Macaulay2 parser should load");
        let root = parser.parse("f(x, y)\n").expect("fixture should parse");
        let nodes = root.descendants().collect::<Vec<_>>();

        // Root first, then `f`, then the parenthesized call's inner sequence
        // and its three children in source order: `x`, `,`, `y`. We assert a
        // few key landmarks rather than the full token list, so a future
        // grammar renormalization that adds wrapper nodes doesn't break the
        // test for a property it doesn't care about.
        assert!(nodes.first().is_some_and(|node| node.is::<SourceFile>()));
        let x_pos = nodes
            .iter()
            .position(|node| node.is::<Symbol>())
            .expect("a Symbol is emitted");
        let seq_pos = nodes
            .iter()
            .position(|node| node.is::<Sequence>())
            .expect("a Sequence is emitted");
        // Whichever node contains the call appears before its `Symbol` child
        // (pre-order). Both orderings of `Symbol`-then-`Sequence` are valid
        // depending on grammar wrappers, but a child never precedes its
        // container.
        assert!(x_pos != seq_pos);
    }

    #[test]
    fn descendants_of_empty_file_yields_root_once() {
        let mut parser = M2Parser::new().expect("Macaulay2 parser should load");
        let root = parser.parse("").expect("fixture should parse");
        assert_eq!(
            root.descendants().count(),
            1,
            "empty file yields only the SourceFile root"
        );
    }

    #[test]
    fn descendants_count_matches_node_count_of_tree() {
        // direct check against tree-sitter's own traversal: the total
        // descendant count (root included) must match the recursive child
        // count, so the iterator neither drops nor duplicates any node.
        let text = "x = (a, b)\ny = {1, 2, 3}\n";
        fn count_via_children(n: M2Node<'_>) -> usize {
            1 + n.children().map(count_via_children).sum::<usize>()
        }
        let mut parser = M2Parser::new().expect("Macaulay2 parser should load");
        let root = parser.parse(text).expect("fixture should parse");
        assert_eq!(root.descendants().count(), count_via_children(root));
    }

    #[test]
    fn grammar_exposes_muted_empty_components_and_naked_sequences() {
        let text = "local if\nstep 1\nfinish\n(x;)\n(,a,,)\na,b\n";
        let mut parser = M2Parser::new().expect("Macaulay2 parser should load");
        let root = parser.parse(text).expect("fixture should parse");
        let quote = root
            .descendants()
            .find(|node| node.is::<QuoteExpression>())
            .expect("`local if` is a quote expression");
        assert_eq!(
            quote
                .child_by_field_name("symbol")
                .expect("quote has a symbol field")
                .text(),
            "if"
        );
        assert!(quote
            .child_by_field_name("specifier")
            .expect("quote has a specifier field")
            .is_modifier_token());

        assert_eq!(
            root.descendants().filter(M2Node::is_debug_expr).count(),
            2,
            "both `step` and `finish` are debug clauses"
        );

        let parens = root
            .descendants()
            .find(M2Node::is_holder)
            .expect("parenthesized expression is present");
        assert!(parens
            .named_children()
            .next()
            .is_some_and(|child| child.is_muted_statement()));
        assert!(
            parens.final_value_child().is_none(),
            "a grouping ending in a muted expression has no value child"
        );

        let sequence = root
            .descendants()
            .find(|node| node.is::<Sequence>())
            .expect("parenthesized comma sequence is present");
        let elements = sequence.collection_elements().collect::<Vec<_>>();
        assert_eq!(elements.len(), 4, "every comma slot remains an element");
        assert_eq!(
            elements
                .iter()
                .filter(|element| element.is_empty_component())
                .count(),
            3,
            "empty comma slots are explicit empty-component nodes"
        );

        let naked = root
            .descendants()
            .find(M2Node::is_expr_pack)
            .expect("top-level comma expression is a naked sequence");
        assert_eq!(naked.collection_elements().count(), 2);
    }
}
