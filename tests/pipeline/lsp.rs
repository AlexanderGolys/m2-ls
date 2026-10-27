//! Black-box coverage for the complete stdio language-server pipeline.

use std::env;

use serde_json::{json, Value};
use tower_lsp::lsp_types::Url;

use crate::support::{document_position, position, response_array, LspProcess, TestWorkspace};

const SOURCE: &str = include_str!("../fixtures/capability_spectrum.m2");

async fn code_actions_at(
    server: &mut LspProcess,
    uri: &str,
    needle: &str,
    diagnostics: &[Value],
) -> Value {
    let position = position(SOURCE, needle, 0);
    server
        .request(
            "textDocument/codeAction",
            json!({
                "textDocument": {"uri": uri},
                "range": {
                    "start": position.clone(),
                    "end": position
                },
                "context": {
                    "diagnostics": diagnostics
                }
            }),
        )
        .await
}

fn action_replacement<'actions>(
    actions: &'actions Value,
    title: &str,
    uri: &str,
) -> Option<&'actions str> {
    actions
        .as_array()?
        .iter()
        .find(|action| action["title"] == title)?["edit"]["changes"]
        .get(uri)?
        .as_array()?
        .first()?["newText"]
        .as_str()
}

fn action_titles(actions: &Value) -> Vec<&str> {
    actions
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|action| action["title"].as_str())
        .collect()
}

#[tokio::test]
async fn workspace_symbols_include_reassignments_and_exclude_function_body_bindings() {
    let source = concat!(
        "outer := x -> (localBinding := x; globalBinding = x; localBinding)\n",
        "outer = y -> y\n",
    );
    let workspace = TestWorkspace::new(source);
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    server.open_document(&workspace.uri, source).await;

    let symbols = server
        .request("workspace/symbol", json!({"query": ""}))
        .await;
    let symbols = response_array(&symbols);
    let outer_symbols = symbols
        .iter()
        .filter(|symbol| {
            symbol["name"] == "outer"
                && symbol["location"]["uri"] == workspace.uri
                && symbol["containerName"].is_null()
        })
        .collect::<Vec<_>>();
    assert_eq!(outer_symbols.len(), 2);
    assert_eq!(outer_symbols[0]["location"]["range"]["start"]["line"], 0);
    assert_eq!(outer_symbols[1]["location"]["range"]["start"]["line"], 1);
    assert!(symbols
        .iter()
        .all(|symbol| { symbol["name"] != "localBinding" && symbol["name"] != "globalBinding" }));

    let document_symbols = server
        .request(
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": workspace.uri}}),
        )
        .await;
    let document_symbols = response_array(&document_symbols);
    let outer_symbols = document_symbols
        .iter()
        .filter(|symbol| symbol["name"] == "outer")
        .collect::<Vec<_>>();
    assert_eq!(outer_symbols.len(), 2);
    assert_eq!(outer_symbols[0]["selectionRange"]["start"]["line"], 0);
    assert_eq!(outer_symbols[1]["selectionRange"]["start"]["line"], 1);

    let children = &document_symbols[0]["children"];
    assert!(response_array(children)
        .iter()
        .any(|symbol| symbol["name"] == "localBinding"));
    assert!(response_array(children)
        .iter()
        .any(|symbol| symbol["name"] == "globalBinding"));

    server.shutdown().await;
}

#[tokio::test]
async fn files_loaded_by_an_open_document_are_indexed_without_being_opened() {
    let source = "load \"related.m2\"\ncrossFileResult\n";
    let workspace = TestWorkspace::new(source);
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    server.open_document(&workspace.uri, source).await;

    let symbols = server
        .request("workspace/symbol", json!({"query": "crossFile"}))
        .await;
    assert!(
        response_array(&symbols).iter().any(|symbol| {
            symbol["name"] == "crossFileResult"
                && symbol["location"]["uri"] == workspace.related_uri
        }),
        "a loaded file should be indexed from disk: {symbols}"
    );

    let definition = server
        .request(
            "textDocument/definition",
            document_position(&workspace.uri, position(source, "crossFileResult", 0)),
        )
        .await;
    assert!(
        definition.to_string().contains(&workspace.related_uri),
        "a use should resolve into the loaded file: {definition}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn an_invalid_setting_is_shown_to_the_user_and_the_valid_ones_still_apply() {
    let source = "f = x -> (unused := 1; x)\n";
    let workspace = TestWorkspace::new(source);
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    let opened = server.open_document(&workspace.uri, source).await;
    assert!(!response_array(&opened["params"]["diagnostics"]).is_empty());

    server
        .notify(
            "workspace/didChangeConfiguration",
            json!({
                "settings": {
                    "macaulay2": {
                        "server": { "path": "m2-ls" },
                        "diagnostics": {
                            "enabled": false,
                            "disabled": ["not-a-rule"]
                        }
                    }
                }
            }),
        )
        .await;

    let message = server.wait_for_notification("window/showMessage").await;
    let text = message["params"]["message"].as_str().unwrap_or_default();
    assert!(
        text.contains("`diagnostics.disabled[0]`: unknown diagnostic `not-a-rule`"),
        "{message}"
    );
    assert!(
        !text.contains("server"),
        "client-owned keys are not reported: {message}"
    );
    let republished = server
        .wait_for_notification("textDocument/publishDiagnostics")
        .await;
    assert!(
        response_array(&republished["params"]["diagnostics"]).is_empty(),
        "`enabled: false` should still apply: {republished}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn example_document_exercises_the_capability_spectrum_over_stdio() {
    let workspace = TestWorkspace::new(SOURCE);
    let mut server = LspProcess::spawn().await;
    let initialized = server.initialize(&workspace.root_uri()).await;

    assert_eq!(initialized["serverInfo"]["name"], env!("CARGO_PKG_NAME"));
    assert_eq!(
        initialized["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    for capability in [
        "hoverProvider",
        "referencesProvider",
        "renameProvider",
        "documentFormattingProvider",
        "foldingRangeProvider",
        "workspaceSymbolProvider",
        "completionProvider",
        "signatureHelpProvider",
        "definitionProvider",
        "declarationProvider",
        "typeDefinitionProvider",
        "implementationProvider",
        "documentLinkProvider",
        "documentSymbolProvider",
        "documentHighlightProvider",
        "codeActionProvider",
        "inlayHintProvider",
        "semanticTokensProvider",
        "typeHierarchyProvider",
    ] {
        assert!(
            initialized["capabilities"].get(capability).is_some(),
            "initialize should advertise {capability}"
        );
    }

    let unopened_workspace_symbols = server
        .request("workspace/symbol", json!({"query": "crossFile"}))
        .await;
    assert!(
        response_array(&unopened_workspace_symbols).is_empty(),
        "files neither open nor loaded should stay out of the index: {unopened_workspace_symbols}"
    );

    let unopened_hover = server
        .request(
            "textDocument/hover",
            document_position(
                "file:///document-that-was-not-opened.m2",
                json!({"line": 0, "character": 0}),
            ),
        )
        .await;
    assert!(unopened_hover.is_null());

    server
        .notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": workspace.uri,
                    "languageId": "macaulay2",
                    "version": 1,
                    "text": SOURCE
                }
            }),
        )
        .await;
    let diagnostics = server
        .wait_for_notification("textDocument/publishDiagnostics")
        .await;
    assert_eq!(diagnostics["params"]["uri"], workspace.uri);
    let document_diagnostics = response_array(&diagnostics["params"]["diagnostics"]).to_vec();
    server
        .notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": workspace.related_uri,
                    "languageId": "macaulay2",
                    "version": 1,
                    "text": "crossFileResult = localValue\ntoJSON\n"
                }
            }),
        )
        .await;
    let related_diagnostics = server
        .wait_for_notification("textDocument/publishDiagnostics")
        .await;
    assert_eq!(related_diagnostics["params"]["uri"], workspace.related_uri);

    let hover = server
        .request(
            "textDocument/hover",
            document_position(&workspace.uri, position(SOURCE, "toJSON", 2)),
        )
        .await;
    assert!(
        hover["contents"]["value"]
            .as_str()
            .is_some_and(|markdown| markdown.contains("package `JSON`")),
        "imported package hover should survive the entire server pipeline: {hover}"
    );
    for occurrence in [1, 5] {
        let source_hover = server
            .request(
                "textDocument/hover",
                document_position(&workspace.uri, position(SOURCE, "toJSON", occurrence)),
            )
            .await;
        assert!(
            source_hover["contents"]["value"]
                .as_str()
                .is_some_and(|markdown| markdown.contains("user-defined")),
            "source definitions should own toJSON before the import and after the later redefinition: {source_hover}"
        );
    }
    for (occurrence, expected_type) in [(1, "String"), (3, "ZZ")] {
        let reassigned_hover = server
            .request(
                "textDocument/hover",
                document_position(&workspace.uri, position(SOURCE, "reassigned", occurrence)),
            )
            .await;
        assert!(
            reassigned_hover["contents"]["value"]
                .as_str()
                .is_some_and(|markdown| markdown.contains(&format!("`{expected_type}` · "))),
            "reassignment types should be source ordered: {reassigned_hover}"
        );
    }
    let unimported_hover = server
        .request(
            "textDocument/hover",
            document_position(&workspace.related_uri, json!({"line": 1, "character": 1})),
        )
        .await;
    // The other document's own global `toJSON` is visible across the
    // workspace, as its definition is; the package's object is not.
    assert!(
        !unimported_hover.to_string().contains("package `JSON`"),
        "loading JSON in one document must not register it in another: {unimported_hover}"
    );

    let completion = server
        .request(
            "textDocument/completion",
            document_position(
                &workspace.uri,
                json!({
                    "line": position(SOURCE, "toJ\n", 0)["line"],
                    "character": position(SOURCE, "toJ\n", 0)["character"]
                        .as_u64()
                        .expect("the fixture position should be numeric")
                        + 3
                }),
            ),
        )
        .await;
    assert!(
        response_array(&completion)
            .iter()
            .any(|item| item["label"] == "toJSON"),
        "completion should include an imported package symbol: {completion}"
    );

    let signature_help = server
        .request(
            "textDocument/signatureHelp",
            document_position(
                &workspace.uri,
                json!({
                    "line": position(SOURCE, "toJSON(result)", 0)["line"],
                    "character": position(SOURCE, "toJSON(result)", 0)["character"]
                        .as_u64()
                        .expect("the fixture position should be numeric")
                        + 9
                }),
            ),
        )
        .await;
    assert!(
        response_array(&signature_help["signatures"])
            .iter()
            .any(|signature| signature["label"] == "toJSON(Symbol) -> String"),
        "signature help should use the imported package partition: {signature_help}"
    );

    let semantic_tokens = server
        .request(
            "textDocument/semanticTokens/full",
            json!({"textDocument": {"uri": workspace.uri}}),
        )
        .await;
    assert!(
        !response_array(&semantic_tokens["data"]).is_empty(),
        "semantic token response should contain encoded tokens"
    );

    let document_symbols = server
        .request(
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": workspace.uri}}),
        )
        .await;
    assert!(
        response_array(&document_symbols)
            .iter()
            .any(|symbol| symbol["name"] == "double"),
        "document symbols should include analyzed bindings: {document_symbols}"
    );

    let ordered_actions = code_actions_at(
        &mut server,
        &workspace.uri,
        "orderedLeft",
        &document_diagnostics,
    )
    .await;
    assert_eq!(
        action_titles(&ordered_actions),
        ["Simplify unnecessary null branch", "Simplify if condition"]
    );

    for (needle, title, replacement) in [
        (
            "flatC",
            "Flatten nested if into else-if chain",
            "if flatA then flatOne else if flatB then flatTwo else if flatC then flatThree else flatFour",
        ),
        (
            "innerCondition",
            "Flatten nested if into else-if chain",
            "if not outerCondition then outerElse else if innerCondition then innerThen else innerElse",
        ),
        (
            "readyElse",
            "Simplify unnecessary null branch",
            "if readyElse then valueElse",
        ),
        (
            "attrStrings",
            "Simplify unnecessary null branch",
            "if member(\"Flexible\", attrStrings) then null",
        ),
        (
            "readySimple",
            "Simplify unnecessary null branch",
            "if not readySimple then valueSimple",
        ),
        (
            "binaryLeft",
            "Simplify unnecessary null branch",
            "if binaryLeft >= binaryRight then binaryValue",
        ),
        (
            "equalLeft",
            "Simplify unnecessary null branch",
            "if equalLeft != equalRight then equalValue",
        ),
        (
            "strictLeft",
            "Simplify unnecessary null branch",
            "if strictLeft =!= strictRight then strictValue",
        ),
        (
            "negatedReady",
            "Simplify unnecessary null branch",
            "if negatedReady then negatedValue",
        ),
        (
            "a\\nb\\tc",
            "Convert to raw string",
            "///a\nb\tc\"///",
        ),
        ("tryEcho", "Simplify try", "try tryEcho"),
        (
            "tryResult",
            "Simplify try",
            "try tryValue then\n    tryResult",
        ),
        ("bareTryValue", "Simplify try", "try bareTryValue"),
        (
            "simpleLeft",
            "Simplify if condition",
            "if simpleLeft != simpleRight then simpleValue",
        ),
        (
            "unequalLeft",
            "Simplify if condition",
            "if unequalLeft == unequalRight then unequalThen else unequalElse",
        ),
        (
            "lessLeft",
            "Simplify if condition",
            "if lessLeft >= lessRight then lessValue",
        ),
        (
            "doubleNotValue",
            "Simplify if condition",
            "if doubleNotValue then doubleNotResult",
        ),
    ] {
        let actions = code_actions_at(
            &mut server,
            &workspace.uri,
            needle,
            &document_diagnostics,
        )
        .await;
        assert_eq!(
            action_replacement(&actions, title, &workspace.uri),
            Some(replacement),
            "unexpected {title:?} edit at {needle:?}: {actions}"
        );
    }

    let ambiguous_actions = code_actions_at(
        &mut server,
        &workspace.uri,
        "memberValue.3",
        &document_diagnostics,
    )
    .await;
    let ambiguous_diagnostic = document_diagnostics
        .iter()
        .find(|diagnostic| diagnostic["code"] == "X03")
        .expect("the ambiguous member expression should produce X03");
    assert_eq!(
        ambiguous_diagnostic["message"],
        "This is parsed as application to a float literal; use `memberValue#3` for member access"
    );
    assert_eq!(
        action_replacement(
            &ambiguous_actions,
            "Rewrite as member access",
            &workspace.uri
        ),
        Some("memberValue#3")
    );

    for (needle, absent_title) in [
        ("existingB", "Flatten nested if into else-if chain"),
        ("a\\nb\"", "Convert to raw string"),
        ("a\\/\\/\\/b", "Convert to raw string"),
        ("\\101\\102\\103", "Convert to raw string"),
        ("exceptValue", "Simplify try"),
        ("simpleCondition", "Simplify if condition"),
    ] {
        let actions =
            code_actions_at(&mut server, &workspace.uri, needle, &document_diagnostics).await;
        assert!(
            !action_titles(&actions).contains(&absent_title),
            "{absent_title:?} must not be offered at {needle:?}: {actions}"
        );
    }

    let inlay_hints = server
        .request(
            "textDocument/inlayHint",
            json!({
                "textDocument": {"uri": workspace.uri},
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 100, "character": 0}
                }
            }),
        )
        .await;
    assert!(
        !response_array(&inlay_hints).is_empty(),
        "analyzed bindings should produce inlay hints"
    );

    let local_value_use = position(SOURCE, "localValue", 2);
    let highlights = server
        .request(
            "textDocument/documentHighlight",
            document_position(&workspace.uri, local_value_use.clone()),
        )
        .await;
    assert!(
        response_array(&highlights).len() >= 3,
        "documentation, declaration, and use should be highlighted: {highlights}"
    );

    let references = server
        .request(
            "textDocument/references",
            json!({
                "textDocument": {"uri": workspace.uri},
                "position": local_value_use.clone(),
                "context": {"includeDeclaration": true}
            }),
        )
        .await;
    assert!(
        response_array(&references).len() >= 3,
        "references should include documentation, declaration, and use: {references}"
    );
    assert!(
        response_array(&references)
            .iter()
            .any(|location| location["uri"] == workspace.related_uri),
        "global references should cross open workspace documents: {references}"
    );

    let prepare_rename = server
        .request(
            "textDocument/prepareRename",
            document_position(&workspace.uri, local_value_use.clone()),
        )
        .await;
    assert_eq!(
        prepare_rename["start"],
        position(SOURCE, "localValue", 2),
        "prepare rename should identify the requested symbol"
    );

    let boundary_operand = position(SOURCE, "boundaryParameter", 1);
    let boundary_prepare_rename = server
        .request(
            "textDocument/prepareRename",
            document_position(&workspace.uri, boundary_operand.clone()),
        )
        .await;
    assert_eq!(
        boundary_prepare_rename["start"], boundary_operand,
        "a symbol beginning at a zero-width application boundary should remain addressable"
    );

    let rename = server
        .request(
            "textDocument/rename",
            json!({
                "textDocument": {"uri": workspace.uri},
                "position": local_value_use.clone(),
                "newName": "renamedValue"
            }),
        )
        .await;
    assert!(
        rename["changes"][&workspace.uri]
            .as_array()
            .is_some_and(|edits| edits.len() >= 3),
        "rename should update documentation, declaration, and use: {rename}"
    );
    assert!(
        rename["changes"][&workspace.related_uri]
            .as_array()
            .is_some_and(|edits| edits.len() == 1),
        "rename should include references in another workspace document: {rename}"
    );

    let definition = server
        .request(
            "textDocument/definition",
            document_position(&workspace.uri, local_value_use),
        )
        .await;
    assert_eq!(definition["uri"], workspace.uri);
    assert_eq!(
        definition["range"]["start"],
        position(SOURCE, "localValue", 1),
        "go-to-definition should resolve the local declaration"
    );

    let hierarchy = server
        .request(
            "textDocument/prepareTypeHierarchy",
            document_position(&workspace.uri, position(SOURCE, "ZZ", 0)),
        )
        .await;
    let zz = response_array(&hierarchy)
        .first()
        .expect("ZZ should prepare a type hierarchy item")
        .clone();
    assert_eq!(zz["name"], "ZZ");
    let supertypes = server
        .request("typeHierarchy/supertypes", json!({"item": zz.clone()}))
        .await;
    assert!(
        response_array(&supertypes)
            .iter()
            .any(|item| item["name"] == "Number"),
        "ZZ should have Number as a static supertype: {supertypes}"
    );
    let subtypes = server
        .request("typeHierarchy/subtypes", json!({"item": zz}))
        .await;
    assert!(
        subtypes.is_array(),
        "known types should return a subtype collection"
    );

    let formatting = server
        .request(
            "textDocument/formatting",
            json!({
                "textDocument": {"uri": workspace.uri},
                "options": {
                    "tabSize": 4,
                    "insertSpaces": true
                }
            }),
        )
        .await;
    assert!(
        !response_array(&formatting).is_empty(),
        "the intentionally unformatted example should produce edits"
    );

    let folding = server
        .request(
            "textDocument/foldingRange",
            json!({"textDocument": {"uri": workspace.uri}}),
        )
        .await;
    assert!(
        !response_array(&folding).is_empty(),
        "the multiline conditional should produce a folding range"
    );

    let workspace_symbols = server
        .request("workspace/symbol", json!({"query": ""}))
        .await;
    assert!(
        response_array(&workspace_symbols).iter().any(|symbol| {
            symbol["name"] == "double" && symbol["location"]["uri"] == workspace.uri
        }),
        "workspace symbols should include document symbols from the primary source file: {workspace_symbols}"
    );
    assert!(
        response_array(&workspace_symbols).iter().any(|symbol| {
            symbol["name"] == "crossFileResult"
                && symbol["location"]["uri"] == workspace.related_uri
        }),
        "workspace symbols should include document symbols from other source files: {workspace_symbols}"
    );

    server
        .notify(
            "workspace/didChangeConfiguration",
            json!({
                "settings": {
                    "m2-ls": {
                        "diagnostics": {
                            "enabled": false
                        },
                        "formatting": {
                            "indent": 2,
                            "useTabs": false,
                            "compactFactorOperators": false
                        },
                        "inlayHints": {
                            "show": ["bindings"]
                        }
                    }
                }
            }),
        )
        .await;
    server
        .wait_for_server_request("workspace/inlayHint/refresh")
        .await;
    let disabled_diagnostics = server
        .wait_for_notification("textDocument/publishDiagnostics")
        .await;
    assert!(
        response_array(&disabled_diagnostics["params"]["diagnostics"]).is_empty(),
        "disabling diagnostics should republish an empty collection"
    );
    let configured_formatting = server
        .request(
            "textDocument/formatting",
            json!({
                "textDocument": {"uri": workspace.uri},
                "options": {
                    "tabSize": 8,
                    "insertSpaces": false
                }
            }),
        )
        .await;
    assert!(
        response_array(&configured_formatting)
            .first()
            .and_then(|edit| edit["newText"].as_str())
            .is_some_and(|text| text.contains("  encoded")),
        "server formatting settings should override client formatting options: {configured_formatting}"
    );
    assert!(
        server
            .server_requests
            .iter()
            .any(|method| method == "workspace/inlayHint/refresh"),
        "changing negotiated inlay-hint settings should request a client refresh"
    );

    let edit_probe = position(SOURCE, "editProbe", 0);
    let edit_probe_line = edit_probe["line"]
        .as_u64()
        .expect("the edit-probe line should be numeric");
    let edit_probe_character = edit_probe["character"]
        .as_u64()
        .expect("the edit-probe character should be numeric");
    let local_value = position(SOURCE, "localValue=1", 0);
    let local_value_line = local_value["line"]
        .as_u64()
        .expect("the local-value line should be numeric");
    let local_value_character = local_value["character"]
        .as_u64()
        .expect("the local-value character should be numeric");
    server
        .notify(
            "textDocument/didChange",
            json!({
                "textDocument": {
                    "uri": workspace.uri,
                    "version": 2
                },
                "contentChanges": [
                    {
                        "range": {
                            "start": {"line": edit_probe_line, "character": edit_probe_character + 4},
                            "end": {"line": edit_probe_line, "character": edit_probe_character + 5}
                        },
                        "text": "p"
                    },
                    {
                        "range": {
                            "start": {"line": edit_probe_line, "character": edit_probe_character + 8},
                            "end": {"line": edit_probe_line, "character": edit_probe_character + 9}
                        },
                        "text": "E"
                    },
                    {
                        "range": {
                            "start": {"line": local_value_line, "character": local_value_character + 11},
                            "end": {"line": local_value_line, "character": local_value_character + 12}
                        },
                        "text": "2"
                    }
                ]
            }),
        )
        .await;
    server
        .wait_for_notification("textDocument/publishDiagnostics")
        .await;
    let changed_symbols = server
        .request(
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": workspace.uri}}),
        )
        .await;
    assert!(
        response_array(&changed_symbols)
            .iter()
            .any(|symbol| symbol["name"] == "editprobE"),
        "multiple incremental changes must be applied in request order: {changed_symbols}"
    );
    let changed_hover = server
        .request(
            "textDocument/hover",
            document_position(&workspace.uri, position(SOURCE, "localValue", 1)),
        )
        .await;
    assert!(
        changed_hover["contents"]["value"]
            .as_str()
            .is_some_and(|markdown| markdown.contains("user-defined binding")),
        "incremental document updates should rebuild analysis: {changed_hover}"
    );

    let import_line = position(SOURCE, "needsPackage \"JSON\"", 0)["line"]
        .as_u64()
        .expect("the package-import line should be numeric");
    server
        .notify(
            "textDocument/didChange",
            json!({
                "textDocument": {
                    "uri": workspace.uri,
                    "version": 3
                },
                "contentChanges": [{
                    "range": {
                        "start": {"line": import_line, "character": 0},
                        "end": {"line": import_line + 1, "character": 0}
                    },
                    "text": ""
                }]
            }),
        )
        .await;
    server
        .wait_for_notification("textDocument/publishDiagnostics")
        .await;
    let source_without_json = SOURCE.replacen("needsPackage \"JSON\"\n", "", 1);
    let formerly_imported_hover = server
        .request(
            "textDocument/hover",
            document_position(&workspace.uri, position(&source_without_json, "toJSON", 2)),
        )
        .await;
    assert!(
        formerly_imported_hover["contents"]["value"]
            .as_str()
            .is_some_and(|markdown| markdown.contains("user-defined")),
        "removing an import must rederive source-ordered object visibility: {formerly_imported_hover}"
    );

    let replacement_source = "replacementValue := 2\nreplacementValue\n";
    server
        .notify(
            "textDocument/didChange",
            json!({
                "textDocument": {
                    "uri": workspace.uri,
                    "version": 4
                },
                "contentChanges": [{"text": replacement_source}]
            }),
        )
        .await;
    server
        .wait_for_notification("textDocument/publishDiagnostics")
        .await;
    let replacement_hover = server
        .request(
            "textDocument/hover",
            document_position(
                &workspace.uri,
                position(replacement_source, "replacementValue", 1),
            ),
        )
        .await;
    assert!(
        replacement_hover["contents"]["value"]
            .as_str()
            .is_some_and(|markdown| markdown.contains("`ZZ` · ")),
        "a full-content change must replace and reanalyze the document: {replacement_hover}"
    );

    server
        .notify(
            "textDocument/didClose",
            json!({"textDocument": {"uri": workspace.uri}}),
        )
        .await;
    let closed_hover = server
        .request(
            "textDocument/hover",
            document_position(
                &workspace.uri,
                position(replacement_source, "replacementValue", 1),
            ),
        )
        .await;
    assert!(closed_hover.is_null());

    server.shutdown().await;
}

#[tokio::test]
async fn runtime_syntax_example_reaches_analysis_without_error_diagnostics() {
    let source = include_str!("../fixtures/weird_valid_syntax.m2");
    let workspace = TestWorkspace::new(source);
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    server
        .notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": workspace.uri,
                    "languageId": "macaulay2",
                    "version": 1,
                    "text": source
                }
            }),
        )
        .await;

    let diagnostics = server
        .wait_for_notification("textDocument/publishDiagnostics")
        .await;
    let errors = response_array(&diagnostics["params"]["diagnostics"])
        .iter()
        .filter(|diagnostic| diagnostic["severity"] == 1)
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "runtime-valid syntax should complete the pipeline without errors: {errors:?}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn package_objects_become_visible_only_after_their_source_inclusion() {
    let source = "toJSON\nneedsPackage \"JSON\"\ntoJSON\n";
    let workspace = TestWorkspace::new(source);
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    server
        .notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": workspace.uri,
                    "languageId": "macaulay2",
                    "version": 1,
                    "text": source
                }
            }),
        )
        .await;
    server
        .wait_for_notification("textDocument/publishDiagnostics")
        .await;

    let before = server
        .request(
            "textDocument/hover",
            document_position(&workspace.uri, json!({"line": 0, "character": 1})),
        )
        .await;
    assert!(
        before.is_null(),
        "JSON must not be registered before needsPackage: {before}"
    );

    let after = server
        .request(
            "textDocument/hover",
            document_position(&workspace.uri, json!({"line": 2, "character": 1})),
        )
        .await;
    assert!(
        after["contents"]["value"]
            .as_str()
            .is_some_and(|markdown| markdown.contains("package `JSON`")),
        "JSON must be registered after needsPackage: {after}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn lambda_functions_keep_editor_roles_and_structural_layout() {
    let source = "\
expandMacro (Macro, String) := String => (m, block) ->
resultSource (transformOf m)(tokenStream parseMacroTree block)

matchingMacroClose = (src, bodyStart, outerName) -> (
    nestedNames := {};
    k := bodyStart;
    while k < #src do (
    k = k + 1;
    )
)
";
    let workspace = TestWorkspace::new(source);
    let mut server = LspProcess::spawn().await;
    let initialized = server.initialize(&workspace.root_uri()).await;
    server
        .notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": workspace.uri,
                    "languageId": "macaulay2",
                    "version": 1,
                    "text": source
                }
            }),
        )
        .await;
    server
        .wait_for_notification("textDocument/publishDiagnostics")
        .await;

    let formatting = server
        .request(
            "textDocument/formatting",
            json!({
                "textDocument": {"uri": workspace.uri},
                "options": {
                    "tabSize": 4,
                    "insertSpaces": true
                }
            }),
        )
        .await;
    let formatted = response_array(&formatting)
        .first()
        .and_then(|edit| edit["newText"].as_str())
        .expect("the unindented lambda body should produce a whole-document edit");
    assert!(
        formatted.contains(
            "(m, block) ->\n    resultSource (transformOf m)(tokenStream parseMacroTree block)"
        ),
        "the lambda body should be indented as an operator continuation: {formatted}"
    );
    assert!(
        formatted.contains("    while k < #src do (\n        k = k + 1;\n    )"),
        "a control-body opener should remain beside its keyword: {formatted}"
    );

    let folding = server
        .request(
            "textDocument/foldingRange",
            json!({"textDocument": {"uri": workspace.uri}}),
        )
        .await;
    assert!(
        response_array(&folding)
            .iter()
            .any(|range| range["startLine"] == 3 && range["endLine"] == 9),
        "the function-body fold should include its closing bracket: {folding}"
    );

    let semantic_tokens = server
        .request(
            "textDocument/semanticTokens/full",
            json!({"textDocument": {"uri": workspace.uri}}),
        )
        .await;
    let variable_type = response_array(
        &initialized["capabilities"]["semanticTokensProvider"]["legend"]["tokenTypes"],
    )
    .iter()
    .position(|token_type| token_type == "variable")
    .expect("the negotiated semantic-token legend should contain variable")
        as u64;
    let target = position(source, "k :=", 0);
    let target_line = target["line"]
        .as_u64()
        .expect("the target line should be numeric");
    let target_character = target["character"]
        .as_u64()
        .expect("the target character should be numeric");
    let mut line = 0;
    let mut character = 0;
    let token_type = response_array(&semantic_tokens["data"])
        .chunks_exact(5)
        .find_map(|token| {
            let delta_line = token[0].as_u64()?;
            let delta_character = token[1].as_u64()?;
            let length = token[2].as_u64()?;
            if delta_line == 0 {
                character += delta_character;
            } else {
                line += delta_line;
                character = delta_character;
            }
            (line == target_line
                && target_character >= character
                && target_character < character + length)
                .then(|| token[3].as_u64())
                .flatten()
        })
        .expect("the local k binding should have a semantic token");
    assert_eq!(
        token_type, variable_type,
        "a local copy of an untyped parameter should remain a variable"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn formatting_preserves_collection_and_assignment_control_flow_layout() {
    let source = concat!(
        "[xx := 1;yy = 2, zz := 3;]\n",
        "i=0;j=0;\n",
        "x =\n",
        "if x === null then (\n",
        "    2\n",
        ") else 3.1\n",
        "f = x -> (\n",
        "    x + 1)\n",
    );
    let workspace = TestWorkspace::new(source);
    let mut server = LspProcess::spawn().await;
    server
        .initialize_with_options(
            &workspace.root_uri(),
            json!({
                "formatting": {
                    "indent": 4,
                    "useTabs": false,
                    "softLineWidth": null,
                    "hardLineWidth": null,
                    "controlFlowLayout": "multilineCompactElse",
                    "compactFactorOperators": false,
                    "breakAfterSemicolon": true
                }
            }),
        )
        .await;
    server
        .notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": workspace.uri,
                    "languageId": "macaulay2",
                    "version": 1,
                    "text": source
                }
            }),
        )
        .await;
    server
        .wait_for_notification("textDocument/publishDiagnostics")
        .await;

    let formatting = server
        .request(
            "textDocument/formatting",
            json!({
                "textDocument": {"uri": workspace.uri},
                "options": {
                    "tabSize": 4,
                    "insertSpaces": true
                }
            }),
        )
        .await;
    let formatted = response_array(&formatting)
        .first()
        .and_then(|edit| edit["newText"].as_str())
        .expect("the unformatted source should produce a whole-document edit");
    assert_eq!(
        formatted,
        concat!(
            "[xx := 1; yy = 2, zz := 3;]\n",
            "i = 0;\n",
            "j = 0;\n",
            "x =\n",
            "if x === null then (\n",
            "    2\n",
            ") else 3.1\n",
            "f = x -> (\n",
            "    x + 1\n",
            ")\n",
        )
    );

    server.shutdown().await;
}

/// A client that disappears without `shutdown`, such as an editor that crashed
/// or was killed, closes the server's input. The server must end as well, with
/// the exit code the LSP specification gives a session ended without shutdown.
#[tokio::test]
async fn server_exits_when_the_client_disconnects() {
    let workspace = TestWorkspace::new(SOURCE);
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    server.notify("initialized", json!({})).await;

    assert_eq!(server.disconnect().await.code(), Some(1));
}

/// A Macaulay2 build tree (`Layout#2`) in an arbitrary place, holding a Core loaded through
/// its `loadsequence` and a package that loads a file of its own directory.
struct LibraryFixture {
    root: std::path::PathBuf,
}

impl LibraryFixture {
    fn new() -> Self {
        static NEXT_LIBRARY_ID: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(0);
        let sequence = NEXT_LIBRARY_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = env::temp_dir().join(format!("m2-ls-library-{}-{sequence}", std::process::id()));
        let packages = root.join("usr-dist/common/share/Macaulay2");
        std::fs::create_dir_all(packages.join("Core")).expect("Core directory");
        std::fs::create_dir_all(root.join("usr-dist/x86_64-Linux/lib/Macaulay2"))
            .expect("machine directory");
        std::fs::create_dir_all(packages.join("Saturation")).expect("package directory");
        let files = [
            ("Core/Core.m2", "toString = x -> x\n"),
            ("Core/loadsequence", "betti.m2\n"),
            ("Core/betti.m2", "betti = method()\nbetti Ideal := I -> I\n"),
            (
                "Saturation.m2",
                "saturationHelper = I -> I\nload \"./Saturation/impl.m2\"\n",
            ),
            (
                "Saturation/impl.m2",
                "saturate = method()\nsaturate Ideal := I -> saturationHelper I\nbetti Module := M -> M\n",
            ),
        ];
        for (path, text) in files {
            std::fs::write(packages.join(path), text).expect("library source");
        }
        Self { root }
    }

    fn uri(&self, path: &str) -> String {
        tower_lsp::lsp_types::Url::from_file_path(
            self.root.join("usr-dist/common/share/Macaulay2").join(path),
        )
        .expect("library path URI")
        .to_string()
    }

    fn sources_settings(&self, enabled: bool) -> Value {
        json!({
            "sources": {
                "enabled": enabled,
                "paths": [self.root.join("usr-dist")]
            }
        })
    }
}

impl Drop for LibraryFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The `(uri, start line)` of every location `method` answers at `position`.
async fn locations_at(
    server: &mut LspProcess,
    method: &str,
    uri: &str,
    position: Value,
) -> Vec<(String, u64)> {
    let response = server
        .request(method, document_position(uri, position))
        .await;
    let locations = match &response {
        Value::Array(locations) => locations.clone(),
        Value::Null => Vec::new(),
        location => vec![location.clone()],
    };
    let mut lines = locations
        .iter()
        .map(|location| {
            (
                location["uri"].as_str().expect("location URI").to_string(),
                location["range"]["start"]["line"]
                    .as_u64()
                    .expect("location line"),
            )
        })
        .collect::<Vec<_>>();
    lines.sort();
    lines
}

#[tokio::test]
async fn navigation_enters_configured_library_sources_of_loaded_packages() {
    let library = LibraryFixture::new();
    let source = "I = ideal 1\nbetti I\nsaturate I\ntoString I\n";
    let workspace = TestWorkspace::new(source);
    let mut server = LspProcess::spawn().await;
    server
        .initialize_with_options(&workspace.root_uri(), library.sources_settings(true))
        .await;
    server.open_document(&workspace.uri, source).await;

    assert_eq!(
        locations_at(
            &mut server,
            "textDocument/definition",
            &workspace.uri,
            position(source, "betti", 0)
        )
        .await,
        vec![(library.uri("Core/betti.m2"), 0)],
        "Core's files are the ones its loadsequence lists"
    );
    assert_eq!(
        locations_at(
            &mut server,
            "textDocument/definition",
            &workspace.uri,
            position(source, "toString", 0)
        )
        .await,
        vec![(library.uri("Core/Core.m2"), 0)]
    );
    assert_eq!(
        locations_at(
            &mut server,
            "textDocument/definition",
            &workspace.uri,
            position(source, "ideal", 0)
        )
        .await,
        Vec::new(),
        "a Core name no library file assigns has no definition to jump to"
    );
    assert_eq!(
        locations_at(
            &mut server,
            "textDocument/definition",
            &workspace.uri,
            position(source, "saturate", 0)
        )
        .await,
        vec![(library.uri("Saturation/impl.m2"), 0)],
        "a package's definitions come from the files it loads"
    );
    assert_eq!(
        locations_at(
            &mut server,
            "textDocument/implementation",
            &workspace.uri,
            position(source, "betti", 0)
        )
        .await,
        vec![
            (library.uri("Core/betti.m2"), 1),
            (library.uri("Saturation/impl.m2"), 2),
        ],
        "every loaded package may install a library method"
    );

    let package_file = library.uri("Saturation/impl.m2");
    let package_source = std::fs::read_to_string(
        library
            .root
            .join("usr-dist/common/share/Macaulay2/Saturation/impl.m2"),
    )
    .expect("package source");
    server.open_document(&package_file, &package_source).await;
    assert_eq!(
        locations_at(
            &mut server,
            "textDocument/definition",
            &package_file,
            position(&package_source, "saturationHelper", 0)
        )
        .await,
        vec![(library.uri("Saturation.m2"), 0)],
        "inside a library file, the package's private names resolve"
    );
    server.shutdown().await;

    let mut disabled = LspProcess::spawn().await;
    disabled
        .initialize_with_options(&workspace.root_uri(), library.sources_settings(false))
        .await;
    disabled.open_document(&workspace.uri, source).await;
    let definition = disabled
        .request(
            "textDocument/definition",
            document_position(&workspace.uri, position(source, "betti", 0)),
        )
        .await;
    assert_eq!(
        definition,
        Value::Null,
        "disabled library navigation finds no sources"
    );
    disabled.shutdown().await;
}

/// Documentation keys name the same library objects as ordinary expressions.
/// Method setup can create an object without assigning its name in source.
#[tokio::test]
async fn definition_of_documented_library_method_finds_installed_methods() {
    let library = LibraryFixture::new();
    let methods = "transpose Matrix := M -> M\n";
    std::fs::write(
        library
            .root
            .join("usr-dist/common/share/Macaulay2/Core/betti.m2"),
        methods,
    )
    .expect("method source");
    let source = "document { Key => transpose, \"Transpose a matrix.\" }\ntranspose matrix{{1}}\n";
    let workspace = TestWorkspace::new(source);
    let mut server = LspProcess::spawn().await;
    server
        .initialize_with_options(&workspace.root_uri(), library.sources_settings(true))
        .await;
    server.open_document(&workspace.uri, source).await;

    for occurrence in 0..2 {
        assert_eq!(
            locations_at(
                &mut server,
                "textDocument/definition",
                &workspace.uri,
                position(source, "transpose", occurrence),
            )
            .await,
            vec![(library.uri("Core/betti.m2"), 0)],
        );
    }
    server.shutdown().await;
}

#[tokio::test]
async fn standard_only_semantic_tokens_apply_live_with_a_refresh() {
    let source = "clearAll\nQQ[x]\n";
    let workspace = TestWorkspace::new(source);
    let mut server = LspProcess::spawn().await;
    let initialized = server.initialize(&workspace.root_uri()).await;
    server.open_document(&workspace.uri, source).await;

    let legend = &initialized["capabilities"]["semanticTokensProvider"]["legend"];
    let token_types = response_array(&legend["tokenTypes"]);
    let type_index = |name: &str| {
        token_types
            .iter()
            .position(|token_type| token_type == name)
            .unwrap_or_else(|| panic!("the legend should offer {name}: {legend}")) as u64
    };
    let function_type = type_index("function");
    let algebraic_modifier = 1u64
        << response_array(&legend["tokenModifiers"])
            .iter()
            .position(|modifier| modifier == "algebraic")
            .expect("algebraic modifier");
    assert!(!token_types.iter().any(|token_type| token_type == "command"));

    let clear_all_type = |tokens: &Value| response_array(&tokens["data"])[3].clone();
    let semantic_tokens_request = json!({"textDocument": {"uri": workspace.uri}});
    let tokens = server
        .request(
            "textDocument/semanticTokens/full",
            semantic_tokens_request.clone(),
        )
        .await;
    assert_eq!(
        clear_all_type(&tokens),
        json!(function_type),
        "the clearAll Command is a function by default"
    );

    assert!(response_array(&tokens["data"])
        .chunks_exact(5)
        .any(|token| token[4].as_u64().unwrap() & algebraic_modifier != 0));
    let readonly_modifier = 1u64
        << response_array(&legend["tokenModifiers"])
            .iter()
            .position(|modifier| modifier == "readonly")
            .expect("readonly modifier");
    assert_ne!(
        response_array(&tokens["data"])[4].as_u64().unwrap() & readonly_modifier,
        0
    );

    server
        .notify(
            "workspace/didChangeConfiguration",
            json!({ "settings": { "m2-ls": { "semanticTokens": { "standardOnly": true } } } }),
        )
        .await;
    server
        .wait_for_server_request("workspace/semanticTokens/refresh")
        .await;
    let tokens = server
        .request("textDocument/semanticTokens/full", semantic_tokens_request)
        .await;
    assert_eq!(
        clear_all_type(&tokens),
        json!(function_type),
        "under standardOnly the same legend carries it as a function"
    );

    assert!(response_array(&tokens["data"])
        .chunks_exact(5)
        .all(|token| token[4].as_u64().unwrap() & algebraic_modifier == 0));
    assert_ne!(
        response_array(&tokens["data"])[4].as_u64().unwrap() & readonly_modifier,
        0
    );

    server.shutdown().await;
}

#[tokio::test]
async fn an_imported_user_package_shows_only_its_exports() {
    let source = "needsPackage \"Pkg\"\ndouble 2\nhelper 2\n";
    let workspace = TestWorkspace::new(source);
    let package_uri = workspace.write_file(
        "Pkg.m2",
        concat!(
            "newPackage \"Pkg\"\n",
            "export {\"double\"}\n",
            "double = x -> 2 * x\n",
            "helper = x -> x\n",
            "endPackage \"Pkg\"\n",
        ),
    );
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    server.open_document(&workspace.uri, source).await;

    let exported = server
        .request(
            "textDocument/definition",
            document_position(&workspace.uri, position(source, "double", 0)),
        )
        .await;
    assert!(
        exported.to_string().contains(&package_uri),
        "an exported name resolves into the imported package: {exported}"
    );
    let private = server
        .request(
            "textDocument/definition",
            document_position(&workspace.uri, position(source, "helper", 0)),
        )
        .await;
    assert!(
        !private.to_string().contains(&package_uri),
        "a private name of the package stays hidden: {private}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn package_documentation_reaches_hover_across_the_workspace() {
    let source = "needsPackage \"Pkg\"\ndouble(2, Smoothing => 1)\n";
    let workspace = TestWorkspace::new(source);
    workspace.write_file(
        "Pkg.m2",
        concat!(
            "newPackage \"Pkg\"\n",
            "export {\"double\", \"Smoothing\"}\n",
            "double = method(Options => {Smoothing => 0})\n",
            "double ZZ := o -> x -> 2 * x\n",
            "beginDocumentation()\n",
            "load \"Pkg/doc.m2\"\n",
        ),
    );
    std::fs::create_dir(
        Url::parse(&workspace.root_uri())
            .unwrap()
            .to_file_path()
            .unwrap()
            .join("Pkg"),
    )
    .expect("the documentation directory should be created");
    let documentation = concat!(
        "doc ///\n",
        "Key\n",
        "  double\n",
        "  (double, ZZ)\n",
        "Headline\n",
        "  doubles a number\n",
        "Usage\n",
        "  double x\n",
        "Inputs\n",
        "  x:ZZ\n",
        "  Smoothing => ZZ\n",
        "Outputs\n",
        "  :ZZ -- twice x\n",
        "///\n",
        "doc ///\n",
        "Key\n",
        "  Smoothing\n",
        "  [double, Smoothing]\n",
        "Headline\n",
        "  how much to smooth\n",
        "///\n",
    );
    let documentation_uri = workspace.write_file("Pkg/doc.m2", documentation);
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    server.open_document(&workspace.uri, source).await;

    let hover_markdown = |hover: Value| {
        hover["contents"]["value"]
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    let function = hover_markdown(
        server
            .request(
                "textDocument/hover",
                document_position(&workspace.uri, position(source, "double", 0)),
            )
            .await,
    );
    for expected in [
        "user-defined method function",
        "doubles a number",
        "## Usage",
        "## Parameters\n\n- **x** — `ZZ`",
        "## Options\n\n- **`Smoothing`** — `ZZ`",
        "## Returns\n\n- `ZZ`, twice x",
    ] {
        assert!(function.contains(expected), "{expected:?} in {function}");
    }
    let option = hover_markdown(
        server
            .request(
                "textDocument/hover",
                document_position(&workspace.uri, position(source, "Smoothing", 0)),
            )
            .await,
    );
    assert!(option.contains("how much to smooth"), "{option}");

    server
        .open_document(&documentation_uri, documentation)
        .await;
    let key = hover_markdown(
        server
            .request(
                "textDocument/hover",
                document_position(&documentation_uri, position(documentation, "double", 0)),
            )
            .await,
    );
    assert!(key.contains("doubles a number"), "{key}");

    server.shutdown().await;
}
