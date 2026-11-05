# VS Code extension

Install [Macaulay2 LSP](https://marketplace.visualstudio.com/items?itemName=alexandergolys.macaulay2-language-server)
from the Extensions view. The extension includes the server; a separate Cargo
installation is unnecessary.

Open an `.m2` file. VS Code starts the server automatically and identifies the
file as Macaulay2. Hover a name, request completion, or use **Go to Definition**
to explore your code. **Format Document** uses the server's formatter.

To keep type hints visible, set the following in your VS Code settings:

```json
{
  "[macaulay2]": {
    "editor.inlayHints.enabled": "on"
  }
}
```

The extension's default shows hints while holding `Ctrl+Alt`. Search for
`macaulay2` in Settings to adjust diagnostics, formatting, and type hints.
See [the extension repository](https://github.com/AlexanderGolys/vscode-macaulay2)
for its editor-specific options.
