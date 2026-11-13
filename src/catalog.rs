//! Downloaded builtin metadata and documentation, kept outside the executable.
//!
//! The catalog is generated and published by the M2Book package: every M2Book
//! release carries it as its one `.jsonl` asset, under any name, and is
//! verified against the SHA-256 digest GitHub records for that asset.
//! The server follows the compatible M2Book release line by default;
//! `M2_LS_CATALOG_VERSION` can restrict it with a Cargo-style version requirement.
//! A catalog is downloaded on first use (or by `m2-ls --download-catalog`) and again only when M2Book publishes
//! a new release; otherwise the cached copy is used offline. The cache holds a
//! single file named by the checksum its content must match.

use std::cmp::Reverse;
use std::env;
use std::fmt::{self, Display};
use std::fs;
use std::io::Error as IOError;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use semver::{Version, VersionReq};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::builtin_index::BuiltinIndex;
use crate::settings::CatalogSettings;

/// Environment variable selecting a Cargo-style M2Book release requirement.
const CATALOG_VERSION_ENV: &str = "M2_LS_CATALOG_VERSION";
/// API describing M2Book's published releases and their assets.
const RELEASES_API: &str = "https://api.github.com/repos/AlexanderGolys/M2Book/releases";
/// Maximum release metadata page size supported by GitHub.
const RELEASES_PER_PAGE: usize = 100;
/// Maximum accepted release metadata response size.
const MAX_RELEASE_METADATA_BYTES: u64 = 8 * 1024 * 1024;
/// Algorithm prefix of the asset digests GitHub reports.
const ASSET_DIGEST_PREFIX: &str = "sha256:";
/// Prefix of checksum-addressed catalog cache filenames.
const CATALOG_FILE_PREFIX: &str = "m2-index-";
/// Suffix of JSONL catalog files: the one release asset carrying it, and
/// every cached copy.
const CATALOG_FILE_SUFFIX: &str = ".jsonl";
/// Environment variable selecting a local catalog instead of the download cache.
const CATALOG_ENV: &str = "M2_LS_CATALOG";
/// Maximum time allowed for a catalog HTTP request.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
/// Maximum catalog response size accepted before aborting a download.
const MAX_CATALOG_BYTES: u64 = 256 * 1024 * 1024;
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
    /// Returns an error when no platform cache directory can be determined or
    /// `M2_LS_CATALOG_VERSION` is not a valid Cargo-style requirement.
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
            None => {
                let requirement = match env::var(CATALOG_VERSION_ENV) {
                    Ok(requirement) => requirement,
                    Err(env::VarError::NotPresent) => CatalogSettings::DEFAULT_VERSION.to_owned(),
                    Err(error) => return Err(IOError::new(ErrorKind::InvalidInput, error)),
                };
                let requirement = if requirement == "latest" {
                    "*"
                } else {
                    &requirement
                };
                let requirement = VersionReq::parse(requirement).map_err(|error| {
                    IOError::new(
                        ErrorKind::InvalidInput,
                        format!("{CATALOG_VERSION_ENV}: {error}"),
                    )
                })?;
                CatalogCache::user().map(|cache| Self::Cache(cache.with_requirement(requirement)))
            }
        }
    }

    /// Select the local override or a cache isolated for the configured release bound.
    ///
    /// # Errors
    ///
    /// Returns an error if no platform cache directory can be determined.
    pub fn configured(settings: &CatalogSettings) -> io::Result<Self> {
        match env::var_os(CATALOG_ENV) {
            Some(path) => Ok(Self::Override(PathBuf::from(path))),
            None => CatalogCache::user()
                .map(|cache| Self::Cache(cache.with_requirement(settings.version.clone()))),
        }
    }

    /// Load optional metadata according to configuration, without accessing it when disabled.
    /// Unavailable catalogs leave source analysis enabled and report the failure on stderr.
    pub fn load_configured(settings: &CatalogSettings) -> String {
        if !settings.enabled {
            return String::new();
        }
        Self::configured(settings)
            .and_then(|source| source.load())
            .unwrap_or_else(|error| {
                eprintln!(
                    "m2-ls: builtin catalog unavailable: {error}; continuing with source analysis"
                );
                String::new()
            })
    }

    /// Path of a verified catalog, downloading the selected M2Book release when
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

    /// Read and validate the catalog for semantic analysis and hover docs,
    /// downloading the selected M2Book release when nothing is cached yet.
    ///
    /// # Errors
    ///
    /// Returns the lookup, download, or verification errors from [`Self::path`],
    /// or an error when the catalog is not UTF-8 or has an unsupported wire format.
    pub fn load(&self) -> io::Result<String> {
        let corpus = match self {
            Self::Override(path) => fs::read_to_string(Self::resolve_override(path)?),
            Self::Cache(cache) => match cache.verified()? {
                Some(cached) => String::from_utf8(cached.bytes)
                    .map_err(|error| IOError::new(ErrorKind::InvalidData, error)),
                None => fs::read_to_string(cache.update()?),
            },
        }?;
        BuiltinIndex::validate(&corpus)
            .map_err(|error| IOError::new(ErrorKind::InvalidData, error))?;
        Ok(corpus)
    }

    /// Load the configured catalog, or continue without builtin metadata.
    ///
    /// Failures are reported on stderr. Source analysis remains available when
    /// the cache, local override, or network is unavailable or incompatible.
    pub fn load_available() -> String {
        Self::current()
            .and_then(|source| source.load())
            .unwrap_or_else(|error| {
                eprintln!(
                    "m2-ls: builtin catalog unavailable: {error}; continuing with source analysis"
                );
                String::new()
            })
    }

    /// Validates an explicitly configured catalog path before opening it.
    fn resolve_override(path: &Path) -> io::Result<PathBuf> {
        fs::canonicalize(path).map_err(|error| {
            IOError::new(
                IOError::kind(&error),
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

    /// Read a digest written as 64 hexadecimal digits, in either case.
    fn from_hex(digest: &str) -> Option<Self> {
        (digest.len() == SHA256_HEX_LENGTH && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| Self(digest.to_ascii_lowercase()))
    }

    /// Read a digest as GitHub reports it for a release asset: `sha256:` and
    /// the hexadecimal digest.
    fn of_asset_digest(digest: &str) -> Option<Self> {
        digest
            .strip_prefix(ASSET_DIGEST_PREFIX)
            .and_then(Self::from_hex)
    }

    /// The digest a cache file name promises, if it names a cached catalog.
    fn of_file_name(name: &str) -> Option<Self> {
        name.strip_prefix(CATALOG_FILE_PREFIX)?
            .strip_suffix(CATALOG_FILE_SUFFIX)
            .and_then(Self::from_hex)
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

/// One published M2Book catalog: the release that carries it, where to
/// download it, and the digest GitHub reports for it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatalogRelease {
    /// Release tag of the M2Book release carrying the catalog.
    tag: String,
    /// Download URL of the catalog asset.
    url: String,
    /// Expected checksum used to verify catalog contents.
    digest: CatalogDigest,
}

impl CatalogRelease {
    /// The release tag of M2Book that published this catalog.
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// The digest GitHub reports for the catalog asset.
    pub fn digest(&self) -> &CatalogDigest {
        &self.digest
    }
}

/// A file attached to a published release, as the GitHub API describes it.
#[derive(Debug, Deserialize)]
struct ReleaseAsset {
    /// File name of the asset.
    name: String,
    /// Where the asset is downloaded from.
    browser_download_url: String,
    /// `sha256:` and the asset's digest; absent only for assets uploaded
    /// before GitHub began recording digests.
    digest: Option<String>,
}

/// Published release metadata: its tag, whether it is a draft, and its
/// assets.
#[derive(Debug, Deserialize)]
struct PublishedRelease {
    /// Tag naming the release and its version.
    tag_name: String,
    /// Unpublished releases cannot provide a selected catalog.
    draft: bool,
    /// Files attached to the release.
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

impl PublishedRelease {
    /// The semantic version of the tag, with or without a leading `v`.
    fn version(&self) -> Option<Version> {
        Version::parse(self.tag_name.strip_prefix('v').unwrap_or(&self.tag_name)).ok()
    }

    /// Highest published semantic version satisfying the requirement.
    fn select(releases: impl IntoIterator<Item = Self>, requirement: &VersionReq) -> Option<Self> {
        releases
            .into_iter()
            .filter(|release| !release.draft)
            .filter_map(|release| Some((release.version()?, release)))
            .filter(|(version, _)| requirement.matches(version))
            .max_by(|left, right| left.0.cmp(&right.0))
            .map(|(_, release)| release)
    }

    /// The catalog this release publishes: its only `.jsonl` asset, whatever
    /// its name, verified by the digest GitHub reports for it. Other assets,
    /// such as a checksum file, are ignored.
    fn catalog(self) -> io::Result<CatalogRelease> {
        let tag = self.tag_name;
        let mut catalogs = self
            .assets
            .into_iter()
            .filter(|asset| asset.name.ends_with(CATALOG_FILE_SUFFIX));
        let (Some(asset), None) = (catalogs.next(), catalogs.next()) else {
            return Err(IOError::new(
                ErrorKind::NotFound,
                format!("M2Book {tag} must carry exactly one {CATALOG_FILE_SUFFIX} catalog asset"),
            ));
        };
        let digest = asset
            .digest
            .as_deref()
            .and_then(CatalogDigest::of_asset_digest)
            .ok_or_else(|| {
                IOError::new(
                    ErrorKind::InvalidData,
                    format!(
                        "GitHub reports no SHA-256 digest for {} of M2Book {tag}",
                        asset.name
                    ),
                )
            })?;
        Ok(CatalogRelease {
            tag,
            url: asset.browser_download_url,
            digest,
        })
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

    /// The catalog of the highest published release satisfying a Cargo-style
    /// requirement; `*` is the release GitHub marks as latest.
    fn matching(&self, requirement: &VersionReq) -> io::Result<CatalogRelease> {
        if *requirement == VersionReq::STAR {
            return self
                .metadata::<PublishedRelease>(&format!("{RELEASES_API}/latest"))?
                .catalog();
        }
        let mut selected: Option<PublishedRelease> = None;
        for page in 1.. {
            let releases: Vec<PublishedRelease> = self.metadata(&format!(
                "{RELEASES_API}?per_page={RELEASES_PER_PAGE}&page={page}"
            ))?;
            let last_page = releases.len() < RELEASES_PER_PAGE;
            selected = PublishedRelease::select(selected.into_iter().chain(releases), requirement);
            if last_page {
                break;
            }
        }
        selected
            .ok_or_else(|| {
                IOError::new(
                    ErrorKind::NotFound,
                    format!("no published M2Book release matches {requirement}"),
                )
            })?
            .catalog()
    }

    /// Reads release metadata from the GitHub API.
    fn metadata<Metadata: for<'de> Deserialize<'de>>(&self, url: &str) -> io::Result<Metadata> {
        let bytes = self
            .agent
            .get(url)
            .header(
                "User-Agent",
                concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
            )
            .call()
            .map_err(IOError::other)?
            .body_mut()
            .with_config()
            .limit(MAX_RELEASE_METADATA_BYTES)
            .read_to_vec()
            .map_err(IOError::other)?;
        serde_json::from_slice(&bytes).map_err(IOError::other)
    }

    /// Downloads the release catalog, at most the catalog size limit; the
    /// cache verifies it against the release digest before storing it.
    fn catalog(&self, release: &CatalogRelease) -> io::Result<Vec<u8>> {
        self.agent
            .get(&release.url)
            .call()
            .map_err(IOError::other)?
            .body_mut()
            .with_config()
            .limit(MAX_CATALOG_BYTES)
            .read_to_vec()
            .map_err(IOError::other)
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

/// Directory holding one downloaded catalog, named by its digest.
/// Constrained release selections use separate subdirectories.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatalogCache {
    /// Directory holding the downloaded catalog cache.
    directory: PathBuf,
    /// Release constraint associated with this isolated cache directory.
    requirement: VersionReq,
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
        Self {
            directory,
            requirement: VersionReq::STAR,
        }
    }

    /// Isolate a constrained catalog from the default latest-release cache.
    ///
    /// Use a fresh cache value when choosing a different requirement. Equivalent
    /// requirements share the directory derived from their normalized spelling.
    pub fn with_requirement(mut self, requirement: VersionReq) -> Self {
        if requirement != VersionReq::STAR {
            self.directory = self
                .directory
                .join(CatalogDigest::of(requirement.to_string().as_bytes()).to_string());
        }
        self.requirement = requirement;
        self
    }

    /// The directory holding the cached catalog.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Download the newest matching M2Book catalog unless it is already cached,
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
            IOError::new(
                IOError::kind(&error),
                format!("could not download the builtin catalog from M2Book: {error}; for offline use, set {CATALOG_ENV} to a local catalog file"),
            )
        })
    }

    /// Fetches the current release and stores its verified catalog in the cache.
    fn fetch_latest(&self, releases: &M2BookReleases) -> io::Result<PathBuf> {
        let release = releases.matching(&self.requirement)?;
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

    /// The cached catalog after checking its checksum, UTF-8, and wire format.
    /// A damaged catalog is removed and counts as missing, so it is
    /// downloaded again.
    fn verified(&self) -> io::Result<Option<VerifiedCatalog>> {
        let Some((path, digest)) = self.cached_file()? else {
            return Ok(None);
        };
        let bytes = fs::read(&path)?;
        let valid_format = std::str::from_utf8(&bytes)
            .ok()
            .is_some_and(|text| BuiltinIndex::validate(text).is_ok());
        if !digest.matches(&bytes) || !valid_format {
            eprintln!(
                "Removing the damaged builtin catalog {}: checksum or catalog format validation failed",
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
            Err(error) if IOError::kind(&error) == ErrorKind::NotFound => return Ok(None),
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
        catalogs.sort_by_key(|(modified, _, _)| Reverse(*modified));
        let mut catalogs = catalogs.into_iter().map(|(_, path, digest)| (path, digest));
        let newest = catalogs.next();
        catalogs.try_for_each(|(path, _)| Self::remove(&path))?;
        Ok(newest)
    }

    /// Cache a downloaded catalog in place of the previous one, after checking
    /// its digest and wire format. Nothing is cached when either check fails.
    fn store(&self, release: &CatalogRelease, catalog: &[u8]) -> io::Result<PathBuf> {
        if !release.digest.matches(catalog) {
            return Err(IOError::new(
                ErrorKind::InvalidData,
                format!(
                    "builtin catalog does not match the checksum published by M2Book {}",
                    release.tag
                ),
            ));
        }
        let text = std::str::from_utf8(catalog)
            .map_err(|error| IOError::new(ErrorKind::InvalidData, error))?;
        BuiltinIndex::validate(text).map_err(|error| {
            IOError::new(
                ErrorKind::InvalidData,
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
            Err(error) if IOError::kind(&error) != ErrorKind::NotFound => Err(error),
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
            IOError::new(
                ErrorKind::NotFound,
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
            url: format!("https://example.invalid/{tag}/catalog.jsonl"),
            digest: CatalogDigest::of(catalog),
        }
    }

    fn asset(name: &str, digest: Option<&str>) -> ReleaseAsset {
        ReleaseAsset {
            name: name.to_owned(),
            browser_download_url: format!("https://example.invalid/{name}"),
            digest: digest.map(str::to_owned),
        }
    }

    fn published(tag: &str, draft: bool, assets: Vec<ReleaseAsset>) -> PublishedRelease {
        PublishedRelease {
            tag_name: tag.to_owned(),
            draft,
            assets,
        }
    }

    fn catalog_files(directory: &Path) -> Vec<String> {
        fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn constraints_select_the_highest_published_matching_version() {
        let select = |requirement: &str| {
            let releases = ["v0.1.1", "0.1.3", "v0.2.0", "v0.1.4-beta.1", "not-semver"]
                .map(|tag| published(tag, false, Vec::new()))
                .into_iter()
                .chain([published("v0.1.9", true, Vec::new())]);
            PublishedRelease::select(releases, &VersionReq::parse(requirement).unwrap())
                .map(|release| release.tag_name)
        };
        assert_eq!(select(">=0.1.1, <0.2"), Some("0.1.3".to_owned()));
        assert_eq!(select("=0.1.1"), Some("v0.1.1".to_owned()));
        assert_eq!(select("^0.1.1"), Some("0.1.3".to_owned()));
        assert_eq!(select("=0.1.4-beta.1"), Some("v0.1.4-beta.1".to_owned()));
        assert_eq!(select(">=1"), None);
    }

    #[test]
    fn constrained_caches_cannot_reuse_an_unrelated_release() {
        let directory = tempfile::tempdir().unwrap();
        let latest = CatalogCache::new(directory.path().to_owned());
        let pinned = latest
            .clone()
            .with_requirement(VersionReq::parse("=0.1.1").unwrap());
        let corpus = b"{\"kind\":\"meta\"}\n";
        let release = release_of("v0.1.1", corpus);
        let latest_path = latest.store(&release, corpus).unwrap();
        assert!(pinned.verified().unwrap().is_none());
        let pinned_path = pinned.store(&release, corpus).unwrap();
        assert_ne!(latest_path, pinned_path);
        assert!(latest_path.exists());
        let broken = b"not JSON";
        assert!(pinned.store(&release_of("v0.1.2", broken), broken).is_err());
        assert_eq!(pinned.verified().unwrap().unwrap().path, pinned_path);
        assert!(CatalogSource::Cache(pinned).load().is_ok());
    }

    #[test]
    fn malformed_local_catalogs_are_reported_before_analysis() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.jsonl");
        fs::write(&path, "invalid JSON").unwrap();
        assert_eq!(
            io::Error::kind(&CatalogSource::Override(path).load().unwrap_err()),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn the_catalog_is_the_one_jsonl_asset_under_any_name() {
        let digest = "AB".repeat(32);
        let github_digest = format!("sha256:{digest}");
        let release = published(
            "v0.1.3",
            false,
            vec![
                asset("m2-index.jsonl.sha256", Some(&github_digest)),
                asset("builtins.jsonl", Some(&github_digest)),
            ],
        )
        .catalog()
        .unwrap();
        assert_eq!(release.tag(), "v0.1.3");
        assert_eq!(release.url, "https://example.invalid/builtins.jsonl");
        assert_eq!(release.digest().to_string(), digest.to_ascii_lowercase());

        for assets in [
            Vec::new(),
            vec![asset("notes.txt", Some(&github_digest))],
            vec![
                asset("a.jsonl", Some(&github_digest)),
                asset("b.jsonl", Some(&github_digest)),
            ],
        ] {
            let error = published("v1", false, assets).catalog().unwrap_err();
            assert_eq!(io::Error::kind(&error), io::ErrorKind::NotFound);
        }
        for digest in [None, Some("md5:abc"), Some("sha256:not-hex")] {
            let error = published("v1", false, vec![asset("index.jsonl", digest)])
                .catalog()
                .unwrap_err();
            assert_eq!(io::Error::kind(&error), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn release_metadata_reads_the_github_api_shape() {
        let release: PublishedRelease = serde_json::from_str(
            r#"{"tag_name": "v0.1.2", "draft": false, "assets": [{"name": "m2-index.jsonl",
                "browser_download_url": "https://example.invalid/m2-index.jsonl",
                "digest": "sha256:fb64052a95943d7b90bd0af7234f2a8372ce68028e4d9c6f3ef443086d8fa882",
                "size": 58040028}]}"#,
        )
        .unwrap();
        assert_eq!(release.version(), Some(Version::new(0, 1, 2)));
        assert_eq!(
            release.catalog().unwrap().digest().to_string(),
            "fb64052a95943d7b90bd0af7234f2a8372ce68028e4d9c6f3ef443086d8fa882"
        );
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
