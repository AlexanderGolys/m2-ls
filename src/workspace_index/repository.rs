//! Repository source discovery, separate from import and package visibility.

use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::Url;
use walkdir::WalkDir;

use crate::macaulay2::Macaulay2Conventions;

/// Build output is not repository source, even when it contains copied fixtures.
const BUILD_DIRECTORY: &str = "target";

/// The source files available to a repository's import graph.
pub struct RepositorySources;

impl RepositorySources {
    /// Uses the enclosing workspace root supplied by the editor. Root markers
    /// are client configuration; the server does not search parent Git directories.
    pub fn root(file: &Url, workspace_folders: &[PathBuf]) -> Option<PathBuf> {
        let path = file.to_file_path().ok()?;
        workspace_folders
            .iter()
            .filter(|folder| path.starts_with(folder))
            .max_by_key(|folder| folder.components().count())
            .cloned()
    }

    /// Every ordinary `.m2` file below the root, excluding hidden directories
    /// and build output. Symlinked directories are not followed, preventing cycles.
    pub fn files(root: &Path) -> Vec<Url> {
        WalkDir::new(root)
            .into_iter()
            .filter_entry(|entry| {
                entry.depth() == 0
                    || entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| !name.starts_with('.') && name != BUILD_DIRECTORY)
            })
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_file()
                    && entry.path().extension().is_some_and(|extension| {
                        extension == Macaulay2Conventions::SOURCE_EXTENSION
                    })
            })
            .filter_map(|entry| Url::from_file_path(entry.path()).ok())
            .collect()
    }
}
