# Neovim with `m2.nvim`

[`m2.nvim`](https://github.com/AlexanderGolys/m2.nvim) sets up filetype detection,
syntax highlighting, and the language server together. It requires Neovim
0.11+, Git, a C compiler, Rust/Cargo (Rust 1.88+), and the `tree-sitter` CLI.

Install the CLI if needed:

```bash
cargo install --locked tree-sitter-cli
```

Add the plugin to your lazy.nvim configuration:

```lua
{
  "AlexanderGolys/m2.nvim",
  version = "*",
  lazy = false,
  opts = {},
}
```

The plugin prepares missing dependencies automatically. It uses `m2-ls` from
`PATH` when available, or installs it with Cargo. Open an `.m2` file; language
features become available when setup finishes. If you already configured
`m2-ls`, the plugin respects that setup rather than starting another client.

Use `K` for hover, `grn` for rename, and `:lua vim.lsp.buf.format()` to format
the buffer. Enable type hints with `:lua vim.lsp.inlay_hint.enable(true)`.
See [Neovim's LSP controls](nvim.md) for more commands.

The plugin also provides an optional terminal helper for evaluating code when
Macaulay2 is installed. See its [README](https://github.com/AlexanderGolys/m2.nvim)
for terminal mappings and plugin options.
