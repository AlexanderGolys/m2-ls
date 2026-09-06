# Configuration

Every setting is optional. The defaults favor useful diagnostics, conventional
formatting, and a restrained amount of type information.

The following is a complete generic configuration object:

```json
{
  "m2-ls": {
    "diagnostics": {
      "enabled": true,
      "disabled": []
    },
    "formatting": {
      "indentWidth": null,
      "useTabs": null,
      "softLineWidth": 100,
      "hardLineWidth": 100,
      "controlFlowLayout": "multilineCompactElse",
      "compactFactorOperators": false,
      "breakAfterSemicolon": true
    },
    "inlayHints": {
      "expressionTypes": false,
      "allKnownTypes": false
    }
  }
}
```

Changes can be applied while the server is running if the editor sends updated
workspace settings.

## Diagnostics

| Setting | Default | What it changes |
| --- | --- | --- |
| `diagnostics.enabled` | `true` | Shows or hides all diagnostics. |
| `diagnostics.disabled` | `[]` | Hides selected rules by name or stable code. |

For example, this keeps diagnostics enabled but hides unused bindings and
condition-type warnings:

```json
{
  "m2-ls": {
    "diagnostics": {
      "disabled": ["unused-binding", "T02"]
    }
  }
}
```

## Formatting

| Setting | Default | What it changes |
| --- | --- | --- |
| `formatting.indentWidth` | editor choice | Overrides the editor's requested indentation width. |
| `formatting.useTabs` | editor choice | Chooses tabs or spaces. |
| `formatting.softLineWidth` | `100` | Preferred point for wrapping; `0` or `null` disables it. |
| `formatting.hardLineWidth` | `100` | Width that forces wrapping where possible; `0` or `null` disables it. |
| `formatting.controlFlowLayout` | `multilineCompactElse` | Chooses `compact`, `multiline`, or `multilineCompactElse`. |
| `formatting.compactFactorOperators` | `false` | Chooses `2*x` instead of `2 * x`. |
| `formatting.breakAfterSemicolon` | `true` | Starts the next statement on a new line. |

The older `formatting.maxLineWidth` setting remains accepted and sets both line
widths at once. Prefer the separate soft and hard settings in new
configurations.

## Inlay hints

| Setting | Default | What it changes |
| --- | --- | --- |
| `inlayHints.expressionTypes` | `false` | Adds argument, subexpression, and call-result types. |
| `inlayHints.allKnownTypes` | `false` | Also shows obvious literal and collection types, and enables expression types. |

The editor must enable inlay hints before these settings are visible. The
meaning of the displayed notation is described in
[Hover, signatures, and type hints](features/insight.md#type-hints).
