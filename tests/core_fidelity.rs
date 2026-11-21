//! How closely `m2-fmt` reproduces the Macaulay2 Core library's own layout.
//!
//! Run against a checkout's `M2/Macaulay2/m2` directory:
//!
//! ```text
//! M2_CORE_DIR=~/M2/M2/Macaulay2/m2 cargo test --test core_fidelity -- --ignored --nocapture
//! ```
//!
//! `M2_FMT_CONFIG` optionally names a settings file passed to `m2-fmt --config`,
//! to measure a profile other than the defaults. Every file is formatted twice:
//! the first run must succeed (`m2-fmt` refuses output that parses differently
//! from its input) and the second must change nothing.
//!
//! For each file the table reports the share of non-blank lines that come out
//! byte-identical and the share whose indentation, in visual columns with a tab
//! stop every [`Corpus::TAB_WIDTH`] columns, is unchanged. Shares are compared
//! line by line when formatting keeps the number of non-blank lines, and as
//! multisets otherwise. The summary averages files with equal weight.
//!
//! Baseline, the 2.9.0 defaults on Core `stable` `a5ff8d7` (93 files): 32.4 %
//! byte-identical, 32.7 % indent kept, and 4 files (`document.m2`,
//! `methods.m2`, `packages.m2`, `system.m2`) not a fixpoint.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{env, fs, thread};

/// The corpus files and how `m2-fmt` treats them.
struct Corpus {
    /// The `.m2` files measured, sorted by name.
    files: Vec<PathBuf>,
    /// The settings file under test, if any.
    config: Option<PathBuf>,
}

/// One file's measurement.
struct FileFidelity {
    /// What went wrong, if formatting was rejected or is not a fixpoint.
    problem: Option<String>,
    /// The file name.
    name: String,
    /// Non-blank lines of the original.
    lines: usize,
    /// Of those, lines reproduced byte for byte.
    identical: usize,
    /// Of those, lines whose indentation column is kept.
    indent_kept: usize,
}

impl Corpus {
    /// Files measured: a generated export list would dominate the statistics,
    /// and files this short say nothing about layout.
    const EXCLUDED: &[&str] = &["exports.m2"];
    /// The fewest code lines, neither blank nor comment-only, a measured file has.
    const MIN_CODE_LINES: usize = 5;
    /// Columns of a tab stop, as Emacs and the Core sources assume.
    const TAB_WIDTH: usize = 8;

    /// The corpus named by `M2_CORE_DIR`.
    fn from_env() -> Self {
        let directory = PathBuf::from(env::var("M2_CORE_DIR").expect("set M2_CORE_DIR"));
        let mut files = fs::read_dir(&directory)
            .expect("M2_CORE_DIR is readable")
            .map(|entry| entry.expect("directory entry").path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "m2"))
            .filter(|path| !Self::EXCLUDED.contains(&Self::name(path)))
            .filter(|path| {
                Self::code_lines(&fs::read_to_string(path).unwrap()) >= Self::MIN_CODE_LINES
            })
            .collect::<Vec<_>>();
        files.sort();
        Self {
            files,
            config: env::var_os("M2_FMT_CONFIG").map(PathBuf::from),
        }
    }

    /// The file name of a corpus path.
    fn name(path: &Path) -> &str {
        path.file_name().and_then(|name| name.to_str()).unwrap()
    }

    /// Lines that are neither blank nor comment-only.
    fn code_lines(text: &str) -> usize {
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("--"))
            .count()
    }

    /// `text` formatted by `m2-fmt` with the settings under test, or why it
    /// was refused.
    fn format(&self, text: &str) -> Result<String, String> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_m2-fmt"));
        if let Some(config) = &self.config {
            command.arg("--config").arg(config);
        }
        let mut child = command
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("m2-fmt starts");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        Ok(String::from_utf8(output.stdout).unwrap())
    }

    /// Format one file twice, checking the second run is a fixpoint.
    fn measure(&self, path: &Path) -> FileFidelity {
        let name = Self::name(path).to_string();
        let original = fs::read_to_string(path).unwrap();
        let formatted = match self.format(&original) {
            Ok(formatted) => formatted,
            Err(problem) => {
                let mut fidelity = FileFidelity::compare(name, &original, &original);
                fidelity.problem = Some(problem);
                return fidelity;
            }
        };
        let mut fidelity = FileFidelity::compare(name, &original, &formatted);
        fidelity.problem = match self.format(&formatted) {
            Ok(again) if again == formatted => None,
            Ok(_) => Some("formatting twice changes the result".into()),
            Err(problem) => Some(format!("formatting the result again: {problem}")),
        };
        fidelity
    }
}

impl FileFidelity {
    /// The fidelity of `formatted` to `original`.
    fn compare(name: String, original: &str, formatted: &str) -> Self {
        let before = Self::non_blank_lines(original);
        let after = Self::non_blank_lines(formatted);
        let (identical, indent_kept) = if before.len() == after.len() {
            let pairs = || before.iter().zip(&after);
            (
                pairs().filter(|(a, b)| a == b).count(),
                pairs()
                    .filter(|(a, b)| Self::indent(a) == Self::indent(b))
                    .count(),
            )
        } else {
            (
                Self::shared(&before, &after, |line| line.to_string()),
                Self::shared(&before, &after, |line| {
                    format!("{}\u{0}{}", Self::indent(line), line.trim_start())
                }),
            )
        };
        Self {
            problem: None,
            name,
            lines: before.len(),
            identical,
            indent_kept,
        }
    }

    /// The lines holding anything but whitespace.
    fn non_blank_lines(text: &str) -> Vec<&str> {
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .collect()
    }

    /// How many lines of `before` have a counterpart in `after` with the
    /// same `key`, each counterpart used once.
    fn shared(before: &[&str], after: &[&str], key: impl Fn(&str) -> String) -> usize {
        let mut available = HashMap::<String, usize>::new();
        for line in after {
            *available.entry(key(line)).or_default() += 1;
        }
        before
            .iter()
            .filter(|line| {
                available.get_mut(&key(line)).is_some_and(|count| {
                    *count > 0 && {
                        *count -= 1;
                        true
                    }
                })
            })
            .count()
    }

    /// The visual column a line's text starts at.
    fn indent(line: &str) -> usize {
        line.chars()
            .take_while(|character| matches!(character, ' ' | '\t'))
            .fold(0, |column, character| match character {
                '\t' => column + Corpus::TAB_WIDTH - column % Corpus::TAB_WIDTH,
                _ => column + 1,
            })
    }

    /// `count` as a percentage of `total`.
    fn share(count: usize, total: usize) -> f64 {
        100.0 * count as f64 / total.max(1) as f64
    }

    /// Percentage of lines reproduced byte for byte.
    fn identical_share(&self) -> f64 {
        Self::share(self.identical, self.lines)
    }

    /// Percentage of lines whose indentation column is kept.
    fn indent_share(&self) -> f64 {
        Self::share(self.indent_kept, self.lines)
    }
}

#[test]
#[ignore = "needs a Macaulay2 checkout in M2_CORE_DIR"]
fn core_library_fidelity() {
    let corpus = Corpus::from_env();
    let workers = thread::available_parallelism().map_or(1, usize::from);
    let mut results = thread::scope(|scope| {
        corpus
            .files
            .chunks(corpus.files.len().div_ceil(workers))
            .map(|chunk| {
                scope.spawn(|| {
                    chunk
                        .iter()
                        .map(|path| corpus.measure(path))
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    results.sort_by(|a, b| a.indent_share().total_cmp(&b.indent_share()));
    println!(
        "{:<28} {:>6} {:>10} {:>12}",
        "file", "lines", "identical", "indent kept"
    );
    for file in &results {
        println!(
            "{:<28} {:>6} {:>9.1}% {:>11.1}%",
            file.name,
            file.lines,
            file.identical_share(),
            file.indent_share()
        );
    }
    let average = |share: fn(&FileFidelity) -> f64| {
        results.iter().map(share).sum::<f64>() / results.len() as f64
    };
    println!(
        "{:<28} {:>6} {:>9.1}% {:>11.1}%",
        format!("average of {} files", results.len()),
        results.iter().map(|file| file.lines).sum::<usize>(),
        average(FileFidelity::identical_share),
        average(FileFidelity::indent_share)
    );
    let problems = results
        .iter()
        .filter_map(|file| Some(format!("{}: {}", file.name, file.problem.as_ref()?)))
        .collect::<Vec<_>>();
    assert!(
        problems.is_empty(),
        "{} files:\n{}",
        problems.len(),
        problems.join("\n")
    );
}
