# Claude Code

[Claude Code](https://code.claude.com/docs) runs language servers from plugins.
This repository is a plugin marketplace with one plugin, `m2-ls`, which starts
`m2-ls` for `.m2` files. Install [`m2-ls` with Cargo](cargo.md) first, so the
command is on `PATH`, then add the marketplace and the plugin:

```bash
claude plugin marketplace add AlexanderGolys/m2-ls
claude plugin install m2-ls@m2-ls
```

Inside a session the same commands are `/plugin marketplace add
AlexanderGolys/m2-ls` and `/plugin install m2-ls@m2-ls`. A local checkout works
as the marketplace too: give its path instead of the repository name. Plugins
load when a session starts, so begin a new one after installing.

## What Claude gets

Claude's `LSP` tool then answers hover, definition, references, document and
workspace symbols, and call hierarchy for Macaulay2 code from `m2-ls`. The
diagnostics `m2-ls` publishes for a file Claude reads or edits come back to
Claude unasked, worded as the fix:

```text
⚠ [Line 13:12] `Ip2` is first used without a binding; assign it or introduce
  the symbolic constant with `protect Ip2` before use [S31] (unintroduced-symbol)
```

## Configuration

The plugin sends no settings, so the server reads its
[configuration files](configuration.md#delivery): the global
`~/.m2-ls/M2LS.toml`, then `M2LS.toml` in the workspace root.
