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
project root, which is the scope of workspace search and cross-file rename. To
let go-to-definition reach the installed Macaulay2 library source, set
`M2_LSP_SOURCE_PATH` to its root(s).

## Configuration

Every setting is optional and applies without a restart. Send them under the
`m2-ls` section of your client's settings (or as `initializationOptions`):

```json
{
  "m2-ls": {
    "diagnostics": { "disabled": ["unused-binding", "T02"] },
    "formatting": { "indentWidth": 4, "hardLineWidth": 100 },
    "inlayHints": { "expressionTypes": true }
  }
}
```

| Setting | Default | Effect |
| --- | --- | --- |
| `diagnostics.enabled` | `true` | Shows or hides all diagnostics. |
| `diagnostics.disabled` | `[]` | Hides rules by name or code, e.g. `unused-binding` or `T02`. |
| `formatting.indentWidth`, `formatting.useTabs` | editor's choice | Indentation used by the formatter. |
| `formatting.softLineWidth`, `formatting.hardLineWidth` | `100` | Preferred and forced wrapping widths; `0` disables. |
| `formatting.controlFlowLayout` | `multilineCompactElse` | `compact`, `multiline`, or `multilineCompactElse`. |
| `formatting.compactFactorOperators` | `false` | `2*x` instead of `2 * x`. |
| `formatting.breakAfterSemicolon` | `true` | Starts the next statement on a new line. |
| `inlayHints.expressionTypes` | `false` | Adds argument, subexpression, and call-result types. |
| `inlayHints.allKnownTypes` | `false` | Also shows types that are obvious from literals. |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[GPL-3.0](LICENSE)
