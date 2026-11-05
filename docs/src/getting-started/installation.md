# Installation and editor setup

Choose the setup for your editor, open an `.m2` file, and start using hover,
completion, diagnostics, and navigation. Analysis does not require Macaulay2
to be installed and never executes your code.

- **[VS Code](vscode.md):** install the extension, which includes the server.
- **[Neovim with `m2.nvim`](m2-nvim.md):** let the plugin set up the language
  server and syntax highlighting.
- **[Neovim without plugins](nvim.md):** install the server with Cargo and
  configure Neovim's built-in LSP client.
- **[Emacs](emacs.md):** connect Eglot to the server.
- **[Helix](helix.md):** add the server to your language configuration.
- **[Other LSP clients](other-editors.md):** start `m2-ls` over stdio.

For editors that need a separately installed server, follow
[Installing with Cargo](cargo.md). The optional builtin documentation catalog
is handled automatically.

If Macaulay2 is installed, navigation can also reach its library sources.
See [source discovery](configuration.md#sources) to add package directories,
and [initialization files](configuration.md#init-file) for your `init.m2`.
