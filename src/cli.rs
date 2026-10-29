//! Standalone commands sharing the server's formatter and diagnostics.
//!
//! [`Tool::run`] reads the process arguments and standard input, emits the
//! selected command's output, and returns a process exit status.
//!
//! ```no_run
//! let status = m2_ls::cli::Tool::Formatter.run();
//! assert_eq!(status, std::process::ExitCode::SUCCESS);
//! ```

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString, Url};

use crate::capabilities::formatting::{format_document_with_options, FormatOptions};
use crate::catalog::CatalogSource;
use crate::diagnostics::visible_diagnostics;
use crate::document::{DocumentSnapshot, FileProperties};
use crate::init_file::InitFiles;
use crate::node_metadata::SyntaxParser;
use crate::object_registry::ObjectRegistry;
use crate::package_index::{Macaulay2Environment, SourceResolver};
use crate::settings::{ServerSettings, SettingProblem};

/// The standalone tool to execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// Format a file in place, or print formatted stdin.
    Formatter,
    /// Report static diagnostics without executing Macaulay2.
    Linter,
}

/// The one source a tool reads.
enum Input {
    /// Standard input, named `-` on the command line.
    Stdin,
    /// The file at this path.
    File(PathBuf),
}

impl Input {
    /// How diagnostics and messages name the input: the path as given.
    fn label(&self) -> String {
        match self {
            Self::Stdin => "-".into(),
            Self::File(path) => path.display().to_string(),
        }
    }

    /// The whole text of the input.
    fn read(&self) -> Result<String, String> {
        match self {
            Self::Stdin => {
                let mut text = String::new();
                io::stdin()
                    .read_to_string(&mut text)
                    .map_err(|error| error.to_string())?;
                Ok(text)
            }
            Self::File(path) => fs::read_to_string(path).map_err(|error| error.to_string()),
        }
    }

    /// The document URI diagnostics are reported against.
    fn uri(&self) -> Result<Url, String> {
        let path = match self {
            Self::Stdin => std::env::current_dir()
                .map_err(|error| error.to_string())?
                .join("stdin.m2"),
            Self::File(path) => fs::canonicalize(path).map_err(|error| error.to_string())?,
        };
        Url::from_file_path(path).map_err(|()| "invalid file path".into())
    }
}

/// What one command line asks a tool to do.
struct Options {
    /// The source to format or lint.
    input: Input,
    /// `--check`: report whether the file needs formatting instead of writing
    /// it.
    check: bool,
    /// `--stdout`: print the formatted file instead of writing it.
    stdout: bool,
    /// `--deny-warnings`: fail on warnings as well as errors.
    deny_warnings: bool,
    /// `--config`: the JSON settings file, read as the server reads its
    /// settings.
    config: Option<PathBuf>,
}

impl Tool {
    /// The executable's name, as usage and messages show it.
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

    /// The usage text `--help` prints.
    fn help(self) -> String {
        let specific = match self {
            Self::Formatter => "    --check          Exit 1 if the file needs formatting; do not write\n    --stdout         Print the formatted file instead of writing it\n",
            Self::Linter => "    --deny-warnings  Exit 1 for warnings as well as errors\n",
        };
        format!(
            "Usage: {} [OPTIONS] <FILE | ->\n\nReads one .m2 file, or stdin for -.\n\n{specific}    --config FILE    Read JSON settings (same format as m2-ls)\n    -h, --help       Show help\n    -V, --version    Show version\n    --               Treat the next argument as the file\n\nExit codes: 0 success, 1 findings/check failure, 2 usage or I/O error.\n",
            self.name()
        )
    }

    /// The options of the process's command line; `None` when it asked only for
    /// help or the version, which are then printed.
    fn parse(self) -> Result<Option<Options>, String> {
        let mut paths = Vec::new();
        let mut check = false;
        let mut stdout = false;
        let mut deny_warnings = false;
        let mut config = None;
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
                Some("--check") if self == Self::Formatter => check = true,
                Some("--stdout") if self == Self::Formatter => stdout = true,
                Some("--deny-warnings") if self == Self::Linter => deny_warnings = true,
                Some("--config") => {
                    config = Some(PathBuf::from(args.next().ok_or("--config needs a file")?));
                }
                Some("--") => {
                    paths.extend(args.map(PathBuf::from));
                    break;
                }
                Some(flag) if flag.starts_with('-') && flag != "-" => {
                    return Err(format!("unknown option {flag}; see --help"));
                }
                _ => paths.push(PathBuf::from(arg)),
            }
        }
        if check && stdout {
            return Err("--check and --stdout cannot be combined".into());
        }
        let [path] = <[PathBuf; 1]>::try_from(paths)
            .map_err(|_| "expected exactly one file, or - for stdin; see --help")?;
        let input = if path.as_os_str() == "-" {
            Input::Stdin
        } else {
            Input::File(path)
        };
        Ok(Some(Options {
            input,
            check,
            stdout,
            deny_warnings,
            config,
        }))
    }

    /// Read the input and run the tool on it, returning the exit status.
    fn execute(self) -> Result<u8, String> {
        let Some(options) = self.parse()? else {
            return Ok(0);
        };
        let settings = options.settings()?;
        let label = options.input.label();
        let text = options
            .input
            .read()
            .map_err(|error| format!("{label}: {error}"))?;
        match self {
            Self::Formatter => Self::format(&options, &settings, text),
            Self::Linter => Self::lint(&options, &settings, text),
        }
        .map_err(|error| format!("{label}: {error}"))
    }

    /// Format `text`, writing, printing or only checking the result as
    /// `options` say. Returns the exit status.
    fn format(options: &Options, settings: &ServerSettings, text: String) -> Result<u8, String> {
        let mut parser = SyntaxParser::new().ok_or("could not initialize parser")?;
        let tree = parser
            .parse_tree(&text, None)
            .ok_or("could not parse source")?;
        let root = tree.root(&text);
        if root.has_error() {
            return Err("cannot format source with syntax errors".into());
        }
        let formatted =
            format_document_with_options(root, &FormatOptions::from(&settings.formatting));
        match &options.input {
            _ if options.check => {
                if formatted != text {
                    println!("{}: needs formatting", options.input.label());
                    return Ok(1);
                }
            }
            Input::File(path) if !options.stdout => {
                if formatted != text {
                    fs::write(path, formatted).map_err(|error| error.to_string())?;
                }
            }
            Input::Stdin | Input::File(_) => io::stdout()
                .write_all(formatted.as_bytes())
                .map_err(|error| error.to_string())?,
        }
        Ok(0)
    }

    /// Print the diagnostics the server would publish for `text`, analyzed with
    /// the init file `settings` and its directive select. Returns the exit
    /// status: `1` for an error, or a warning under `--deny-warnings`.
    fn lint(options: &Options, settings: &ServerSettings, text: String) -> Result<u8, String> {
        let corpus = CatalogSource::current()
            .and_then(|source| source.load())
            .map_err(|error| error.to_string())?;
        let environment = Macaulay2Environment::current();
        let registry = ObjectRegistry::load(&corpus);
        let uri = options.input.uri()?;
        let label = options.input.label();
        let init_files = InitFiles::default();
        let current_directory = env::current_dir().ok();
        match InitFiles::locate(
            &settings.sources.init_file,
            settings.sources.enabled,
            current_directory.as_deref(),
            &environment,
        ) {
            Ok(location) => init_files.configure(location, &registry),
            Err(problem) => init_files.configure_unavailable(problem),
        };
        if let Some(problem) = init_files.configured().problem() {
            eprintln!("{label}: warning: analyzed without an init file: {problem}");
        }
        let sources = SourceResolver::configured(&environment, &settings.sources);
        let implied_properties = FileProperties::implied_by_location(&uri, &sources);
        let session_registry = |document: Option<&DocumentSnapshot>| {
            let properties = document.map_or(implied_properties, DocumentSnapshot::properties);
            if properties.primary_code {
                return registry.clone();
            }
            let directive = document.and_then(DocumentSnapshot::init_file_directive);
            registry.with_init_file(init_files.start_for(&uri, directive, &environment, &registry))
        };
        let mut document = DocumentSnapshot::from_text_with_implied_properties(
            text,
            &session_registry(None),
            implied_properties,
        )
        .ok_or("could not parse source")?;
        let directed = session_registry(Some(&document));
        document.restart_session(&directed);
        let mut failed = false;
        for diagnostic in visible_diagnostics(&document, &uri, &settings.diagnostics, &sources) {
            let severity = diagnostic.severity.unwrap_or(DiagnosticSeverity::WARNING);
            failed |= severity == DiagnosticSeverity::ERROR
                || (options.deny_warnings && severity == DiagnosticSeverity::WARNING);
            let severity_label = match severity {
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
                "{label}:{}:{}: {severity_label}[{code}]: {}",
                diagnostic.range.start.line + 1,
                diagnostic.range.start.character + 1,
                diagnostic.message
            );
            for note in diagnostic.related_information.unwrap_or_default() {
                println!(
                    "{label}:{}:{}: note: {}",
                    note.location.range.start.line + 1,
                    note.location.range.start.character + 1,
                    note.message
                );
            }
        }
        Ok(u8::from(failed))
    }
}

impl Options {
    /// The settings `--config` names, or the defaults; an invalid value fails
    /// the command, a deprecated key only warns.
    fn settings(&self) -> Result<ServerSettings, String> {
        let Some(path) = &self.config else {
            return Ok(ServerSettings::default());
        };
        let text =
            fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let value =
            serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
        let parsed = ServerSettings::from_value(&value);
        let listed = parsed
            .problems
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        if !parsed.problems.iter().all(SettingProblem::is_deprecated) {
            return Err(listed);
        }
        if !listed.is_empty() {
            eprintln!("{}: warning: {listed}", path.display());
        }
        Ok(parsed.settings)
    }
}
