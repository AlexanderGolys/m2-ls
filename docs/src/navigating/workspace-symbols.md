# Workspace symbols

Workspace symbol search finds a top-level name in any file of the workspace
without knowing where it lives. Type part of the name; matching is
case-insensitive and anywhere in the name, so `norm` finds `normalize`,
`renormalize` and `NormalForm`.

The search covers the same files as [cross-file navigation](go-to.md#workspace):
open documents, the files they load, the user packages they import, and
`init.m2`. Results are sorted by name, then by file.

Library names are not part of the search; for those, [hover](../reading/hover.md)
and [go to definition](go-to.md) on a use.

In Neovim: `:lua vim.lsp.buf.workspace_symbol('norm')`.

## Choosing a search

Use workspace symbols when you know part of a definition's name but have no
use site to start from. Selecting a result opens its source location. Use the
[document outline](document-symbols.md) for nested definitions in one file,
[references](references.md) to find callers or other uses of a known binding,
and [go to implementation](go-to.md) to inspect dispatch installations.

Search results reflect the current index, including unsaved open buffers.
A file outside the dependency graph does not appear merely because it sits
under a workspace folder. Open it or load it from an indexed file to bring
its definitions into the workspace. Results identify declarations; they do
not search comments, documentation prose, or arbitrary source text.
