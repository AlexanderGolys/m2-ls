//! Workspace files end to end: the files `load`, `needs` and `input` name,
//! looked up the way `load` looks them up, and the workspace folders and
//! file operations that keep them current. Each test builds the workspace
//! it needs on disk, odd names, links and permissions included.

use std::fs;

use serde_json::{json, Value};
use tower_lsp::lsp_types::Url;

use crate::support::{document_position, position, response_array, LspProcess, TestWorkspace};

const UNLOADABLE_FILE: &str = "E17";

/// The position just after occurrence `occurrence` of `needle` in `source`.
fn cursor_after(source: &str, needle: &str, occurrence: usize) -> Value {
    let mut cursor = position(source, needle, occurrence);
    let width = needle.encode_utf16().count() as u64;
    cursor["character"] = json!(cursor["character"].as_u64().expect("a column") + width);
    cursor
}

/// The labels completion offers at `cursor` of the document `uri`.
async fn completion_labels(server: &mut LspProcess, uri: &str, cursor: Value) -> Vec<String> {
    let completion = server
        .request("textDocument/completion", document_position(uri, cursor))
        .await;
    if completion.is_null() {
        return Vec::new();
    }
    let items = completion.get("items").unwrap_or(&completion);
    response_array(items)
        .iter()
        .filter_map(|item| item["label"].as_str().map(str::to_string))
        .collect()
}

/// The targets of the document links of `uri` that point at files.
async fn link_targets(server: &mut LspProcess, uri: &str) -> Vec<String> {
    let links = server
        .request(
            "textDocument/documentLink",
            json!({"textDocument": {"uri": uri}}),
        )
        .await;
    let mut targets = response_array(&links)
        .iter()
        .filter_map(|link| link["target"].as_str().map(str::to_string))
        .collect::<Vec<_>>();
    targets.sort();
    targets
}

/// The unloadable-file warnings of `published`, as their line and message.
fn unloadable_files(published: &Value) -> Vec<(u64, String)> {
    response_array(&published["params"]["diagnostics"])
        .iter()
        .filter(|diagnostic| diagnostic["code"] == UNLOADABLE_FILE)
        .map(|diagnostic| {
            (
                diagnostic["range"]["start"]["line"]
                    .as_u64()
                    .expect("a line"),
                diagnostic["message"]
                    .as_str()
                    .expect("a message")
                    .to_string(),
            )
        })
        .collect()
}

/// The URIs of the workspace symbols named `name`.
async fn symbol_locations(server: &mut LspProcess, name: &str) -> Vec<String> {
    let symbols = server
        .request("workspace/symbol", json!({"query": name}))
        .await;
    response_array(&symbols)
        .iter()
        .filter(|symbol| symbol["name"] == name)
        .filter_map(|symbol| symbol["location"]["uri"].as_str().map(str::to_string))
        .collect()
}

/// The new texts of the edits `edit` makes in `uri`, in order.
fn new_texts(edit: &Value, uri: &str) -> Vec<String> {
    edit["changes"][uri]
        .as_array()
        .map(|edits| {
            edits
                .iter()
                .filter_map(|edit| edit["newText"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The codes of the next diagnostics published for `uri`.
async fn next_diagnostic_codes(server: &mut LspProcess, uri: &str) -> Vec<String> {
    loop {
        let published = server
            .wait_for_notification("textDocument/publishDiagnostics")
            .await;
        if published["params"]["uri"] == uri {
            return response_array(&published["params"]["diagnostics"])
                .iter()
                .filter_map(|diagnostic| diagnostic["code"].as_str().map(str::to_string))
                .collect();
        }
    }
}

fn codes(published: &Value) -> Vec<&str> {
    response_array(&published["params"]["diagnostics"])
        .iter()
        .filter_map(|diagnostic| diagnostic["code"].as_str())
        .collect()
}

#[tokio::test]
async fn load_paths_link_navigate_complete_and_report_missing_files() {
    let source = "load \"lib/helper.m2\"\nload \"./missing.m2\"\nload \"lib/\"\n";
    let workspace = TestWorkspace::new(source);
    let helper = workspace.write_file("lib/helper.m2", "helperValue = 1\n");
    workspace.write_file("lib/notes.txt", "not a source file\n");
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    let opened = server.open_document(&workspace.uri, source).await;
    assert_eq!(
        codes(&opened),
        vec![UNLOADABLE_FILE, UNLOADABLE_FILE],
        "the absent file and the half-typed directory name: {opened}"
    );

    let links = server
        .request(
            "textDocument/documentLink",
            json!({"textDocument": {"uri": workspace.uri}}),
        )
        .await;
    assert!(
        response_array(&links)
            .iter()
            .any(|link| link["target"] == helper.as_str()),
        "a load links to the file it runs: {links}"
    );

    let definition = server
        .request(
            "textDocument/definition",
            document_position(&workspace.uri, position(source, "helper.m2", 0)),
        )
        .await;
    assert_eq!(definition["uri"], helper.as_str(), "{definition}");

    let mut cursor = position(source, "lib/\"", 0);
    cursor["character"] = json!(cursor["character"].as_u64().expect("column") + 4);
    let completion = server
        .request(
            "textDocument/completion",
            document_position(&workspace.uri, cursor),
        )
        .await;
    let items = completion.get("items").unwrap_or(&completion);
    let labels = response_array(items)
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        vec!["lib/helper.m2"],
        "only source files: {completion}"
    );

    let created = workspace.write_file("missing.m2", "");
    server
        .notify(
            "workspace/didCreateFiles",
            json!({"files": [{"uri": created}]}),
        )
        .await;
    assert_eq!(
        next_diagnostic_codes(&mut server, &workspace.uri).await,
        vec![UNLOADABLE_FILE],
        "the created file is no longer missing"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn renaming_a_loaded_file_edits_every_load_and_moves_its_index_entry() {
    let source = "load \"lib/helper.m2\"\nhelperValue\n";
    let workspace = TestWorkspace::new(source);
    let helper = workspace.write_file("lib/helper.m2", "load \"../shared.m2\"\nhelperValue = 1\n");
    workspace.write_file("shared.m2", "sharedValue = 2\n");
    // Neither open nor loaded by an open file: found on disk by the rename.
    let unindexed = workspace.write_file("other.m2", "needs \"./lib/helper.m2\"\n");
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    server.open_document(&workspace.uri, source).await;

    let moved = workspace.file_uri("lib/deeper/helper.m2");
    let rename = json!({"files": [{"oldUri": helper, "newUri": moved}]});
    let edit = server
        .request("workspace/willRenameFiles", rename.clone())
        .await;
    let new_text = |uri: &str| -> Vec<String> {
        response_array(&edit["changes"][uri])
            .iter()
            .filter_map(|edit| edit["newText"].as_str().map(str::to_string))
            .collect()
    };
    assert_eq!(
        new_text(&workspace.uri),
        vec![r#""lib/deeper/helper.m2""#],
        "{edit}"
    );
    assert_eq!(new_text(&helper), vec![r#""../../shared.m2""#], "{edit}");
    assert_eq!(
        new_text(&unindexed),
        vec![r#""./lib/deeper/helper.m2""#],
        "{edit}"
    );

    std::fs::create_dir_all(workspace.path("lib/deeper")).expect("the directory is created");
    std::fs::rename(
        workspace.path("lib/helper.m2"),
        workspace.path("lib/deeper/helper.m2"),
    )
    .expect("the file moves");
    server.notify("workspace/didRenameFiles", rename).await;
    assert_eq!(
        next_diagnostic_codes(&mut server, &workspace.uri).await,
        vec![UNLOADABLE_FILE],
        "the open loader is not edited yet"
    );

    let symbols = server
        .request("workspace/symbol", json!({"query": "helperValue"}))
        .await;
    let locations = response_array(&symbols)
        .iter()
        .filter(|symbol| symbol["name"] == "helperValue")
        .map(|symbol| symbol["location"]["uri"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(locations, vec![moved.as_str()], "{symbols}");

    server.shutdown().await;
}

#[tokio::test]
async fn closing_a_workspace_folder_drops_its_closed_files_from_the_index() {
    let source = "load \"related.m2\"\n";
    let workspace = TestWorkspace::new(source);
    let mut server = LspProcess::spawn().await;
    let initialized = server.initialize(&workspace.root_uri()).await;
    assert_eq!(
        initialized["capabilities"]["workspace"]["workspaceFolders"]["supported"],
        true
    );
    server.open_document(&workspace.uri, source).await;
    server
        .notify(
            "textDocument/didClose",
            json!({"textDocument": {"uri": workspace.uri}}),
        )
        .await;
    let indexed = |symbols: &Value| {
        response_array(symbols)
            .iter()
            .any(|symbol| symbol["name"] == "crossFileResult")
    };
    let symbols = server
        .request("workspace/symbol", json!({"query": "crossFile"}))
        .await;
    assert!(indexed(&symbols), "a closed file stays indexed: {symbols}");

    server
        .notify(
            "workspace/didChangeWorkspaceFolders",
            json!({"event": {
                "added": [],
                "removed": [{"uri": workspace.root_uri(), "name": "root"}],
            }}),
        )
        .await;
    let symbols = server
        .request("workspace/symbol", json!({"query": "crossFile"}))
        .await;
    assert!(!indexed(&symbols), "the folder's files leave: {symbols}");

    server.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn odd_load_names_resolve_like_load_and_say_why_they_cannot() {
    let lines = [
        r#"load "lib/helper.m2""#,
        r#"load """#,
        r#"load "lib""#,
        r#"load "locked.m2""#,
        r#"load "sealed/inner.m2""#,
        r#"load "we\"ird.m2""#,
        "load ///a b.m2///",
        r#"load "\x41.m2""#,
        r#"load "!ls""#,
        r#"load "$localhost:7500""#,
        r#"load "nofile.m2""#,
        r#"load "./nofile.m2""#,
        r#"load "plain.m2/inner.m2""#,
        r#"load "link.m2""#,
        r#"load "dangling.m2""#,
        r#"load "hop/../real.m2""#,
    ];
    let source = lines.join("\n") + "\n";
    let workspace = TestWorkspace::new(&source);
    let helper = workspace.write_file("lib/helper.m2", "");
    workspace.write_file("locked.m2", "");
    workspace.write_file("sealed/inner.m2", "");
    let quoted = workspace.write_file("we\"ird.m2", "");
    let spaced = workspace.write_file("a b.m2", "");
    let escaped = workspace.write_file("A.m2", "");
    workspace.write_file("plain.m2", "");
    workspace.write_file("real/target.m2", "");
    let linked = workspace.symlink("link.m2", "real/target.m2");
    workspace.symlink("dangling.m2", "nowhere.m2");
    workspace.create_dir("deep/inner");
    workspace.write_file("deep/real.m2", "");
    workspace.symlink("hop", "deep/inner");
    let physical = Url::from_file_path(
        fs::canonicalize(workspace.path("deep/real.m2")).expect("the file exists"),
    )
    .expect("an absolute path")
    .to_string();
    let permissions_enforced = workspace.lock("locked.m2") & workspace.lock("sealed");

    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    let opened = server.open_document(&workspace.uri, &source).await;
    let mut expected = vec![
        (1, "empty file name"),
        (2, "is a directory"),
        (3, "permission denied"),
        (4, "permission denied"),
        (10, "neither in the directory"),
        (11, "only in the directory"),
        (12, "neither in the directory"),
        (14, "neither in the directory"),
    ];
    if !permissions_enforced {
        expected.retain(|(line, _)| ![3, 4].contains(line));
    }
    let reported = unloadable_files(&opened);
    assert_eq!(
        reported.iter().map(|(line, _)| *line).collect::<Vec<_>>(),
        expected.iter().map(|(line, _)| *line).collect::<Vec<_>>(),
        "{reported:#?}"
    );
    for ((line, message), (_, reason)) in reported.iter().zip(&expected) {
        assert!(message.contains(reason), "line {line}: {message}");
    }

    let mut expected_targets = vec![helper, quoted, spaced, escaped, linked, physical.clone()];
    expected_targets.sort();
    assert_eq!(
        link_targets(&mut server, &workspace.uri).await,
        expected_targets
    );

    let definition = server
        .request(
            "textDocument/definition",
            document_position(&workspace.uri, position(&source, "hop/", 0)),
        )
        .await;
    assert_eq!(
        definition["uri"], physical,
        "`..` after a link leads where the link points"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn load_cycles_and_self_loads_index_each_file_once() {
    let source = "load \"b.m2\"\nload \"capability_spectrum.m2\"\naValue = 1\n";
    let workspace = TestWorkspace::new(source);
    let b = workspace.write_file("b.m2", "load \"capability_spectrum.m2\"\nbValue = 2\n");
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    let opened = server.open_document(&workspace.uri, source).await;
    assert!(unloadable_files(&opened).is_empty(), "{opened}");

    assert_eq!(symbol_locations(&mut server, "bValue").await, vec![b]);
    assert_eq!(
        symbol_locations(&mut server, "aValue").await,
        vec![workspace.uri.clone()]
    );

    server.shutdown().await;
}

#[tokio::test]
async fn documents_without_a_place_on_disk_report_no_load_problems() {
    let source = "load \"nofile.m2\"\nload \"\"\n";
    let workspace = TestWorkspace::new("");
    let untitled = "untitled:Untitled-1";
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    let opened = server.open_document(untitled, source).await;
    assert_eq!(
        unloadable_files(&opened).len(),
        1,
        "only the empty name is wrong wherever the file is: {opened}"
    );
    assert!(link_targets(&mut server, untitled).await.is_empty());
    assert!(
        completion_labels(&mut server, untitled, cursor_after(source, "load \"", 0))
            .await
            .is_empty()
    );

    server.shutdown().await;
}

#[tokio::test]
async fn renames_rewrite_odd_names_and_leave_path_lookups_alone() {
    let source = concat!(
        "load \"helper.m2\"\n",
        "load ///helper.m2///\n",
        "load \"./gen.m2\"\n",
        "load \"Pkg/lib.m2\"\n",
    );
    let workspace = TestWorkspace::new(source);
    let helper = workspace.write_file("helper.m2", "");
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    server.open_document(&workspace.uri, source).await;

    let edit = server
        .request(
            "workspace/willRenameFiles",
            json!({"files": [{"oldUri": helper, "newUri": workspace.file_uri("odd\"name/helper.m2")}]}),
        )
        .await;
    assert_eq!(
        new_texts(&edit, &workspace.uri),
        vec![r#""odd\"name/helper.m2""#, r#"///odd"name/helper.m2///"#],
        "each literal is rewritten in its own delimiters: {edit}"
    );

    let edit = server
        .request(
            "workspace/willRenameFiles",
            json!({"files": [{"oldUri": workspace.uri, "newUri": workspace.file_uri("sub/main.m2")}]}),
        )
        .await;
    assert_eq!(
        new_texts(&edit, &workspace.uri),
        vec![r#""../helper.m2""#, "///../helper.m2///", r#""../gen.m2""#],
        "a moved loader keeps pointing where it did, even at a file not there yet, \
         and leaves a name looked up along `path` alone: {edit}"
    );

    let invalid = json!({"files": [
        {"oldUri": "not a uri", "newUri": "neither"},
        {"oldUri": "untitled:a", "newUri": "untitled:b"},
    ]});
    let edit = server
        .request("workspace/willRenameFiles", invalid.clone())
        .await;
    assert!(edit.is_null(), "{edit}");
    server.notify("workspace/didRenameFiles", invalid).await;
    let garbage = json!({"files": [{"uri": "::"}, {"uri": "untitled:c"}]});
    server
        .notify("workspace/didCreateFiles", garbage.clone())
        .await;
    server.notify("workspace/didDeleteFiles", garbage).await;
    assert!(
        symbol_locations(&mut server, "helperValue")
            .await
            .is_empty(),
        "the server still answers"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn renaming_a_folder_rewrites_loads_into_it_but_not_within_it() {
    let source = "load \"lib/helper.m2\"\n";
    let workspace = TestWorkspace::new(source);
    let helper = workspace.write_file("lib/helper.m2", "load \"./util.m2\"\n");
    workspace.write_file("lib/util.m2", "");
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    server.open_document(&workspace.uri, source).await;

    let edit = server
        .request(
            "workspace/willRenameFiles",
            json!({"files": [{
                "oldUri": workspace.file_uri("lib"),
                "newUri": workspace.file_uri("src/lib"),
            }]}),
        )
        .await;
    assert_eq!(
        new_texts(&edit, &workspace.uri),
        vec![r#""src/lib/helper.m2""#]
    );
    assert!(new_texts(&edit, &helper).is_empty(), "{edit}");

    server.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn load_completion_writes_names_its_literal_can_hold_and_follows_links() {
    let source = concat!(
        "load \"lib/\"\n",
        "load ///lib/p///\n",
        "load \"locked/\"\n",
        "load \"lib/.\"\n",
    );
    let workspace = TestWorkspace::new(source);
    workspace.write_file("lib/we\"ird.m2", "");
    workspace.write_file("lib/.hidden.m2", "");
    workspace.write_file("lib/plain.m2", "");
    workspace.write_file("lib/notes.txt", "");
    workspace.create_dir("lib/sub");
    workspace.create_dir("other");
    workspace.symlink("lib/linked", "../other");
    workspace.symlink("lib/broken", "nowhere");
    workspace.write_file("locked/inside.m2", "");
    let permissions_enforced = workspace.lock("locked");
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    server.open_document(&workspace.uri, source).await;

    assert_eq!(
        completion_labels(
            &mut server,
            &workspace.uri,
            cursor_after(source, "\"lib/", 0)
        )
        .await,
        vec![
            "lib/linked/",
            "lib/plain.m2",
            "lib/sub/",
            r#"lib/we\"ird.m2"#
        ]
    );
    assert_eq!(
        completion_labels(
            &mut server,
            &workspace.uri,
            cursor_after(source, "///lib/", 0)
        )
        .await,
        vec!["lib/linked", "lib/plain.m2", "lib/sub", r#"lib/we"ird.m2"#],
        "a folder's `/` would close a verbatim string"
    );
    if permissions_enforced {
        assert!(completion_labels(
            &mut server,
            &workspace.uri,
            cursor_after(source, "locked/", 0)
        )
        .await
        .is_empty());
    }
    assert_eq!(
        completion_labels(
            &mut server,
            &workspace.uri,
            cursor_after(source, "lib/.", 0)
        )
        .await,
        vec!["lib/.hidden.m2"]
    );

    server.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn files_becoming_readable_or_vanishing_on_disk_update_their_loaders() {
    let source = "load \"dep.m2\"\n";
    let workspace = TestWorkspace::new(source);
    let dep = workspace.write_file("dep.m2", "depValue = 1\n");
    if !workspace.lock("dep.m2") {
        return;
    }
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    let opened = server.open_document(&workspace.uri, source).await;
    let reported = unloadable_files(&opened);
    assert!(
        reported.len() == 1 && reported[0].1.contains("permission denied"),
        "{reported:?}"
    );
    assert!(symbol_locations(&mut server, "depValue").await.is_empty());

    workspace.unlock("dep.m2");
    server
        .notify(
            "workspace/didChangeWatchedFiles",
            json!({"changes": [{"uri": dep, "type": 2}]}),
        )
        .await;
    assert!(next_diagnostic_codes(&mut server, &workspace.uri)
        .await
        .is_empty());
    assert_eq!(
        symbol_locations(&mut server, "depValue").await,
        vec![dep.clone()]
    );

    fs::remove_file(workspace.path("dep.m2")).expect("the file is removed");
    server
        .notify(
            "workspace/didChangeWatchedFiles",
            json!({"changes": [{"uri": dep, "type": 3}]}),
        )
        .await;
    assert_eq!(
        next_diagnostic_codes(&mut server, &workspace.uri).await,
        vec![UNLOADABLE_FILE]
    );
    assert!(symbol_locations(&mut server, "depValue").await.is_empty());

    server.shutdown().await;
}
