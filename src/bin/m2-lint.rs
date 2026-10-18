//! Standalone Macaulay2 linter.

fn main() -> std::process::ExitCode {
    m2_ls::cli::Tool::Linter.run()
}
