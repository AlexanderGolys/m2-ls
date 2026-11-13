//! Published suggestions for explicit counters that can become `for` loops.

use crate::support::response_array;
use crate::support::DocumentSession;
use serde_json::json;

/// Explicit zero starts have a deletion fix for bounded, conditional, and
/// unbounded numeric loops. Applying it retains the rest of the loop verbatim.
#[tokio::test]
async fn default_for_starts_have_a_quick_fix() {
    let mut session = DocumentSession::open("x = 1\n").await;
    for source in [
        "for i from 0 to 5 do print i\n",
        "for i from 0 when i < 5 list i\n",
        "for i from 0 do break\n",
    ] {
        session.replace(source).await;
        let finding = session
            .diagnostics()
            .iter()
            .find(|finding| finding["code"] == "S30")
            .unwrap_or_else(|| panic!("missing zero-start hint: {source}"))
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
        let action = response_array(&actions)
            .iter()
            .find(|action| action["title"] == "Omit `from 0`")
            .unwrap_or_else(|| panic!("missing deletion action: {actions}"));
        let edit = &action["edit"]["changes"][&uri][0];
        let start = edit["range"]["start"]["character"].as_u64().unwrap() as usize;
        let end = edit["range"]["end"]["character"].as_u64().unwrap() as usize;
        let mut rewritten = source.to_string();
        rewritten.replace_range(start..end, edit["newText"].as_str().unwrap());
        assert_eq!(rewritten, source.replace(" from 0", ""));
        session.replace(&rewritten).await;
        assert!(!session.diagnostic_codes().contains(&"S30"));
        assert!(!session.diagnostic_codes().contains(&"X01"));
    }
    for source in [
        "for i from 1 to 5 list i\n",
        "for i from (0 + start) to 5 list i\n",
        "for i to 5 list i\n",
    ] {
        session.replace(source).await;
        assert!(!session.diagnostic_codes().contains(&"S30"), "{source}");
    }
    session.shutdown().await;
}

/// Removing the explicit start preserves comments between `from` and zero.
#[tokio::test]
async fn default_for_start_fixes_retain_header_comments() {
    let source = "for i from -- the first index\n0 to 5 list i\n";
    let mut session = DocumentSession::open(source).await;
    let finding = session
        .diagnostics()
        .iter()
        .find(|finding| finding["code"] == "S30")
        .expect("a commented zero lower bound should be redundant")
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
    let action = response_array(&actions)
        .iter()
        .find(|action| action["title"] == "Omit `from 0`")
        .unwrap();
    let replacement = action["edit"]["changes"][&uri][0]["newText"]
        .as_str()
        .unwrap();
    assert!(replacement.contains("-- the first index"), "{actions}");
    let rewritten = format!("for i{replacement} to 5 list i\n");
    session.replace(&rewritten).await;
    assert!(!session.diagnostic_codes().contains(&"X01"), "{rewritten}");
    assert!(!session.diagnostic_codes().contains(&"S30"));
    session.shutdown().await;
}

/// The hint uses inclusive bounds, omits the default zero start, and recognizes
/// both ordinary and augmented unit increments, including semicolon endings.
#[tokio::test]
async fn counted_while_loops_suggest_concise_inclusive_ranges() {
    let mut session =
        DocumentSession::open("f = text -> (i := 0; while i < #text do (print text#i; i += 1))\n")
            .await;
    for (initial, condition, increment, header) in [
        ("0", "i < #text", "i += 1", "for i to #text - 1 do"),
        ("2", "i <= #text", "i = i + 1;", "for i from 2 to #text do"),
        ("0", "i < 10", "i += 1", "for i to 10 - 1 do"),
    ] {
        let source = format!(
            "f = text -> (i := {initial}; while {condition} do (print text#i; {increment}))\n"
        );
        session.replace(&source).await;
        let finding = session
            .diagnostics()
            .iter()
            .find(|finding| finding["code"] == "S29")
            .unwrap_or_else(|| panic!("missing for-loop hint: {source}"));
        assert_eq!(finding["source"], "prefer-for-loop");
        assert_eq!(finding["severity"], 4);
        assert!(
            finding["message"].as_str().unwrap().contains(header),
            "{finding}"
        );
    }
    session.shutdown().await;
}

/// A new `for` scope or its automatic increment must not change jumps, values
/// used after the loop, captured counters, or paths bypassing the increment.
#[tokio::test]
async fn irregular_counter_loops_keep_their_while_form() {
    let mut session = DocumentSession::open("x = 1\n").await;
    for block in [
        "i := 0; while i < #text do (i = stop ?? i + 1)",
        "i := 0; while i < #text do (i += 1); i",
        "i = 0; while i < #text do (print text#i; i += 1)",
        "i := 0; while i < #text do (i = stop; i += 1)",
        "i := 0; while i < #text do (if ready then continue; i += 1)",
        "i := 0; while i < #text do (saved = () -> i; i += 1)",
        "i := 0; while i < #text do (i += 2)",
        "i := 0; while i < #text do (print text#i)",
        "i := 0; while i < #text list (i += 1)",
        "i := 0; while i < (bound := #text) do (i += 1); bound",
        "print i; i := 0; while i < #text do (i += 1)",
        "i := 0; while i < #text do (text = text | \"a\"; i += 1)",
        "i := 0; while i < #text do (text#(#text) = 1; i += 1)",
        "i := 0; while i < #text do (text |= \"a\"; i += 1)",
        "i := 0; while i < limit() do (i += 1)",
    ] {
        let source = format!("f = text -> ({block})\n");
        session.replace(&source).await;
        assert!(
            !session.diagnostic_codes().contains(&"S29"),
            "{source}: {:?}",
            session.diagnostics()
        );
    }
    session.shutdown().await;
}

/// The registered hint participates in ordinary diagnostic suppression.
#[tokio::test]
async fn for_loop_suggestions_respect_diagnostic_suppression() {
    let source = "-- m2-ls: disable prefer-for-loop\nf = text -> (i := 0; while i < #text do (print text#i; i += 1))\n";
    let session = DocumentSession::open(source).await;
    assert!(!session.diagnostic_codes().contains(&"S29"));
    session.shutdown().await;
}
