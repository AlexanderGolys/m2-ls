//! Null-tested updates and their coalescing quick fix through the LSP.

use serde_json::json;

use crate::support::{response_array, DocumentSession, LspProcess, TestWorkspace};

/// The fallback's side effects stay inside its conditional block while every
/// terminal assignment becomes a value for the single enclosing assignment.
#[tokio::test]
async fn assignment_chains_coalesce_without_losing_block_side_effects() {
    let source = r#"scanStep = (stop, invocation, text, i, found) -> (
        if stop =!= null then
            i = stop
        else if invocation =!= null and hasArgument invocation then (
            found#(#found) = invocation;
            i = invocation.invocationEnd
        ) else i = if isNameChar text#i then
                wordEnd(text, i)
            else i + 1
    )
"#;
    let mut session = DocumentSession::open(source).await;
    let finding = session
        .diagnostics()
        .iter()
        .find(|item| item["code"] == "S04")
        .expect("the complete assignment chain should coalesce")
        .clone();
    let uri = session.uri().to_string();
    let actions = session
        .request(
            "textDocument/codeAction",
            json!({
                "textDocument": {"uri": uri},
                "range": finding["range"],
                "context": {"diagnostics": [finding]}
            }),
        )
        .await;
    let replacement = response_array(&actions)
        .iter()
        .find(|action| action["title"] == "Use `??` coalescence")
        .expect("a coalescing quick fix")["edit"]["changes"][&uri][0]["newText"]
        .as_str()
        .unwrap();
    let compact = replacement.split_whitespace().collect::<String>();
    assert_eq!(compact, "i=stop??(ifinvocation=!=nullandhasArgumentinvocationthen(found#(#found)=invocation;invocation.invocationEnd)elseifisNameChartext#ithenwordEnd(text,i)elsei+1)");
    session
        .replace(&format!(
            "scanStep = (stop, invocation, text, i, found) -> ({replacement})\n"
        ))
        .await;
    assert!(!session.diagnostic_codes().contains(&"X01"));
    assert!(!session.diagnostic_codes().contains(&"S04"));
    session.shutdown().await;
}

/// Incomplete paths, different targets, local declarations, and discarded block
/// results cannot supply the value of one enclosing assignment.
#[tokio::test]
async fn assignment_chains_reject_paths_that_do_not_update_the_same_binding() {
    let mut session = DocumentSession::open("x = 1\n").await;
    for fallback in [
        "if ready then i = 1 else j = 2",
        "if ready then i = 1",
        "if ready then i = 1 else (i := 2; i = 3)",
        "if ready then i = 1 else ((i, j) := (2, 3); i = 4)",
        "if ready then i = 1 else (log 2; i = 3;)",
        "if ready then i = 1 else i += 2",
    ] {
        let source =
            format!("f = (stop, i, j, ready) -> if stop =!= null then i = stop else {fallback}\n");
        session.replace(&source).await;
        assert!(
            !session.diagnostic_codes().contains(&"S04"),
            "{source}: {:?}",
            session.diagnostics()
        );
    }
    session.shutdown().await;
}

/// Reversed null tests also lift assignment chains, preserving fallback comments
/// and locals with other names inside their original block scope.
#[tokio::test]
async fn reversed_null_tests_preserve_fallback_comments_and_locals() {
    let source = "f = (stop, i, ready) -> if stop === null then (\n    next := 1;\n    if ready then i = -- keep this explanation\n        next else i = 2\n) else i = stop\n";
    let session = DocumentSession::open(source).await;
    let finding = session
        .diagnostics()
        .iter()
        .find(|item| item["code"] == "S04")
        .expect("a reversed null test with complete assignment paths");
    let message = finding["message"].as_str().unwrap();
    assert!(message.contains("i = stop ??"), "{message}");
    assert!(message.contains("next := 1"), "{message}");
    assert!(message.contains("-- keep this explanation"), "{message}");
    session.shutdown().await;
}

/// A non-null value can update an existing variable through `??`, provided the
/// conditional's value is unused. The quick fix must preserve the old fallback.
#[tokio::test]
async fn non_null_assignment_offers_a_coalescing_update() {
    let source = "f = (stop, i) -> (\n    if stop =!= null then\n        i = stop;\n    i\n)\n";
    let workspace = TestWorkspace::new(source);
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    let opened = server.open_document(&workspace.uri, source).await;
    let finding = response_array(&opened["params"]["diagnostics"])
        .iter()
        .find(|item| item["code"] == "S04")
        .expect("a conditional update should offer coalescence")
        .clone();
    assert!(finding["message"]
        .as_str()
        .unwrap()
        .contains("i = stop ?? i"));
    let actions = server
        .request(
            "textDocument/codeAction",
            json!({
                "textDocument": {"uri": workspace.uri},
                "range": finding["range"],
                "context": {"diagnostics": [finding]}
            }),
        )
        .await;
    assert!(
        response_array(&actions).iter().any(|action| {
            action["edit"]["changes"][&workspace.uri]
                .as_array()
                .is_some_and(|edits| edits.iter().any(|edit| edit["newText"] == "i = stop ?? i"))
        }),
        "{actions}"
    );
    server.shutdown().await;
    let mut session = DocumentSession::open(source).await;
    // The conditional returns null on the missing branch; a value-consuming
    // context must retain that result rather than returning the old target.
    session
        .replace("f = (stop, i) -> if stop =!= null then i = stop\n")
        .await;
    assert!(!session.diagnostic_codes().contains(&"S04"));
    session
        .replace("f = (stop, i) -> (if null =!= stop then i = stop; i)\n")
        .await;
    assert!(session.diagnostic_codes().contains(&"S04"));
    session
        .replace("f = (stop, i) -> (if stop =!= null then i := stop; i)\n")
        .await;
    assert!(!session.diagnostic_codes().contains(&"S04"));
    session
        .replace("f = (stop, i) -> (if stop =!= null then i = 1; i)\n")
        .await;
    assert!(!session.diagnostic_codes().contains(&"S04"));
    session.shutdown().await;
}
