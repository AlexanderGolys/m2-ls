# Install and connect your editor

## Install `m2-ls`

Install the released server with Cargo:

```bash
cargo install m2-ls
mkdir -p ~/.local/bin
install -m755 ~/.cargo/bin/m2-ls ~/.local/bin/m2-ls
```

To use the current checkout instead:

```bash
cargo build --release
mkdir -p ~/.local/bin
install -m755 target/release/m2-ls ~/.local/bin/m2-ls
```

The editor starts `m2-ls` itself. Macaulay2 does not have to be launched in the
background for editor assistance to work.

## Neovim 0.11 or newer

Add the following to `init.lua`, or to a Lua file loaded by it:

```lua
vim.filetype.add({
  extension = {
    m2 = 'macaulay2',
  },
})

vim.lsp.config('m2_ls', {
  cmd = { vim.fn.expand('~/.local/bin/m2-ls') },
  filetypes = { 'macaulay2' },
  root_markers = { '.git' },
  settings = {
    ['m2-ls'] = {},
  },
})

vim.lsp.enable('m2_ls')
```

Open an `.m2` file and run `:checkhealth vim.lsp`. The `m2_ls` client should be
listed as attached. If you replace the binary while Neovim is open, run
`:LspRestart m2_ls`.

Neovim exposes the features through its normal LSP commands. In particular:

- `K` opens hover information.
- `<C-x><C-o>` requests completion in Insert mode without another completion
  plugin.
- `:lua vim.lsp.buf.code_action()` shows available fixes and rewrites.
- `:lua vim.lsp.buf.format()` formats the current document.
- `:lua vim.lsp.inlay_hint.enable(true)` displays type hints.

## Other editors

Any editor with Language Server Protocol support can use `m2-ls`. Configure it
to start `m2-ls` over standard input and output for files ending in `.m2`. Also
give the client a project root: workspace search and cross-file navigation only
cover the `.m2` files below that root.

## Optional library-source jumps

Documentation and type information work from the bundled catalog. To make
navigation continue into the installed Macaulay2 library source, set
`M2_LSP_SOURCE_PATH` to one or more source roots before the editor starts:

```bash
export M2_LSP_SOURCE_PATH=/usr/share/Macaulay2:/opt/Macaulay2/packages
```

Use the normal path separator to list several roots. Without this setting,
source jumps into external packages simply return no destination; the other
language features continue to work.
