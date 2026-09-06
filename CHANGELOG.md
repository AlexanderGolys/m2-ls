# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

## [1.1.0] - 2026-09-06

### Added

- Added structurally attached Markdown documentation with wiki-link navigation,
  hover integration, semantic highlighting, and generated mdBook pages.
- Added `m2-ls docs <source-or-directory>` for generating package documentation.
- Added the `ring-variable-naming` hint, which reports ring variables written as
  multi-character symbols and suggests the indexed form (`x0` becomes `x_0`).

### Changed

- Updated the typed syntax integration from `m2-syn` 0.2 to 0.5 and raised the
  minimum Rust version to 1.88 for its generated-syntax macro dependency.
- Updated `tree-sitter-macaulay2` to 6.2.0, which keeps complete `if` and `for`
  expressions as operands of implicit application, including multiline forms.
- Limited clickable links in ordinary comments to explicit `[[wiki links]]`;
  legacy backtick references remain available in raw documentation strings.
- Kept typed syntax through incomplete edits by parsing with recovery, so an
  unclosed delimiter no longer drops analysis back to untyped traversal.
- Highlighted a quoted name as the symbol it denotes rather than as the value it
  is otherwise bound to, for every quote form and for quoted operators and
  punctuation.

### Fixed

- Stopped warning when an output-cell-shaped name has no matching cell and
  therefore evaluates as an unassigned `Symbol`.
- Attached documentation only to bindings introduced by the directly following
  assignment and only treated a direct `newPackage(...)` call as a package
  declaration.
- Read the `symbol` and `list` keywords as keywords rather than as the identifier
  and list expressions that share their grammar node names.
- Stopped reporting an expression as simplifiable when the suggested rewrite is
  the expression itself, which left a diagnostic its own quick fix could not
  clear.
- Stripped the whole `---` and `-**` doc-comment markers, which previously leaked
  a character that rendered attached documentation as a list.
- Stopped reporting a control transfer as misplaced when it sits inside source
  the grammar could not parse, where its enclosing function or loop is unknown.
- Resolved documentation assets relative to the executable rather than the
  working directory, so `m2-ls docs` works outside the repository root.

## [1.0.0] - 2026-08-27

### Added

- Added source-aware declaration and implementation navigation across the
  workspace, including method installations and local lambda assignments.
- Added exact-point and upper-closure type ranges with normalized union and
  intersection operations.
- Added contextual completion patterns for package imports, types after `new`,
  callable option keys and values, and symbol prefixes with only a few possible
  endings.

### Changed

- Migrated analysis and CST traversal to the typed `m2-syn` 0.2 interface.
- Made completion a pattern-to-query pipeline with shared symbol sources,
  visibility, type filtering, ranking, de-duplication, and prefix edits.
- Tightened method-installation, source-order, scope, and package-visibility
  analysis throughout editor capabilities.
- Updated to `tree-sitter-macaulay2` 6.1 and removed local dependency links from
  release builds.

### Fixed

- Preserved compatibility with the grammar's renamed binary-expression operand
  field.
- Suppressed broad completion lists in ordinary expression positions.

## [0.1.1] - 2026-08-01

### Changed

- Renamed the GitHub repository to `m2-ls` and updated package metadata.
- Documented every implemented language-server capability.
- Removed the GitHub Actions workflow in favor of local release validation.

## [0.1.0] - 2026-08-01

### Added

- Tree-sitter-backed Macaulay2 parsing, diagnostics, formatting, and semantic tokens.
- Completion, hover, signature help, navigation, rename, symbols, highlights, and type hierarchy.
- Source-aware type inference, method dispatch, package visibility, and inlay hints.
- Generated builtin and package metadata with documentation and method signatures.
- Full-process JSON-RPC coverage for core language-server workflows.

[1.1.0]: https://github.com/AlexanderGolys/m2-ls/releases/tag/v1.1.0
[1.0.0]: https://github.com/AlexanderGolys/m2-ls/releases/tag/v1.0.0
[0.1.1]: https://github.com/AlexanderGolys/m2-ls/releases/tag/v0.1.1
[0.1.0]: https://github.com/AlexanderGolys/m2-ls/releases/tag/v0.1.0
