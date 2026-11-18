//! Repository source discovery, separate from import and package visibility.

use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::Url;
use walkdir::WalkDir;

use crate::macaulay2::Macaulay2Conventions;
use crate::settings::WorkspaceSettings;

/// Build output is not repository source, even when it contains copied fixtures.
const BUILD_DIRECTORY: &str = "target";

/// The `.m2` files one discovery found, nearest first, and whether it stopped
/// before seeing every file of its root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveredSources {
    /// The files, the ones nearest to where discovery started first.
    pub files: Vec<Url>,
    /// Whether one of the [`WorkspaceSettings`] limits ended the walk.
    pub truncated: bool,
}

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

    /// Ordinary `.m2` files below `root` within the `limits`, nearest to
    /// `start` first: those under `start`, then those under each parent directory up
    /// to `root` that were not seen yet. Opening `Core/m2/ring.m2` in a
    /// checkout thus takes Core's sources before the packages beside it.
    /// Hidden directories and build output are skipped, and symlinked
    /// directories are not followed, preventing cycles.
    pub fn nearest_files(
        start: &Path,
        root: &Path,
        limits: WorkspaceSettings,
    ) -> DiscoveredSources {
        let mut discovered = DiscoveredSources::default();
        let mut entries = 0;
        let mut seen: Option<&Path> = None;
        for directory in start
            .ancestors()
            .take_while(|directory| directory.starts_with(root))
        {
            let walk = WalkDir::new(directory)
                .sort_by_file_name()
                .into_iter()
                .filter_entry(|entry| {
                    entry.depth() == 0
                        || Some(entry.path()) != seen
                            && entry.file_name().to_str().is_some_and(|name| {
                                !name.starts_with('.') && name != BUILD_DIRECTORY
                            })
                })
                .filter_map(Result::ok);
            for entry in walk {
                entries += 1;
                if discovered.files.len() == limits.index_file_limit
                    || entries > limits.scan_entry_limit
                {
                    discovered.truncated = true;
                    return discovered;
                }
                let is_source = entry.file_type().is_file()
                    && entry.path().extension().is_some_and(|extension| {
                        extension == Macaulay2Conventions::SOURCE_EXTENSION
                    });
                if is_source {
                    discovered
                        .files
                        .extend(Url::from_file_path(entry.path()).ok());
                }
            }
            seen = Some(directory);
        }
        discovered
    }
}
