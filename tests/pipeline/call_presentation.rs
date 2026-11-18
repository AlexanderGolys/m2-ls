//! Call and producer presentation verified through the running language server.

use serde_json::{json, Value};

use crate::support::{response_array, DocumentSession};

/// Optional arguments have named signature slots, selected by key rather
/// than by their order in the source call.
#[tokio::test]
async fn signature_help_selects_named_options() {
    let source = concat!(
        "f = method(Options => {Limit => 1, Verbose => false})\n",
        "f ZZ := opts -> n -> n\n",
        "f(2, Verbose => true, Limit => 4)\n",
    );
    let mut session = DocumentSession::open(source).await;
    let help = session
        .request_at("textDocument/signatureHelp", "true", 0)
        .await;
    let signature = &help["signatures"][0];
    assert!(
        signature["label"].as_str().unwrap().contains("Verbose =>"),
        "{help}"
    );
    let active = help["activeParameter"].as_u64().unwrap() as usize;
    assert!(
        signature["parameters"][active]["label"]
            .as_str()
            .unwrap()
            .starts_with("Verbose =>"),
        "{help}"
    );
    session
        .replace("f = {Limit => 1} >> opts -> (value) -> value\nf(2, Limit => 4)\n")
        .await;
    let help = session
        .request_at("textDocument/signatureHelp", "4", 0)
        .await;
    let signature = &help["signatures"][0];
    assert!(
        signature["label"]
            .as_str()
            .unwrap()
            .starts_with("f(value, Limit =>"),
        "{help}"
    );
    assert_eq!(help["activeParameter"], 1, "{help}");
    session.shutdown().await;
}

/// Trivial producers are independent of literal filtering; inner computations
/// and explicit holder requests retain their own hints.
#[tokio::test]
async fn trivial_producers_are_filtered_independently_of_literals() {
    let source = concat!(
        "Widget = new Type of HashTable\n",
        "widget = method(TypicalValue => Widget)\n",
        "widget ZZ := n -> new Widget\n",
        "a = widget 1\n",
        "b = if true then 1 else 2\n",
        "c = for i to 2 list i\n",
        "d = 7\n",
        "e = isPrime 7\n",
        "h = (widget 2)\n",
    );
    let mut session = DocumentSession::open(source).await;
    let settings = |trivial| {
        json!({
            "everywhere": false,
            "argNames": {"enabled": false},
            "types": {"after": {"call": false, "endOfLine": false, "endOfCell": false,
                "assignment": true, "brackets": ["holder"], "return": false}},
            "hide": {"literal": false, "trivial": trivial, "calls": []}
        })
    };
    session.set_inlay_hint_settings(settings(false)).await;
    let visible = hints(&mut session, source).await;
    for line in 3..=7 {
        assert!(
            visible.iter().any(|hint| hint["position"]["line"] == line),
            "{visible:?}"
        );
    }
    session.set_inlay_hint_settings(settings(true)).await;
    let hidden = hints(&mut session, source).await;
    for line in 3..=6 {
        assert!(
            !hidden.iter().any(|hint| hint["position"]["line"] == line),
            "{hidden:?}"
        );
    }
    for line in [7, 8] {
        assert!(
            hidden.iter().any(|hint| hint["position"]["line"] == line),
            "{hidden:?}"
        );
    }
    session.shutdown().await;
}

/// Collects hints across the complete fixture, including its final empty line.
async fn hints(session: &mut DocumentSession, source: &str) -> Vec<Value> {
    response_array(&session.request("textDocument/inlayHint", json!({
        "textDocument": {"uri": session.uri()},
        "range": {"start": {"line": 0, "character": 0}, "end": {"line": source.lines().count(), "character": 0}}
    })).await).to_vec()
}

/// Backticks in data strings do not create references to code bindings.
#[tokio::test]
async fn data_strings_do_not_create_documentation_references() {
    let source = "value = 1\ntext = ///A `value` in data///\nvalue\n";
    let mut session = DocumentSession::open(source).await;
    let references = session
        .request(
            "textDocument/references",
            json!({
                "textDocument": {"uri": session.uri()},
                "position": {"line": 0, "character": 0},
                "context": {"includeDeclaration": true}
            }),
        )
        .await;
    let references = response_array(&references);
    assert!(
        references
            .iter()
            .all(|reference| reference["range"]["start"]["line"] != 1),
        "{references:?}"
    );
    session.shutdown().await;
}

/// Declaration lints distinguish missing values, intentional protected
/// constants, and invalid calls of fixed-parameter source functions.
#[tokio::test]
async fn declarations_and_fixed_calls_report_their_contracts() {
    let mut session = DocumentSession::open("").await;
    for (source, code, expected) in [
        ("f = (x,y) -> x\nf 1\n", "T07", true),
        ("g = (x) -> x\ng(1,2)\n", "T07", true),
        ("g = x -> x\ng(1,2)\n", "T07", false),
        ("f = method(TypicalValue => missingType)\n", "E22", true),
        ("x = Unintroduced\n", "S31", true),
        ("protect Introduced\nx = Introduced\n", "S31", false),
        ("x = symbol Explicit\n", "S31", false),
        (
            "Widget = new Type of HashTable\nwidget = method()\n",
            "S10",
            true,
        ),
        (
            "Widget = new Type of HashTable\nwidget = method(TypicalValue => Widget)\n",
            "S10",
            false,
        ),
        ("matrix {{1}}\n", "S10", false),
    ] {
        session.replace(source).await;
        session.save().await;
        assert_eq!(
            session.diagnostic_codes().contains(&code),
            expected,
            "{source}: {:?}",
            session.diagnostics()
        );
    }
    session.shutdown().await;
}

/// Option declarations in the current file supply completions without
/// requiring a catalog record for the user-defined function.
#[tokio::test]
async fn local_option_declarations_supply_completions() {
    let source = "f = method(Options => {Limit => 1})\nf ZZ := opts -> n -> n\nf(2, Lim)\n";
    let mut session = DocumentSession::open(source).await;
    let completions = session.completion_labels("Lim", 1).await;
    assert!(completions.contains(&"Limit".to_owned()), "{completions:?}");
    session.shutdown().await;
}
