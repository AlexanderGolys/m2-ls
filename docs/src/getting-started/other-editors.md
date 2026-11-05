# Any other LSP client

Install [`m2-ls` with Cargo](cargo.md), then register a language server with
these values in your editor:

| Setting | Value |
| --- | --- |
| Command | `m2-ls` |
| Arguments | None |
| Transport | Standard input and output (stdio) |
| File extension | `.m2` |
| Language identifier | `macaulay2` |

Use your project directory as the workspace root. Open an `.m2` file and try
hover or **Go to Definition** on a locally defined name.

Defaults work without further configuration. To customize them, send the
[settings](configuration.md) as `initializationOptions`. Clients can send later
changes through `workspace/didChangeConfiguration`; catalog settings are read
at initialization and require a restart.

The server does not request settings through `workspace/configuration`, so
configure clients to send their settings rather than waiting for a request.
