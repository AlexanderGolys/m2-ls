# Installing with Cargo

Cargo installs `m2-ls` for editors that use a server from `PATH`. It works on
Linux, macOS, and Windows; Macaulay2 itself is optional.

With Rust 1.88 or newer installed, run:

```bash
cargo install m2-ls
```

Check the installation:

```bash
m2-ls --version
```

Use `m2-ls` as the command in your editor's LSP configuration. The editor
starts and stops the server as needed.

To update, run `cargo install m2-ls` again and restart the editor's language
server. Continue with [Neovim](nvim.md), [Emacs](emacs.md), [Helix](helix.md),
or [another LSP client](other-editors.md).
