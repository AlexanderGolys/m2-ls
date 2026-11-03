//! Architectural checks: the typed syntax snapshot is the only syntax representation.

use std::fs;
use std::path::{Path, PathBuf};

/// Names of the retired untyped syntax layer that must not return.
const RETIRED_SYNTAX_NAMES: [&str; 6] = [
    "SyntaxNode",
    "SyntaxTree",
    "SyntaxParser",
    "LayoutNode",
    "CellIndex",
    "self_cell",
];

/// Every Rust source file below `directory`.
fn rust_files(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory).expect("source directory is readable") {
        let path = entry.expect("directory entry is readable").path();
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    files
}

#[test]
fn retired_untyped_syntax_layer_stays_removed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = rust_files(&root.join("src"));
    sources.push(root.join("Cargo.toml"));
    for path in sources {
        if path.ends_with("architecture.rs") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("source file is readable");
        for name in RETIRED_SYNTAX_NAMES {
            assert!(
                !text
                    .split(|character: char| !character.is_alphanumeric() && character != '_')
                    .any(|word| word == name),
                "{} mentions retired `{name}`",
                path.display()
            );
        }
    }
}

#[test]
fn capabilities_consume_the_shared_snapshot_instead_of_parsing() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/capabilities");
    for path in rust_files(&root) {
        let text = fs::read_to_string(&path).expect("capability source is readable");
        let production = text.split("#[cfg(test)]").next().unwrap_or(&text);
        let parses = production
            .lines()
            .filter(|line| line.contains("SyntaxSnapshot::parse("))
            .count();
        let allowed = usize::from(path.ends_with("formatting.rs"));
        assert!(
            parses <= allowed,
            "{} re-parses source outside the shared snapshot",
            path.display()
        );
    }
}
