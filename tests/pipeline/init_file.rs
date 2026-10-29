//! The init file end to end: the file every document starts with, as
//! `sources.initFile` selects it, the `-- m2-ls: initFile …` directive
//! selecting another or none, and what the server reports when one cannot be
//! read or parsed.

use std::fs;

use serde_json::{json, Value};

use crate::support::{document_position, position, response_array, LspProcess, TestWorkspace};

const UNAVAILABLE_INIT_FILE: &str = "E18";
const SYNTAX_ERROR: &str = "X01";
const AMBIGUOUS_FLOAT_MEMBER_ACCESS: &str = "X03";

/// An init file loading `JSON` and defining `initHelper`.
const INIT_FILE: &str = "needsPackage \"JSON\"\ninitHelper = x -> x + 1\n";

/// A document using what [`INIT_FILE`] provides.
const USES_INIT_FILE: &str = "y = initHelper 3\nz = toJSON 1\n";

/// Start a server for the workspace `root` with `initFile` as
/// `sources.initFile`, completing the initialization.
async fn start(root: &TestWorkspace, init_file: Value) -> LspProcess {
    let mut server = LspProcess::spawn().await;
    server
        .initialize_with_options(
            &root.root_uri(),
            json!({ "sources": { "initFile": init_file } }),
        )
        .await;
    server.notify("initialized", json!({})).await;
    server
}

/// The next diagnostics published for `uri`, skipping those of other files.
async fn diagnostics_for(server: &mut LspProcess, uri: &str) -> Vec<Value> {
    loop {
        let published = server
            .wait_for_notification("textDocument/publishDiagnostics")
            .await;
        if published["params"]["uri"] == uri {
            return response_array(&published["params"]["diagnostics"]).to_vec();
        }
    }
}

/// The codes of `diagnostics`.
fn codes(diagnostics: &[Value]) -> Vec<&str> {
    diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic["code"].as_str())
        .collect()
}

/// Open `text` at `uri`, returning its diagnostics.
async fn open(server: &mut LspProcess, uri: &str, text: &str) -> Vec<Value> {
    server
        .notify(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": uri, "languageId": "macaulay2", "version": 1, "text": text
            }}),
        )
        .await;
    diagnostics_for(server, uri).await
}

/// Where the definition of the first `name` in `text`, open at `uri`, is.
async fn definition(server: &mut LspProcess, uri: &str, text: &str, name: &str) -> String {
    server
        .request(
            "textDocument/definition",
            document_position(uri, position(text, name, 0)),
        )
        .await
        .to_string()
}

/// The inlay hint labels of the document `uri`.
async fn inlay_labels(server: &mut LspProcess, uri: &str) -> String {
    server
        .request(
            "textDocument/inlayHint",
            json!({
                "textDocument": {"uri": uri},
                "range": {"start": {"line": 0, "character": 0},
                          "end": {"line": 100, "character": 0}}
            }),
        )
        .await
        .to_string()
}

#[tokio::test]
async fn every_document_starts_with_the_configured_init_file() {
    let workspace = TestWorkspace::new("");
    let home = TestWorkspace::new("");
    let init_uri = home.write_file("init.m2", INIT_FILE);
    let mut server = start(&workspace, json!(home.path("init.m2"))).await;
    assert!(diagnostics_for(&mut server, &init_uri).await.is_empty());

    let uri = workspace.write_file("uses.m2", USES_INIT_FILE);
    open(&mut server, &uri, USES_INIT_FILE).await;

    assert!(
        definition(&mut server, &uri, USES_INIT_FILE, "initHelper")
            .await
            .contains(&init_uri),
        "the init file's global is known in every document"
    );
    assert!(
        inlay_labels(&mut server, &uri).await.contains("String"),
        "the package the init file loads is loaded"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn a_directive_starts_its_document_without_an_init_file_or_with_another() {
    let workspace = TestWorkspace::new("");
    let home = TestWorkspace::new("");
    let init_uri = home.write_file("init.m2", INIT_FILE);
    let project_init_uri = workspace.write_file("project-init.m2", "projectHelper = 2\n");
    let mut server = start(&workspace, json!(home.path("init.m2"))).await;
    diagnostics_for(&mut server, &init_uri).await;

    let without = format!("-- m2-ls: initFile none\n{USES_INIT_FILE}");
    let without_uri = workspace.write_file("without.m2", &without);
    open(&mut server, &without_uri, &without).await;
    assert!(
        !definition(&mut server, &without_uri, &without, "initHelper")
            .await
            .contains(&init_uri),
        "`initFile none` hides the init file's globals"
    );
    assert!(
        !inlay_labels(&mut server, &without_uri)
            .await
            .contains("String"),
        "`initFile none` leaves the init file's packages unloaded"
    );

    let other = "-- m2-ls: initFile project-init.m2\nprojectHelper\ninitHelper\n";
    let other_uri = workspace.write_file("other.m2", other);
    open(&mut server, &other_uri, other).await;
    assert!(definition(&mut server, &other_uri, other, "projectHelper")
        .await
        .contains(&project_init_uri));
    assert!(!definition(&mut server, &other_uri, other, "initHelper")
        .await
        .contains(&init_uri));

    // Another document still starts with the configured one, and does not
    // see what the directive selected.
    let plain = "initHelper\nprojectHelper\n";
    let plain_uri = workspace.write_file("plain.m2", plain);
    open(&mut server, &plain_uri, plain).await;
    assert!(definition(&mut server, &plain_uri, plain, "initHelper")
        .await
        .contains(&init_uri));
    assert!(!definition(&mut server, &plain_uri, plain, "projectHelper")
        .await
        .contains(&project_init_uri));

    server.shutdown().await;
}

#[tokio::test]
async fn a_directive_naming_an_unreadable_file_warns_on_the_directive() {
    let workspace = TestWorkspace::new("");
    let mut server = start(&workspace, json!(false)).await;
    workspace.create_dir("init-directory.m2");

    for (target, reason) in [
        ("missing-init.m2", "there is no init file"),
        ("init-directory.m2", "is a directory"),
    ] {
        let text = format!("-- m2-ls: initFile {target}\nx = 1\n");
        let uri = workspace.write_file(&format!("uses-{target}"), &text);
        let diagnostics = open(&mut server, &uri, &text).await;
        let warning = diagnostics
            .iter()
            .find(|diagnostic| diagnostic["code"] == UNAVAILABLE_INIT_FILE)
            .unwrap_or_else(|| panic!("no {UNAVAILABLE_INIT_FILE} for {target}: {diagnostics:?}"));
        assert_eq!(warning["range"]["start"]["line"], 0);
        let message = warning["message"].as_str().expect("a message");
        assert!(message.contains(reason), "{message}");
    }

    #[cfg(unix)]
    {
        workspace.write_file("locked-init.m2", "x = 1\n");
        if workspace.lock("locked-init.m2") {
            let text = "-- m2-ls: initFile locked-init.m2\n";
            let uri = workspace.write_file("uses-locked.m2", text);
            let diagnostics = open(&mut server, &uri, text).await;
            assert!(
                diagnostics.iter().any(|diagnostic| {
                    diagnostic["code"] == UNAVAILABLE_INIT_FILE
                        && diagnostic["message"]
                            .as_str()
                            .is_some_and(|message| message.contains("may not be read"))
                }),
                "{diagnostics:?}"
            );
            workspace.unlock("locked-init.m2");
        }
    }

    server.shutdown().await;
}

#[tokio::test]
async fn an_init_file_outside_the_workspace_reports_only_what_does_not_parse() {
    let workspace = TestWorkspace::new("");
    let home = TestWorkspace::new("");
    let init = "L = {10, 20}\nsecond = L.1\nbroken = (1 + ]\n";
    let init_uri = home.write_file("init.m2", init);
    let mut server = start(&workspace, json!(home.path("init.m2"))).await;

    let published = diagnostics_for(&mut server, &init_uri).await;
    let published = codes(&published);
    assert!(published.contains(&SYNTAX_ERROR), "{published:?}");
    assert!(
        !published.contains(&AMBIGUOUS_FLOAT_MEMBER_ACCESS),
        "findings in parsed code outside the workspace are not the user's: {published:?}"
    );

    let uri = workspace.write_file("uses.m2", "second\n");
    let buffer = open(&mut server, &uri, "second\n").await;
    assert!(
        !codes(&buffer).contains(&SYNTAX_ERROR),
        "the init file's errors are its own, not the buffer's: {buffer:?}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn an_init_file_inside_the_workspace_reports_every_finding() {
    let workspace = TestWorkspace::new("");
    let init_uri = workspace.write_file("init.m2", "L = {10, 20}\nsecond = L.1\n");
    let mut server = start(&workspace, json!("init.m2")).await;

    let published = diagnostics_for(&mut server, &init_uri).await;
    assert!(
        codes(&published).contains(&AMBIGUOUS_FLOAT_MEMBER_ACCESS),
        "{published:?}"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn a_configured_init_file_that_cannot_be_read_is_reported_once() {
    let workspace = TestWorkspace::new("");
    let missing = workspace.path("absent-init.m2");
    let mut server = start(&workspace, json!(missing)).await;

    let message = server.wait_for_notification("window/showMessage").await;
    let text = message["params"]["message"].as_str().expect("a message");
    assert!(text.contains("there is no init file"), "{text}");
    assert!(text.contains("sources.initFile"), "{text}");

    server.shutdown().await;
}

#[tokio::test]
async fn an_init_file_changed_on_disk_restarts_the_sessions_starting_with_it() {
    let workspace = TestWorkspace::new("");
    let home = TestWorkspace::new("");
    let init_uri = home.write_file("init.m2", INIT_FILE);
    let mut server = start(&workspace, json!(home.path("init.m2"))).await;
    diagnostics_for(&mut server, &init_uri).await;
    let text = "renamedHelper\n";
    let uri = workspace.write_file("uses.m2", text);
    open(&mut server, &uri, text).await;
    assert!(!definition(&mut server, &uri, text, "renamedHelper")
        .await
        .contains(&init_uri));

    fs::write(home.path("init.m2"), "renamedHelper = 1\n").expect("the init file changes");
    server
        .notify(
            "workspace/didChangeWatchedFiles",
            json!({"changes": [{"uri": init_uri, "type": 2}]}),
        )
        .await;
    diagnostics_for(&mut server, &init_uri).await;

    assert!(definition(&mut server, &uri, text, "renamedHelper")
        .await
        .contains(&init_uri));

    server.shutdown().await;
}
