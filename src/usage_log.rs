//! Results of real linting and indexing, separate from operational telemetry.

use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower_lsp::lsp_types::{Diagnostic, Url};

/// Callback accepting one meaningful result and its structured fields.
type UsageCallback = dyn Fn(&str, Value) + Send + Sync;

/// Result callback installed only by a real command-line entry point.
static CALLBACK: OnceLock<Box<UsageCallback>> = OnceLock::new();

/// Location and opt-out settings for the shared usage log.
#[derive(Debug)]
struct UsageLogConfig;

impl UsageLogConfig {
    /// Path override; `off` disables recording, including in test subprocesses.
    const ENVIRONMENT: &str = "M2_LS_USAGE_LOG";
    /// Application directory below the user's persistent state directory.
    const DIRECTORY: &str = "m2-ls";
    /// Append-only result history, shared by the server and linter.
    const FILE: &str = "usage.jsonl";

    /// The configured path, without creating directories during discovery.
    fn path() -> Option<PathBuf> {
        if let Some(path) = env::var_os(Self::ENVIRONMENT) {
            return (!path.is_empty() && path != "off").then(|| PathBuf::from(path));
        }
        let state = env::var_os("XDG_STATE_HOME")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))?;
        Some(state.join(Self::DIRECTORY).join(Self::FILE))
    }
}

/// Synchronized output and the last published diagnostic snapshot for each URI.
#[derive(Debug)]
struct UsageLog<W> {
    /// Open append-only output, shared by this process's callback calls.
    file: W,
    /// Identical republishes in one editor session do not add another row.
    diagnostics: HashMap<String, Value>,
}

impl<W: Write> UsageLog<W> {
    /// Append one result. A write failure never changes the tool's own result.
    fn record(&mut self, tool: &str, event: &str, data: Value) {
        let uri = data.get("uri").and_then(Value::as_str);
        if tool == "m2-ls" && event == "diagnostics" {
            if let Some(uri) = uri {
                if self.diagnostics.get(uri) == Some(&data) {
                    return;
                }
            }
        }
        let row = json!({
            "timestamp_ms": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
            "pid": std::process::id(),
            "tool": tool,
            "version": env!("CARGO_PKG_VERSION"),
            "event": event,
            "data": data,
        });
        let Ok(mut bytes) = serde_json::to_vec(&row) else {
            return;
        };
        bytes.push(b'\n');
        if self.file.write_all(&bytes).is_ok() && tool == "m2-ls" && event == "diagnostics" {
            if let Some(uri) = uri {
                self.diagnostics.insert(uri.to_owned(), data);
            }
        }
    }
}

/// Enable persistent result logging for a real process. Unit tests cannot enable it.
/// Test subprocesses opt out through `M2_LS_USAGE_LOG=off`.
pub fn enable(tool: &'static str) {
    if cfg!(test) || CALLBACK.get().is_some() {
        return;
    }
    let Some(path) = UsageLogConfig::path() else {
        return;
    };
    let opened = (|| {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        OpenOptions::new().create(true).append(true).open(&path)
    })();
    let file = match opened {
        Ok(file) => file,
        Err(error) => {
            eprintln!("{tool}: cannot open usage log {}: {error}", path.display());
            return;
        }
    };
    let log = Mutex::new(UsageLog {
        file,
        diagnostics: HashMap::new(),
    });
    let _ = CALLBACK.set(Box::new(move |event, data| {
        log.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .record(tool, event, data);
    }));
}

/// Record one meaningful result when the executable installed a callback.
pub fn record(event: &str, data: Value) {
    if let Some(callback) = CALLBACK.get() {
        callback(event, data);
    }
}

/// Record the actual findings, including empty results, without retaining source text.
pub fn diagnostics(uri: &Url, text: &str, findings: &[Diagnostic]) {
    if CALLBACK.get().is_some() {
        record(
            "diagnostics",
            json!({
                "uri": uri,
                "source_hash": format!("{:x}", Sha256::digest(text.as_bytes())),
                "count": findings.len(),
                "diagnostics": findings,
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_snapshots_keep_changes_and_clean_results_without_protocol_noise() {
        let mut log = UsageLog {
            file: Vec::new(),
            diagnostics: HashMap::new(),
        };
        let findings = json!({"uri": "file:///work/code.m2", "source_hash": "one", "count": 1,
            "diagnostics": [{"code": "T04", "message": "codomain mismatch"}]});
        log.record("m2-ls", "diagnostics", findings.clone());
        log.record("m2-ls", "diagnostics", findings.clone());
        log.record(
            "m2-ls",
            "diagnostics",
            json!({"uri": "file:///work/code.m2",
            "source_hash": "two", "count": 0, "diagnostics": []}),
        );
        log.record(
            "m2-ls",
            "indexing",
            json!({"packages": 3, "indexed_files": 8}),
        );
        // Separate CLI checks are real observations even when their findings match.
        log.record("m2-lint", "diagnostics", findings.clone());
        log.record("m2-lint", "diagnostics", findings);
        let text = String::from_utf8(log.file).unwrap();
        let rows = text
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0]["data"]["diagnostics"][0]["code"], "T04");
        assert_eq!(rows[1]["data"]["count"], 0);
        assert_eq!(rows[2]["data"]["packages"], 3);
        assert!(rows
            .iter()
            .all(|row| row["timestamp_ms"].is_number() && row["version"].is_string()));
    }

    #[test]
    fn tests_cannot_enable_a_file_writer() {
        enable("m2-lint");
        record("diagnostics", json!({"count": 1}));
        assert!(CALLBACK.get().is_none());
    }
}
