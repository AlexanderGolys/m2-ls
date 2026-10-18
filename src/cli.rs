//! Standalone commands sharing the server's formatter and diagnostics.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString, Url};

use crate::capabilities::formatting::{format_document_with_options, FormatOptions};
use crate::diagnostics::visible_diagnostics;
use crate::document::DocumentSnapshot;
use crate::node_metadata::M2Parser;
use crate::object_registry::ObjectRegistry;
use crate::package_index::{InitializationFile, Macaulay2Environment};
use crate::settings::ServerSettings;

/// The standalone tool to execute.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// Format files in place, or print formatted stdin.
    Formatter,
    /// Report static diagnostics without executing Macaulay2.
    Linter,
}

#[derive(Default)]
struct Options {
    paths: Vec<PathBuf>,
    check: bool,
    stdout: bool,
    deny_warnings: bool,
    config: Option<PathBuf>,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Self::Formatter => "m2-fmt",
            Self::Linter => "m2-lint",
        }
    }

    /// Run on a worker with the same stack reservation as the server, keeping
    /// deeply nested generated expressions off the process's small main stack.
    pub fn run(self) -> ExitCode {
        let result = std::thread::Builder::new()
            .name(self.name().into())
            .stack_size(crate::RUNTIME_THREAD_STACK_BYTES)
            .spawn(move || self.execute());
        match result {
            Ok(worker) => match worker.join() {
                Ok(Ok(code)) => ExitCode::from(code),
                Ok(Err(error)) => {
                    eprintln!("{}: {error}", self.name());
                    ExitCode::from(2)
                }
                Err(panic) => std::panic::resume_unwind(panic),
            },
            Err(error) => {
                eprintln!("{}: {error}", self.name());
                ExitCode::from(2)
            }
        }
    }

    fn help(self) -> String {
        let specific = match self {
            Self::Formatter => "    --check          Report files needing formatting; do not write\n    --stdout         Print one formatted file instead of writing it\n",
            Self::Linter => "    --deny-warnings  Exit 1 for warnings as well as errors\n",
        };
        format!(
            "Usage: {} [OPTIONS] [FILE | DIRECTORY | -]...\n\nDefaults to .; directories recursively select .m2 files, skipping hidden\nentries, target, and directory symlinks. Use - alone to read stdin.\n\n{specific}    --config FILE    Read JSON settings (same format as m2-ls)\n    -h, --help       Show help\n    -V, --version    Show version\n    --               Treat remaining arguments as paths\n\nExit codes: 0 success, 1 findings/check failure, 2 usage or I/O error.\n",
            self.name()
        )
    }

    fn parse(self) -> Result<Option<Options>, String> {
        let mut options = Options::default();
        let mut args = std::env::args_os().skip(1);
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("-h" | "--help") => {
                    print!("{}", self.help());
                    return Ok(None);
                }
                Some("-V" | "--version") => {
                    println!("{} {}", self.name(), env!("CARGO_PKG_VERSION"));
                    return Ok(None);
                }
                Some("--check") if self == Self::Formatter => options.check = true,
                Some("--stdout") if self == Self::Formatter => options.stdout = true,
                Some("--deny-warnings") if self == Self::Linter => options.deny_warnings = true,
                Some("--config") => {
                    options.config =
                        Some(PathBuf::from(args.next().ok_or("--config needs a file")?));
                }
                Some("--") => {
                    options.paths.extend(args.map(PathBuf::from));
                    break;
                }
                Some(flag) if flag.starts_with('-') && flag != "-" => {
                    return Err(format!("unknown option {flag}; see --help"));
                }
                _ => options.paths.push(PathBuf::from(arg)),
            }
        }
        if options.check && options.stdout {
            return Err("--check and --stdout cannot be combined".into());
        }
        if options.paths.is_empty() {
            options.paths.push(PathBuf::from("."));
        }
        if options.paths.iter().any(|path| path == Path::new("-")) && options.paths.len() != 1 {
            return Err("stdin (-) must be the only input".into());
        }
        Ok(Some(options))
    }

    fn execute(self) -> Result<u8, String> {
        let Some(options) = self.parse()? else {
            return Ok(0);
        };
        let settings = options.settings()?;
        let stdin = options.paths == [PathBuf::from("-")];
        let files = if stdin {
            BTreeSet::new()
        } else {
            options.files()?
        };
        if options.stdout && !stdin && files.len() != 1 {
            return Err("--stdout requires exactly one file".into());
        }
        let mut registry = None;
        if self == Self::Linter {
            let mut knowledge = ObjectRegistry::load(include_str!("data/m2-index.jsonl"));
            InitializationFile::load(&Macaulay2Environment::current(), &mut knowledge);
            registry = Some(knowledge);
        }
        let mut status = 0;
        let inputs: Vec<_> = if stdin {
            vec![PathBuf::from("-")]
        } else {
            files.into_iter().collect()
        };
        for path in inputs {
            let result: Result<u8, String> = (|| {
                let text = if stdin {
                    let mut text = String::new();
                    io::stdin()
                        .read_to_string(&mut text)
                        .map_err(|error| error.to_string())?;
                    text
                } else {
                    fs::read_to_string(&path).map_err(|error| error.to_string())?
                };
                match self {
                    Self::Formatter => {
                        let mut parser = M2Parser::new().ok_or("could not initialize parser")?;
                        let tree = parser
                            .parse_tree(&text, None)
                            .ok_or("could not parse source")?;
                        let root = tree.root(&text);
                        if root.has_error() {
                            return Err("cannot format source with syntax errors".into());
                        }
                        let formatted = format_document_with_options(
                            root,
                            &FormatOptions::from(&settings.formatting),
                        );
                        if options.check {
                            if formatted != text {
                                println!("{}: needs formatting", path.display());
                                return Ok(1);
                            }
                        } else if stdin || options.stdout {
                            io::stdout()
                                .write_all(formatted.as_bytes())
                                .map_err(|error| error.to_string())?;
                        } else if formatted != text {
                            fs::write(&path, formatted).map_err(|error| error.to_string())?;
                        }
                        Ok(0)
                    }
                    Self::Linter => {
                        let document = DocumentSnapshot::from_text(
                            text,
                            registry.as_ref().expect("linter registry"),
                        )
                        .ok_or("could not parse source")?;
                        let uri_path = if stdin {
                            std::env::current_dir()
                                .map_err(|error| error.to_string())?
                                .join("stdin.m2")
                        } else {
                            path.clone()
                        };
                        let uri =
                            Url::from_file_path(uri_path).map_err(|()| "invalid file path")?;
                        let mut failed = false;
                        for diagnostic in
                            visible_diagnostics(&document, &uri, &settings.diagnostics)
                        {
                            let severity =
                                diagnostic.severity.unwrap_or(DiagnosticSeverity::WARNING);
                            failed |= severity == DiagnosticSeverity::ERROR
                                || (options.deny_warnings
                                    && severity == DiagnosticSeverity::WARNING);
                            let label = match severity {
                                DiagnosticSeverity::ERROR => "error",
                                DiagnosticSeverity::WARNING => "warning",
                                DiagnosticSeverity::INFORMATION => "info",
                                _ => "hint",
                            };
                            let code = match diagnostic.code {
                                Some(NumberOrString::String(code)) => code,
                                _ => String::new(),
                            };
                            println!(
                                "{}:{}:{}: {label}[{code}]: {}",
                                path.display(),
                                diagnostic.range.start.line + 1,
                                diagnostic.range.start.character + 1,
                                diagnostic.message
                            );
                            for note in diagnostic.related_information.unwrap_or_default() {
                                println!(
                                    "{}:{}:{}: note: {}",
                                    path.display(),
                                    note.location.range.start.line + 1,
                                    note.location.range.start.character + 1,
                                    note.message
                                );
                            }
                        }
                        Ok(u8::from(failed))
                    }
                }
            })();
            match result {
                Ok(code) => status = status.max(code),
                Err(error) => {
                    eprintln!("{}: {}: {error}", self.name(), path.display());
                    status = 2;
                }
            }
        }
        Ok(status)
    }
}

impl Options {
    fn settings(&self) -> Result<ServerSettings, String> {
        let Some(path) = &self.config else {
            return Ok(ServerSettings::default());
        };
        let text =
            fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let value =
            serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
        let parsed = ServerSettings::from_value(&value);
        if !parsed.problems.is_empty() {
            return Err(parsed
                .problems
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "));
        }
        Ok(parsed.settings)
    }

    fn files(&self) -> Result<BTreeSet<PathBuf>, String> {
        let mut files = BTreeSet::new();
        for path in &self.paths {
            Self::collect(path, &mut files)?;
        }
        if files.is_empty() {
            return Err("no .m2 files found".into());
        }
        Ok(files)
    }

    fn collect(path: &Path, files: &mut BTreeSet<PathBuf>) -> Result<(), String> {
        let metadata =
            fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
        if metadata.is_dir() {
            for entry in
                fs::read_dir(path).map_err(|error| format!("{}: {error}", path.display()))?
            {
                let entry = entry.map_err(|error| error.to_string())?;
                let name = entry.file_name();
                let kind = entry.file_type().map_err(|error| error.to_string())?;
                if name.to_string_lossy().starts_with('.') || name == "target" || kind.is_symlink()
                {
                    continue;
                }
                if kind.is_dir() || entry.path().extension().is_some_and(|ext| ext == "m2") {
                    Self::collect(&entry.path(), files)?;
                }
            }
        } else if metadata.is_file() {
            files.insert(
                fs::canonicalize(path).map_err(|error| format!("{}: {error}", path.display()))?,
            );
        } else {
            return Err(format!("{}: expected a file or directory", path.display()));
        }
        Ok(())
    }
}
