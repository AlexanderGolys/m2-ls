//! Stdio language-server executable.

use std::process::ExitCode;

use m2_ls::catalog::CatalogSource;

fn main() -> ExitCode {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    match arguments.as_slice() {
        [] => {
            m2_ls::run_server();
            ExitCode::SUCCESS
        }
        [argument] if argument == "--download-catalog" => {
            match CatalogSource::current().and_then(|source| source.update()) {
                Ok(path) => {
                    println!("{}", path.display());
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("m2-ls: {error}");
                    ExitCode::from(2)
                }
            }
        }
        [argument] if argument == "--help" || argument == "-h" => {
            println!(
                "Usage: m2-ls [OPTIONS]\n\nWithout options, starts the language server on stdio.\n\n    --download-catalog  Download M2Book's latest builtin catalog unless cached, and print its path\n    -h, --help          Show help\n    -V, --version       Show version"
            );
            ExitCode::SUCCESS
        }
        [argument] if argument == "--version" || argument == "-V" => {
            println!("m2-ls {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("m2-ls: expected no arguments or one option; see --help");
            ExitCode::from(2)
        }
    }
}
