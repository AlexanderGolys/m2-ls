//! End-to-end analysis behavior observed through standard LSP capabilities.

use serde_json::{json, Value};
use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::support::{position, response_array, DocumentSession};

fn diagnostic_lines(session: &DocumentSession, code: &str) -> Vec<u64> {
    session
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic["code"] == code)
        .filter_map(|diagnostic| diagnostic["range"]["start"]["line"].as_u64())
        .collect()
}

async fn replace_and_assert_diagnostic(
    session: &mut DocumentSession,
    source: &str,
    code: &str,
    expected: bool,
) {
    session.replace(source).await;
    assert_eq!(
        session.diagnostic_codes().contains(&code),
        expected,
        "unexpected {code} diagnostic state for:\n{source}\nall diagnostics: {:?}",
        session.diagnostics()
    );
}

async fn hover_type_at(session: &mut DocumentSession, line: u64, character: u64) -> String {
    let hover = session
        .request(
            "textDocument/hover",
            json!({
                "textDocument": {"uri": session.uri()},
                "position": {
                    "line": line,
                    "character": character
                }
            }),
        )
        .await;
    let markdown = hover["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a markdown hover at {line}:{character}, got {hover}"));
    markdown
        .lines()
        .find_map(|line| line.strip_prefix("Type: `")?.strip_suffix('`'))
        .unwrap_or_else(|| panic!("hover should contain a type line: {markdown}"))
        .to_string()
}

async fn inlay_labels(session: &mut DocumentSession) -> Vec<String> {
    let hints = session
        .request(
            "textDocument/inlayHint",
            json!({
                "textDocument": {"uri": session.uri()},
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 100, "character": 0}
                }
            }),
        )
        .await;
    hints
        .as_array()
        .unwrap_or_else(|| panic!("expected inlay hints, got {hints}"))
        .iter()
        .filter_map(|hint| hint["label"].as_str().map(str::to_string))
        .collect()
}

async fn inlay_labels_by_line(session: &mut DocumentSession) -> Vec<(u64, String)> {
    let hints = session
        .request(
            "textDocument/inlayHint",
            json!({
                "textDocument": {"uri": session.uri()},
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 100, "character": 0}
                }
            }),
        )
        .await;
    hints
        .as_array()
        .unwrap_or_else(|| panic!("expected inlay hints, got {hints}"))
        .iter()
        .filter_map(|hint| {
            Some((
                hint["position"]["line"].as_u64()?,
                hint["label"].as_str()?.to_string(),
            ))
        })
        .collect()
}

async fn inlay_hints(session: &mut DocumentSession) -> Vec<Value> {
    session
        .request(
            "textDocument/inlayHint",
            json!({
                "textDocument": {"uri": session.uri()},
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 100, "character": 0}
                }
            }),
        )
        .await
        .as_array()
        .cloned()
        .expect("expected inlay hints")
}

async fn semantic_tokens(session: &mut DocumentSession) -> Vec<(u64, u64, u64, u64)> {
    let response = session
        .request(
            "textDocument/semanticTokens/full",
            json!({"textDocument": {"uri": session.uri()}}),
        )
        .await;
    let encoded = response_array(&response["data"]);
    let mut line = 0;
    let mut character = 0;
    encoded
        .chunks_exact(5)
        .map(|token| {
            let delta_line = token[0]
                .as_u64()
                .expect("token line delta should be numeric");
            let delta_character = token[1]
                .as_u64()
                .expect("token character delta should be numeric");
            if delta_line == 0 {
                character += delta_character;
            } else {
                line += delta_line;
                character = delta_character;
            }
            (
                line,
                character,
                token[3].as_u64().expect("token type should be numeric"),
                token[4]
                    .as_u64()
                    .expect("token modifiers should be numeric"),
            )
        })
        .collect()
}

fn token_at(
    tokens: &[(u64, u64, u64, u64)],
    source: &str,
    needle: &str,
    occurrence: usize,
) -> (u64, u64) {
    let position = position(source, needle, occurrence);
    let line = position["line"]
        .as_u64()
        .expect("fixture line should be numeric");
    let character = position["character"]
        .as_u64()
        .expect("fixture character should be numeric");
    tokens
        .iter()
        .find_map(|(token_line, token_character, token_type, modifiers)| {
            (*token_line == line && *token_character == character)
                .then_some((*token_type, *modifiers))
        })
        .unwrap_or_else(|| panic!("missing semantic token for {needle:?} occurrence {occurrence}"))
}

#[tokio::test]
async fn unassigned_symbols_are_enum_members_until_their_binding() {
    let source = "a = b\nZZ[a, b, c]\nb = c\n";
    let mut session = DocumentSession::open(source).await;
    let tokens = semantic_tokens(&mut session).await;
    let enum_member = session.semantic_token_type("enumMember");
    let variable = session.semantic_token_type("variable");

    for occurrence in [0, 1] {
        assert_eq!(token_at(&tokens, source, "a", occurrence).0, variable);
    }
    for occurrence in [0, 1] {
        assert_eq!(token_at(&tokens, source, "b", occurrence).0, enum_member);
    }
    assert_eq!(token_at(&tokens, source, "b", 2).0, variable);
    for occurrence in [0, 1] {
        assert_eq!(token_at(&tokens, source, "c", occurrence).0, enum_member);
    }

    session.shutdown().await;
}

#[tokio::test]
async fn loop_parameters_are_scoped_bindings_across_lsp_capabilities() {
    let source = concat!(
        "for loopVar in outer do (for loopVar in inner do use loopVar; use loopVar)\n",
        "use loopVar\n",
        "unknownName 1\n",
        "unknownName 2\n",
    );
    let mut session = DocumentSession::open(source).await;
    let tokens = semantic_tokens(&mut session).await;
    let parameter = session.semantic_token_type("parameter");

    for occurrence in 0..=3 {
        assert_eq!(
            token_at(&tokens, source, "loopVar", occurrence).0,
            parameter,
            "loop declaration and uses should be parameter tokens"
        );
    }
    assert_ne!(
        token_at(&tokens, source, "loopVar", 4).0,
        parameter,
        "the same spelling after the loop is outside its binding"
    );

    let inner_references = session
        .request(
            "textDocument/references",
            json!({
                "textDocument": {"uri": session.uri()},
                "position": position(source, "loopVar", 2),
                "context": {"includeDeclaration": true}
            }),
        )
        .await;
    assert_eq!(
        response_array(&inner_references).len(),
        2,
        "{inner_references}"
    );

    let outer_references = session
        .request(
            "textDocument/references",
            json!({
                "textDocument": {"uri": session.uri()},
                "position": position(source, "loopVar", 3),
                "context": {"includeDeclaration": true}
            }),
        )
        .await;
    assert_eq!(
        response_array(&outer_references).len(),
        2,
        "{outer_references}"
    );

    let highlights = session
        .request(
            "textDocument/documentHighlight",
            json!({
                "textDocument": {"uri": session.uri()},
                "position": position(source, "loopVar", 2)
            }),
        )
        .await;
    assert_eq!(response_array(&highlights).len(), 2, "{highlights}");

    let unassigned_references = session
        .request(
            "textDocument/references",
            json!({
                "textDocument": {"uri": session.uri()},
                "position": position(source, "unknownName", 0),
                "context": {"includeDeclaration": true}
            }),
        )
        .await;
    assert_eq!(
        response_array(&unassigned_references).len(),
        2,
        "{unassigned_references}"
    );

    session.shutdown().await;
}

#[tokio::test]
async fn control_markers_and_prefixed_symbols_are_symmetric_over_lsp() {
    let source = concat!(
        "value = 1\n",
        "#value\n",
        "value\n",
        "f := x -> if x then return x else return 0\n",
        "for item to 3 do if item then break else continue\n",
    );
    let mut session = DocumentSession::open(source).await;

    let prefixed_references = session
        .request(
            "textDocument/references",
            json!({
                "textDocument": {"uri": session.uri()},
                "position": position(source, "value", 1),
                "context": {"includeDeclaration": true}
            }),
        )
        .await;
    assert_eq!(
        response_array(&prefixed_references).len(),
        3,
        "{prefixed_references}"
    );

    let arrow_highlights = session
        .request(
            "textDocument/documentHighlight",
            json!({
                "textDocument": {"uri": session.uri()},
                "position": position(source, "->", 0)
            }),
        )
        .await;
    assert_eq!(
        response_array(&arrow_highlights).len(),
        3,
        "{arrow_highlights}"
    );

    for keyword in ["for", "to 3", "do if"] {
        let loop_highlights = session
            .request(
                "textDocument/documentHighlight",
                json!({
                    "textDocument": {"uri": session.uri()},
                    "position": position(source, keyword, 0)
                }),
            )
            .await;
        assert_eq!(
            response_array(&loop_highlights).len(),
            5,
            "cursor on {keyword:?}: {loop_highlights}"
        );
    }

    session.shutdown().await;
}

#[tokio::test]
async fn diverging_error_branches_do_not_widen_inferred_types() {
    let source = concat!(
        "value := if condition then error \"bad\" else 1\n",
        "value\n",
        "f := x -> if condition then error \"bad\" else 2\n",
    );
    let mut session = DocumentSession::open(source).await;

    assert_eq!(hover_type_at(&mut session, 1, 0).await, "ZZ");
    let labels = inlay_labels(&mut session).await;
    assert!(
        labels.iter().filter(|label| label.as_str() == "ZZ").count() >= 2,
        "the assignment and lambda result should both remain ZZ: {labels:?}"
    );

    session.shutdown().await;
}

#[tokio::test]
async fn method_codomains_are_type_parameters_and_all_known_types_are_configurable() {
    let source = concat!(
        "p = method(TypicalValue => List)\n",
        "p(ZZ) := Array => x -> [x]\n",
        "literalSequence = (1, \"a\", {2, 3})\n",
    );
    let mut session = DocumentSession::open(source).await;
    let tokens = semantic_tokens(&mut session).await;
    assert_eq!(
        token_at(&tokens, source, "Array", 0).0,
        session.semantic_token_type("typeParameter")
    );

    session.set_expression_type_hints(false).await;
    let calm = inlay_labels_by_line(&mut session).await;
    assert!(
        calm.iter().all(|(line, _)| *line != 2),
        "literal sequences should be trivial by default: {calm:?}"
    );

    session.set_all_known_type_hints(true).await;
    let complete = inlay_labels_by_line(&mut session).await;
    assert!(
        complete
            .iter()
            .any(|(line, label)| *line == 2 && label == "Sequence"),
        "allKnownTypes should expose the literal sequence type: {complete:?}"
    );

    session.shutdown().await;
}

#[tokio::test]
async fn control_flow_conditions_require_booleans_without_function_coloring() {
    let source = concat!(
        "while i do 2;\n",
        "while i == 0 do 2;\n",
        "while i(0) do 2;\n",
        "while true do 2;\n",
        "condition := true\n",
        "while condition do 2;\n",
        "while 1 do 2;\n",
        "if j then 2 else 3\n",
        "if false then 2 else 3\n",
        "if condition then 2 else 3\n",
        "if 1 then 2 else 3\n",
        "callable = value -> value\n",
        "if callable then 2 else 3\n",
    );
    let mut session =
        DocumentSession::open_with_related(source, "i = value -> value\nj = value -> value\n")
            .await;
    let tokens = semantic_tokens(&mut session).await;

    assert_eq!(
        token_at(&tokens, source, "i", 1).0,
        session.semantic_token_type("enumMember")
    );
    assert_eq!(
        token_at(&tokens, source, "i == 0", 0).0,
        session.semantic_token_type("enumMember")
    );
    assert_eq!(
        token_at(&tokens, source, "i(0)", 0).0,
        session.semantic_token_type("function")
    );
    assert_eq!(
        token_at(&tokens, source, "j", 0).0,
        session.semantic_token_type("enumMember")
    );
    assert_eq!(
        token_at(&tokens, source, "callable", 1).0,
        session.semantic_token_type("variable")
    );
    assert_eq!(diagnostic_lines(&session, "T02"), vec![0, 6, 7, 10, 12]);

    session.shutdown().await;
}

#[tokio::test]
async fn comparison_conditions_default_to_boolean_unless_a_codomain_is_recorded() {
    let source = concat!(
        "if left == right then 1\n",
        "if left === right then 1\n",
        "if left != right then 1\n",
        "if left =!= right then 1\n",
        "if left < right then 1\n",
        "if left <= right then 1\n",
        "if left > right then 1\n",
        "if left >= right then 1\n",
        "ZZ == ZZ := Function => (left, right) -> (value -> value)\n",
        "if 1 == 2 then 1\n",
    );
    let session = DocumentSession::open(source).await;

    assert_eq!(diagnostic_lines(&session, "T02"), vec![9]);

    session.shutdown().await;
}

#[tokio::test]
async fn unknown_condition_types_remain_possible_booleans() {
    let source = concat!(
        "predicate = method()\n",
        "if predicate() then 1\n",
        "callable = value -> value\n",
        "if callable then 1\n",
    );
    let session = DocumentSession::open(source).await;

    assert_eq!(diagnostic_lines(&session, "T02"), vec![3]);

    session.shutdown().await;
}

#[tokio::test]
async fn only_original_core_compiled_functions_are_builtin() {
    let source = "f = scan\nf\nscan\nscan = f\nscan\n";
    let mut session = DocumentSession::open(source).await;
    let tokens = semantic_tokens(&mut session).await;
    let function = session.semantic_token_type("function");
    let builtin = session.semantic_token_modifier("builtin");

    for occurrence in [0, 1] {
        let (token_type, modifiers) = token_at(&tokens, source, "scan", occurrence);
        assert_eq!(token_type, function);
        assert_eq!(modifiers, builtin);
    }
    for occurrence in [2, 3] {
        let (token_type, modifiers) = token_at(&tokens, source, "scan", occurrence);
        assert_eq!(token_type, function);
        assert_eq!(modifiers & builtin, 0);
    }
    for occurrence in 0..=2 {
        let (token_type, modifiers) = token_at(&tokens, source, "f", occurrence);
        assert_eq!(token_type, function);
        assert_eq!(modifiers & builtin, 0);
    }

    session.shutdown().await;
}

#[tokio::test]
async fn registered_source_roles_drive_semantic_tokens_through_the_server() {
    let source = concat!(
        "p = method(TypicalValue => List)\n",
        "p(ZZ) := Array => x -> [x]\n",
        "f(Strategy => LongPolynomial)\n",
        "R.name\n",
        "h#\"key\"\n",
        "match \"pattern\"\n",
        "needsPackage \"JSON\"\n",
    );
    let mut session = DocumentSession::open(source).await;
    let tokens = semantic_tokens(&mut session).await;

    for (needle, expected_role, description) in [
        ("ZZ", "typeParameter", "method domain"),
        ("Array", "typeParameter", "method codomain"),
        ("Strategy", "enumMember", "option key"),
        ("name", "property", "quoted member key"),
        ("\"key\"", "property", "lookup key"),
        ("\"JSON\"", "namespace", "package argument"),
    ] {
        assert_eq!(
            token_at(&tokens, source, needle, 0).0,
            session.semantic_token_type(expected_role),
            "unexpected semantic role for {description}"
        );
    }

    session.shutdown().await;
}

#[tokio::test]
async fn output_references_preserve_the_referenced_cells_package_environment() {
    let source = "toJSON 1\nneedsPackage \"JSON\"\nooo\n";
    let mut session = DocumentSession::open(source).await;
    let hints = inlay_labels_by_line(&mut session).await;
    let first_cell = hints
        .iter()
        .filter(|(line, _)| *line == 0)
        .map(|(_, label)| label.as_str())
        .collect::<Vec<_>>();

    assert!(
        first_cell.iter().any(|label| label.contains("Thing")),
        "the call before needsPackage should remain unresolved: {hints:?}"
    );
    assert!(
        first_cell
            .iter()
            .all(|label| !label.contains("String") && !label.contains("MethodFunctionSingle")),
        "the later package environment leaked into the referenced cell: {hints:?}"
    );

    session.shutdown().await;
}

#[tokio::test]
async fn source_type_descendants_are_classified_through_the_typechecker() {
    let source = "TT = new Type\nTTT = new TT\nTTT\n";
    let mut session = DocumentSession::open(source).await;
    let tokens = semantic_tokens(&mut session).await;
    let class = session.semantic_token_type("class");

    for line in 0..=2 {
        assert_eq!(
            tokens
                .iter()
                .find_map(|(token_line, character, token_type, _)| {
                    (*token_line == line && *character == 0).then_some(*token_type)
                }),
            Some(class),
            "line {} should classify its binding as a class",
            line + 1
        );
    }

    session.shutdown().await;
}

#[tokio::test]
async fn output_references_follow_prior_cell_types_without_semantic_tokens() {
    let source = concat!(
        "1\n",
        "a = oo\n",
        "\"hello\"\n",
        "b = ooo\n",
        "c = o3\n",
        "d = oooo\n",
    );
    let mut session = DocumentSession::open(source).await;

    for (line, expected) in [(1, "ZZ"), (3, "ZZ"), (4, "String"), (5, "String")] {
        assert_eq!(hover_type_at(&mut session, line, 0).await, expected);
    }

    let tokens = semantic_tokens(&mut session).await;
    for line in [1, 3, 4, 5] {
        assert!(
            tokens
                .iter()
                .all(|(token_line, character, _, _)| *token_line != line || *character != 4),
            "output reference on line {} should not have a semantic token",
            line + 1
        );
    }

    session.replace("1\n\"hidden\";\na = oo\n").await;
    assert_eq!(hover_type_at(&mut session, 2, 0).await, "ZZ");

    session.replace("o100 := \"hello\"\nx = o100\n").await;
    assert_eq!(hover_type_at(&mut session, 1, 0).await, "String");
    let tokens = semantic_tokens(&mut session).await;
    assert!(
        tokens
            .iter()
            .any(|(line, character, _, _)| *line == 1 && *character == 4),
        "a resolved user binding named like an output reference must retain its semantic token"
    );

    session
        .replace("x = oo\ny = o0\nz = o9\nw = oooo\nsymbol oo\n")
        .await;
    for line in 0..=3 {
        assert_eq!(hover_type_at(&mut session, line, 0).await, "Symbol");
    }

    session.replace("ooo =!= null\n").await;
    assert!(
        session.diagnostics().is_empty(),
        "an unavailable output reference is a valid unassigned Symbol: {:?}",
        session.diagnostics()
    );

    session.shutdown().await;
}

#[tokio::test]
async fn installation_and_syntax_diagnostics_run_through_the_server_pipeline() {
    let mut session = DocumentSession::open("ZZ > ZZ := (a, b) -> a\n").await;
    assert!(session.diagnostic_codes().contains(&"E02"));

    for (source, code, expected) in [
        ("ZZ * ZZ := (a, b) -> a\n", "E02", false),
        ("ZZ * ZZ := (a) -> a\n", "E03", true),
        ("ZZ * ZZ := a -> a\n", "E03", false),
        ("ZZ * ZZ = (a, b, c) -> c\n", "E03", false),
        ("ZZ * ZZ = (a, b) -> a\n", "E03", true),
        ("f = x -> x\nf ZZ := y -> y\n", "E01", true),
        ("f = method()\nf ZZ := y -> y\n", "E01", false),
        ("f = first {ideal}\nf ZZ := y -> y\n", "E01", false),
        ("X ?? Y := (x, y) -> x\n", "E01", true),
        ("?? X := x -> x\n", "E01", false),
        ("f = method()\nf ZZ = x -> x\n", "E04", true),
        ("f = x -> x\nf ZZ = y -> y\n", "E04", true),
        ("f = 1\nf ZZ = y -> y\n", "E04", false),
        ("f = method()\nf ZZ := x -> x\n", "E04", false),
        ("ZZ * ZZ = (a, b, c) -> c\n", "E04", false),
        ("if x then y\n    else z", "X01", true),
        (
            "apply(-3..3, i -> try 1/i then 1 / i except err do err)",
            "X01",
            false,
        ),
        ("if x then y else z", "X01", false),
        ("if x then y", "X01", false),
        ("gb(I, strategy => 4)\n", "S01", true),
        ("hashTable {a => 1, b => 2}\n", "S01", false),
        ("x.3\n", "X03", true),
        ("x .3\n", "X03", false),
    ] {
        replace_and_assert_diagnostic(&mut session, source, code, expected).await;
    }

    session.replace("X ?? Y := (x, y) -> x\n").await;
    let diagnostic = session
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic["code"] == "E01")
        .expect("binary null-coalescing installation should be diagnosed");
    assert_eq!(diagnostic["severity"], 2);
    assert!(diagnostic["message"]
        .as_str()
        .is_some_and(|message| message.contains("never dispatches")));

    session.shutdown().await;
}

#[tokio::test]
async fn install_method_calls_contribute_explicit_installations() {
    let source = concat!(
        "f = method(TypicalValue => List)\n",
        "installMethod(f, ZZ, x -> [x])\n",
        "result = f 1\n",
        "result\n",
        "Accumulator = new Type\n",
        "installMethod(symbol +=, Accumulator, (left, right) -> left)\n",
        "installMethod(symbol <-, String, peek)\n",
    );
    let mut session = DocumentSession::open(source).await;

    assert_eq!(hover_type_at(&mut session, 3, 0).await, "List");
    assert!(
        !session.diagnostic_codes().contains(&"E02"),
        "explicit operator installation must not require Flexible: {:?}",
        session.diagnostics()
    );
    assert!(
        !session.diagnostic_codes().contains(&"E03"),
        "explicit installation arity should follow the installed callable form: {:?}",
        session.diagnostics()
    );
    assert!(!session.diagnostic_codes().contains(&"E09"));

    let tokens = semantic_tokens(&mut session).await;
    let type_parameter = session.semantic_token_type("typeParameter");
    for line in [1, 5, 6] {
        assert!(
            tokens
                .iter()
                .any(|(token_line, _, token_type, _)| *token_line == line
                    && *token_type == type_parameter),
            "missing explicit installation type role on line {}: {tokens:?}",
            line + 1
        );
    }

    for source in [
        "String <- Thing := (left, right) -> left\n",
        "(String <- Thing) := (left, right) -> left\n",
    ] {
        session.replace(source).await;
        assert_eq!(diagnostic_lines(&session, "E09"), vec![0]);
    }

    session.shutdown().await;
}

#[tokio::test]
async fn new_expressions_are_method_installation_heads() {
    let source = concat!(
        "M = new Type of BasicList\n",
        "new Type of BasicList from Function := (target, parent, convert) -> hashTable {}\n",
        "new M from (ZZ, ZZ) := (target, left, right) -> {left, right}\n",
        "new M := target -> {}\n",
    );
    let mut session = DocumentSession::open(source).await;
    assert!(
        !session.diagnostic_codes().contains(&"E03"),
        "valid new installations should have their complete runtime arity: {:?}",
        session.diagnostics()
    );

    session
        .replace("M = new Type\nnew M from ZZ := (value) -> value\n")
        .await;
    assert_eq!(diagnostic_lines(&session, "E03"), vec![1]);
    session.shutdown().await;
}

#[tokio::test]
async fn lexical_types_and_operator_methods_respect_scope_and_source_order() {
    let source = concat!(
        "f := () -> (T := new Type of HashTable; T)\n",
        "T ZZ = (a, b, c, d) -> a\n",
        "if 1 + 2 then 3\n",
        "ZZ + ZZ := Boolean => (a, b) -> true\n",
    );
    let session = DocumentSession::open(source).await;

    assert_eq!(diagnostic_lines(&session, "E03"), Vec::<u64>::new());
    assert_eq!(diagnostic_lines(&session, "T02"), vec![2]);

    session.shutdown().await;
}

#[tokio::test]
async fn unresolved_package_types_do_not_leak_into_core_completion() {
    let mut session = DocumentSession::open("Simplicial\n").await;
    let labels = session.completion_labels("Simplicial", 0).await;

    assert!(!labels
        .iter()
        .any(|label| { matches!(label.as_str(), "SimplicialComplex" | "SimplicialMap") }));

    session.shutdown().await;
}

#[tokio::test]
async fn ring_constructor_aliases_resolve_in_their_lexical_scope() {
    let source = "f := () -> (P := QQ; R := P[x]; x)\n";
    let mut session = DocumentSession::open(source).await;

    assert_eq!(hover_type_at(&mut session, 0, 32).await, "R");

    session.shutdown().await;
}

#[tokio::test]
async fn inlay_hint_ranges_are_end_exclusive() {
    let source = "x = 1\ny = 2\n";
    let mut session = DocumentSession::open(source).await;
    let hints = session
        .request(
            "textDocument/inlayHint",
            json!({
                "textDocument": {"uri": session.uri()},
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 1, "character": 1}
                }
            }),
        )
        .await;

    assert!(response_array(&hints)
        .iter()
        .all(|hint| { hint["position"] != json!({"line": 1, "character": 1}) }));

    session.shutdown().await;
}

#[tokio::test]
async fn method_codomain_diagnostics_offer_annotation_quick_fixes() {
    let mut session = DocumentSession::open("f ZZ := x -> [x]\n").await;
    let missing = session
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic["code"] == "T03")
        .expect("a directly deduced missing codomain should produce a hint")
        .clone();
    assert_eq!(missing["severity"], 4);
    assert!(missing["message"]
        .as_str()
        .is_some_and(|message| message.contains("`Array`")));
    assert_eq!(
        missing["range"],
        json!({
            "start": {"line": 0, "character": 0},
            "end": {"line": 0, "character": 4}
        })
    );

    let uri = session.uri().to_string();
    let actions = session
        .request(
            "textDocument/codeAction",
            json!({
                "textDocument": {"uri": uri},
                "range": missing["range"],
                "context": {"diagnostics": [missing]}
            }),
        )
        .await;
    let add = response_array(&actions)
        .iter()
        .find(|action| action["title"] == "Add codomain annotation")
        .expect("the missing-codomain hint should carry a quick fix");
    assert_eq!(add["kind"], "quickfix");
    assert_eq!(add["edit"]["changes"][&uri][0]["newText"], "Array => ");
    assert_eq!(
        add["edit"]["changes"][&uri][0]["range"],
        json!({
            "start": {"line": 0, "character": 8},
            "end": {"line": 0, "character": 8}
        })
    );

    session.replace("f ZZ := List => x -> [x]\n").await;
    let mismatch = session
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic["code"] == "T04")
        .expect("an incompatible explicit codomain should produce a warning")
        .clone();
    assert_eq!(mismatch["severity"], 2);

    let actions = session
        .request(
            "textDocument/codeAction",
            json!({
                "textDocument": {"uri": uri},
                "range": mismatch["range"],
                "context": {"diagnostics": [mismatch]}
            }),
        )
        .await;
    assert!(actions.is_null());

    for source in [
        "f ZZ := Array => x -> [x]\n",
        "f ZZ := VisibleList => x -> [x]\n",
        "f ZZ := x -> x\n",
    ] {
        session.replace(source).await;
        assert!(
            !session.diagnostic_codes().contains(&"T04"),
            "a correct or compatible codomain must not warn: {:?}",
            session.diagnostics()
        );
    }
    assert!(session.diagnostic_codes().contains(&"T03"));
    assert!(session
        .diagnostics()
        .iter()
        .any(|diagnostic| diagnostic["code"] == "T03"
            && diagnostic["message"]
                .as_str()
                .is_some_and(|message| message.contains("`ZZ`"))));

    session.replace("f ZZ := x -> mystery x\n").await;
    assert!(!session.diagnostic_codes().contains(&"T03"));

    session.shutdown().await;
}

#[tokio::test]
async fn nonexact_call_arguments_join_every_possible_dispatch_codomain() {
    let source = concat!(
        "children CstNode := List => node -> apply(childRawIndices node, rawIndex ->\n",
        "    cstNodeAt(node.CstOwner, append(node.CstPath, rawIndex)))\n",
    );
    let mut session = DocumentSession::open(source).await;

    assert!(
        !session.diagnostic_codes().contains(&"T04"),
        "an argument inferred only as Thing must not select the exact Thing overload: {:?}",
        session.diagnostics()
    );

    let hover = session.request_at("textDocument/hover", "apply", 0).await;
    let markdown = hover["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("expected apply hover markdown, got {hover}"));
    assert!(
        !markdown.starts_with("**apply** `(Thing, Function) -> Iterator`"),
        "a nonexact Thing range must not pin the Thing overload: {markdown}"
    );
    for signature in [
        "`(BasicList, Function) -> BasicList`",
        "`(ZZ, Function) -> List`",
        "`(String, Function) -> Sequence`",
        "`(Thing, Function) -> Iterator`",
    ] {
        assert!(
            markdown.contains(signature),
            "the possible dispatch range should contain {signature}: {markdown}"
        );
    }

    session
        .replace(concat!(
            "rangeResult = apply(mystery value, identity)\n",
            "integerResult = apply(3, identity)\n",
            "stringResult = apply(\"x\", identity)\n",
            "rangeResult\n",
            "integerResult\n",
            "stringResult\n",
        ))
        .await;
    assert_eq!(
        hover_type_at(&mut session, 3, 0).await,
        "BasicList | Iterator"
    );
    assert_eq!(hover_type_at(&mut session, 4, 0).await, "List");
    assert_eq!(hover_type_at(&mut session, 5, 0).await, "Sequence");

    session.shutdown().await;
}

#[tokio::test]
async fn declarative_diagnostics_expose_their_coupled_quick_fixes() {
    let cases = [
        ("x#0 := 1\n", "X05", "Use `=` for part assignment", "="),
        (
            "f = method()\nf ZZ = x -> x\n",
            "E04",
            "Use `:=` for method installation",
            ":=",
        ),
        (
            "x = 1\nprotect x\n",
            "E05",
            "Protect the symbol itself",
            "symbol ",
        ),
        (
            "gb(I, strategy => 4)\n",
            "S01",
            "Capitalize option key",
            "Strategy",
        ),
        (
            "if (condition) then x else y\n",
            "S03",
            "Remove redundant parentheses",
            "condition",
        ),
        (
            "value = if value === null then fallback else value\n",
            "S04",
            "Use `??=` coalescing assignment",
            "value ??= fallback",
        ),
        (
            "if candidate =!= null then candidate else fallback\n",
            "S04",
            "Use `??` coalescence",
            "candidate ?? fallback",
        ),
    ];
    let mut session = DocumentSession::open(cases[0].0).await;

    for (source, code, title, replacement) in cases {
        session.replace(source).await;
        let diagnostic = session
            .diagnostics()
            .iter()
            .find(|diagnostic| diagnostic["code"] == code)
            .unwrap_or_else(|| panic!("expected {code} for `{source}`"))
            .clone();
        let uri = session.uri().to_string();
        let response = session
            .request(
                "textDocument/codeAction",
                json!({
                    "textDocument": {"uri": uri.clone()},
                    "range": diagnostic["range"],
                    "context": {"diagnostics": [diagnostic]}
                }),
            )
            .await;
        assert!(
            !response.is_null(),
            "expected `{title}` action for `{source}`"
        );
        let action = response_array(&response)
            .iter()
            .find(|action| action["title"] == title)
            .unwrap_or_else(|| panic!("expected `{title}` action for `{source}`"));
        assert_eq!(action["edit"]["changes"][&uri][0]["newText"], replacement);
    }

    session.shutdown().await;
}

#[tokio::test]
async fn expression_simplification_actions_are_backed_by_hints() {
    let cases = [
        (
            "if ready then value else null\n",
            "Simplify unnecessary null branch",
        ),
        (
            "if not (left == right) then value\n",
            "Simplify if condition",
        ),
        (
            "if outer then one else (if inner then two else three)\n",
            "Flatten nested if into else-if chain",
        ),
        ("try value then value\n", "Simplify try"),
    ];
    let mut session = DocumentSession::open(cases[0].0).await;

    for (source, title) in cases {
        session.replace(source).await;
        let diagnostic = session
            .diagnostics()
            .iter()
            .find(|diagnostic| diagnostic["code"] == "S05")
            .unwrap_or_else(|| panic!("expected a simplification hint for `{source}`"))
            .clone();
        assert_eq!(diagnostic["severity"], 4);

        let uri = session.uri().to_string();
        let response = session
            .request(
                "textDocument/codeAction",
                json!({
                    "textDocument": {"uri": uri},
                    "range": diagnostic["range"],
                    "context": {"diagnostics": [diagnostic]}
                }),
            )
            .await;
        let action = response_array(&response)
            .iter()
            .find(|action| action["title"] == title)
            .unwrap_or_else(|| panic!("expected `{title}` for `{source}`"));
        assert_eq!(action["diagnostics"][0]["code"], "S05");
    }

    session.replace("if ready then value\n").await;
    assert!(!session.diagnostic_codes().contains(&"S05"));
    session.shutdown().await;
}

#[tokio::test]
async fn control_transfers_require_their_runtime_body() {
    let mut session = DocumentSession::open("f := x -> return x\n").await;

    for (source, expected) in [
        ("f := x -> return x\n", false),
        ("return x\n", true),
        ("if c then break\n", true),
        ("for i from (break i) to 3 list i\n", true),
        ("for i to 3 list if i > 1 then break i\n", false),
        ("for i to 3 do if i > 1 then break i\n", false),
        ("while c do break c\n", false),
        ("for i to 3 list continue i\n", false),
        ("for i to 3 do continue\n", false),
        ("for i to 3 do continue i\n", true),
        ("while c list continue c\n", false),
        ("while c do continue c\n", true),
        ("apply(0..3, i -> continue i)\n", true),
        ("apply({1}, {2}, (i, j) -> continue(i + j))\n", true),
        ("scan(0..3, i -> continue)\n", true),
        ("scan(0..3, i -> continue i)\n", true),
        ("apply(0..3, i -> break i)\n", false),
        ("scan(0..3, i -> break i)\n", false),
        ("f := i -> break i\n", true),
        ("for i to 3 do (f := x -> break; f i)\n", true),
        ("for i to 3 list (for j to 3 do continue i)\n", true),
        ("apply(i -> break i, {1})\n", true),
        ("f := apply -> apply({1}, i -> break i)\n", true),
    ] {
        replace_and_assert_diagnostic(&mut session, source, "E08", expected).await;
        assert!(
            !session.diagnostic_codes().contains(&"X01"),
            "control-transfer fixture should parse:\n{source}\nall diagnostics: {:?}",
            session.diagnostics()
        );
    }

    session.replace("scan(0..3, i -> continue i)\n").await;
    let diagnostic = session
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic["code"] == "E08")
        .expect("value-bearing continue in scan should be rejected");
    assert_eq!(diagnostic["severity"], 1);
    assert!(diagnostic["message"]
        .as_str()
        .is_some_and(|message| message.contains("requires a `list` clause")));

    session.shutdown().await;
}

#[tokio::test]
async fn assignment_and_protection_diagnostics_preserve_source_sensitive_analysis() {
    let source = concat!(
        "[x, y] = [a, b, c]\n",
        "[p, q] = {r}\n",
        "[m, n] = (s)\n",
        "[u, v] = [g, h]\n",
        "[i, [j, k]] = [1, {2, 3, 4}]\n",
        "[c, d] = ()\n",
        "[e, f] = (a, b)\n",
    );
    let mut session = DocumentSession::open(source).await;
    assert_eq!(diagnostic_lines(&session, "X06"), vec![0, 1, 4, 5]);

    session
        .replace("(x, (y, z, z), w) = (1, [2, \"3\"], 3)\n")
        .await;
    let diagnostic = session
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic["code"] == "X06")
        .expect("nested assignment shape mismatch should be diagnosed");
    assert_eq!(
        diagnostic["range"],
        json!({
            "start": {"line": 0, "character": 4},
            "end": {"line": 0, "character": 13}
        })
    );

    session
        .replace("[x, y] = [ignored; 2, 3]\n[x, y, z] = [, 1,]\n[x, y] = [, 1,]\n")
        .await;
    assert_eq!(diagnostic_lines(&session, "X06"), vec![2]);

    session
        .replace(
            "(x, y) = 1\n[x, y] := \"aa\"\nz = \"a\"; {x, x} = z\n[a, b] = true\n[x] = \"a\"\nf = z -> ((x, y) := z)\n(x, y) := unknownValue\nvalues = {1, 2}; [x, y] = values\nvalues = (1, 2); [x, y] = values\n[a, [b, c]] = [1, 2]\n",
        )
        .await;
    assert_eq!(diagnostic_lines(&session, "T01"), vec![0, 1, 2, 3, 9]);

    session
        .replace(
            "x#i := e\n(x+1,y) = (1,2)\n(x+1,y) := (1,2)\n(f()) <- (1)\nsource(String,Number) := peek\np(ZZ, ZZ) := (i, j) -> {i, j}\n",
        )
        .await;
    assert_eq!(diagnostic_lines(&session, "X05"), vec![0]);
    assert_eq!(diagnostic_lines(&session, "X04"), vec![1, 2]);

    session
        .replace(
            "assigned = target\nprotect assigned\nprotect unassigned\nprotect later\nlater = target\n",
        )
        .await;
    assert_eq!(diagnostic_lines(&session, "E05"), vec![1]);

    session
        .replace(
            "x = y\nprotect symbol x\nprotect (if c then symbol x else symbol y)\nprotect (if c then 1 else symbol y)\nprotect (1 + 2)\n",
        )
        .await;
    assert_eq!(diagnostic_lines(&session, "E06"), vec![2, 3]);

    session.replace("f = x -> protect x\n").await;
    assert_eq!(diagnostic_lines(&session, "E05"), vec![0]);

    session
        .replace("protect ZZ\nx = y\nprotect = f\nprotect x\n")
        .await;
    assert_eq!(diagnostic_lines(&session, "E05"), vec![0]);

    session.replace("f := x -> x\nx = 1\n").await;
    assert!(!session.diagnostic_codes().contains(&"S02"));

    session
        .replace(
            "if condition then (\n  conditionalExport = true;\n  branchLocal := 1;\n);\nif conditionalExport == true then null\n",
        )
        .await;
    assert_eq!(diagnostic_lines(&session, "S02"), vec![2]);

    session.shutdown().await;
}

#[tokio::test]
async fn callback_equal_assignments_are_not_unused_variables() {
    let source = concat!(
        "A = apply(A, e -> e_(T | toList(n..(n + d - 1))));\n",
        "-- successive projections eliminate the variables 'T'.\n",
        "if A =!= {} then\n",
        "    scan(T, t -> (\n",
        "        D := fourierMotzkin'(A, V, t);\n",
        "        A = D#0;\n",
        "        V = D#1;\n",
        "    )\n",
        "    );\n",
        "-- output formatting\n",
        "A = apply(A, e -> primitive e);\n",
    );
    let mut session = DocumentSession::open(source).await;
    assert!(
        !session.diagnostic_codes().contains(&"S02"),
        "escaping callback assignments must not be reported as unused: {:?}",
        session.diagnostics()
    );

    session.replace("f = x -> (unused := x; x)\n").await;
    let diagnostic = session
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic["code"] == "S02")
        .expect("an unused lambda-local variable should still be reported");
    assert_eq!(diagnostic["message"], "Unused variable unused");

    session.shutdown().await;
}

#[tokio::test]
async fn inferred_binding_types_flow_to_hover_across_language_constructs() {
    let mut session = DocumentSession::open("value := 1\nvalue\n").await;
    assert_eq!(hover_type_at(&mut session, 1, 0).await, "ZZ");

    for (source, probes) in [
        (
            "clearAll = new Command from { () -> () }\nclearAll\n",
            vec![(1, 0, "Command")],
        ),
        (
            "I := new Ideal from {}\nR := ring I\nS := ring unknownName\nR\nS\n",
            vec![(3, 0, "Ring"), (4, 0, "Ring")],
        ),
        (
            "x := 1\ny := 2\nz := x + y\nz\n",
            vec![(3, 0, "ZZ")],
        ),
        (
            "listed := for i to 3 list i\ndone := for j to 3 do j\nlooped := while condition do 1\nlisted\ndone\nlooped\n",
            vec![(3, 0, "List"), (4, 0, "Nothing"), (5, 0, "Nothing")],
        ),
        (
            "p = method(Binary => true, TypicalValue => List)\np(ZZ,ZZ) := p(List,ZZ) := (i,j) -> {i,j}\nx := p(1, 2)\nx\n",
            vec![(3, 0, "List")],
        ),
        (
            "f = method()\nf ZZ := x -> -x\ny := f 1\ny\n",
            vec![(3, 0, "Thing")],
        ),
        (
            "f = method(TypicalValue => List)\nf ZZ := Ring => x -> x\ny := f 1\ny\n",
            vec![(3, 0, "Ring")],
        ),
        (
            "f = method()\nf ZZ := String => x -> \"\"\nf RR := Boolean => x -> true\nargument := if condition then 1 else 2.0\nresult := f argument\nresult\n",
            vec![(5, 0, "Boolean | String")],
        ),
        (
            "argument := if condition then 1 else 2.0\nresult := argument + argument\nresult\n",
            vec![(2, 0, "RR | ZZ")],
        ),
        (
            "x = 1\nx = x + 1\nx\n",
            vec![(2, 0, "ZZ")],
        ),
        (
            "a := new CCi\nb := a + a\nb\n",
            vec![(2, 0, "CCi")],
        ),
        ("y := unindexedName\ny\n", vec![(1, 0, "Symbol")]),
        (
            "l := {1,2}\na := [1,2]\nb := <|1,2|>\ne := ()\nf := (1)\ng := (1,2)\nl\na\nb\ne\nf\ng\n",
            vec![
                (6, 0, "List"),
                (7, 0, "Array"),
                (8, 0, "AngleBarList"),
                (9, 0, "Sequence"),
                (10, 0, "ZZ"),
                (11, 0, "Sequence"),
            ],
        ),
    ] {
        session.replace(source).await;
        for (line, character, expected) in probes {
            assert_eq!(
                hover_type_at(&mut session, line, character).await,
                expected,
                "unexpected inferred type at {line}:{character} for:\n{source}"
            );
        }
    }

    session.replace("I = ideal(12,18)\nI\n").await;
    let ideal_signature = session
        .request(
            "textDocument/signatureHelp",
            json!({
                "textDocument": {"uri": session.uri()},
                "position": {"line": 0, "character": 11}
            }),
        )
        .await;
    assert_eq!(
        hover_type_at(&mut session, 1, 0).await,
        "Ideal",
        "ideal call signature: {ideal_signature}"
    );

    for (source, expected) in [
        (
            "joined := if condition then 1 else 2.0\njoined\n",
            "RR | ZZ",
        ),
        ("joined := if condition then 1\njoined\n", "ZZ?"),
        ("joined := if condition then null else 1\njoined\n", "ZZ?"),
        (
            "joined := try unknownName then 1 else 2.0\njoined\n",
            "RR | ZZ",
        ),
        (
            "joined := if condition then 1 else if other then 2.0\njoined\n",
            "Nothing | RR | ZZ",
        ),
        ("fallback := try 1\nfallback\n", "ZZ?"),
    ] {
        session.replace(source).await;
        let labels = inlay_labels(&mut session).await;
        assert!(
            labels.iter().any(|label| label == expected),
            "expected {expected:?} in inlay labels for:\n{source}\ngot {labels:?}"
        );
    }

    session.shutdown().await;
}

#[tokio::test]
async fn callable_aliases_reinstall_methods_and_reassignments_remain_source_ordered() {
    let source = concat!(
        "f = method()\n",
        "f Thing := x -> x\n",
        "g = f\n",
        "f ZZ := QQ => x -> x / 2\n",
        "x = g 1\n",
        "y = f 1\n",
        "g ZZ := ZZ => x -> x * 2\n",
        "z = g 1\n",
        "w = f 1\n",
        "f ZZ := Array => x -> [x]\n",
        "x = f 1\n",
        "y = g 1\n",
        "u = x\n",
        "v = y\n",
    );
    let mut session = DocumentSession::open(source).await;

    for (line, expected) in [
        (4, "QQ"),
        (5, "QQ"),
        (7, "ZZ"),
        (8, "ZZ"),
        (10, "Array"),
        (11, "Array"),
        (12, "Array"),
        (13, "Array"),
    ] {
        assert_eq!(
            hover_type_at(&mut session, line, 0).await,
            expected,
            "unexpected binding type on line {}",
            line + 1
        );
    }

    let hints = inlay_labels_by_line(&mut session).await;
    for line in [10, 11] {
        assert!(
            hints
                .iter()
                .any(|(hint_line, label)| *hint_line == line && label == "Array"),
            "reassignment on line {} should have an Array hint: {hints:?}",
            line + 1
        );
    }

    session.replace("x = 1\na = x\nx = \"a\"\nb = x\n").await;
    for line in [1, 3] {
        let declaration = session
            .request(
                "textDocument/declaration",
                json!({
                    "textDocument": {"uri": session.uri()},
                    "position": {"line": line, "character": 4}
                }),
            )
            .await;
        assert_eq!(declaration["range"]["start"]["line"], 0);

        let definitions = session
            .request(
                "textDocument/definition",
                json!({
                    "textDocument": {"uri": session.uri()},
                    "position": {"line": line, "character": 4}
                }),
            )
            .await;
        assert_eq!(
            response_array(&definitions)
                .iter()
                .map(|location| location["range"]["start"]["line"]
                    .as_u64()
                    .expect("definition line"))
                .collect::<Vec<_>>(),
            [0, 2]
        );
    }

    session.shutdown().await;
}

#[tokio::test]
async fn declarations_use_binding_identity_then_fall_back_to_the_first_unbound_reference() {
    let source = concat!(
        "future\n",
        "future = 1\n",
        "future\n",
        "f = name -> name\n",
        "g = name -> name\n",
        "unassigned\n",
        "h = unassigned -> unassigned\n",
        "unassigned\n",
        "ZZ\n",
        "ZZ\n",
    );
    let mut session = DocumentSession::open(source).await;

    for occurrence in [0, 2] {
        let declaration = session
            .request_at("textDocument/declaration", "future", occurrence)
            .await;
        assert_eq!(declaration["range"]["start"], position(source, "future", 1));
    }
    let future_definitions = session
        .request_at("textDocument/definition", "future", 0)
        .await;
    assert_eq!(
        future_definitions["range"]["start"],
        position(source, "future", 1)
    );

    for (occurrence, declaration_occurrence) in [(1, 0), (3, 2)] {
        let declaration = session
            .request_at("textDocument/declaration", "name", occurrence)
            .await;
        assert_eq!(
            declaration["range"]["start"],
            position(source, "name", declaration_occurrence)
        );
    }

    let local_declaration = session
        .request_at("textDocument/declaration", "unassigned", 2)
        .await;
    assert_eq!(
        local_declaration["range"]["start"],
        position(source, "unassigned", 1)
    );

    for occurrence in [0, 3] {
        let declaration = session
            .request_at("textDocument/declaration", "unassigned", occurrence)
            .await;
        assert_eq!(
            declaration["range"]["start"],
            position(source, "unassigned", 0)
        );
    }

    let indexed_declaration = session
        .request_at("textDocument/declaration", "ZZ", 1)
        .await;
    assert_eq!(
        indexed_declaration["range"]["start"],
        position(source, "ZZ", 0)
    );

    session.shutdown().await;
}

#[tokio::test]
async fn type_definitions_and_method_implementations_use_analysis_facts() {
    let source = concat!(
        "Token = new Type\n",
        "value = new Token\n",
        "value\n",
        "p = method()\n",
        "p(Token) := x -> x\n",
        "p(ZZ) := x -> x\n",
        "p\n",
    );
    let mut session = DocumentSession::open(source).await;

    let type_definition = session
        .request_at("textDocument/typeDefinition", "value", 1)
        .await;
    assert_eq!(
        type_definition["range"]["start"],
        position(source, "Token", 0)
    );

    let implementations = session
        .request_at("textDocument/implementation", "p", 3)
        .await;
    assert_eq!(
        response_array(&implementations)
            .iter()
            .map(|location| location["range"]["start"]["line"]
                .as_u64()
                .expect("implementation line"))
            .collect::<Vec<_>>(),
        [4, 5]
    );

    session.shutdown().await;
}

#[tokio::test]
async fn implementations_are_workspace_scoped_method_installations_or_lambda_assignments() {
    let source = concat!(
        "p = method()\n",
        "p(ZZ) := x -> x\n",
        "p(QQ) := x -> x\n",
        "p\n",
        "p 1\n",
        "f = x -> x\n",
        "f = x -> x + 1\n",
        "f = 1\n",
        "f\n",
        "colonLambda := (x -> x)\n",
        "colonLambda\n",
        "outer = x -> x\n",
        "g = outer -> (\n",
        "    outer = y -> y;\n",
        "    outer\n",
        ")\n",
        "outer\n",
        "workspaceLambda\n",
        "workspaceMethod\n",
        "ghost\n",
        "ideal\n",
    );
    let related = concat!(
        "workspaceLambda := (x -> x)\n",
        "workspaceMethod = method()\n",
        "workspaceMethod(ZZ) := x -> x\n",
        "p(RR) := x -> x\n",
        "ghost(ZZ) := x -> x\n",
    );
    let mut session = DocumentSession::open_with_related(source, related).await;

    let methods = session
        .request_at("textDocument/implementation", "p", 3)
        .await;
    let methods = response_array(&methods);
    assert_eq!(methods.len(), 3);
    assert_eq!(
        methods
            .iter()
            .filter(|location| location["uri"] == session.uri())
            .map(|location| location["range"]["start"].clone())
            .collect::<Vec<_>>(),
        [position(source, "p", 1), position(source, "p", 2)]
    );
    assert!(methods.iter().any(
        |location| location["uri"] != session.uri() && location["range"]["start"]["line"] == 3
    ));

    let pinned_method = session
        .request_at("textDocument/implementation", "p", 4)
        .await;
    assert_eq!(pinned_method["range"]["start"], position(source, "p", 1));

    let lambdas = session
        .request_at("textDocument/implementation", "f", 3)
        .await;
    assert_eq!(
        response_array(&lambdas)
            .iter()
            .map(|location| location["range"]["start"].clone())
            .collect::<Vec<_>>(),
        [position(source, "f", 0), position(source, "f", 1)]
    );

    let colon_lambda = session
        .request_at("textDocument/implementation", "colonLambda", 1)
        .await;
    assert_eq!(
        colon_lambda["range"]["start"],
        position(source, "colonLambda", 0)
    );

    let local_lambda = session
        .request_at("textDocument/implementation", "outer", 3)
        .await;
    assert_eq!(local_lambda["range"]["start"], position(source, "outer", 2));
    let global_lambda = session
        .request_at("textDocument/implementation", "outer", 4)
        .await;
    assert_eq!(
        global_lambda["range"]["start"],
        position(source, "outer", 0)
    );

    for (name, implementation_line) in [("workspaceLambda", 0), ("workspaceMethod", 2)] {
        let implementation = session
            .request_at("textDocument/implementation", name, 0)
            .await;
        assert_ne!(implementation["uri"], session.uri());
        assert_eq!(
            implementation["range"]["start"]["line"],
            implementation_line
        );
    }

    let library_implementation = session
        .request_at("textDocument/implementation", "ideal", 0)
        .await;
    assert!(library_implementation.is_null());

    let invalid_installation = session
        .request_at("textDocument/implementation", "ghost", 0)
        .await;
    assert!(invalid_installation.is_null());

    session.shutdown().await;
}

#[tokio::test]
async fn workspace_method_declarations_validate_live_unresolved_installations() {
    let source = "p(ZZ) := x -> x\np\n";
    let mut session = DocumentSession::open_with_related(source, "p = method()\n").await;

    let implementation = session
        .request_at("textDocument/implementation", "p", 1)
        .await;
    assert_eq!(implementation["uri"], session.uri());
    assert_eq!(implementation["range"]["start"], position(source, "p", 0));

    session.shutdown().await;
}

#[tokio::test]
async fn method_calls_exclude_installations_that_occur_later() {
    let source = "p = method()\np 1\np(ZZ) := x -> x\np\n";
    let mut session = DocumentSession::open(source).await;

    let call_implementation = session
        .request_at("textDocument/implementation", "p", 1)
        .await;
    assert!(call_implementation.is_null());

    let method_implementations = session
        .request_at("textDocument/implementation", "p", 3)
        .await;
    assert_eq!(
        method_implementations["range"]["start"],
        position(source, "p", 2)
    );

    session.shutdown().await;
}

#[tokio::test]
async fn document_links_resolve_and_completion_surfaces_contextual_choices() {
    let source = concat!(
        "-- header\n",
        "x = 1\n",
        "-- use [[x]]\n",
        "request = needsPackage \"J\"\n",
        "determinant(Str)\n",
        "\n",
    );
    let mut session = DocumentSession::open(source).await;

    let links = session
        .request(
            "textDocument/documentLink",
            json!({"textDocument": {"uri": session.uri()}}),
        )
        .await;
    let local_link = response_array(&links)
        .iter()
        .find(|link| link["tooltip"] == "Open `x`")
        .expect("the documentation reference should be a document link")
        .clone();
    assert!(local_link["target"].is_null());
    let resolved = session.request("documentLink/resolve", local_link).await;
    assert!(
        resolved["target"]
            .as_str()
            .is_some_and(|target| target.ends_with("#L2,1")),
        "resolved local link should retain the definition position: {resolved}"
    );

    let package_completions = session.completion_labels("J", 0).await;
    assert!(package_completions.iter().any(|label| label == "JSON"));
    let option_completions = session.completion_labels("Str", 0).await;
    assert!(option_completions.iter().any(|label| label == "Strategy"));

    let empty_slot = session
        .request(
            "textDocument/completion",
            json!({
                "textDocument": {"uri": session.uri()},
                "position": {"line": 5, "character": 0}
            }),
        )
        .await;
    assert!(empty_slot.is_null());

    session.shutdown().await;
}

#[tokio::test]
async fn attached_markdown_is_visible_in_hover() {
    let source = "-- Computes **the answer** with [[ideal]].\nanswer := 42\nanswer\n";
    let mut session = DocumentSession::open(source).await;

    let hover = session.request_at("textDocument/hover", "answer", 1).await;
    assert!(
        hover["contents"]["value"]
            .as_str()
            .is_some_and(|markdown| markdown.contains("Computes **the answer** with `ideal`.")),
        "attached documentation should be part of the binding hover: {hover}"
    );

    session.shutdown().await;
}

#[tokio::test]
async fn attached_markdown_excludes_nested_assignments() {
    let source = concat!(
        "-- Documents only f.\n",
        "f := x -> (g := y -> y; g x)\n",
        "f\n",
    );
    let mut session = DocumentSession::open(source).await;

    let outer = session.request_at("textDocument/hover", "f", 1).await;
    assert!(
        outer["contents"]["value"]
            .as_str()
            .is_some_and(|markdown| markdown.contains("Documents only f.")),
        "the directly following binding should own the documentation: {outer}"
    );

    let nested = session.request_at("textDocument/hover", "g", 0).await;
    assert!(
        nested["contents"]["value"]
            .as_str()
            .is_some_and(|markdown| !markdown.contains("Documents only f.")),
        "a nested binding must not inherit the outer binding's documentation: {nested}"
    );

    session.shutdown().await;
}

#[tokio::test]
async fn method_installation_documentation_stays_with_its_signature() {
    let source = concat!(
        "p = method()\n",
        "-- Handles integers.\n",
        "p ZZ := value -> value\n",
        "p\n",
    );
    let mut session = DocumentSession::open(source).await;

    let installation = session.request_at("textDocument/hover", "p", 1).await;
    assert!(
        installation["contents"]["value"]
            .as_str()
            .is_some_and(|markdown| markdown.contains("Handles integers.")),
        "installation hover should include its own docs: {installation}"
    );

    let function = session.request_at("textDocument/hover", "p", 2).await;
    assert!(
        function["contents"]["value"]
            .as_str()
            .is_some_and(|markdown| !markdown.contains("Handles integers.")),
        "installation docs should not replace the method function docs: {function}"
    );

    session.shutdown().await;
}

#[test]
fn documentation_command_generates_only_attached_pages() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should follow the Unix epoch")
        .as_nanos();
    let fixture = std::env::temp_dir().join(format!("m2-ls-docs-{}-{nonce}", std::process::id()));
    let source_dir = fixture.join("source");
    let output_dir = fixture.join("book");
    fs::create_dir_all(&source_dir).expect("temporary source directory should be created");
    fs::write(
        source_dir.join("Package.m2"),
        concat!(
            "-* Package **documentation**. *-\n",
            "newPackage(\"Package\")\n",
            "-- Computes [[helper]].\n",
            "answer := 42\n",
            "-- Helps with examples.\n",
            "helper := 1\n",
            "\n",
            "-- Detached ordinary comment.\n",
        ),
    )
    .expect("documentation fixture should be written");
    fs::write(
        source_dir.join("NotPackage.m2"),
        "-* This is not package documentation. *-\nx = newPackage\n",
    )
    .expect("non-package fixture should be written");

    // The Tree-sitter assets the book embeds are build outputs, not sources, so
    // they are gitignored and absent on a clean checkout. Build a stand-in and
    // point the command at it, keeping the test hermetic and independent of the
    // working directory.
    let assets = fixture.join("assets");
    for (path, contents) in [
        ("parsers/macaulay2.so", "stand-in parser"),
        ("queries/macaulay2/highlights.scm", "; stand-in queries"),
        ("theme/treesitter.css", "/* stand-in theme */"),
    ] {
        let asset = assets.join(path);
        fs::create_dir_all(asset.parent().expect("asset path has a parent"))
            .expect("asset directory should be created");
        fs::write(&asset, contents).expect("asset fixture should be written");
    }

    let result = Command::new(env!("CARGO_BIN_EXE_m2-ls"))
        .current_dir(&fixture)
        .env("M2_LS_DOC_ASSETS", &assets)
        .arg("docs")
        .arg(&source_dir)
        .arg("--output")
        .arg(&output_dir)
        .arg("--no-build")
        .output()
        .expect("documentation command should run");
    assert!(
        result.status.success(),
        "documentation command failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("generated 3 documentation pages"));

    let summary = fs::read_to_string(output_dir.join("src/SUMMARY.md"))
        .expect("generated summary should exist");
    assert!(summary.contains("[Package]"));
    assert!(summary.contains("[answer]"));
    assert!(summary.contains("[helper]"));
    assert!(!summary.contains("Detached"));
    assert!(!summary.contains("NotPackage"));

    let answer = fs::read_to_string(output_dir.join("src/items/Package--answer.md"))
        .expect("documented binding page should exist");
    assert!(answer.contains("[helper](Package--helper.md)"));
    assert!(output_dir.join("parsers/macaulay2.so").is_file());

    fs::remove_dir_all(&fixture).expect("temporary documentation fixture should be removed");
}

#[tokio::test]
async fn completion_patterns_filter_known_symbols_by_runtime_type() {
    let source = concat!(
        "LocalType = new Type\n",
        "plain = Loc\n",
        "constructed = new Loc\n",
    );
    let mut session = DocumentSession::open(source).await;

    let plain = session.completion_labels("plain = Loc", 0).await;
    assert!(plain.iter().any(|label| label == "Local"), "got {plain:?}");
    assert!(
        plain.iter().any(|label| label == "LocalDictionary"),
        "got {plain:?}"
    );

    let constructed = session.completion_labels("constructed = new Loc", 0).await;
    assert!(
        constructed.iter().any(|label| label == "LocalType"),
        "got {constructed:?}"
    );
    assert!(
        constructed.iter().any(|label| label == "LocalDictionary"),
        "got {constructed:?}"
    );
    assert!(!constructed.iter().any(|label| label == "Local"));

    session.shutdown().await;
}

#[tokio::test]
async fn ring_generator_reassignments_preserve_binding_identity() {
    let mut session = DocumentSession::open("R = QQ[x]\nx = 1\nx\n").await;
    let references = session
        .request(
            "textDocument/references",
            json!({
                "textDocument": {"uri": session.uri()},
                "position": {"line": 2, "character": 0},
                "context": {"includeDeclaration": true}
            }),
        )
        .await;
    let lines = response_array(&references)
        .iter()
        .map(|location| {
            location["range"]["start"]["line"]
                .as_u64()
                .expect("reference line")
        })
        .collect::<Vec<_>>();
    assert_eq!(lines, vec![0, 1, 2]);

    let highlights = session
        .request(
            "textDocument/documentHighlight",
            json!({
                "textDocument": {"uri": session.uri()},
                "position": {"line": 2, "character": 0}
            }),
        )
        .await;
    let lines = response_array(&highlights)
        .iter()
        .map(|highlight| {
            highlight["range"]["start"]["line"]
                .as_u64()
                .expect("highlight line")
        })
        .collect::<Vec<_>>();
    assert_eq!(lines, vec![0, 1, 2]);
    session.shutdown().await;
}

#[tokio::test]
async fn inlay_hints_track_values_destructuring_and_reassignments() {
    let source = concat!(
        "f = (count, text) -> text\n",
        "value = f(2, \"a\")\n",
        "[x, [y, z]] = [1, {\"a\", 1.5}]\n",
        "x = 2\n",
        "x = if condition then 3 else 4\n",
        "x = if condition then \"a\" else \"b\"\n",
        "broad = method()\n",
        "broad(Thing) := (item) -> item\n",
        "unknown = broad(missing)\n",
        "literalString = \"a\"\n",
        "literalArray = [1]\n",
        "literalInteger = 1\n",
        "literalReal = 1.1\n",
        "literalParenthesized = (2)\n",
        "lambda = argument -> argument\n",
        "constructed = new MutableList from {1, 2}\n",
    );
    let mut session = DocumentSession::open(source).await;
    session.set_expression_type_hints(false).await;
    let hints = inlay_hints(&mut session).await;
    assert!(hints.iter().all(|hint| hint["kind"] == 1));

    let type_hints = hints
        .iter()
        .filter(|hint| hint["kind"] == 1)
        .map(|hint| {
            (
                hint["position"]["line"].as_u64().expect("hint line"),
                hint["position"]["character"]
                    .as_u64()
                    .expect("hint character"),
                hint["label"].as_str().expect("hint label"),
            )
        })
        .collect::<Vec<_>>();
    for expected in [(2, 2, "ZZ"), (2, 6, "String"), (2, 9, "RR")] {
        assert!(
            type_hints.contains(&expected),
            "missing destructuring hint {expected:?}: {type_hints:?}"
        );
    }
    assert!(type_hints
        .iter()
        .any(|(line, _, label)| { *line == 5 && *label == "String" }));
    for expected in [
        (0, 25, "↑Thing"),
        (1, 5, "↑Thing"),
        (7, 30, "↑Thing"),
        (8, 7, "↑Thing"),
        (14, 29, "↑Thing"),
    ] {
        assert!(
            type_hints.contains(&expected),
            "missing unresolved type hint {expected:?}: {type_hints:?}"
        );
    }
    for quiet_line in [3, 4, 9, 10, 11, 12, 13, 15] {
        assert!(
            type_hints.iter().all(|(line, _, _)| *line != quiet_line),
            "line {} should not have a type hint: {type_hints:?}",
            quiet_line + 1
        );
    }

    session.shutdown().await;
}

#[tokio::test]
async fn expression_type_hints_follow_typechecker_substitutions() {
    let mut session = DocumentSession::open("value = (toList 1, toList 2)\n").await;
    let hints = inlay_hints(&mut session).await;
    let list_positions = hints
        .iter()
        .filter(|hint| hint["label"] == "-> ↑List")
        .map(|hint| {
            hint["position"]["character"]
                .as_u64()
                .expect("hint position")
        })
        .collect::<Vec<_>>();

    assert!(
        list_positions.contains(&17),
        "missing first call: {hints:?}"
    );
    assert!(
        list_positions.contains(&27),
        "missing second call: {hints:?}"
    );
    assert!(hints.iter().all(|hint| hint["kind"] == 1));

    session.shutdown().await;
}

#[tokio::test]
async fn expression_type_hints_render_complete_inferred_subsets() {
    let source = concat!(
        "if condition then 1 else toList 2\n",
        "if condition then 1 else \"text\"\n",
    );
    let mut session = DocumentSession::open(source).await;
    let hints = inlay_labels_by_line(&mut session).await;

    assert!(
        hints.contains(&(0, "ZZ | ↑List".to_string())),
        "missing upper-set and exact-point union: {hints:?}"
    );
    assert!(
        hints.contains(&(1, "String | ZZ".to_string())),
        "missing exact-point union: {hints:?}"
    );

    session.shutdown().await;
}

#[tokio::test]
async fn inferred_subsets_are_reduced_with_source_type_edges() {
    let source = concat!(
        "TokenStream = new Type of HashTable\n",
        "known = method()\n",
        "known Thing := TokenStream => x -> new TokenStream\n",
        "f = x -> if condition then known x else mystery x\n",
    );
    let mut session = DocumentSession::open(source).await;
    let hints = inlay_labels_by_line(&mut session).await;

    assert!(
        hints
            .iter()
            .any(|(line, label)| *line == 3 && label == "-> TokenStream"),
        "fixture did not infer the source codomain: {hints:?}"
    );
    assert!(
        hints
            .iter()
            .any(|(line, label)| *line == 3 && label == "↑Thing"),
        "unknown reduced result was hidden: {hints:?}"
    );
    assert!(
        hints.iter().all(|(_, label)| {
            !label.contains("↑TokenStream | ↑Thing") && !label.contains("↑Thing | ↑TokenStream")
        }),
        "source subtype was not absorbed by Thing: {hints:?}"
    );

    session.shutdown().await;
}

#[tokio::test]
async fn call_hints_replace_parameter_names_with_types_and_codomains() {
    let source = concat!(
        "f = method()\n",
        "f ZZ := List => x -> toList x\n",
        "argument = 1\n",
        "f argument\n",
    );
    let mut session = DocumentSession::open(source).await;
    let hints = inlay_hints(&mut session).await;

    assert!(
        hints.iter().all(|hint| hint["kind"] == 1),
        "parameter-name hints should be gone: {hints:?}"
    );
    assert!(
        hints
            .iter()
            .all(|hint| { hint["position"]["line"] != 1 || hint["label"] != "↑ZZ" }),
        "installation-typed parameters should not be repeated: {hints:?}"
    );

    let terminal_labels = hints
        .iter()
        .filter(|hint| hint["position"] == json!({"line": 3, "character": 10}))
        .map(|hint| hint["label"].as_str().expect("hint label"))
        .collect::<Vec<_>>();
    assert_eq!(terminal_labels, ["ZZ", "-> ↑List"]);

    session.shutdown().await;
}

#[tokio::test]
async fn lambda_bodies_keep_intermediate_and_return_type_hints() {
    let source = concat!(
        "f = x -> (\n",
        "    x === x;\n",
        "    toList x;\n",
        "    toList x\n",
        ")\n",
    );
    let mut session = DocumentSession::open(source).await;
    let hints = inlay_labels_by_line(&mut session).await;

    assert!(
        hints.contains(&(1, "Boolean".to_string())),
        "semicolon hid the comparison type inside a lambda: {hints:?}"
    );
    assert!(
        hints
            .iter()
            .any(|(line, label)| *line == 2 && label.as_str() == "-> ↑List"),
        "semicolon hid a call codomain inside a lambda: {hints:?}"
    );
    assert!(
        hints
            .iter()
            .any(|(line, label)| *line == 3 && label.as_str() == "↑List"),
        "missing lambda return type: {hints:?}"
    );
    assert!(hints.iter().all(|(_, label)| label != "Nothing"));

    session.shutdown().await;
}

#[tokio::test]
async fn lambda_return_values_receive_nontrivial_type_hints() {
    let source = concat!(
        "direct = x -> toList x\n",
        "scoped = x -> (ignored := x; toList x)\n",
        "explicit = x -> (return toList x;)\n",
        "commented = x -> (return -- explanation\n",
        "    toList x;)\n",
        "literal = x -> 1\n",
        "constructed = x -> new MutableList from {}\n",
        "explicitLiteral = x -> (return 1;)\n",
        "explicitConstructed = x -> (return new MutableList from {};)\n",
        "boolean = x -> true\n",
        "nothing = x -> null\n",
        "explicitBoolean = x -> (return false;)\n",
        "explicitNothing = x -> (return null;)\n",
    );
    let mut session = DocumentSession::open(source).await;
    session.set_expression_type_hints(false).await;
    assert!(
        !session.diagnostic_codes().contains(&"X01"),
        "a line comment must remain whitespace between `return` and its value: {:?}",
        session.diagnostics()
    );
    let type_hints = inlay_hints(&mut session)
        .await
        .into_iter()
        .filter(|hint| hint["kind"] == 1)
        .map(|hint| {
            (
                hint["position"]["line"].as_u64().expect("hint line"),
                hint["label"].as_str().expect("hint label").to_string(),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        type_hints,
        [
            (0, "↑List".into()),
            (1, "↑Thing".into()),
            (1, "↑List".into()),
            (2, "↑List".into()),
            (4, "↑List".into()),
        ]
    );

    session.shutdown().await;
}

#[tokio::test]
async fn trivial_parallel_assignments_hint_each_target_occurrence() {
    let mut session = DocumentSession::open("(x, x) := (1, 3)\n").await;
    session.set_expression_type_hints(false).await;
    let hints = inlay_hints(&mut session).await;
    let type_hints = hints
        .iter()
        .filter(|hint| hint["kind"] == 1)
        .map(|hint| {
            (
                hint["position"]["line"].as_u64().expect("hint line"),
                hint["position"]["character"]
                    .as_u64()
                    .expect("hint character"),
                hint["label"].as_str().expect("hint label"),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(type_hints, [(0, 2, "ZZ"), (0, 5, "ZZ")]);

    session.shutdown().await;
}

#[tokio::test]
async fn assignment_type_hint_follows_the_binding_target() {
    let mut session = DocumentSession::open("x = y\n").await;
    session.set_expression_type_hints(false).await;
    let hints = inlay_hints(&mut session).await;
    let type_hint = hints
        .iter()
        .find(|hint| hint["kind"] == 1)
        .expect("the assignment should have a type hint");

    assert_eq!(type_hint["position"], json!({"line": 0, "character": 1}));

    session.shutdown().await;
}

#[tokio::test]
async fn indexed_callable_aliases_preserve_identity_for_local_installations() {
    let source = "f = ideal\ng = f\nf ZZ := x -> x\ny = g 1\n";
    let mut session = DocumentSession::open(source).await;

    assert!(!session.diagnostic_codes().contains(&"E01"));
    assert_eq!(hover_type_at(&mut session, 3, 0).await, "Thing");

    let signature_help = session
        .request(
            "textDocument/signatureHelp",
            json!({
                "textDocument": {"uri": session.uri()},
                "position": {"line": 3, "character": 6}
            }),
        )
        .await;
    assert!(
        signature_help["signatures"]
            .as_array()
            .is_some_and(|signatures| signatures
                .iter()
                .any(|signature| { signature["label"] == "g(ZZ)" })),
        "indexed alias installation should drive signature help: {signature_help}"
    );

    session.shutdown().await;
}

#[tokio::test]
async fn core_method_function_single_call_reaches_hover() {
    let mut session = DocumentSession::open("I = ideal(12,18)\nI\n").await;
    assert_eq!(hover_type_at(&mut session, 1, 0).await, "Ideal");
    session.shutdown().await;
}

#[tokio::test]
async fn scopes_bindings_and_callable_metadata_drive_editor_capabilities() {
    let source = concat!(
        "outside := 1\n",
        "f = parameter -> (\n",
        "  inside := parameter;\n",
        "  inside\n",
        ")\n",
        "outside\n",
        "[a, [b, c]] = [1, {2, 3}]\n",
        "p = method(Binary => true, TypicalValue => List)\n",
        "p(ZZ,ZZ) := p(List,ZZ) := (i,j) -> {i,j}\n",
        "result := p(1,2)\n",
        "result\n",
    );
    let mut session = DocumentSession::open(source).await;

    let local_completions = session.completion_labels("inside", 1).await;
    assert!(local_completions.iter().any(|label| label == "inside"));
    let global_completions = session.completion_labels("outside", 1).await;
    assert!(global_completions.iter().any(|label| label == "outside"));
    assert!(!global_completions.iter().any(|label| label == "inside"));

    let definition = session
        .request_at("textDocument/definition", "parameter", 1)
        .await;
    assert_eq!(
        definition["range"]["start"],
        position(source, "parameter", 0)
    );

    let symbols = session
        .request(
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": session.uri()}}),
        )
        .await;
    let symbol_names = response_array(&symbols)
        .iter()
        .filter_map(|symbol| symbol["name"].as_str())
        .collect::<Vec<_>>();
    for expected in ["outside", "f", "a", "b", "c", "p", "result"] {
        assert!(
            symbol_names.contains(&expected),
            "document symbols should contain {expected}: {symbols}"
        );
    }

    let signatures = session
        .request_at("textDocument/signatureHelp", "1,2", 0)
        .await;
    assert!(
        response_array(&signatures["signatures"])
            .iter()
            .any(|signature| signature["label"] == "p(ZZ, ZZ) -> List"),
        "local installed method signatures should reach signature help: {signatures}"
    );
    assert_eq!(hover_type_at(&mut session, 10, 0).await, "List");

    let tokens = semantic_tokens(&mut session).await;
    let function_token = session.semantic_token_type("function");
    let parameter_token = session.semantic_token_type("parameter");
    let (function_type, function_modifiers) = token_at(&tokens, source, "f", 0);
    assert_eq!(
        function_type, function_token,
        "local callables should be function tokens"
    );
    assert_ne!(
        function_modifiers & (1 << 3),
        0,
        "the callable declaration should carry the declaration modifier"
    );
    for occurrence in [0, 1] {
        assert_eq!(
            token_at(&tokens, source, "parameter", occurrence).0,
            parameter_token,
            "function parameters should retain the standard parameter role"
        );
    }

    session.shutdown().await;
}

#[tokio::test]
async fn algebraic_runtime_types_and_generator_rebinding_reach_hover() {
    let source = concat!(
        "R = QQ[a..d]\n",
        "I = ideal(a^2,b)\n",
        "Q = R/I\n",
        "M = Q^2\n",
        "J = ideal(a,b)\n",
        "N = M/J\n",
        "a\n",
        "b\n",
        "R\n",
        "I\n",
        "Q\n",
        "M\n",
        "J\n",
        "N\n",
    );
    let mut session = DocumentSession::open(source).await;
    let hints = inlay_labels_by_line(&mut session).await;
    for (line, expected) in [
        (6, "Q"),
        (7, "Q"),
        (8, "PolynomialRing"),
        (9, "↑Ideal"),
        (10, "QuotientRing"),
        (11, "Module"),
        (12, "↑Ideal"),
        (13, "Module"),
    ] {
        assert!(
            hints
                .iter()
                .any(|(hint_line, label)| *hint_line == line && label == expected),
            "missing {expected} hint on line {line}: {hints:?}"
        );
    }

    session
        .replace(
            "before := u_0\nS = ZZ/101[t_0..t_3]\nafter := t_1\nV = QQ[Variables => 3, VariableBaseName => v]\ngenerated := v_2\nt\nS\nbefore\nafter\ngenerated\n",
        )
        .await;
    let hints = inlay_labels_by_line(&mut session).await;
    for (line, expected) in [
        (5, "IndexedVariableTable"),
        (6, "PolynomialRing"),
        (7, "IndexedVariable"),
        (8, "S"),
        (9, "V"),
    ] {
        assert!(
            hints
                .iter()
                .any(|(hint_line, label)| *hint_line == line && label == expected),
            "missing {expected} hint on line {line}: {hints:?}"
        );
    }

    session
        .replace("A = QQ[x]\nJ = ideal(x^2)\nQ = A/J\nx\nQ\nT = QQ[y]/J\ny\nT\n")
        .await;
    let hints = inlay_labels_by_line(&mut session).await;
    for (line, expected) in [(3, "Q"), (4, "QuotientRing"), (6, "T"), (7, "QuotientRing")] {
        assert!(
            hints
                .iter()
                .any(|(hint_line, label)| *hint_line == line && label == expected),
            "missing {expected} hint on line {line}: {hints:?}"
        );
    }

    session
        .replace("R = QQ[x]\nf = x^2\nJ = ideal(f)\nf\nJ\n")
        .await;
    let hints = inlay_labels_by_line(&mut session).await;
    assert!(hints.contains(&(3, "↑RingElement".to_string())));
    assert!(hints.contains(&(4, "↑Ideal".to_string())));

    session.shutdown().await;
}

#[tokio::test]
async fn numeric_literal_promotion_preserves_the_ring_element_type() {
    let mut session = DocumentSession::open(
        "S = QQ[x]\nR = S\na := 1_R\nb := (2)_R\nc := (2.5)_(R)\nq := 3_QQ\na\nb\nc\nq\n",
    )
    .await;

    for (line, expected) in [(6, "S"), (7, "S"), (8, "S"), (9, "QQ")] {
        assert_eq!(hover_type_at(&mut session, line, 0).await, expected);
    }

    session.shutdown().await;
}
