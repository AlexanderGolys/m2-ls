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
- **Document code in comments.** `--` comments above a definition become hover
  docs with `[[wiki links]]`, and `m2-ls docs` turns them into a browsable book.

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

## Learn more

The [user guide](docs/src/index.md) covers each feature in depth:

- [Setup](docs/src/setup.md)
- [Configuration](docs/src/configuration.md): diagnostics, formatting, and inlay
  hints, all changeable without a restart
- [Documentation in comments](docs/src/features/documentation.md)
- [Known limitations](docs/src/limitations.md)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[GPL-3.0](LICENSE)
