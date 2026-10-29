//! Workspace file operations: the creations, deletions and renames of source
//! files and directories the client reports, and the edits a rename needs to
//! keep every `load`/`needs`/`input` running the file it ran before.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::{
    FileOperationFilter, FileOperationPattern, FileOperationPatternKind,
    FileOperationRegistrationOptions, FileRename, TextEdit, Url,
    WorkspaceFileOperationsServerCapabilities,
};
use walkdir::{DirEntry, WalkDir};

use crate::file_load::{FileLoad, FileLookup, LoadResolution};
use crate::macaulay2::Macaulay2Conventions;

/// The URI scheme of the files the server follows on disk.
const FILE_SCHEME: &str = "file";
/// The glob matching every directory, whose renames move the files in it.
const ANY_DIRECTORY_GLOB: &str = "**";

/// The file operations the server takes part in: before a rename, to edit
/// the loads of what moves; after a creation, deletion or rename, to keep the
/// workspace index and the missing-file diagnostics current.
pub fn file_operations_capability() -> WorkspaceFileOperationsServerCapabilities {
    let source_files = FileOperationFilter {
        scheme: Some(FILE_SCHEME.to_string()),
        pattern: FileOperationPattern {
            glob: format!("**/*.{}", Macaulay2Conventions::SOURCE_EXTENSION),
            matches: Some(FileOperationPatternKind::File),
            options: None,
        },
    };
    let directories = FileOperationFilter {
        scheme: Some(FILE_SCHEME.to_string()),
        pattern: FileOperationPattern {
            glob: ANY_DIRECTORY_GLOB.to_string(),
            matches: Some(FileOperationPatternKind::Folder),
            options: None,
        },
    };
    let operations = Some(FileOperationRegistrationOptions {
        filters: vec![source_files, directories],
    });
    WorkspaceFileOperationsServerCapabilities {
        will_rename: operations.clone(),
        did_rename: operations.clone(),
        did_create: operations.clone(),
        did_delete: operations,
        ..WorkspaceFileOperationsServerCapabilities::default()
    }
}

/// Files and directories moved by one rename operation, each from where it
/// was to where it goes. A moved directory moves everything under it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileMoves {
    /// Original and destination paths participating in the rename operation.
    moves: Vec<(PathBuf, PathBuf)>,
}

impl FromIterator<FileRename> for FileMoves {
    /// The moves of the renames between `file:` URIs.
    fn from_iter<I: IntoIterator<Item = FileRename>>(renames: I) -> Self {
        let path = |uri: &str| Url::parse(uri).ok()?.to_file_path().ok();
        Self {
            moves: renames
                .into_iter()
                .filter_map(|rename| Some((path(&rename.old_uri)?, path(&rename.new_uri)?)))
                .collect(),
        }
    }
}

impl FileMoves {
    /// Where the moved files and directories were.
    pub fn origins(&self) -> impl Iterator<Item = &Path> {
        self.moves.iter().map(|(origin, _)| origin.as_path())
    }

    /// Where the moved files and directories go.
    pub fn destinations(&self) -> impl Iterator<Item = &Path> {
        self.moves
            .iter()
            .map(|(_, destination)| destination.as_path())
    }

    /// Where the moves take `file`; `None` when it stays.
    pub fn destination(&self, file: &Url) -> Option<Url> {
        let path = file.to_file_path().ok()?;
        self.moves.iter().find_map(|(origin, destination)| {
            let rest = path.strip_prefix(origin).ok()?;
            let moved = if rest.as_os_str().is_empty() {
                destination.clone()
            } else {
                destination.join(rest)
            };
            Url::from_file_path(moved).ok()
        })
    }

    /// The place a rename keeps `load`, run from `loader`, pointing at:
    /// what is where it looks first, or that place itself when its name is
    /// tied to it (absolute, `~/`, `./` or `../`) though nothing is there
    /// yet. A plain name finding nothing beside its loader is looked up
    /// along `path`, so a rename leaves it alone.
    fn tracked_target(load: &FileLoad, loader: &Url) -> Option<Url> {
        let tracked = match load.resolve(loader, &[]) {
            resolution if resolution.is_on_disk() => true,
            LoadResolution::Missing => load.lookup() != FileLookup::BesideLoaderThenPath,
            _ => false,
        };
        tracked.then(|| load.beside(loader)).flatten()
    }

    /// The edits renaming the files `loaders` load, so that each load runs,
    /// from where its file goes, the file it ran before from where that
    /// goes, rewriting its literal with the delimiters it can be written in.
    pub fn load_edits(
        &self,
        loaders: impl IntoIterator<Item = (Url, Vec<FileLoad>)>,
    ) -> HashMap<Url, Vec<TextEdit>> {
        let mut edits = HashMap::new();
        for (loader, loads) in loaders {
            let moved_loader = self.destination(&loader);
            let loader_edits = loads
                .iter()
                .filter_map(|load| {
                    let target = Self::tracked_target(load, &loader)?;
                    let moved_target = self.destination(&target);
                    if moved_loader.is_none() && moved_target.is_none() {
                        return None;
                    }
                    let name = load.name_for(
                        moved_loader.as_ref().unwrap_or(&loader),
                        moved_target.as_ref().unwrap_or(&target),
                    )?;
                    (name != load.name)
                        .then(|| Some(TextEdit::new(load.literal, load.literal_for(&name)?)))
                        .flatten()
                })
                .collect::<Vec<_>>();
            if !loader_edits.is_empty() {
                edits.insert(loader, loader_edits);
            }
        }
        edits
    }

    /// The source files under `folders` that `is_known` does not already
    /// account for and that may load a moved file, with their loads: every
    /// file that moves, and every file mentioning the name of something that
    /// does. Hidden directories are skipped.
    pub fn loaders_on_disk(
        &self,
        folders: &[PathBuf],
        is_known: impl Fn(&Url) -> bool,
    ) -> Vec<(Url, Vec<FileLoad>)> {
        let moved_names = self
            .origins()
            .filter_map(Path::file_name)
            .filter_map(|name| name.to_str())
            .collect::<Vec<_>>();
        folders
            .iter()
            .flat_map(|folder| {
                WalkDir::new(folder)
                    .into_iter()
                    .filter_entry(|entry| entry.depth() == 0 || !Self::is_hidden(entry))
                    .filter_map(Result::ok)
            })
            .filter(|entry| entry.file_type().is_file() && Self::is_source_file(entry.path()))
            .filter_map(|entry| {
                let file = Url::from_file_path(entry.path()).ok()?;
                if is_known(&file) {
                    return None;
                }
                let text = fs::read_to_string(entry.path()).ok()?;
                let moves = self
                    .origins()
                    .any(|origin| entry.path().starts_with(origin));
                if !moves && !moved_names.iter().any(|name| text.contains(name)) {
                    return None;
                }
                let loads = FileLoad::collect_from_text(&text);
                (!loads.is_empty()).then_some((file, loads))
            })
            .collect()
    }

    /// Whether this node is a hidden.
    fn is_hidden(entry: &DirEntry) -> bool {
        entry.file_name().to_string_lossy().starts_with('.')
    }

    /// Whether this node is a source file.
    fn is_source_file(path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension == Macaulay2Conventions::SOURCE_EXTENSION)
    }
}
