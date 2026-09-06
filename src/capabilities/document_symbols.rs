//! Document-symbol extraction for Macaulay2 source files.
//!
//! The outline is intentionally static: it reports bindings introduced by the
//! document, nesting bindings created in function bodies beneath their
//! function. Runtime values and names supplied by indexed packages are not
//! treated as document definitions.

use tower_lsp::lsp_types::Range as TextRange;
use tower_lsp::lsp_types::*;

use crate::analysis::AssignmentFactKind;
use crate::document::DocumentSnapshot;
use crate::meta::BindingRole;
use crate::object_registry::ObjectName;

#[derive(Debug, Clone)]
pub struct WorkspaceSourceSymbol {
    pub name: ObjectName,
    pub kind: SymbolKind,
    pub location: Location,
}

/// The outline view of a document: assignment bindings (functions nested under
/// their body), method installations, and indexed variables.
pub fn collect_document_symbols(document: &DocumentSnapshot) -> Vec<DocumentSymbol> {
    let analysis = document.analysis();
    let mut declarations = Vec::new();

    for binding in analysis
        .binding_states()
        .filter(|binding| binding.role == BindingRole::Ordinary)
    {
        declarations.push(Declaration {
            name: binding.name.name().to_string(),
            detail: binding
                .state
                .inferred_type
                .as_ref()
                .and_then(crate::typesystem::InferredType::label),
            kind: binding.state.presentation_kind,
            range: binding.state.definition_range,
            selection_range: binding.state.span,
            scope_idx: binding.state.scope_idx,
            child_scope_idx: binding
                .state
                .value_range
                .and_then(|range| analysis.scope_with_range(range)),
        });
    }

    for assignment in analysis.assignment_facts() {
        let child_scope_idx = assignment
            .value_span
            .and_then(|range| analysis.scope_with_range(range));
        let (kind, detail) = match assignment.kind {
            AssignmentFactKind::MethodInstallation(id) => {
                let Some(installation) = analysis.method_installation(id) else {
                    continue;
                };
                (
                    SymbolKind::METHOD,
                    Some(method_signature_detail(
                        analysis,
                        installation,
                        &assignment.label,
                    )),
                )
            }
            AssignmentFactKind::IndexedVariable => {
                (SymbolKind::VARIABLE, Some("indexed variable".to_string()))
            }
            AssignmentFactKind::ScopedCallable if child_scope_idx.is_some() => {
                (SymbolKind::METHOD, Some(assignment.label.clone()))
            }
            AssignmentFactKind::ScopedCallable => continue,
        };
        declarations.push(Declaration {
            name: assignment.label.clone(),
            detail,
            kind,
            range: assignment.span,
            selection_range: assignment.target_span,
            scope_idx: assignment.scope_idx,
            child_scope_idx,
        });
    }

    declarations.sort_by_key(|declaration| {
        (
            declaration.range.start.line,
            declaration.range.start.character,
            declaration.selection_range.start.line,
            declaration.selection_range.start.character,
        )
    });
    build_document_symbol_tree(analysis, declarations)
}

fn method_signature_detail(
    analysis: &crate::analysis::Analysis,
    installation: &crate::analysis::MethodInstallation,
    target: &str,
) -> String {
    analysis
        .method_installation_codomain(installation)
        .map_or_else(
            || target.to_string(),
            |codomain| format!("{target} -> {codomain}"),
        )
}

/// Build a `DocumentSymbol` while keeping the deprecated LSP field isolated in
/// one compatibility boundary.
#[allow(deprecated)]
fn document_symbol(
    name: String,
    detail: Option<String>,
    kind: SymbolKind,
    range: TextRange,
    selection_range: TextRange,
    children: Option<Vec<DocumentSymbol>>,
) -> DocumentSymbol {
    DocumentSymbol {
        name,
        detail,
        kind,
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children,
    }
}

#[derive(Debug)]
struct Declaration {
    name: String,
    detail: Option<String>,
    kind: SymbolKind,
    range: TextRange,
    selection_range: TextRange,
    scope_idx: usize,
    child_scope_idx: Option<usize>,
}

pub fn collect_workspace_symbols(
    document: &DocumentSnapshot,
    uri: &Url,
) -> Vec<WorkspaceSourceSymbol> {
    collect_document_symbols(document)
        .into_iter()
        .map(|symbol| WorkspaceSourceSymbol {
            name: ObjectName::new(symbol.name),
            kind: symbol.kind,
            location: Location {
                uri: uri.clone(),
                range: symbol.selection_range,
            },
        })
        .collect()
}

fn build_document_symbol_tree(
    analysis: &crate::analysis::Analysis,
    declarations: Vec<Declaration>,
) -> Vec<DocumentSymbol> {
    let mut container_scopes = vec![false; analysis.scope_count()];
    container_scopes[0] = true;
    for declaration in &declarations {
        if let Some(scope_idx) = declaration.child_scope_idx {
            container_scopes[scope_idx] = true;
        }
    }

    let mut by_scope: Vec<Vec<Declaration>> =
        (0..analysis.scope_count()).map(|_| Vec::new()).collect();
    for declaration in declarations {
        let scope_idx = nearest_container_scope(analysis, &container_scopes, declaration.scope_idx);
        by_scope[scope_idx].push(declaration);
    }

    build_scope_symbols(0, &mut by_scope)
}

fn nearest_container_scope(
    analysis: &crate::analysis::Analysis,
    container_scopes: &[bool],
    mut scope_idx: usize,
) -> usize {
    loop {
        if container_scopes[scope_idx] {
            return scope_idx;
        }
        let Some(parent_idx) = analysis.parent_scope(scope_idx) else {
            return 0;
        };
        scope_idx = parent_idx;
    }
}

fn build_scope_symbols(scope_idx: usize, by_scope: &mut [Vec<Declaration>]) -> Vec<DocumentSymbol> {
    std::mem::take(&mut by_scope[scope_idx])
        .into_iter()
        .map(|declaration| {
            let children = declaration
                .child_scope_idx
                .map(|child_scope_idx| build_scope_symbols(child_scope_idx, by_scope))
                .filter(|children| !children.is_empty());
            document_symbol(
                declaration.name,
                declaration.detail,
                declaration.kind,
                declaration.range,
                declaration.selection_range,
                children,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::DocumentSnapshot;
    use crate::node_metadata::{M2Node, M2Parser};
    use crate::object_registry::ObjectRegistry;
    use m2_syn::nodes::{ExprLambda as LambdaExpression, Symbol};
    use tower_lsp::lsp_types::Range as TextRange;

    fn document(text: &str, builtins: &ObjectRegistry) -> DocumentSnapshot {
        DocumentSnapshot::from_text(text.to_string(), builtins).expect("fixture should parse")
    }

    #[test]
    fn document_symbol_ranges_use_lsp_utf16_columns() {
        let text = "\"😀\"; f := 1";
        let builtins = ObjectRegistry::default();

        let document = document(text, &builtins);
        let symbols = collect_document_symbols(&document);

        assert_eq!(symbols[0].name, "f");
        assert_eq!(
            symbols[0].selection_range,
            TextRange::new(pos!(0, 6), pos!(0, 7))
        );
    }

    #[test]
    fn document_symbols_exclude_package_indexed_option_keys() {
        let text = "newPackage(\"P\", Version => \"0.1\", DebuggingMode => false)\n";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let document = document(text, &builtins);
        let symbols = collect_document_symbols(&document);

        assert!(
            symbols
                .iter()
                .all(|symbol| symbol.name != "Version" && symbol.name != "DebuggingMode"),
            "package option keys must not be document symbols: {:?}",
            symbols
                .iter()
                .map(|symbol| &symbol.name)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn document_symbols_exclude_custom_option_keys() {
        let text = "f = method(Options => {MyOpt => 1})\ng(MyOpt => 2)\n";
        let builtins = ObjectRegistry::default();
        let document = document(text, &builtins);
        let symbols = collect_document_symbols(&document);

        assert!(
            symbols.iter().all(|symbol| symbol.name != "MyOpt"),
            "option keys must not be document symbols"
        );
    }

    #[test]
    fn document_symbols_include_top_level_and_nested_assignments() {
        let text = "f := x -> (y := x + 1; y)\nR = QQ[a]\n";
        let builtins = ObjectRegistry::default();

        let document = document(text, &builtins);
        let symbols = collect_document_symbols(&document);

        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].name, "f");
        assert_eq!(symbols[0].kind, SymbolKind::FUNCTION);
        assert_eq!(
            symbols[0]
                .children
                .as_ref()
                .expect("function should expose local assignment children")[0]
                .name,
            "y"
        );
        assert_eq!(
            symbols[0]
                .children
                .as_ref()
                .expect("function should expose local assignment children")[0]
                .kind,
            SymbolKind::VARIABLE
        );
        assert_eq!(symbols[1].name, "R");
        assert_eq!(symbols[1].kind, SymbolKind::VARIABLE);
    }

    #[test]
    fn document_symbols_include_every_binding_state_in_m2_scopes() {
        let text =
            "x := 1\nx := 2\ny = 1\ny = 2\nf := x -> (x = 2; K = x; z := 3; z := 4)\nK = 3\n";
        let builtins = ObjectRegistry::default();

        let document = document(text, &builtins);
        let symbols = collect_document_symbols(&document);

        assert_eq!(
            symbols
                .iter()
                .map(|symbol| symbol.name.as_str())
                .collect::<Vec<_>>(),
            vec!["x", "x", "y", "y", "f", "K"]
        );

        let children = symbols[4]
            .children
            .as_ref()
            .expect("function should expose local binding children");

        assert_eq!(
            children
                .iter()
                .map(|symbol| symbol.name.as_str())
                .collect::<Vec<_>>(),
            vec!["K", "z", "z"]
        );
    }

    #[test]
    fn document_symbols_include_nested_equal_assignment_functions() {
        let text = "f = () -> (g = () -> (x = 1))";
        let builtins = ObjectRegistry::default();

        let document = document(text, &builtins);
        let symbols = collect_document_symbols(&document);

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "f");
        assert_eq!(symbols[0].kind, SymbolKind::FUNCTION);

        let g = &symbols[0]
            .children
            .as_ref()
            .expect("f should expose its local bindings")[0];
        assert_eq!(g.name, "g");
        assert_eq!(g.kind, SymbolKind::FUNCTION);

        let x = &g
            .children
            .as_ref()
            .expect("g should expose its local bindings")[0];
        assert_eq!(x.name, "x");
        assert_eq!(x.kind, SymbolKind::VARIABLE);
    }

    #[test]
    fn document_symbols_emit_every_top_level_reassignment() {
        let text = "x = 1\nargs = {}\nargs = append(args, 1)\n";
        let builtins = ObjectRegistry::default();

        let document = document(text, &builtins);
        let symbols = collect_document_symbols(&document);
        let args_symbols = symbols
            .iter()
            .filter(|symbol| symbol.name == "args")
            .collect::<Vec<_>>();

        assert_eq!(
            args_symbols.len(),
            2,
            "every top-level reassignment should be a document symbol"
        );
        assert_eq!(
            args_symbols[0].selection_range.start,
            pos!(1, 0),
            "the first args symbol should point at its initial assignment"
        );
        assert_eq!(
            args_symbols[1].selection_range.start,
            pos!(2, 0),
            "the second args symbol should point at its reassignment"
        );
    }

    #[test]
    fn document_symbols_cover_static_top_level_extractor_bindings() {
        fn has_function_ancestor(node: M2Node) -> bool {
            node.ancestors()
                .any(|parent| parent.is::<LambdaExpression>())
        }

        fn collect_static_top_level_bindings(node: M2Node, names: &mut Vec<String>) {
            if node.is_assignment() && !has_function_ancestor(node) {
                if let Some(left) = node.child_by_field_name("left") {
                    if left.is::<Symbol>() {
                        let name = left.text().to_string();
                        if !names.contains(&name) {
                            names.push(name);
                        }
                    }
                }
            }

            for child in node.children() {
                collect_static_top_level_bindings(child, names);
            }
        }

        let text = include_str!("../../tests/fixtures/formatting_example.m2");
        let builtins = ObjectRegistry::default();
        let mut parser = M2Parser::new().expect("Macaulay2 parser should load");
        let root = parser.parse(text).expect("fixture should parse");
        let mut expected = Vec::new();
        collect_static_top_level_bindings(root, &mut expected);

        let document = document(text, &builtins);
        let symbols = collect_document_symbols(&document);
        let actual = symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>();

        for name in expected {
            assert!(
                actual.contains(&name.as_str()),
                "missing static top-level document symbol `{name}`"
            );
        }
    }

    #[test]
    fn document_symbols_distinguish_m2_assignment_forms() {
        let text = "\
Thing Thing := (a, b) -> a
Thing .. Thing := (a, b) -> a
toString Tally := f
(x,y) := (1,2)
z = 3
x#i = e
x_i = e
x <- e
(f()) <- e
String * String = (x, y, e) -> e
- String := peek
String ^~ := peek
";
        let builtins = ObjectRegistry::default();

        let document = document(text, &builtins);
        let symbols = collect_document_symbols(&document);

        assert_eq!(
            symbols
                .iter()
                .map(|symbol| (symbol.name.as_str(), symbol.detail.as_deref(), symbol.kind))
                .collect::<Vec<_>>(),
            vec![
                ("Thing Thing", Some("Thing Thing"), SymbolKind::METHOD),
                ("Thing .. Thing", Some("Thing .. Thing"), SymbolKind::METHOD),
                ("toString Tally", Some("toString Tally"), SymbolKind::METHOD),
                ("x", None, SymbolKind::VARIABLE),
                ("y", None, SymbolKind::VARIABLE),
                ("z", Some("ZZ"), SymbolKind::VARIABLE),
                ("x_i", Some("indexed variable"), SymbolKind::VARIABLE),
                (
                    "String * String",
                    Some("String * String"),
                    SymbolKind::METHOD
                ),
                ("- String", Some("- String"), SymbolKind::METHOD),
                ("String ^~", Some("String ^~"), SymbolKind::METHOD),
            ]
        );
    }

    #[test]
    fn document_symbols_cover_bracket_and_nested_destructuring_targets() {
        // `[x, y] := …` and nested `[p, [q, r]] := …` bind exactly like the
        // sequence form — the outline must list every bound name.
        let text = "[x, y] := {1, 2}\n[p, [q, r]] := [1, [2, 3]]\n";
        let builtins = ObjectRegistry::default();

        let document = document(text, &builtins);
        let symbols = collect_document_symbols(&document);
        let names: Vec<_> = symbols.iter().map(|symbol| symbol.name.as_str()).collect();

        assert_eq!(names, vec!["x", "y", "p", "q", "r"]);
    }

    #[test]
    fn document_symbols_cover_inheritance_type_and_method_examples() {
        let text = "\
X = new Type of BasicList
Y = new Type of X
Z = new Type of X
- X := t -> apply(t,i -> -i)
Y + X := (a,b) -> \"Y + X\"
X + Z := (a,b) -> \"X + Z\"
";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));

        let document = document(text, &builtins);
        let symbols = collect_document_symbols(&document);

        assert_eq!(
            symbols
                .iter()
                .map(|symbol| (symbol.name.as_str(), symbol.detail.as_deref(), symbol.kind))
                .collect::<Vec<_>>(),
            vec![
                ("X", Some("Type"), SymbolKind::CLASS),
                ("Y", Some("Type"), SymbolKind::CLASS),
                ("Z", Some("Type"), SymbolKind::CLASS),
                ("- X", Some("- X"), SymbolKind::METHOD),
                ("Y + X", Some("Y + X"), SymbolKind::METHOD),
                ("X + Z", Some("X + Z"), SymbolKind::METHOD),
            ]
        );
    }

    #[test]
    fn document_symbol_details_reuse_binding_types_and_method_codomains() {
        let text = "\
p = method(TypicalValue => List)
p(ZZ, ZZ) := (i, j) -> {i, j}
p(CC, CC) := Array => (i, j) -> [i, j]
x := 1
";
        let builtins = ObjectRegistry::default();

        let document = document(text, &builtins);
        let symbols = collect_document_symbols(&document);

        assert_eq!(
            symbols
                .iter()
                .map(|symbol| (symbol.name.as_str(), symbol.detail.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("p", Some("MethodFunction")),
                ("p(ZZ, ZZ)", Some("p(ZZ, ZZ) -> List")),
                ("p(CC, CC)", Some("p(CC, CC) -> Array")),
                ("x", Some("ZZ")),
            ]
        );
    }

    #[test]
    fn document_symbols_exclude_option_keys_from_function_children() {
        let text = "f := x -> g(x, Strategy => LongPolynomial)";
        let builtins = ObjectRegistry::default();

        let document = document(text, &builtins);
        let symbols = collect_document_symbols(&document);

        assert!(symbols[0].children.is_none());
    }

    #[test]
    fn document_symbols_keep_to_type_functions_as_functions() {
        let text = "toString := x -> x";
        let builtins = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));

        let document = document(text, &builtins);
        let symbols = collect_document_symbols(&document);

        assert_eq!(symbols[0].name, "toString");
        assert_eq!(symbols[0].kind, SymbolKind::FUNCTION);
    }
}
