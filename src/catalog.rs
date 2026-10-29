//! Downloaded builtin metadata and documentation, kept outside the executable.
//!
//! The catalog is generated and published by the M2Book package: every M2Book
//! release carries it as a release asset, next to a SHA-256 checksum asset.
//! The server follows M2Book's latest release. A catalog is downloaded on first
//! use (or by `m2-ls --download-catalog`) and again only when M2Book publishes
//! a new release; otherwise the cached copy is used offline. The cache holds a
//! single file named by the checksum its content must match.

use std::env;
use std::fmt::{self, Display};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};
use ureq::ResponseExt;

use crate::builtin_index::BuiltinIndex;

/// M2Book release page used to discover the latest catalog version.
const RELEASES_URL: &str = "https://github.com/AlexanderGolys/M2Book/releases";
/// Path prefix identifying a redirected release-tag page.
const RELEASE_TAG_PATH: &str = "/releases/tag/";
/// Release asset containing builtin metadata and documentation.
const CATALOG_ASSET: &str = "m2-index.jsonl";
/// Release asset containing the catalog's expected SHA-256 checksum.
const CHECKSUM_ASSET: &str = "m2-index.jsonl.sha256";
/// Prefix of checksum-addressed catalog cache filenames.
const CATALOG_FILE_PREFIX: &str = "m2-index-";
/// Suffix of cached JSONL catalog filenames.
const CATALOG_FILE_SUFFIX: &str = ".jsonl";
/// Environment variable selecting a local catalog instead of the download cache.
const CATALOG_ENV: &str = "M2_LS_CATALOG";
/// Maximum time allowed for a catalog HTTP request.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
/// Maximum catalog response size accepted before aborting a download.
const MAX_CATALOG_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum checksum response size accepted before aborting a download.
const MAX_CHECKSUM_BYTES: u64 = 1024;
/// Number of hexadecimal digits required for a SHA-256 checksum.
const SHA256_HEX_LENGTH: usize = 64;

/// Where the builtin catalog comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CatalogSource {
    /// A local JSONL file selected by `M2_LS_CATALOG`: used as is, without
    /// network access or cache writes.
    Override(PathBuf),
    /// Catalogs downloaded from M2Book releases into the user's cache.
    Cache(CatalogCache),
}

impl CatalogSource {
    /// The `M2_LS_CATALOG` override when set, otherwise the user cache.
    ///
    /// This selects a location without downloading or reading the catalog.
    ///
    /// # Errors
    ///
    /// Returns an error when no platform cache directory can be determined.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// let source = m2_ls::catalog::CatalogSource::current()?;
    /// let corpus = source.load()?;
    /// assert!(!corpus.is_empty());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn current() -> io::Result<Self> {
        match env::var_os(CATALOG_ENV) {
            Some(path) => Ok(Self::Override(PathBuf::from(path))),
            None => CatalogCache::user().map(Self::Cache),
        }
    }

    /// Path of a verified catalog, downloading M2Book's latest one when
    /// nothing is cached yet.
    ///
    /// Cached downloads are checked against their checksum-addressed filename.
    /// A local override is canonicalized but is not checked against a remote digest.
    ///
    /// # Errors
    ///
    /// Returns filesystem errors, or download and verification errors when no
    /// usable cached catalog exists.
    pub fn path(&self) -> io::Result<PathBuf> {
        match self {
            Self::Override(path) => Self::resolve_override(path),
            Self::Cache(cache) => match cache.verified()? {
                Some(cached) => Ok(cached.path),
                None => cache.update(),
            },
        }
    }

    /// Path of a verified catalog after checking for a newer M2Book release.
    /// An override is never updated.
    ///
    /// # Errors
    ///
    /// Returns errors locating the override, fetching release assets, verifying
    /// the checksum or catalog schema, or updating the cache on disk.
    pub fn update(&self) -> io::Result<PathBuf> {
        match self {
            Self::Override(path) => Self::resolve_override(path),
            Self::Cache(cache) => cache.update(),
        }
    }

    /// Read the catalog for semantic analysis and hover docs, downloading
    /// M2Book's latest one when nothing is cached yet.
    ///
    /// # Errors
    ///
    /// Returns the lookup, download, or verification errors from [`Self::path`],
    /// or an error when the catalog cannot be read as UTF-8 text.
    pub fn load(&self) -> io::Result<String> {
        match self {
            Self::Override(path) => fs::read_to_string(Self::resolve_override(path)?),
            Self::Cache(cache) => match cache.verified()? {
                Some(cached) => String::from_utf8(cached.bytes)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
                None => fs::read_to_string(cache.update()?),
            },
        }
    }

    /// Validates an explicitly configured catalog path before opening it.
    fn resolve_override(path: &Path) -> io::Result<PathBuf> {
        fs::canonicalize(path).map_err(|error| {
            io::Error::new(
                io::Error::kind(&error),
                format!("{}: {error}", path.display()),
            )
        })
    }
}

/// Lowercase hexadecimal SHA-256 digest identifying a catalog's content.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatalogDigest(String);

impl CatalogDigest {
    /// The digest of the catalog bytes `catalog`.
    ///
    /// # Examples
    ///
    /// ```
    /// let digest = m2_ls::catalog::CatalogDigest::of(b"abc");
    /// assert_eq!(digest.to_string(),
    ///     "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    /// ```
    pub fn of(catalog: &[u8]) -> Self {
        Self(format!("{:x}", Sha256::digest(catalog)))
    }

    /// Read a digest in `sha256sum` output format: the digest, then optionally
    /// the file name.
    fn parse(checksum: &str) -> Option<Self> {
        checksum
            .split_whitespace()
            .next()
            .filter(|digest| {
                digest.len() == SHA256_HEX_LENGTH
                    && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            .map(|digest| Self(digest.to_ascii_lowercase()))
    }

    /// The digest a cache file name promises, if it names a cached catalog.
    fn of_file_name(name: &str) -> Option<Self> {
        name.strip_prefix(CATALOG_FILE_PREFIX)?
            .strip_suffix(CATALOG_FILE_SUFFIX)
            .and_then(Self::parse)
    }

    /// Cache filename derived from the catalog checksum.
    fn file_name(&self) -> String {
        format!("{CATALOG_FILE_PREFIX}{self}{CATALOG_FILE_SUFFIX}")
    }

    /// Whether the supplied catalog bytes have this expected SHA-256 digest.
    fn matches(&self, catalog: &[u8]) -> bool {
        Self::of(catalog) == *self
    }
}

impl Display for CatalogDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One published M2Book catalog: the release that carries it and the digest
/// published with it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatalogRelease {
    /// Release tag identifying the catalog assets.
    tag: String,
    /// Expected checksum used to verify catalog contents.
    digest: CatalogDigest,
}

impl CatalogRelease {
    /// The release tag of M2Book that published this catalog.
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// The digest the release publishes for its catalog.
    pub fn digest(&self) -> &CatalogDigest {
        &self.digest
    }
}

/// M2Book's GitHub releases, which publish the catalog as a release asset.
struct M2BookReleases {
    /// HTTP client configured for catalog requests.
    agent: ureq::Agent,
}

impl M2BookReleases {
    /// Constructs an HTTP release client with the catalog download timeout.
    fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(DOWNLOAD_TIMEOUT))
            .build()
            .new_agent();
        Self { agent }
    }

    /// The latest release with its published digest. The tag is read from
    /// where GitHub redirects the latest-release page, so both assets come
    /// from the same release.
    fn latest(&self) -> io::Result<CatalogRelease> {
        let response = self
            .agent
            .head(format!("{RELEASES_URL}/latest"))
            .call()
            .map_err(io::Error::other)?;
        let tag = Self::tag_of_release_page(response.get_uri().path())?;
        let checksum = self.asset(&tag, CHECKSUM_ASSET, MAX_CHECKSUM_BYTES)?;
        let digest =
            CatalogDigest::parse(&String::from_utf8_lossy(&checksum)).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{CHECKSUM_ASSET} of M2Book {tag} does not contain a SHA-256 digest"),
                )
            })?;
        Ok(CatalogRelease { tag, digest })
    }

    /// Extracts and validates the release tag from the redirected release-page path.
    fn tag_of_release_page(path: &str) -> io::Result<String> {
        path.rsplit_once(RELEASE_TAG_PATH)
            .map(|(_, tag)| tag.to_owned())
            .filter(|tag| !tag.is_empty() && !tag.contains('/'))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("M2Book has no published release at {RELEASES_URL}"),
                )
            })
    }

    /// Downloads the release catalog and verifies it against the expected checksum.
    fn catalog(&self, release: &CatalogRelease) -> io::Result<Vec<u8>> {
        self.asset(&release.tag, CATALOG_ASSET, MAX_CATALOG_BYTES)
    }

    /// Downloads a named release asset while enforcing the supplied byte limit.
    fn asset(&self, tag: &str, asset: &str, limit: u64) -> io::Result<Vec<u8>> {
        self.agent
            .get(format!("{RELEASES_URL}/download/{tag}/{asset}"))
            .call()
            .map_err(io::Error::other)?
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map_err(io::Error::other)
    }
}

/// A cached catalog whose content matched the digest in its file name.
struct VerifiedCatalog {
    /// Filesystem path of the represented file.
    path: PathBuf,
    /// Expected checksum used to verify catalog contents.
    digest: CatalogDigest,
    /// Verified catalog contents retained to avoid reopening the cache file.
    bytes: Vec<u8>,
}

/// Directory holding the downloaded catalog: one file, named by its digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatalogCache {
    /// Directory holding the downloaded catalog cache.
    directory: PathBuf,
}

impl CatalogCache {
    /// The cache below the platform's user cache directory.
    ///
    /// Linux uses `XDG_CACHE_HOME`, falling back to `~/.cache`. macOS uses
    /// `~/Library/Caches`; Windows uses `LOCALAPPDATA`.
    ///
    /// # Errors
    ///
    /// Returns an error if the required environment variables are unavailable.
    pub fn user() -> io::Result<Self> {
        Self::user_cache_directory()
            .map(|directory| Self::new(directory.join(env!("CARGO_PKG_NAME"))))
    }

    /// The cache in `directory`.
    ///
    /// Construction performs no filesystem or network operations.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::Path;
    /// let cache = m2_ls::catalog::CatalogCache::new("catalog-cache".into());
    /// assert_eq!(cache.directory(), Path::new("catalog-cache"));
    /// ```
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    /// The directory holding the cached catalog.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Download M2Book's latest catalog unless it is already the cached one,
    /// and return the path of the verified catalog in use.
    ///
    /// The download is validated before replacing the previous cached catalog.
    ///
    /// # Errors
    ///
    /// Returns network, checksum, schema-validation, or filesystem errors.
    /// An unsuccessful download does not install an unverified cache entry.
    pub fn update(&self) -> io::Result<PathBuf> {
        self.fetch_latest(&M2BookReleases::new()).map_err(|error| {
            io::Error::new(
                io::Error::kind(&error),
                format!("could not download the builtin catalog from M2Book: {error}; for offline use, set {CATALOG_ENV} to a local catalog file"),
            )
        })
    }

    /// Fetches the current release and stores its verified catalog in the cache.
    fn fetch_latest(&self, releases: &M2BookReleases) -> io::Result<PathBuf> {
        let release = releases.latest()?;
        if let Some(cached) = self.verified()? {
            if cached.digest == release.digest {
                return Ok(cached.path);
            }
        }
        eprintln!(
            "Downloading the Macaulay2 builtin catalog of M2Book {} to {}",
            release.tag,
            self.directory.display()
        );
        let catalog = releases.catalog(&release)?;
        self.store(&release, &catalog)
    }

    /// The cached catalog after checking it against the digest in its name.
    /// A damaged catalog is removed and counts as missing, so it is
    /// downloaded again.
    fn verified(&self) -> io::Result<Option<VerifiedCatalog>> {
        let Some((path, digest)) = self.cached_file()? else {
            return Ok(None);
        };
        let bytes = fs::read(&path)?;
        if !digest.matches(&bytes) {
            eprintln!(
                "Removing the damaged builtin catalog {}: its content does not match its checksum",
                path.display()
            );
            Self::remove(&path)?;
            return Ok(None);
        }
        Ok(Some(VerifiedCatalog {
            path,
            digest,
            bytes,
        }))
    }

    /// The cached catalog file. Only an interrupted replacement leaves more
    /// than one; the newest is kept and the others are removed.
    fn cached_file(&self) -> io::Result<Option<(PathBuf, CatalogDigest)>> {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if io::Error::kind(&error) == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut catalogs = Vec::new();
        for entry in entries {
            let entry = entry?;
            if let Some(digest) = CatalogDigest::of_file_name(&entry.file_name().to_string_lossy())
            {
                let modified = entry
                    .metadata()?
                    .modified()
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                catalogs.push((modified, entry.path(), digest));
            }
        }
        catalogs.sort_by(|left, right| right.0.cmp(&left.0));
        let mut catalogs = catalogs.into_iter().map(|(_, path, digest)| (path, digest));
        let newest = catalogs.next();
        catalogs.try_for_each(|(path, _)| Self::remove(&path))?;
        Ok(newest)
    }

    /// Cache a downloaded catalog in place of the previous one, after checking
    /// its digest and wire format. Nothing is cached when either check fails.
    fn store(&self, release: &CatalogRelease, catalog: &[u8]) -> io::Result<PathBuf> {
        if !release.digest.matches(catalog) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "builtin catalog does not match the checksum published by M2Book {}",
                    release.tag
                ),
            ));
        }
        let text = std::str::from_utf8(catalog)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        BuiltinIndex::validate(text).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "M2Book {} published a catalog in an unsupported format: {error}",
                    release.tag
                ),
            )
        })?;
        let previous = self.cached_file()?;
        fs::create_dir_all(&self.directory)?;
        let path = self.directory.join(release.digest.file_name());
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        temporary.write_all(catalog)?;
        temporary.persist(&path).map_err(|error| error.error)?;
        if let Some((previous, _)) = previous.filter(|(previous, _)| *previous != path) {
            Self::remove(&previous)?;
        }
        Ok(path)
    }

    /// Removes a cache file while treating an already absent path as success.
    fn remove(path: &Path) -> io::Result<()> {
        match fs::remove_file(path) {
            Err(error) if io::Error::kind(&error) != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Resolves the platform-specific cache directory, honoring the relevant environment overrides.
    fn user_cache_directory() -> io::Result<PathBuf> {
        #[cfg(target_os = "windows")]
        let directory = env::var_os("LOCALAPPDATA").map(PathBuf::from);
        #[cfg(target_os = "macos")]
        let directory = env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Caches"));
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let directory = env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")));
        directory.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no user cache directory; set {CATALOG_ENV} to a local catalog file"),
            )
        })
    }
}

/// The catalog the tests run against, loaded once per test process.
#[cfg(test)]
#[doc(hidden)]
pub fn test_corpus() -> &'static str {
    static CORPUS: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CORPUS.get_or_init(|| {
        CatalogSource::current()
            .and_then(|source| source.load())
            .expect("test catalog must be available")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release_of(tag: &str, catalog: &[u8]) -> CatalogRelease {
        CatalogRelease {
            tag: tag.to_owned(),
            digest: CatalogDigest::of(catalog),
        }
    }

    fn catalog_files(directory: &Path) -> Vec<String> {
        fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn release_tag_comes_from_the_latest_release_redirect() {
        assert_eq!(
            M2BookReleases::tag_of_release_page("/AlexanderGolys/M2Book/releases/tag/v0.2.0")
                .unwrap(),
            "v0.2.0"
        );
        let error =
            M2BookReleases::tag_of_release_page("/AlexanderGolys/M2Book/releases").unwrap_err();
        assert_eq!(io::Error::kind(&error), io::ErrorKind::NotFound);
    }

    #[test]
    fn checksum_asset_accepts_sha256sum_output_only() {
        let digest = "AB".repeat(32);
        assert_eq!(
            CatalogDigest::parse(&format!("{digest}  m2-index.jsonl\n")),
            Some(CatalogDigest(digest.to_ascii_lowercase()))
        );
        assert_eq!(CatalogDigest::parse("not-a-digest m2-index.jsonl"), None);
        assert_eq!(CatalogDigest::parse(""), None);
    }

    #[test]
    fn the_cache_holds_one_catalog_replaced_by_a_newer_release() {
        let directory = tempfile::tempdir().unwrap();
        let cache = CatalogCache::new(directory.path().to_owned());
        assert!(cache.verified().unwrap().is_none());

        let first = test_corpus().as_bytes();
        let first_path = cache.store(&release_of("v1", first), first).unwrap();
        assert_eq!(cache.verified().unwrap().unwrap().path, first_path);

        let second = format!("{}\n", test_corpus());
        let second_release = release_of("v2", second.as_bytes());
        let second_path = cache.store(&second_release, second.as_bytes()).unwrap();
        let cached = cache.verified().unwrap().unwrap();
        assert_eq!(cached.digest, second_release.digest);
        assert_eq!(fs::read_to_string(second_path).unwrap(), second);
        assert_eq!(
            catalog_files(directory.path()),
            [second_release.digest.file_name()]
        );
    }

    #[test]
    fn invalid_downloads_never_create_a_cache_entry() {
        let directory = tempfile::tempdir().unwrap();
        let cache = CatalogCache::new(directory.path().to_owned());

        let truncated = &test_corpus().as_bytes()[..1000];
        let mismatched = release_of("v1", test_corpus().as_bytes());
        let error = cache.store(&mismatched, truncated).unwrap_err();
        assert_eq!(io::Error::kind(&error), io::ErrorKind::InvalidData);

        let unsupported = br#"{"kind":"function"}"#;
        let error = cache
            .store(&release_of("v2", unsupported), unsupported)
            .unwrap_err();
        assert!(error.to_string().contains("unsupported format"), "{error}");

        assert!(catalog_files(directory.path()).is_empty());
    }

    #[test]
    fn a_damaged_catalog_is_removed_and_counts_as_missing() {
        let directory = tempfile::tempdir().unwrap();
        let cache = CatalogCache::new(directory.path().to_owned());
        let unrelated = directory.path().join("unrelated");
        fs::write(&unrelated, "kept").unwrap();
        let catalog = test_corpus().as_bytes();

        let path = cache.store(&release_of("v1", catalog), catalog).unwrap();
        fs::write(&path, "damaged").unwrap();
        assert!(cache.verified().unwrap().is_none());
        assert!(!path.exists());
        assert!(unrelated.exists());

        cache.store(&release_of("v1", catalog), catalog).unwrap();
        assert!(cache.verified().unwrap().is_some());
    }
}
