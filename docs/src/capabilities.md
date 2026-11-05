# LSP capability reference

Use this reference when an editor exposes a protocol request by name, or when
you are checking whether it can display a particular feature. Each entry links
to the user guide for examples and behavior. A server capability still needs
client support: advertising inline values, for example, does not make every
editor request them.

## Reading code

`textDocument/semanticTokens/full`
: Returns semantic tokens for the complete document, with a legend of token
  types and modifiers. [Semantic highlighting](reading/semantic-highlighting.md)
  explains the categories and interaction with an editor's syntax highlighter.
  Range requests and semantic-token delta updates are not advertised.

`textDocument/documentHighlight`
: Returns related ranges in the current document, with read/write distinctions
  where applicable. [Document highlights](reading/document-highlight.md) also
  covers control-flow keywords and function or loop exits.

`textDocument/hover`
: Returns the selected name's code, type, signatures, and available documentation.
  [Hover](reading/hover.md) explains dispatch-aware signatures and source order.

`textDocument/signatureHelp`
: Returns callable signatures and the active argument. `(` and `,` trigger
  requests; `,` also retriggers an existing popup. See
  [signature help](reading/signature-help.md).

`textDocument/inlayHint`
: Returns type and parameter labels within the requested range. Placement and
  hide filters are described in [inlay hints](reading/inlay-hints.md).

`textDocument/inlineValue`
: Returns statically resolved ring-generator values in the requested range.
  This is separate from inlay type hints and does not evaluate arbitrary
  expressions. See [ring inline values](reading/rings.md#inline-values).

`textDocument/foldingRange`
: Returns structural ranges an editor may collapse. See [folding](reading/folding.md)
  for blocks, comments, and strings.

## Navigation and search

`textDocument/declaration`
: Finds the first binding introducing a name.

`textDocument/definition`
: Finds assignments defining a name, or opens a resolved literal file load.

`textDocument/implementation`
: Finds method installations, narrowed to applicable domains at a call site
  when argument types are known.

`textDocument/typeDefinition`
: Finds the definition of the selected value's inferred type. These four jumps
  are compared in [go to definition and friends](navigating/go-to.md).

`textDocument/references`
: Finds scope-aware references, optionally including the declaration. Local
  bindings stay within their document; globals use the indexed workspace.
  See [references](navigating/references.md).

`textDocument/documentSymbol`
: Returns a nested outline of definitions in the current file. See
  [document symbols](navigating/document-symbols.md).

`workspace/symbol`
: Searches indexed source declarations by a case-insensitive substring. See
  [workspace symbols](navigating/workspace-symbols.md) for search scope.

`textDocument/prepareTypeHierarchy`
: Selects a type for inheritance browsing; on an ordinary value it selects
  that value's runtime class.

`typeHierarchy/supertypes`, `typeHierarchy/subtypes`
: Return direct inheritance parents and children. Expand repeatedly to walk
  further. [Type hierarchy](navigating/type-hierarchy.md) explains source and
  catalog types, aliases, metaclasses, and missing locations.

`textDocument/documentLink`, `documentLink/resolve`
: Collect links first, then resolve the requested target when the client needs
  it. Supported targets include documentation names, packages, and literal
  file loads. See [document links](navigating/document-links.md).

## Editing and diagnostics

`textDocument/completion`
: Returns context-sensitive package, path, type, option, and narrow-prefix name
  completions. See [completion](writing/completion.md) for triggers and limits.

`textDocument/publishDiagnostics`
: The server publishes diagnostics as documents change. This is a notification,
  rather than a request to run checks. The [diagnostic catalog](writing/diagnostics.md#catalog)
  lists every code, severity, and meaning; the same checks power `m2-lint`.

`textDocument/codeAction`
: Offers `quickfix`, `refactor.rewrite`, and `source.fixAll` actions. See
  [code actions](writing/code-actions.md) for concrete rewrites and the
  distinction between fixes and explicit suppression actions.

`textDocument/formatting`
: Returns document-wide formatting edits using the configured layout. Range
  formatting is not provided. See [formatting](writing/formatting.md).

`textDocument/prepareRename`, `textDocument/rename`
: Checks that a user binding can be renamed, then returns edits for its
  declaration and references. See [rename](navigating/rename.md).

`workspace/willRenameFiles`
: Prepares load-string edits before editor file or directory moves. File
  operation notifications and watched-file changes keep the index current.
  See [workspace files](navigating/workspace-files.md#moving).

`workspace/didCreateFiles`, `workspace/didDeleteFiles`, `workspace/didRenameFiles`
: Notify the server after editor file operations so loaded dependencies and
  affected documents can be refreshed. These notifications update analysis;
  `willRenameFiles` supplies the edits before a move.

## Workspace lifecycle and explicit commands

Document open, incremental change, save, and close notifications
: Keep the server's view synchronized with editor buffers. Unsaved open buffers
  take precedence over disk contents during workspace analysis.

Workspace folders and file watching
: Track workspace membership and source changes on disk. They do not imply
  every source file under a workspace root is indexed; discovery follows the
  dependencies described in [workspace files](navigating/workspace-files.md).

`workspace/executeCommand`
: Exposes `m2-ls.executeFile`, `m2-ls.executeCell`, and `m2-ls.runTests` for
  [explicit M2 execution](beyond/runtime-commands.md), plus
  `m2-ls.printHierarchies` for [object inspection](navigating/object-hierarchies.md).
  The inspection command needs no arguments; runtime commands require the
  argument object documented on their page.

Configuration
: Initialization settings and subsequent configuration updates control analysis
  and presentation. See the [configuration reference](getting-started/configuration.md)
  for accepted values, defaults, and which changes take effect immediately.
