# Neovim without plugins

Install [`m2-ls` with Cargo](cargo.md), then add this to your Neovim 0.11+
configuration. Neovim's built-in LSP client provides the editor features;
no LSP plugin is required.

Neovim does not know the `.m2` extension, so register a filetype first, then
describe and enable the server:

```lua
vim.filetype.add({
  extension = { m2 = 'macaulay2' },
})

vim.lsp.config('m2_ls', {
  cmd = { 'm2-ls' },
  filetypes = { 'macaulay2' },
  root_markers = { '.git' },
  settings = {
    ['m2-ls'] = {
      inlayHints = { types = { after = { call = true } } },
    },
  },
})

vim.lsp.enable('m2_ls')
```

Open an `.m2` file and run `:checkhealth vim.lsp`; `m2_ls` should be listed as
attached. After replacing the binary, `:LspRestart m2_ls` picks up the new one.

Everything the server offers is reached through Neovim's standard LSP
mappings:

`K`
: [Hover](../reading/hover.md).

`<C-s>` in Insert mode
: [Signature help](../reading/signature-help.md).

`<C-x><C-o>` in Insert mode
: [Completion](../writing/completion.md), without any completion plugin.

`grn`, `grr`, `gri`, `gra`
: [Rename](../navigating/rename.md), [references](../navigating/references.md),
  [implementation](../navigating/go-to.md), [code actions](../writing/code-actions.md).

`gO`
: [Document symbols](../navigating/document-symbols.md).

`<C-]>` (tag jump)
: [Go to definition](../navigating/go-to.md).

`:lua vim.lsp.buf.format()`
: [Formatting](../writing/formatting.md).

`:lua vim.lsp.inlay_hint.enable(true)`
: [Inlay type hints](../reading/inlay-hints.md); they are off in Neovim until enabled.

Semantic highlighting is on by default. The groups Neovim derives from the
token names are listed in
[Semantic highlighting](../reading/semantic-highlighting.md#neovim).
