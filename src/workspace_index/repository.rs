//! Repository source discovery, separate from import and package visibility.

use std::fmt;
use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::Url;
use walkdir::WalkDir;

use crate::macaulay2::Macaulay2Conventions;

/// Build output is not repository source, even when it contains copied fixtures.
const BUILD_DIRECTORY: &str = "target";

/// Directory entries a repository scan may visit. A root above this is a home
/// directory or a filesystem, not a project, and is not indexed.
const MAX_SCANNED_ENTRIES: usize = 100_000;

/// A root holding more entries than [`MAX_SCANNED_ENTRIES`] to scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryTooLarge {
    /// The directory whose scan was abandoned.
    pub root: PathBuf,
}

impl fmt::Display for RepositoryTooLarge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} holds more than {MAX_SCANNED_ENTRIES} entries; not indexing it as a repository",
            self.root.display()
        )
    }
}

impl std::error::Error for RepositoryTooLarge {}

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
    /// A root with more than [`MAX_SCANNED_ENTRIES`] entries is refused whole
    /// rather than indexed partially.
    pub fn files(root: &Path) -> Result<Vec<Url>, RepositoryTooLarge> {
        let entries = WalkDir::new(root)
            .into_iter()
            .filter_entry(|entry| {
                entry.depth() == 0
                    || entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| !name.starts_with('.') && name != BUILD_DIRECTORY)
            })
            .filter_map(Result::ok)
            .take(MAX_SCANNED_ENTRIES + 1)
            .collect::<Vec<_>>();
        if entries.len() > MAX_SCANNED_ENTRIES {
            return Err(RepositoryTooLarge {
                root: root.to_path_buf(),
            });
        }
        Ok(entries
            .into_iter()
            .filter(|entry| {
                entry.file_type().is_file()
                    && entry.path().extension().is_some_and(|extension| {
                        extension == Macaulay2Conventions::SOURCE_EXTENSION
                    })
            })
            .filter_map(|entry| Url::from_file_path(entry.path()).ok())
            .collect())
    }
}
