//! Process-level contracts for the standalone tools.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "m2-cli-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn run(&self, binary: &str, args: &[&str], stdin: &str) -> Output {
        let mut child = Command::new(binary)
            .args(args)
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

const FMT: &str = env!("CARGO_BIN_EXE_m2-fmt");
const LINT: &str = env!("CARGO_BIN_EXE_m2-lint");

#[test]
fn formatter_check_write_and_stdin_agree() {
    let workspace = Workspace::new();
    let source = "x=1+2\n";
    fs::write(workspace.0.join("file.m2"), source).unwrap();
    let formatted = workspace.run(FMT, &["-"], source);
    assert!(formatted.status.success());
    assert_ne!(formatted.stdout, source.as_bytes());
    let check = workspace.run(FMT, &["--check", "file.m2"], "");
    assert_eq!(check.status.code(), Some(1));
    assert_eq!(
        fs::read_to_string(workspace.0.join("file.m2")).unwrap(),
        source
    );
    let stdout = workspace.run(FMT, &["--stdout", "file.m2"], "");
    assert_eq!(stdout.stdout, formatted.stdout);
    assert!(workspace.run(FMT, &["file.m2"], "").status.success());
    assert_eq!(
        fs::read(workspace.0.join("file.m2")).unwrap(),
        formatted.stdout
    );
    assert!(workspace
        .run(FMT, &["--check", "file.m2"], "")
        .status
        .success());
}

#[test]
fn tools_take_exactly_one_file() {
    let workspace = Workspace::new();
    fs::create_dir(workspace.0.join("nested")).unwrap();
    for name in ["a.m2", "b.m2", "nested/c.m2"] {
        fs::write(workspace.0.join(name), "x=1\n").unwrap();
    }
    for binary in [FMT, LINT] {
        for args in [
            vec![],
            vec!["a.m2", "b.m2"],
            vec!["nested"],
            vec!["-", "a.m2"],
        ] {
            let output = workspace.run(binary, &args, "");
            assert_eq!(output.status.code(), Some(2), "{binary} {args:?}");
        }
    }
    for name in ["a.m2", "b.m2", "nested/c.m2"] {
        assert_eq!(fs::read_to_string(workspace.0.join(name)).unwrap(), "x=1\n");
    }
    let lint = workspace.run(LINT, &["nested/c.m2"], "");
    assert!(lint.status.success(), "{lint:?}");
    let lint = workspace.run(
        LINT,
        &["--deny-warnings", "-"],
        "L = {10, 20}\nsecond = L.1\n",
    );
    assert!(
        String::from_utf8_lossy(&lint.stdout).starts_with("-:2:"),
        "{lint:?}"
    );
    fs::write(
        workspace.0.join("nested/c.m2"),
        "L = {10, 20}\nsecond = L.1\n",
    )
    .unwrap();
    let lint = workspace.run(LINT, &["nested/c.m2"], "");
    assert!(
        String::from_utf8_lossy(&lint.stdout).starts_with("nested/c.m2:2:"),
        "{lint:?}"
    );
}

#[test]
fn lint_reports_shared_diagnostics_and_respects_config() {
    let workspace = Workspace::new();
    let source = "if 1 then 2 else 3\n";
    let lint = workspace.run(LINT, &["--deny-warnings", "-"], source);
    assert_eq!(lint.status.code(), Some(1), "{lint:?}");
    let stdout = String::from_utf8(lint.stdout).unwrap();
    assert!(stdout.contains("-:1:"), "{stdout}");
    assert!(stdout.contains("warning[T"), "{stdout}");
    fs::write(
        workspace.0.join("settings.json"),
        r#"{"diagnostics":{"enabled":false}}"#,
    )
    .unwrap();
    let filtered = workspace.run(LINT, &["--config", "settings.json", "-"], source);
    assert!(filtered.status.success(), "{filtered:?}");
    assert!(filtered.stdout.is_empty());
    assert!(workspace.run(LINT, &["-"], "1 + 2\n").status.success());
    assert_eq!(
        workspace.run(LINT, &["-"], "x = (1 + ]\n").status.code(),
        Some(1)
    );
}

#[test]
fn usage_and_io_errors_have_distinct_exit_status() {
    let workspace = Workspace::new();
    for args in [
        vec!["--unknown"],
        vec!["missing.m2"],
        vec!["-", "file.m2"],
        vec!["--config"],
    ] {
        assert_eq!(workspace.run(FMT, &args, "").status.code(), Some(2));
    }
    fs::write(
        workspace.0.join("settings.json"),
        r#"{"formatting":{"indent":"bad"}}"#,
    )
    .unwrap();
    assert_eq!(
        workspace
            .run(FMT, &["--config", "settings.json", "-"], "")
            .status
            .code(),
        Some(2)
    );
    fs::write(
        workspace.0.join("settings.json"),
        r#"{"inlayHints":{"calls":"every"}}"#,
    )
    .unwrap();
    let deprecated = workspace.run(LINT, &["--config", "settings.json", "-"], "1 + 2\n");
    assert!(deprecated.status.success(), "{deprecated:?}");
    assert!(
        String::from_utf8_lossy(&deprecated.stderr).contains("deprecated, read as"),
        "{deprecated:?}"
    );
    assert!(workspace.run(FMT, &["--help"], "").status.success());
    assert!(workspace.run(LINT, &["--version"], "").status.success());
}

#[test]
fn lint_warning_policy_and_formatter_configuration() {
    let workspace = Workspace::new();
    let warning_source = "L = {10, 20}\nsecond = L.1\n";
    let warning = workspace.run(LINT, &["-"], warning_source);
    assert!(warning.status.success(), "{warning:?}");
    assert!(String::from_utf8_lossy(&warning.stdout).contains("warning[X03]"));
    assert_eq!(
        workspace
            .run(LINT, &["--deny-warnings", "-"], warning_source)
            .status
            .code(),
        Some(1)
    );
    fs::write(
        workspace.0.join("settings.json"),
        r#"{"diagnostics":{"disabled":["X03"]},"formatting":{"indent":2}}"#,
    )
    .unwrap();
    assert!(workspace
        .run(
            LINT,
            &["--deny-warnings", "--config", "settings.json", "-"],
            warning_source
        )
        .status
        .success());
    let formatted = workspace.run(
        FMT,
        &["--config", "settings.json", "-"],
        "f = x -> (\nx+1\n)\n",
    );
    assert!(formatted.status.success());
    assert!(
        String::from_utf8_lossy(&formatted.stdout).contains("\n  x + 1\n"),
        "{formatted:?}"
    );
}

#[test]
fn formatter_leaves_invalid_source_untouched() {
    let workspace = Workspace::new();
    let invalid = "x = (1 + ]\n";
    fs::write(workspace.0.join("a.m2"), invalid).unwrap();
    assert_eq!(workspace.run(FMT, &["a.m2"], "").status.code(), Some(2));
    assert_eq!(
        fs::read_to_string(workspace.0.join("a.m2")).unwrap(),
        invalid
    );
}
