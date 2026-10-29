//! Bounded structured diagnostics and timings, independent of the LSP stream.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// Maximum log size before rotation replaces the previous log backup.
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;
/// Duration threshold above which operation timing is emitted without verbose logging.
const SLOW_OPERATION: Duration = Duration::from_millis(100);
/// Minimum interval between aggregate telemetry summaries.
const SUMMARY_INTERVAL: Duration = Duration::from_secs(30);
/// Bound on distinct operation names retained in aggregate statistics.
const MAX_STATISTICS_KEYS: usize = 128;
/// Process-wide logger initialized on the first telemetry operation.
static LOGGER: OnceLock<Logger> = OnceLock::new();
/// Process-wide counter allocating timing-event identities.
static NEXT_SPAN: AtomicU64 = AtomicU64::new(1);
/// Number of typed navigation lookups since process startup.
static SYNTAX_LOOKUPS: AtomicU64 = AtomicU64::new(0);
/// Number of syntax-child enumerations since process startup.
static SYNTAX_CHILDREN: AtomicU64 = AtomicU64::new(0);

/// Counts one navigation lookup for aggregate syntax telemetry.
pub fn syntax_lookup() {
    SYNTAX_LOOKUPS.fetch_add(1, Ordering::Relaxed);
}
/// Counts one syntax-child enumeration for aggregate telemetry.
pub fn syntax_children() {
    SYNTAX_CHILDREN.fetch_add(1, Ordering::Relaxed);
}

/// Bounded structured event writer with synchronized timing statistics.
struct Logger {
    /// Filesystem path of the represented file.
    path: PathBuf,
    /// Whether to record detailed timings even for fast operations.
    verbose: bool,
    /// Source-ordered state of the selected binding.
    state: Mutex<LogState>,
}

/// Mutable file, byte count, and accumulated operation statistics behind the logger lock.
struct LogState {
    /// Open log file, absent when logging could not be initialized.
    file: Option<File>,
    /// Bytes already written to the current log file, used to decide when to rotate.
    bytes: u64,
    /// Accumulated timings grouped by operation name.
    statistics: HashMap<String, Statistics>,
    /// Time at which aggregate statistics were last written.
    last_summary: Instant,
}

/// Count, cumulative duration, and maximum duration for one timed operation.
#[derive(Default)]
struct Statistics {
    /// Number of completed operations contributing to these statistics.
    count: u64,
    /// Sum of operation durations in milliseconds.
    total_ms: f64,
    /// Longest recorded operation duration in milliseconds.
    maximum_ms: f64,
}

/// Initializes the process logger once from the logging environment setting.
fn logger() -> &'static Logger {
    LOGGER.get_or_init(|| {
        let setting = std::env::var_os("M2_LS_LOG");
        let path = setting
            .as_ref()
            .filter(|value| *value != "1" && *value != "true" && !value.is_empty())
            .map_or_else(|| PathBuf::from("/tmp/m2-ls.log"), PathBuf::from);
        Logger::new(path, setting.is_some())
    })
}

impl Logger {
    /// Opens bounded structured logging at the configured path and records the verbosity policy.
    fn new(path: PathBuf, verbose: bool) -> Self {
        let bytes = fs::metadata(&path).map_or(0, |metadata| metadata.len());
        Self {
            path,
            verbose,
            state: Mutex::new(LogState {
                file: None,
                bytes,
                statistics: HashMap::new(),
                last_summary: Instant::now(),
            }),
        }
    }

    /// Writes a structured log event, rotating the file at the configured size bound.
    fn write(&self, state: &mut LogState, event: Value) {
        let row = json!({
            "timestamp_ms": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
            "pid": std::process::id(),
            "thread": format!("{:?}", std::thread::current().id()),
            "event": event,
        });
        let Ok(mut row) = serde_json::to_vec(&row) else {
            return;
        };
        row.push(b'\n');
        if state.bytes.saturating_add(row.len() as u64) > MAX_LOG_BYTES {
            state.file.take();
            let backup = self.path.with_extension("log.1");
            if fs::rename(&self.path, backup).is_err() && self.path.exists() {
                return;
            }
            state.bytes = 0;
        }
        if state.file.is_none() {
            state.file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .ok();
        }
        if let Some(file) = &mut state.file {
            if file.write_all(&row).is_ok() {
                state.bytes += row.len() as u64;
            }
        }
    }

    /// Records a structured event without writing to the protocol stream.
    fn event(&self, event: Value) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.write(&mut state, event);
    }

    /// Records a finished operation and updates its aggregate duration statistics.
    fn complete(&self, operation: &str, fields: Value, elapsed: Duration, force: bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let milliseconds = elapsed.as_secs_f64() * 1000.0;
        let key = fields.get("method").and_then(Value::as_str).map_or_else(
            || operation.to_string(),
            |method| format!("{operation}:{method}"),
        );
        if state.statistics.len() < MAX_STATISTICS_KEYS || state.statistics.contains_key(&key) {
            let statistics = state.statistics.entry(key).or_default();
            statistics.count += 1;
            statistics.total_ms += milliseconds;
            statistics.maximum_ms = statistics.maximum_ms.max(milliseconds);
        }
        if self.verbose || force || elapsed >= SLOW_OPERATION {
            self.write(
                &mut state,
                json!({"phase": "end", "operation": operation,
                "duration_ms": milliseconds, "slow": elapsed >= SLOW_OPERATION, "fields": fields}),
            );
        }
        if state.last_summary.elapsed() >= SUMMARY_INTERVAL {
            self.summary(&mut state);
        }
    }

    /// Writes accumulated operation counts and duration statistics.
    fn summary(&self, state: &mut LogState) {
        let mut statistics = state
            .statistics
            .iter()
            .map(|(name, value)| {
                json!({"operation": name, "count": value.count, "total_ms": value.total_ms,
                "max_ms": value.maximum_ms, "mean_ms": value.total_ms / value.count as f64})
            })
            .collect::<Vec<_>>();
        statistics.sort_by(|left, right| {
            right["total_ms"]
                .as_f64()
                .partial_cmp(&left["total_ms"].as_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        self.write(
            state,
            json!({"operation": "timing_summary", "statistics": statistics, "syntax_lookups": SYNTAX_LOOKUPS.load(Ordering::Relaxed), "syntax_child_enumerations": SYNTAX_CHILDREN.load(Ordering::Relaxed)}),
        );
        state.last_summary = Instant::now();
    }
}

/// A wall-clock interval. Unfinished request futures retain their cancellation
/// status when dropped, and verbose start rows expose work still in progress.
pub struct Timing {
    /// Stable operation name used to group telemetry events.
    operation: &'static str,
    /// Structured context attached to the timed operation.
    fields: Value,
    /// Instant at which timing began.
    started: Instant,
    /// Whether to emit this operation even below the slow-operation threshold.
    force: bool,
}

impl Timing {
    /// Begins a timed operation with the supplied structured context.
    pub fn start(operation: &'static str, mut fields: Value) -> Self {
        let id = NEXT_SPAN.fetch_add(1, Ordering::Relaxed);
        fields["span_id"] = json!(id);
        if logger().verbose {
            logger().event(json!({"operation": operation, "phase": "start", "fields": fields}));
        }
        Self {
            operation,
            fields,
            started: Instant::now(),
            force: false,
        }
    }

    /// Adds a context field to this timed operation.
    pub fn record(&mut self, key: &str, value: impl Into<Value>) {
        self.fields[key] = value.into();
    }

    /// Records the failure context and forces emission of the timed operation.
    pub fn error(&mut self, error: impl Into<Value>) {
        self.record("error", error);
        self.force = true;
    }
}

impl Drop for Timing {
    fn drop(&mut self) {
        logger().complete(
            self.operation,
            std::mem::take(&mut self.fields),
            self.started.elapsed(),
            self.force,
        );
    }
}

/// Records a structured event without writing to the protocol stream.
pub fn event(event: Value) {
    logger().event(event);
}

/// Writes accumulated operation counts and duration statistics.
pub fn summary() {
    let logger = logger();
    let mut state = logger
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    logger.summary(&mut state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_keep_request_context_and_statistics_without_verbose_logging() {
        let path =
            std::env::temp_dir().join(format!("m2-ls-telemetry-{}.jsonl", std::process::id()));
        let logger = Logger::new(path.clone(), false);
        logger.complete(
            "rpc",
            json!({"method": "textDocument/definition", "request_id": 7,
            "error": {"code": -32600}}),
            Duration::from_millis(2),
            true,
        );
        {
            let mut state = logger.state.lock().unwrap();
            logger.summary(&mut state);
        }
        let rows = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(rows[0]["event"]["fields"]["error"]["code"], -32600);
        assert_eq!(rows[0]["event"]["fields"]["request_id"], 7);
        assert_eq!(
            rows[1]["event"]["statistics"][0]["operation"],
            "rpc:textDocument/definition"
        );
        assert_eq!(rows[1]["event"]["statistics"][0]["count"], 1);
        drop(logger);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn log_rotation_keeps_only_the_current_file_and_one_backup() {
        let path =
            std::env::temp_dir().join(format!("m2-ls-log-rotation-{}.jsonl", std::process::id()));
        let logger = Logger::new(path.clone(), false);
        for sequence in 0..3 {
            let mut state = logger.state.lock().unwrap();
            state.bytes = MAX_LOG_BYTES;
            logger.write(&mut state, json!({"sequence": sequence}));
        }
        let current: Value =
            serde_json::from_str(fs::read_to_string(&path).unwrap().trim()).unwrap();
        let backup: Value = serde_json::from_str(
            fs::read_to_string(path.with_extension("log.1"))
                .unwrap()
                .trim(),
        )
        .unwrap();
        assert_eq!(current["event"]["sequence"], 2);
        assert_eq!(backup["event"]["sequence"], 1);
        drop(logger);
        fs::remove_file(&path).unwrap();
        fs::remove_file(path.with_extension("log.1")).unwrap();
    }
}
