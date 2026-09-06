# Macaulay2 LSP

[![crates.io](https://img.shields.io/crates/v/m2-ls.svg)](https://crates.io/crates/m2-ls)
[![GitHub release](https://img.shields.io/github/v/release/AlexanderGolys/m2-ls?sort=semver)](https://github.com/AlexanderGolys/m2-ls/releases/latest)
[![MSRV](https://img.shields.io/badge/MSRV-1.88-blue.svg)](https://www.rust-lang.org/)

A language server for [Macaulay2](https://macaulay2.com/), built on
Tree-sitter syntax analysis plus a generated database of Macaulay2 builtin
metadata (types, methods, options, documentation).

**Status: stable.** The core capabilities below are implemented and tested.
Type inference remains conservative where runtime behavior cannot be proven.

## Features

- Incremental document synchronization over stdio.
- Push diagnostics from Tree-sitter syntax and semantic analysis, including
  Macaulay2-specific checks: invalid method installations (non-flexible
  operators, no-effect installs, `=` vs `:=`, arity), parallel-assignment
  arity, and option-key conventions. Diagnostic messages double as M2-idiom
  hints.
- Contextual completion for package imports, types after `new`, callable
  options, and symbol prefixes with only a few possible endings.
- Hover documentation, signatures, and inferred types for both local
  symbols and indexed builtin/imported-package objects.
- Signature help for builtin and local methods with active-parameter tracking.
- Full-document semantic tokens for local, builtin, imported, and cross-file
  workspace symbols.
- Go to definition for local, workspace, builtin, package, and documentation
  targets.
- Find references within a lexical scope or across indexed workspace files.
- Document highlights for bindings, control-flow keywords, delimiters, and
  other related syntax.
- Prepare rename and rename for local and workspace symbols, including
  wiki-link documentation references.
- Hierarchical document symbols for bindings, assignments, and functions.
- Workspace symbol search across indexed Macaulay2 files.
- Type hierarchy preparation, supertypes, and subtypes for source and indexed
  types.
- Type inlay hints, with optional per-expression hints, complete inferred subsets,
  and call codomains.
- Quick-fix and refactor code actions for method codomains, ambiguous member
  access, raw strings, conditionals, `try`, and `else if` structure.
- Whole-document formatting with configurable indentation, line wrapping,
  control-flow layout, operator spacing, and semicolon layout.
- Folding ranges for parsed blocks and consecutive comment sections.
- Workspace indexing with live-buffer precedence and watched-file refresh.
- Runtime configuration updates with diagnostic republishing and negotiated
  inlay-hint refresh.
- Expression type inference for literals, collections, applications, operator
  dispatch, and control flow such as `if`, `try`, `for`, and `while`.

## Requirements

- Rust 1.88 or newer with Cargo.
- A `tree-sitter-macaulay2` grammar (fetched as a pinned dependency by Cargo).
- Macaulay2 itself is only needed at runtime by your editor; the builtin
  metadata is checked in.

## Build and install

Install the latest crates.io release:

```sh
cargo install m2-ls
```

To build the current source checkout instead:

```sh
cargo build --release
install -m755 target/release/m2-ls ~/.local/bin/m2-ls
```

The binary is `m2-ls` (hyphen). Point your editor's LSP client at the installed
path.

## Neovim setup

The native configuration API below requires Neovim 0.11 or newer. After
installing `m2-ls`, add this to `init.lua` or a Lua module loaded by it:

```lua
vim.filetype.add({
  extension = {
    m2 = 'macaulay2',
  },
})

vim.lsp.config('m2_ls', {
  cmd = { vim.fn.expand('~/.cargo/bin/m2-ls') },
  filetypes = { 'macaulay2' },
  root_markers = { '.git' },
  settings = {
    ['m2-ls'] = {
      diagnostics = {
        enabled = true,
        disabled = {},
      },
      formatting = {
        indentWidth = 4,
        useTabs = false,
        softLineWidth = 100,
        hardLineWidth = 100,
        controlFlowLayout = 'multilineCompactElse',
        compactFactorOperators = false,
        breakAfterSemicolon = true,
      },
      inlayHints = {
        expressionTypes = false,
        allKnownTypes = false,
      },
    },
  },
})

vim.lsp.enable('m2_ls')
```

If `m2-ls` is already available on Neovim's `$PATH`, `cmd = { 'm2-ls' }` is
equivalent. The explicit Cargo path is useful when a graphical Neovim session
does not inherit the shell's `$PATH`.

Open an `.m2` file and run `:checkhealth vim.lsp` to confirm that `m2_ls` is
attached. Neovim wires up supported navigation, hover, semantic tokens, code
actions, symbols, and LSP omnifunc completion; use `<C-x><C-o>` in Insert mode
to request completion without an additional completion plugin. Useful built-in
commands include `:lua vim.lsp.buf.format()` for formatting and
`:lua vim.lsp.inlay_hint.enable(true)` to display inlay hints. Restart the
client after upgrading the binary with `:LspRestart m2_ls`.

## Settings

Settings may be sent under the `m2-ls` or `macaulay2` section of
`workspace/didChangeConfiguration`, or directly through
`initializationOptions`. Changes apply without restarting the server. When the
client advertises `workspace.inlayHint.refreshSupport`, changing inlay-hint
settings also refreshes hints already visible in the editor. Every object is
optional, so a configuration may specify only the values it wants to change.

| Setting | Type | Default | Effect |
| --- | --- | --- | --- |
| `diagnostics.enabled` | boolean | `true` | Enables or suppresses all published diagnostics. |
| `diagnostics.disabled` | string array | `[]` | Suppresses selected rules by stable name or code, such as `unused-binding` or `T02`. |
| `formatting.indentWidth` | non-negative integer or `null` | client value | Overrides the formatting request's `tabSize`; `0` is treated as `1`, while `null` uses the client value. |
| `formatting.useTabs` | boolean or `null` | client value | Overrides the inverse of the formatting request's `insertSpaces`; `null` uses the client value. |
| `formatting.softLineWidth` | non-negative integer or `null` | `100` | Preferred width used to choose safe parsed line-break positions; `0` or `null` disables the soft target. |
| `formatting.hardLineWidth` | non-negative integer or `null` | `100` | Triggers wrapping when exceeded; `0` or `null` disables forced wrapping. The soft width is clamped to this value. |
| `formatting.maxLineWidth` | non-negative integer or `null` | unset | Compatibility override that sets both line widths; `0` or `null` disables both. When present, it takes precedence over `softLineWidth` and `hardLineWidth`. |
| `formatting.controlFlowLayout` | string enum | `multilineCompactElse` | Selects `compact`, `multiline`, or `multilineCompactElse`; the last form keeps the final `else value` together. |
| `formatting.compactFactorOperators` | boolean | `false` | Uses compact products such as `2*x`; `false` produces `2 * x`. |
| `formatting.breakAfterSemicolon` | boolean | `true` | Places the next statement on a new line; `false` keeps it inline with one space. |
| `inlayHints.expressionTypes` | boolean | `false` | Adds inferred argument and subexpression types plus `-> C` call codomains; `↑T` denotes the principal upper set generated by `T`. |
| `inlayHints.allKnownTypes` | boolean | `false` | Shows every statically known type, including self-describing literals and literal sequences; this also enables expression type hints. |

Diagnostic names and stable codes are listed in `src/diagnostic_registry.rs`.
Invalid diagnostic selectors reject the settings update and leave the previous
configuration active.

## Builtin metadata

The server does not hardcode Macaulay2 builtins. They live in a single
checked-in corpus, `src/data/m2-index.jsonl` — one JSON record per builtin
object (class, methods, options, semantic-token class, and folded hover
markdown). The immutable catalog is partitioned by home package; each document
records its `needsPackage`/`importFrom` inclusions once per text version. Package
objects become visible only after their inclusion and shadow ordinary names in
inclusion/definition order; indexed aliases remain available for explicit
disambiguation. The corpus is a generated artifact produced from an installed
Macaulay2's documentation.

## Repository layout

```text
Cargo.toml
ANALYSIS.md                 incremental m2-syn analysis roadmap
src/
  main.rs                 LSP server entry point and request handlers
  analysis.rs             scopes, bindings, installations, type inference
  builtin_index.rs        canonical builtin records and corpus loading
  typesystem.rs           type relations, dispatch, signatures
  diagnostic_registry.rs  the single registry of every diagnostic
  document.rs             per-document snapshot and incremental edits
  object_registry.rs      shared object catalog + loaded package registry
  workspace_index.rs      cross-file global definition index
  capabilities/           one module per LSP capability
  data/
    m2-index.jsonl        generated builtin corpus
```

## Development

Run from the repository root:

```sh
cargo fmt
cargo clippy --tests
cargo test
cargo build
```

The build is expected to be warning-free. See `CONTRIBUTIONS.md` for contributor
guidance.
