//! Shared framed-stdio language-server integration-test support.

use std::collections::VecDeque;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use std::{env, fs};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;
use tower_lsp::lsp_types::Url;

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
static TEMP_DIRECTORY_ID: AtomicUsize = AtomicUsize::new(0);

/// A child `m2-ls` process with framed JSON-RPC input and output.
pub(crate) struct LspProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    pending: VecDeque<Value>,
    pub(crate) server_requests: Vec<String>,
}

impl LspProcess {
    pub(crate) async fn spawn() -> Self {
        // An empty environment keeps the server from discovering the machine's
        // Macaulay2 installation, so results do not depend on where tests run.
        let mut child = Command::new(env!("CARGO_BIN_EXE_m2-ls"))
            .env_clear()
            .env(
                "M2_LS_CATALOG",
                m2_ls::catalog::CatalogSource::current()
                    .and_then(|source| source.path())
                    .expect("test catalog must be available"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("the m2-ls test binary should start");
        let stdin = child
            .stdin
            .take()
            .expect("the language server should have stdin");
        let stdout = child
            .stdout
            .take()
            .expect("the language server should have stdout");

        Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
            pending: VecDeque::new(),
            server_requests: Vec::new(),
        }
    }

    pub(crate) async fn initialize(&mut self, root_uri: &str) -> Value {
        self.initialize_with_options(
            root_uri,
            json!({ "inlayHints": base_inlay_hint_settings() }),
        )
        .await
    }

    pub(crate) async fn initialize_with_options(
        &mut self,
        root_uri: &str,
        initialization_options: Value,
    ) -> Value {
        self.request(
            "initialize",
            json!({
                "processId": null,
                "rootUri": root_uri,
                "capabilities": {
                    "workspace": {
                        "inlayHint": {
                            "refreshSupport": true
                        },
                        "semanticTokens": {
                            "refreshSupport": true
                        }
                    },
                    "textDocument": {
                        "semanticTokens": {
                            "requests": {
                                "full": true
                            },
                            "tokenTypes": [],
                            "tokenModifiers": [],
                            "formats": ["relative"],
                            "augmentsSyntaxTokens": true
                        },
                        "typeHierarchy": {
                            "dynamicRegistration": false
                        }
                    }
                },
                "initializationOptions": initialization_options
            }),
        )
        .await
    }

    pub(crate) async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let mut message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method
        });
        if !params.is_null() {
            message["params"] = params;
        }
        self.send(message).await;

        loop {
            let message = if let Some(index) = self.pending.iter().position(|message| {
                message.get("method").is_none()
                    && message.get("id").and_then(Value::as_u64) == Some(id)
            }) {
                self.pending
                    .remove(index)
                    .expect("the indexed pending response should exist")
            } else {
                self.read_message().await
            };
            if message.get("method").is_some() {
                self.handle_server_message(message).await;
                continue;
            }
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                self.pending.push_back(message);
                continue;
            }
            assert!(
                message.get("error").is_none(),
                "{method} returned an error: {message}"
            );
            return message.get("result").cloned().unwrap_or(Value::Null);
        }
    }

    pub(crate) async fn notify(&mut self, method: &str, params: Value) {
        let mut message = json!({
            "jsonrpc": "2.0",
            "method": method
        });
        if !params.is_null() {
            message["params"] = params;
        }
        self.send(message).await;
    }

    /// Open `text` at `uri` and return the diagnostics published for it.
    pub(crate) async fn open_document(&mut self, uri: &str, text: &str) -> Value {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "macaulay2",
                    "version": 1,
                    "text": text
                }
            }),
        )
        .await;
        self.wait_for_notification("textDocument/publishDiagnostics")
            .await
    }

    pub(crate) async fn wait_for_notification(&mut self, method: &str) -> Value {
        if let Some(index) = self
            .pending
            .iter()
            .position(|message| message.get("method") == Some(&Value::String(method.to_string())))
        {
            return self
                .pending
                .remove(index)
                .expect("the indexed pending notification should exist");
        }

        loop {
            let message = self.read_message().await;
            if message.get("method").and_then(Value::as_str) == Some(method)
                && message.get("id").is_none()
            {
                return message;
            }
            if message.get("method").is_some() {
                self.handle_server_message(message).await;
            } else {
                self.pending.push_back(message);
            }
        }
    }

    pub(crate) async fn wait_for_server_request(&mut self, method: &str) {
        loop {
            let message = self.read_message().await;
            if message.get("method").and_then(Value::as_str) == Some(method)
                && message.get("id").is_some()
            {
                self.handle_server_message(message).await;
                return;
            }
            if message.get("method").is_some() {
                self.handle_server_message(message).await;
            } else {
                self.pending.push_back(message);
            }
        }
    }

    /// Shuts the server down the way an editor does: `shutdown`, then `exit`,
    /// with its input still open. The server must end on `exit` alone.
    pub(crate) async fn shutdown(mut self) {
        let result = self.request("shutdown", Value::Null).await;
        assert!(result.is_null());
        self.notify("exit", Value::Null).await;
        let status = timeout(RESPONSE_TIMEOUT, self.child.wait())
            .await
            .expect("the language server should exit after shutdown")
            .expect("the language-server process should be waitable");
        assert!(status.success(), "language server exited with {status}");
    }

    /// Closes the server's input without `shutdown`, as a client that crashed
    /// or was killed does, and returns how the server then exits.
    pub(crate) async fn disconnect(self) -> ExitStatus {
        let Self {
            mut child, stdin, ..
        } = self;
        drop(stdin);
        timeout(RESPONSE_TIMEOUT, child.wait())
            .await
            .expect("the language server should exit once its client disconnects")
            .expect("the language-server process should be waitable")
    }

    async fn handle_server_message(&mut self, message: Value) {
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            self.pending.push_back(message);
            return;
        };
        let Some(id) = message.get("id").cloned() else {
            self.pending.push_back(message);
            return;
        };
        self.server_requests.push(method.to_string());
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": null
        }))
        .await;
    }

    async fn send(&mut self, message: Value) {
        let body = serde_json::to_vec(&message).expect("JSON-RPC messages should serialize");
        self.stdin
            .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
            .await
            .expect("the JSON-RPC header should write");
        self.stdin
            .write_all(&body)
            .await
            .expect("the JSON-RPC body should write");
        self.stdin
            .flush()
            .await
            .expect("the JSON-RPC message should flush");
    }

    async fn read_message(&mut self) -> Value {
        timeout(RESPONSE_TIMEOUT, async {
            let mut content_length = None;
            loop {
                let mut header = String::new();
                let read = self
                    .stdout
                    .read_line(&mut header)
                    .await
                    .expect("the language-server header should be readable");
                assert_ne!(read, 0, "language server closed stdout before responding");
                if header == "\r\n" {
                    break;
                }
                if let Some(length) = header.strip_prefix("Content-Length:") {
                    content_length = Some(
                        length
                            .trim()
                            .parse::<usize>()
                            .expect("Content-Length should be an integer"),
                    );
                }
            }

            let mut body = vec![0; content_length.expect("response should have Content-Length")];
            self.stdout
                .read_exact(&mut body)
                .await
                .expect("the language-server response body should be readable");
            serde_json::from_slice(&body).expect("the language server should emit JSON")
        })
        .await
        .expect("timed out waiting for the language server")
    }
}

/// The `inlayHints` settings every test server starts with: every inferred
/// type, uncut, so the tests observe all that inference finds.
fn base_inlay_hint_settings() -> Value {
    json!({
        "everywhere": true,
        "showThing": true,
        "maxLabelLength": null
    })
}

/// A unique on-disk workspace removed when its test finishes.
pub(crate) struct TestWorkspace {
    root: std::path::PathBuf,
    pub(crate) uri: String,
    related_source_path: std::path::PathBuf,
    pub(crate) related_uri: String,
    /// Paths whose permissions [`Self::lock`] took, restored before removal.
    locked: std::cell::RefCell<Vec<std::path::PathBuf>>,
}

impl TestWorkspace {
    pub(crate) fn new(source: &str) -> Self {
        let root = loop {
            let unique = TEMP_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
            let candidate =
                env::temp_dir().join(format!("m2-ls-integration-{}-{unique}", std::process::id()));
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("the integration workspace should be created: {error}"),
            }
        };
        let source_path = root.join("capability_spectrum.m2");
        fs::write(&source_path, source).expect("the integration fixture should be written");
        let uri = Url::from_file_path(&source_path)
            .expect("the fixture path should become a URI")
            .to_string();
        let related_source_path = root.join("related.m2");
        fs::write(&related_source_path, "crossFileResult = localValue\n")
            .expect("the related integration fixture should be written");
        let related_uri = Url::from_file_path(&related_source_path)
            .expect("the related fixture path should become a URI")
            .to_string();

        Self {
            root,
            uri,
            related_source_path,
            related_uri,
            locked: std::cell::RefCell::default(),
        }
    }

    /// Create the directory `name` and its parents.
    pub(crate) fn create_dir(&self, name: &str) {
        fs::create_dir_all(self.path(name)).expect("the workspace directory should be created");
    }

    /// Make `name` a symbolic link to `target`, as written, returning the
    /// link's URI.
    #[cfg(unix)]
    pub(crate) fn symlink(&self, name: &str, target: &str) -> String {
        std::os::unix::fs::symlink(target, self.path(name))
            .expect("the symbolic link should be created");
        self.file_uri(name)
    }

    /// Take every permission from `name`, returning whether the operating
    /// system now refuses it: running as root, it does not.
    #[cfg(unix)]
    pub(crate) fn lock(&self, name: &str) -> bool {
        use std::os::unix::fs::PermissionsExt;
        let path = self.path(name);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000))
            .expect("the permissions should be taken");
        self.locked.borrow_mut().push(path.clone());
        fs::File::open(&path).is_err()
    }

    /// Give back the permissions [`Self::lock`] took from `name`.
    #[cfg(unix)]
    pub(crate) fn unlock(&self, name: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = self.path(name);
        let mode = if path.is_dir() { 0o700 } else { 0o600 };
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))
            .expect("the permissions should be given back");
        self.locked.borrow_mut().retain(|locked| *locked != path);
    }

    /// The path of `name` in the workspace.
    pub(crate) fn path(&self, name: &str) -> std::path::PathBuf {
        self.root.join(name)
    }

    /// The URI of `name` in the workspace, whether or not it exists.
    pub(crate) fn file_uri(&self, name: &str) -> String {
        Url::from_file_path(self.path(name))
            .expect("the workspace file path should become a URI")
            .to_string()
    }

    /// Write `text` to the file `name` of the workspace, creating its
    /// directories, returning its URI.
    pub(crate) fn write_file(&self, name: &str, text: &str) -> String {
        let path = self.root.join(name);
        if let Some(directory) = path.parent() {
            fs::create_dir_all(directory).expect("the workspace directory should be created");
        }
        fs::write(&path, text).expect("the workspace file should be written");
        Url::from_file_path(&path)
            .expect("the workspace file path should become a URI")
            .to_string()
    }

    pub(crate) fn root_uri(&self) -> String {
        Url::from_directory_path(&self.root)
            .expect("the workspace path should become a URI")
            .to_string()
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        #[cfg(unix)]
        for path in self.locked.take() {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o700));
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub(crate) fn position(source: &str, needle: &str, occurrence: usize) -> Value {
    let byte_index = source
        .match_indices(needle)
        .nth(occurrence)
        .map(|(index, _)| index)
        .unwrap_or_else(|| panic!("fixture should contain occurrence {occurrence} of {needle:?}"));
    let prefix = &source[..byte_index];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    let character = source[line_start..byte_index].encode_utf16().count();
    json!({
        "line": line,
        "character": character
    })
}

pub(crate) fn document_position(uri: &str, position: Value) -> Value {
    json!({
        "textDocument": {
            "uri": uri
        },
        "position": position
    })
}

pub(crate) fn response_array(result: &Value) -> &[Value] {
    result
        .as_array()
        .unwrap_or_else(|| panic!("expected an array response, got {result}"))
}

/// One open document backed by a real stdio language-server process.
pub(crate) struct DocumentSession {
    server: LspProcess,
    workspace: TestWorkspace,
    source: String,
    version: i32,
    diagnostics: Value,
    semantic_token_types: Vec<String>,
    semantic_token_modifiers: Vec<String>,
}

impl DocumentSession {
    pub(crate) async fn open(source: &str) -> Self {
        Self::open_with_related(source, "crossFileResult = localValue\n").await
    }

    pub(crate) async fn open_with_related(source: &str, related_source: &str) -> Self {
        let workspace = TestWorkspace::new(source);
        fs::write(&workspace.related_source_path, related_source)
            .expect("the related integration fixture should be replaced");
        let mut server = LspProcess::spawn().await;
        let initialized = server.initialize(&workspace.root_uri()).await;
        let semantic_token_types = response_array(
            &initialized["capabilities"]["semanticTokensProvider"]["legend"]["tokenTypes"],
        )
        .iter()
        .filter_map(|token_type| token_type.as_str().map(str::to_string))
        .collect();
        let semantic_token_modifiers = response_array(
            &initialized["capabilities"]["semanticTokensProvider"]["legend"]["tokenModifiers"],
        )
        .iter()
        .filter_map(|modifier| modifier.as_str().map(str::to_string))
        .collect();
        // Only open documents and the files they load are indexed, so the
        // related file joins the workspace by being open as well.
        server
            .open_document(&workspace.related_uri, related_source)
            .await;
        let diagnostics = server.open_document(&workspace.uri, source).await;

        Self {
            server,
            workspace,
            source: source.to_string(),
            version: 1,
            diagnostics,
            semantic_token_types,
            semantic_token_modifiers,
        }
    }

    pub(crate) async fn replace(&mut self, source: &str) {
        self.version += 1;
        self.server
            .notify(
                "textDocument/didChange",
                json!({
                    "textDocument": {
                        "uri": self.workspace.uri,
                        "version": self.version
                    },
                    "contentChanges": [{
                        "text": source
                    }]
                }),
            )
            .await;
        self.source.clear();
        self.source.push_str(source);
        self.diagnostics = self
            .server
            .wait_for_notification("textDocument/publishDiagnostics")
            .await;
    }

    pub(crate) async fn save(&mut self) {
        self.server
            .notify(
                "textDocument/didSave",
                json!({"textDocument": {"uri": self.workspace.uri}}),
            )
            .await;
        self.diagnostics = self
            .server
            .wait_for_notification("textDocument/publishDiagnostics")
            .await;
    }

    pub(crate) fn diagnostic_codes(&self) -> Vec<&str> {
        response_array(&self.diagnostics["params"]["diagnostics"])
            .iter()
            .filter_map(|diagnostic| diagnostic["code"].as_str())
            .collect()
    }

    pub(crate) fn diagnostics(&self) -> &[Value] {
        response_array(&self.diagnostics["params"]["diagnostics"])
    }

    pub(crate) async fn request(&mut self, method: &str, params: Value) -> Value {
        self.server.request(method, params).await
    }

    /// Apply `changes` to the `inlayHints` settings the session starts with,
    /// and wait for the refresh it causes.
    pub(crate) async fn set_inlay_hint_settings(&mut self, changes: Value) {
        let mut inlay_hints = base_inlay_hint_settings();
        for (key, value) in changes
            .as_object()
            .expect("inlay hint settings are an object")
        {
            inlay_hints[key] = value.clone();
        }
        self.server
            .notify(
                "workspace/didChangeConfiguration",
                json!({ "settings": { "m2-ls": { "inlayHints": inlay_hints } } }),
            )
            .await;
        self.server
            .wait_for_server_request("workspace/inlayHint/refresh")
            .await;
    }

    /// Apply semantic-token settings and wait for the client refresh request.
    pub(crate) async fn set_semantic_token_settings(&mut self, settings: Value) {
        self.server
            .notify(
                "workspace/didChangeConfiguration",
                json!({"settings": {"m2-ls": {"semanticTokens": settings}}}),
            )
            .await;
        self.server
            .wait_for_server_request("workspace/semanticTokens/refresh")
            .await;
    }

    /// Show inlay hints at the default places only, not after every
    /// subexpression.
    pub(crate) async fn show_default_inlay_hint_anchors(&mut self) {
        self.set_inlay_hint_settings(json!({ "everywhere": false }))
            .await;
    }

    /// Also show the types literals state and nearby hints repeat.
    pub(crate) async fn show_every_known_inlay_hint_type(&mut self) {
        self.set_inlay_hint_settings(json!({ "showLiteral": true, "showRepeated": true }))
            .await;
    }

    pub(crate) async fn request_at(
        &mut self,
        method: &str,
        needle: &str,
        occurrence: usize,
    ) -> Value {
        self.server
            .request(
                method,
                document_position(
                    &self.workspace.uri,
                    position(&self.source, needle, occurrence),
                ),
            )
            .await
    }

    pub(crate) async fn completion_labels(
        &mut self,
        needle: &str,
        occurrence: usize,
    ) -> Vec<String> {
        let mut cursor = position(&self.source, needle, occurrence);
        cursor["character"] = Value::from(
            cursor["character"]
                .as_u64()
                .expect("fixture character should be numeric")
                + needle.encode_utf16().count() as u64,
        );
        let response = self
            .server
            .request(
                "textDocument/completion",
                document_position(&self.workspace.uri, cursor),
            )
            .await;
        let items = response
            .as_array()
            .or_else(|| response["items"].as_array())
            .map(Vec::as_slice)
            .unwrap_or_default();
        items
            .iter()
            .filter_map(|item| item["label"].as_str().map(str::to_string))
            .collect()
    }

    pub(crate) fn uri(&self) -> &str {
        &self.workspace.uri
    }

    pub(crate) fn semantic_token_type(&self, name: &str) -> u64 {
        self.semantic_token_types
            .iter()
            .position(|token_type| token_type == name)
            .unwrap_or_else(|| panic!("semantic-token legend should contain {name}")) as u64
    }

    pub(crate) fn semantic_token_modifier(&self, name: &str) -> u64 {
        1 << self
            .semantic_token_modifiers
            .iter()
            .position(|modifier| modifier == name)
            .unwrap_or_else(|| panic!("semantic-token legend should contain {name}"))
    }

    pub(crate) async fn shutdown(self) {
        self.server.shutdown().await;
    }
}
