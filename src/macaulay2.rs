//! Shared Macaulay2 language names, documentation rules, and installation layouts.

/// Macaulay2 names and conventions used for parsing and source discovery.
///
/// Installation paths follow `Layout` in its startup code (`startup.m2.in`,
/// `system.m2`, `last.m2` and `Core.m2`), using configure's default directories.
pub struct Macaulay2Conventions;

impl Macaulay2Conventions {
    /// Quiet startup skips user initialization and prints the installation prefix.
    pub const INSTALLATION_QUERY_ARGUMENTS: [&str; 7] = [
        "-q",
        "--silent",
        "--no-readline",
        "--no-prompts",
        "--stop",
        "-e",
        "print prefixDirectory; exit 0",
    ];

    /// The package-import option of `newPackage`.
    pub const PACKAGE_IMPORTS_OPTION: &str = "PackageImports";

    /// The package-export option of `newPackage`.
    pub const PACKAGE_EXPORTS_OPTION: &str = "PackageExports";

    /// Functions whose first string argument identifies a package to import.
    pub const PACKAGE_IMPORT_FUNCTIONS: [&str; 4] =
        ["needsPackage", "loadPackage", "debug", "importFrom"];

    /// Functions whose first string argument identifies a source file to run.
    pub const FILE_LOAD_FUNCTIONS: [&str; 3] = ["load", "needs", "input"];

    /// The first characters of a name `load` opens as a socket (`$`) or a
    /// pipe (`!`) instead of a file.
    pub const NON_FILE_NAME_PREFIXES: [char; 2] = ['$', '!'];

    /// The prefix of a file name `load` looks up in the home directory.
    pub const HOME_DIRECTORY_PREFIX: &str = "~/";

    /// The prefix of a file name relative to the directory of the loading
    /// file only.
    pub const CURRENT_DIRECTORY_PREFIX: &str = "./";

    /// Source-file extension, without the leading dot.
    pub const SOURCE_EXTENSION: &str = "m2";

    /// Environment variable for additional package source directories.
    pub const SOURCE_PATH_VARIABLE: &str = "M2_LSP_SOURCE_PATH";

    /// Core's list of packages loaded at startup.
    pub const PRELOADED_PACKAGES_KEY: &str = "preloaded packages";

    /// The function that creates a package and binds its global name.
    pub const PACKAGE_CONSTRUCTOR: &str = "newPackage";

    /// The headline length past which `document` warns that headlines must be
    /// shorter than 100 characters.
    pub const HEADLINE_WARNING_LENGTH: usize = 100;

    /// The headline length past which `document` rejects the headline.
    pub const HEADLINE_MAXIMUM_LENGTH: usize = 200;

    /// The function after whose call a package's documentation starts: what
    /// follows it is skipped when the package loads without documentation.
    pub const BEGIN_DOCUMENTATION: &str = "beginDocumentation";

    /// The packages a `beginDocumentation()` call loads with `needsPackage`
    /// for the documentation after it: the hypertext of `Text` (`TO`, `PARA`,
    /// …) and the `doc` function of `SimpleDoc`.
    pub const DOCUMENTATION_PACKAGES: [&str; 2] = ["Text", "SimpleDoc"];

    /// The language Markdown code blocks of Macaulay2 code are tagged with:
    /// the name of its tree-sitter grammar, by which editors highlight them.
    pub const MARKDOWN_CODE_LANGUAGE: &str = "macaulay2";

    /// The columns a tab advances SimpleDoc indentation to the next multiple
    /// of.
    pub const SIMPLE_DOC_TAB_WIDTH: usize = 8;

    /// The command Macaulay2 is run by.
    #[cfg(not(windows))]
    pub const EXECUTABLE: &str = "M2";
    #[cfg(windows)]
    pub const EXECUTABLE: &str = "M2.exe";

    /// `Layout#1#"bin"`, the executables relative to a prefix; under
    /// `Layout#2` the same directory sits in the machine directory.
    pub const BIN: &str = "bin";

    /// `Layout#1#"packages"`, the package sources relative to a prefix: each
    /// package's main file `PKG.m2`, beside the directory `PKG/` of its other
    /// files (`Layout#1#"package"`).
    pub const PKG: &str = "share/Macaulay2";

    /// `Layout#1#"packagelib"` without its package: the architecture-dependent
    /// package files, which with the package sources identify a layout.
    pub const PACKAGE_LIBRARIES: &str = "lib/Macaulay2";

    /// What `Layout#2` puts in place of `${prefix}`, holding the
    /// architecture-independent files; it puts the machine directory
    /// (`version#"machine"`) in place of `${exec_prefix}`.
    pub const ARCHITECTURE_INDEPENDENT: &str = "common";

    /// The prefix a build directory (`topBuilddir`) installs into.
    pub const BUILD_PREFIX: &str = "usr-dist";

    /// The file of a build directory whose first line names its source
    /// directory (`topSrcdir`), relative to the build directory.
    pub const SRC_DIR: &str = "srcdir";

    /// Core's files relative to a source or build directory.
    pub const CORE_SOURCES: &str = "Macaulay2/m2";

    /// The package sources relative to a source directory.
    pub const PACKAGES_DIR: &str = "Macaulay2/packages";

    /// The package made of Macaulay2's basic files, which loads from
    /// `corepath` rather than from `path`.
    pub const CORE: &str = "Core";

    /// `applicationDirectorySuffix`, the user's application directory relative
    /// to `HOME`.
    #[cfg(not(target_os = "macos"))]
    pub const APPLICATION_DIRECTORY_SUFFIX: &str = ".Macaulay2";
    #[cfg(target_os = "macos")]
    pub const APPLICATION_DIRECTORY_SUFFIX: &str = "Library/Application Support/Macaulay2";

    /// The application directory's source directory, first on `path`.
    pub const APPLICATION_CODE: &str = "code";

    /// The prefix inside the application directory that `installPackage`
    /// installs into; it always has `Layout#1`.
    pub const APPLICATION_PREFIX: &str = "local";

    /// The initialization file in the application directory.
    pub const INITIALIZATION_FILE: &str = "init.m2";
}
