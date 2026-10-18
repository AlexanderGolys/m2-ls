//! Standalone Macaulay2 formatter.

fn main() -> std::process::ExitCode {
    m2_ls::cli::Tool::Formatter.run()
}
