# m2-ls

[![crates.io](https://img.shields.io/crates/v/m2-ls.svg)](https://crates.io/crates/m2-ls)
[![GitHub release](https://img.shields.io/github/v/release/AlexanderGolys/m2-ls?sort=semver)](https://github.com/AlexanderGolys/m2-ls/releases/latest)
[![MSRV](https://img.shields.io/badge/MSRV-1.88-blue.svg)](https://www.rust-lang.org/)

**A language server for [Macaulay2](https://macaulay2.com/).** Hover docs,
go-to-definition, rename, type hints, formatting, and diagnostics that know
Macaulay2 — in any editor that speaks LSP, without running your code.

```m2
R = QQ[x0, x1, y]
--     ^^ Ring variable `x0` is a multi-character symbol, not an indexed
--        variable; use `x_0` so the generators form one indexed family

sizeLabel = L -> (
    n := #L;
    if n then "nonempty" else "empty"
    -- ^ if condition must have type `Boolean`, but this expression has type `ZZ`
    )
```

## Why use it

- **Explore without leaving the file.** Hover any builtin for its documentation,
  signatures, options, and result type; signature help follows you through the
  call.
- **Move around by meaning.** Jump to definitions in your workspace or in the
  installed Macaulay2 library, find references, and rename a binding without
  touching an unrelated one that shares its name.
- **Catch Macaulay2-specific mistakes early.** Non-Boolean conditions, `=` where a
  method installation needs `:=`, installs on non-flexible operators, mismatched
  parallel assignments, and more, several with a quick fix.
- **See what the code is.** Semantic highlighting tells types, functions, and
  symbols apart; inlay hints show inferred types.
- **Keep it tidy.** A configurable whole-document formatter, folding, and
  document and workspace symbols.
- **Document code in comments.** `--` comments directly above a definition
  become its hover docs, and `[[name]]` inside them links to other definitions.

When a result depends on what the code does at runtime, `m2-ls` shows less
rather than guess.

## Install

```sh
cargo install m2-ls
```

Requires Rust 1.88 or newer. Macaulay2 itself is optional: the builtin
catalog ships with the server.

## Command-line tools

Installation also provides `m2-lint` and `m2-fmt`, using the same diagnostics
and formatter as the server:

```sh
m2-lint .                      # Recursively lint .m2 files
m2-lint --deny-warnings file.m2 # Fail on warnings as well as errors
m2-fmt .                       # Format .m2 files in place
m2-fmt --check .                # Check formatting without writing (CI)
m2-fmt --stdout file.m2         # Print one formatted file
cat file.m2 | m2-fmt -          # Format stdin to stdout
```

Both commands accept multiple files or directories and default to the current
directory. Directory scans skip hidden entries, `target`, and symlinks. Use `-`
alone to read stdin. `--config settings.json` reads the same JSON settings as
the server, including `diagnostics` and `formatting`; invalid values fail the
command. No configuration file is loaded automatically.

Exit status is `0` on success, `1` for lint errors (also warnings with
`--deny-warnings`) or formatting differences with `--check`, and `2` for usage,
configuration, or I/O errors. Lint hints and information are printed but do not
fail the command. Diagnostic columns use one-based UTF-16 positions.

For a local build, use `cargo run --bin m2-lint -- .` or
`cargo run --bin m2-fmt -- --check .`; `cargo run` still starts the server.

## Connect your editor

Neovim 0.11+:

```lua
vim.filetype.add({ extension = { m2 = 'macaulay2' } })

vim.lsp.config('m2_ls', {
  cmd = { 'm2-ls' },
  filetypes = { 'macaulay2' },
  root_markers = { '.git' },
})

vim.lsp.enable('m2_ls')
```

Any other LSP client: run `m2-ls` over stdio for `.m2` files and give it a
project root, which is the scope of workspace search and cross-file rename.
Navigation also enters the Macaulay2 library source of the loaded packages when
it can find them; point `sources.paths` at an installation it does not find.
Packages you write are read from source: importing `P` whose `P.m2` sits next
to the importing file makes only the names `P` exports visible, and the code of
each `TEST` block is analyzed like the session `check` runs it in.

## Configuration

Every setting is optional and applies without a restart. Send them under the
`m2-ls` section of your client's settings (or as `initializationOptions`):

```json
{
  "m2-ls": {
    "diagnostics": { "disabled": ["unused-binding", "T02"] },
    "formatting": { "indent": 4, "hardLineWidth": 100 },
    "inlayHints": { "expressionTypes": true }
  }
}
```

| Setting | Default | Effect |
| --- | --- | --- |
| `diagnostics.enabled` | `true` | Shows or hides all diagnostics. |
| `diagnostics.disabled` | `[]` | Hides rules by name or code, e.g. `unused-binding` or `T02`. |
| `diagnostics.warningsAsHint`, `diagnostics.warningsAsInfo` | `[]` | Shows these rules' warnings as hints or information instead. |
| `formatting.indent`, `formatting.useTabs` | See source | Indentation used by the formatter. |
| `formatting.softLineWidth`, `formatting.hardLineWidth` | See source | Preferred and forced wrapping widths; `0` disables. |
| `formatting.controlFlowLayout` | See source | `compact`, `multiline`, or `multilineCompactElse`. |
| `formatting.compactFactorOperators` | See source | `2*x` instead of `2 * x`. |
| `formatting.breakAfterSemicolon` | See source | Starts the next statement on a new line. |
| `inlayHints.expressionTypes` | `false` | Adds argument, subexpression, and call-result types. |
| `inlayHints.allKnownTypes` | `false` | Also shows types that are obvious from literals. |
| `semanticTokens.standardOnly` | `false` | Sends only LSP-standard token types and modifiers, for themes that style nothing else: `command` as `function`, `builtin` as `defaultLibrary`, and no `option`, `file`, or `unresolved`. |
| `sources.enabled` | `true` | Lets navigation enter the Macaulay2 library source. |
| `sources.paths` | `[]` | Extra places to find it: an `M2` executable, an installation prefix (a build tree's `usr-dist` included), or a directory to add to Macaulay2's `path`. |

Formatting defaults are defined in [`FormattingSettings::default()`](src/settings.rs);
omitted settings use those values.

An invalid value only affects its own setting, which keeps its default; the
server reports what it ignored in a warning message.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

The server source code is licensed under [MIT](LICENSE-MIT).

The builtin catalog `src/data/m2-index.jsonl` is derived from the Macaulay2
documentation and is distributed under the GNU General Public License, version 2
or version 3 ([GPL-2.0](LICENSE-GPL-2.0), [GPL-3.0](LICENSE-GPL-3.0)), following
Macaulay2's own terms. Documentation of individual Macaulay2 packages remains
under the terms chosen by their authors. Because the catalog is embedded in the
`m2-ls` executable, distributed binaries are also subject to the GPL.
