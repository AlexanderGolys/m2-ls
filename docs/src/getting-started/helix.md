# Helix

Install [`m2-ls` with Cargo](cargo.md), then add the language and server to
Helix's `languages.toml`:

```toml
[[language]]
name = "macaulay2"
scope = "source.macaulay2"
file-types = ["m2"]
roots = [".git"]
comment-tokens = "--"
language-servers = ["m2-ls"]

[language-server.m2-ls]
command = "m2-ls"
```

Open an `.m2` file. Use `K` for hover, `gd` for definitions, and `:format` for
formatting. Run `hx --health macaulay2` to inspect language-server detection.

Helix passes the server's `config` table as initialization options. For example,
to change indentation:

```toml
[language-server.m2-ls.config.formatting]
indent = 4
useTabs = false
```

See [Helix's language configuration documentation](https://docs.helix-editor.com/languages.html)
for further editor options, and [server configuration](configuration.md) for
settings accepted in `config`.
