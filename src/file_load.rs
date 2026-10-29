//! The source files a document runs with `load`, `needs` and `input`, and
//! where Macaulay2 looks for them.
//!
//! Macaulay2 looks for the named file in the directory of the file being
//! loaded, then along `path`, unless the name begins with `/` (used as is),
//! `~/` (in the home directory), `./` or `../` (only the directory of the
//! loading file), or `$`/`!` (a socket or a pipe, not a file). Paths are
//! resolved by the operating system, so a symbolic link followed by `..`
//! leads where the link points, not back where it is.

use std::env;
use std::fs::{self, File};
use std::io::{self, ErrorKind};
use std::iter;
use std::path::{Component, Path, PathBuf};

use tower_lsp::lsp_types::{Position, Range as TextRange, Url};

use crate::macaulay2::Macaulay2Conventions;
use crate::node_metadata::{visit_source_nodes, SyntaxNode, SyntaxParser, TypedSource};
use crate::package_index::first_call_argument;
use crate::source::{DocumentSource, SourceNavigation};
use crate::string_literal::StringDelimiter;

/// How Macaulay2 looks for the file a [`FileLoad`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileLookup {
    /// `/dir/f.m2`: the name itself.
    Absolute,
    /// `~/f.m2`: in the home directory.
    HomeRelative,
    /// `./f.m2`, `../f.m2`: only in the directory of the loading file.
    BesideLoader,
    /// `f.m2`, `dir/f.m2`: in the directory of the loading file, then along
    /// `path`.
    BesideLoaderThenPath,
    /// `$host:port`, `!command`: a socket or a pipe, not a file.
    NotAFile,
}

/// What a load finds for its name, run from one loading file.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LoadResolution {
    /// A file `load` can open.
    File(Url),
    /// A directory where the file should be, which `load` cannot run.
    Directory(Url),
    /// Something at `location` that `load` cannot open for `reason`, such as
    /// a file it may not read or a directory on the way it may not search.
    Inaccessible {
        #[doc = "Resolved source location used for navigation."]
        location: Url,
        #[doc = "Filesystem error category explaining why the resolved load target is inaccessible."]
        reason: ErrorKind,
    },
    /// Nothing at any place `load` looks.
    Missing,
    /// An empty name, which names no file.
    EmptyName,
    /// Nothing the server can tell: a socket or a pipe, a `~/` name without
    /// a home directory, or a loading document with no place on disk.
    Undetermined,
}

impl LoadResolution {
    /// The file the load runs, when it can.
    pub fn file(&self) -> Option<&Url> {
        match self {
            Self::File(file) => Some(file),
            _ => None,
        }
    }

    /// Whether something is where the load looked first or along `path`,
    /// runnable or not.
    pub fn is_on_disk(&self) -> bool {
        matches!(
            self,
            Self::File(_) | Self::Directory(_) | Self::Inaccessible { .. }
        )
    }
}

/// One `load`/`needs`/`input` call running a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLoad {
    /// Where the call starts.
    pub call: Position,
    /// The file name, its escapes read.
    pub name: String,
    /// How the literal naming the file is delimited.
    pub delimiter: StringDelimiter,
    /// The string literal naming the file, delimiters included.
    pub literal: TextRange,
}

impl FileLoad {
    /// Every load under `root` naming its file with a literal, in source
    /// order. A literal with an escape Macaulay2 does not read names no
    /// file and is left out.
    pub fn collect(
        root: SyntaxNode<'_>,
        syntax: Option<&TypedSource>,
        source: &(impl SourceNavigation + ?Sized),
    ) -> Vec<Self> {
        let mut loads = Vec::new();
        visit_source_nodes(root, syntax, |call| {
            let Some(literal) = first_call_argument(call, Self::is_load_function) else {
                return;
            };
            let (Some(name), Some(delimiter)) =
                (literal.string_literal_value(), StringDelimiter::of(literal))
            else {
                return;
            };
            loads.push(Self {
                call: source.position_for_node(call),
                name,
                delimiter,
                literal: source.range_for_node(literal),
            });
        });
        loads
    }

    /// The loads of `text`, parsed without analysis: for files read only for
    /// the files they load.
    pub fn collect_from_text(text: &str) -> Vec<Self> {
        let Some(mut parser) = SyntaxParser::new() else {
            return Vec::new();
        };
        let Some(tree) = parser.parse_tree(text, None) else {
            return Vec::new();
        };
        let source = DocumentSource::new(text.to_string());
        Self::collect(tree.root(text), Some(tree.syntax()), &source)
    }

    /// Whether `node` is the string naming the file of a load.
    pub fn names_file(node: SyntaxNode<'_>) -> bool {
        node.ancestors().any(|call| {
            first_call_argument(call, Self::is_load_function)
                .is_some_and(|literal| literal.id() == node.id())
        })
    }

    /// Whether the symbol names load, needs, or input.
    fn is_load_function(name: &str) -> bool {
        Macaulay2Conventions::FILE_LOAD_FUNCTIONS.contains(&name)
    }

    /// Whether the literal naming the file contains `position`.
    pub fn names_file_at(&self, position: Position) -> bool {
        self.literal.start <= position && position <= self.literal.end
    }

    /// How Macaulay2 looks for the file this load names.
    pub fn lookup(&self) -> FileLookup {
        Self::lookup_of(&self.name)
    }

    /// How Macaulay2 looks for the file `name` names.
    pub fn lookup_of(name: &str) -> FileLookup {
        if name.starts_with(Macaulay2Conventions::NON_FILE_NAME_PREFIXES) {
            FileLookup::NotAFile
        } else if name.starts_with(Macaulay2Conventions::HOME_DIRECTORY_PREFIX) {
            FileLookup::HomeRelative
        } else if Path::new(name).is_absolute() {
            FileLookup::Absolute
        } else if matches!(
            Path::new(name).components().next(),
            Some(Component::CurDir | Component::ParentDir)
        ) {
            FileLookup::BesideLoader
        } else {
            FileLookup::BesideLoaderThenPath
        }
    }

    /// Where `load` looks first for `name` run from `loader`: beside it, in
    /// the home directory, or at the name itself. `None` for a socket or a
    /// pipe, or when that place cannot be known.
    pub fn first_location_of(name: &str, loader: &Url) -> Option<PathBuf> {
        match Self::lookup_of(name) {
            FileLookup::NotAFile => None,
            FileLookup::Absolute => Some(PathBuf::from(name)),
            FileLookup::HomeRelative => Some(
                env::home_dir()?
                    .join(name.strip_prefix(Macaulay2Conventions::HOME_DIRECTORY_PREFIX)?),
            ),
            FileLookup::BesideLoader | FileLookup::BesideLoaderThenPath => {
                Some(loader.to_file_path().ok()?.parent()?.join(name))
            }
        }
    }

    /// Where `load` looks first, run from `loader`, whether or not anything
    /// is there. `None` for a socket or a pipe, or when that place cannot be
    /// known.
    pub fn beside(&self, loader: &Url) -> Option<Url> {
        Self::file_url(&Self::first_location_of(&self.name, loader)?)
    }

    /// What the load finds run from `loader`: the first place it looks, then,
    /// for a name Macaulay2 also looks up along `path`, each directory of
    /// `search_path` in turn.
    pub fn resolve(&self, loader: &Url, search_path: &[PathBuf]) -> LoadResolution {
        if self.name.is_empty() {
            return LoadResolution::EmptyName;
        }
        let Some(first) = Self::first_location_of(&self.name, loader) else {
            return LoadResolution::Undetermined;
        };
        let search_path = if self.lookup() == FileLookup::BesideLoaderThenPath {
            search_path
        } else {
            &[]
        };
        iter::once(first)
            .chain(
                search_path
                    .iter()
                    .map(|directory| directory.join(&self.name)),
            )
            .find_map(|path| Self::probe(&path))
            .unwrap_or(LoadResolution::Missing)
    }

    /// What is at `path`; `None` when nothing is, so `load` looks on.
    fn probe(path: &Path) -> Option<LoadResolution> {
        let location = Self::file_url(path)?;
        Some(match fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => LoadResolution::Directory(location),
            Ok(_) => match File::open(path) {
                Ok(_) => LoadResolution::File(location),
                Err(error) => LoadResolution::Inaccessible {
                    location,
                    reason: io::Error::kind(&error),
                },
            },
            Err(error)
                if matches!(
                    io::Error::kind(&error),
                    ErrorKind::NotFound | ErrorKind::NotADirectory
                ) =>
            {
                return None;
            }
            Err(error) => LoadResolution::Inaccessible {
                location,
                reason: io::Error::kind(&error),
            },
        })
    }

    /// The name that runs `target` from `loader`, written the way this load
    /// writes its name: absolute stays absolute, a `~/` name stays in the
    /// home directory while the target is there, and a name relative to the
    /// loading file stays relative to it, keeping a leading `./`.
    pub fn name_for(&self, loader: &Url, target: &Url) -> Option<String> {
        let target = target.to_file_path().ok()?;
        let name = match self.lookup() {
            FileLookup::Absolute | FileLookup::NotAFile => target,
            FileLookup::HomeRelative => match env::home_dir()
                .and_then(|home| target.strip_prefix(home).ok().map(Path::to_path_buf))
            {
                Some(relative) => {
                    Path::new(Macaulay2Conventions::HOME_DIRECTORY_PREFIX).join(relative)
                }
                None => target,
            },
            FileLookup::BesideLoader | FileLookup::BesideLoaderThenPath => {
                let loader = loader.to_file_path().ok()?;
                let relative = pathdiff::diff_paths(&target, loader.parent()?)?;
                if self
                    .name
                    .starts_with(Macaulay2Conventions::CURRENT_DIRECTORY_PREFIX)
                    && !relative.starts_with(Component::ParentDir)
                {
                    Path::new(Component::CurDir.as_os_str()).join(relative)
                } else {
                    relative
                }
            }
        };
        name.to_str().map(str::to_owned)
    }

    /// The literal naming `name`, delimited like this load's literal when
    /// that can hold it, otherwise quoted.
    pub fn literal_for(&self, name: &str) -> Option<String> {
        self.delimiter
            .literal(name)
            .or_else(|| StringDelimiter::Quote.literal(name))
    }

    /// The URL of the file at `path`. Parsing resolves its `.` and `..`
    /// segments, so a file has one URL however a load reaches it; where the
    /// operating system resolves `..` elsewhere, after a symbolic link, the
    /// URL is that of the file it reaches.
    pub fn file_url(path: &Path) -> Option<Url> {
        let lexical = Url::parse(Url::from_file_path(path).ok()?.as_str()).ok()?;
        let reaches_another_file = path.exists()
            && !lexical.to_file_path().is_ok_and(|lexical_path| {
                same_file::is_same_file(lexical_path, path).unwrap_or(false)
            });
        if reaches_another_file {
            if let Some(canonical) = fs::canonicalize(path)
                .ok()
                .and_then(|canonical| Url::from_file_path(canonical).ok())
            {
                return Some(canonical);
            }
        }
        Some(lexical)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(literal: &str) -> FileLoad {
        let loads = FileLoad::collect_from_text(&format!("load {literal}\n"));
        assert_eq!(loads.len(), 1, "one load of {literal}");
        loads.into_iter().next().expect("the load")
    }

    fn url(path: &str) -> Url {
        Url::from_file_path(path).expect("absolute path")
    }

    #[test]
    fn names_are_read_with_their_escapes() {
        assert_eq!(load(r#""we\"ird.m2""#).name, "we\"ird.m2");
        assert_eq!(load(r#""\x41.m2""#).name, "A.m2");
        assert_eq!(load("///a b.m2///").name, "a b.m2");
        assert!(
            FileLoad::collect_from_text("load \"\\q.m2\"\n").is_empty(),
            "an unreadable escape names no file"
        );
    }

    #[test]
    fn lookup_follows_the_name_prefixes_load_documents() {
        assert_eq!(load(r#""/abs/f.m2""#).lookup(), FileLookup::Absolute);
        assert_eq!(load(r#""~/f.m2""#).lookup(), FileLookup::HomeRelative);
        assert_eq!(load(r#""./f.m2""#).lookup(), FileLookup::BesideLoader);
        assert_eq!(load(r#""../f.m2""#).lookup(), FileLookup::BesideLoader);
        assert_eq!(
            load(r#""dir/f.m2""#).lookup(),
            FileLookup::BesideLoaderThenPath
        );
        assert_eq!(load(r#""$localhost:7500""#).lookup(), FileLookup::NotAFile);
        assert_eq!(load(r#""!ls""#).lookup(), FileLookup::NotAFile);
    }

    #[test]
    fn beside_resolves_relative_names_against_the_loading_file() {
        let loader = url("/project/src/main.m2");
        assert_eq!(
            load(r#""../lib/a b.m2""#).beside(&loader),
            Some(url("/project/lib/a b.m2"))
        );
        assert_eq!(
            load(r#""./x.m2""#).beside(&loader),
            Some(url("/project/src/x.m2"))
        );
        assert_eq!(load(r#""!ls""#).beside(&loader), None);
        let untitled = Url::parse("untitled:Untitled-1").expect("an untitled URI");
        assert_eq!(load(r#""x.m2""#).beside(&untitled), None);
        assert_eq!(
            load(r#""x.m2""#).resolve(&untitled, &[]),
            LoadResolution::Undetermined
        );
        assert_eq!(
            load(r#""""#).resolve(&loader, &[]),
            LoadResolution::EmptyName
        );
    }

    #[test]
    fn rewritten_names_keep_the_style_and_delimiters_of_the_load() {
        let loader = url("/project/src/main.m2");
        let target = url("/project/src/util/x.m2");
        assert_eq!(
            load(r#""x.m2""#).name_for(&loader, &target).as_deref(),
            Some("util/x.m2")
        );
        assert_eq!(
            load(r#""./x.m2""#).name_for(&loader, &target).as_deref(),
            Some("./util/x.m2")
        );
        assert_eq!(
            load(r#""./x.m2""#)
                .name_for(&loader, &url("/project/lib/x.m2"))
                .as_deref(),
            Some("../lib/x.m2")
        );
        assert_eq!(
            load(r#""/project/src/x.m2""#)
                .name_for(&loader, &target)
                .as_deref(),
            Some("/project/src/util/x.m2")
        );
        assert_eq!(
            load(r#""x.m2""#).literal_for("we\"ird.m2").as_deref(),
            Some(r#""we\"ird.m2""#)
        );
        assert_eq!(
            load("///x.m2///").literal_for("we\"ird.m2").as_deref(),
            Some(r#"///we"ird.m2///"#)
        );
        assert_eq!(
            load("///x.m2///").literal_for("a///b.m2").as_deref(),
            Some(r#""a///b.m2""#),
            "a name a verbatim string cannot hold is quoted"
        );
    }
}
