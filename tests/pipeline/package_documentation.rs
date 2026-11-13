//! Package-wide documentation coverage through the published LSP diagnostics.

use serde_json::json;

use crate::support::{response_array, DocumentSession, LspProcess, TestWorkspace};

/// Public exports are checked before documentation begins, with one warning per
/// export string. Private assignments and ordinary scripts need no documentation.
#[tokio::test]
async fn exports_without_any_documentation_are_reported() {
    let source = "newPackage \"P\"\nexport {\"first\", \"second\"}\nexportMutable \"counter\"\nfirst = x -> x\nsecond = 1\ncounter = 0\nprivateHelper = x -> x\n";
    let mut session = DocumentSession::open(source).await;
    let diagnostics = session
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic["code"] == "D07")
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 3, "{diagnostics:?}");
    for (diagnostic, name) in diagnostics.iter().zip(["first", "second", "counter"]) {
        assert_eq!(diagnostic["severity"], 2);
        assert!(diagnostic["message"].as_str().unwrap().contains(name));
    }
    session
        .replace("export \"standalone\"\nstandalone = 1\n")
        .await;
    assert!(!session.diagnostic_codes().contains(&"D07"));
    session.shutdown().await;
}

/// Documentation in another package cannot cover a same-named export. A method
/// key alone also leaves the public head undocumented.
#[tokio::test]
async fn export_coverage_is_scoped_to_direct_keys_in_the_owning_package() {
    let source = "newPackage \"P\"\nexport {\"shared\", \"methodOnly\", \"exempt\"}\nshared = method()\nmethodOnly = method()\nbeginDocumentation()\nundocumented {exempt}\ndocument {Key => (methodOnly, ZZ)}\nendPackage \"P\"\nnewPackage \"Q\"\nexport \"shared\"\nshared = method()\nbeginDocumentation()\ndocument {Key => shared}\nendPackage \"Q\"\n";
    let session = DocumentSession::open(source).await;
    let diagnostics = session
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic["code"] == "D07")
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
    assert!(diagnostics
        .iter()
        .all(|diagnostic| diagnostic["range"]["start"]["line"] == 1));
    session.shutdown().await;
}

/// Loaded documentation and exemptions cover exports from both the entry point
/// and source fragments; unrelated repository pages do not count.
#[tokio::test]
async fn loaded_documentation_covers_package_exports_across_files() {
    let main = "newPackage \"P\"\nexport {\"documented\", \"exempt\", \"missing\"}\nload \"implementation.m2\"\nbeginDocumentation()\nload \"documentation.m2\"\nendPackage \"P\"\n";
    let workspace = TestWorkspace::new(main);
    let fragment = "export {\"fragmentDocumented\", \"fragmentMissing\"}\ndocumented = x -> x\nexempt = 1\nmissing = 2\nfragmentDocumented = 3\nfragmentMissing = 4\n";
    let fragment_uri = workspace.write_file("implementation.m2", fragment);
    let docs = "document {Key => documented}\nundocumented {exempt}\ndoc ///\nKey\n  fragmentDocumented\nHeadline\n  a fragment item\n///\n";
    workspace.write_file("documentation.m2", docs);
    workspace.write_file("unrelated.m2", "document {Key => missing}\n");
    let mut server = LspProcess::spawn().await;
    server.initialize(&workspace.root_uri()).await;
    let opened = server.open_document(&workspace.uri, main).await;
    let findings = response_array(&opened["params"]["diagnostics"])
        .iter()
        .filter(|diagnostic| diagnostic["code"] == "D07")
        .collect::<Vec<_>>();
    assert_eq!(findings.len(), 1, "{opened}");
    assert!(findings[0]["message"]
        .as_str()
        .unwrap()
        .contains("`missing`"));
    let opened = server.open_document(&fragment_uri, fragment).await;
    let findings = response_array(&opened["params"]["diagnostics"])
        .iter()
        .filter(|diagnostic| diagnostic["code"] == "D07")
        .collect::<Vec<_>>();
    assert_eq!(findings.len(), 1, "{opened}");
    assert!(findings[0]["message"]
        .as_str()
        .unwrap()
        .contains("`fragmentMissing`"));

    // Changing an open documentation buffer must update the exporting buffer.
    let docs_uri = workspace.write_file("documentation.m2", docs);
    server.open_document(&docs_uri, docs).await;
    server
        .notify(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": docs_uri, "version": 2},
                "contentChanges": [{"text": format!("{docs}\ndocument {{Key => missing}}\n")}]
            }),
        )
        .await;
    loop {
        let report = server
            .wait_for_notification("textDocument/publishDiagnostics")
            .await;
        if report["params"]["uri"] == workspace.uri
            && !response_array(&report["params"]["diagnostics"])
                .iter()
                .any(|item| item["code"] == "D07")
        {
            break;
        }
    }
    server.shutdown().await;
}
