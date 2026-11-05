# Features overview

`m2-ls` helps you read unfamiliar packages, navigate a project, and catch mistakes
before a long computation runs. Style diagnostics suggest method dispatch,
named record fields, and shared typical values where the source supports them.
Each capability below has its own chapter with
examples, settings, and limits. Your editor decides how to expose the requests.


## Implemented capabilities

[Semantic highlighting](../reading/semantic-highlighting.md)
: Distinguishes types, methods, functions, parameters, and ring variables by
  what each name resolves to, making unfamiliar Macaulay2 code easier to read.

[Document highlights](../reading/document-highlight.md)
: Shows other references of symbol under cursor, control-flow structure,
an transfer control of function under cursor.

[Hover](../reading/hover.md)
: Shows the hover floating window combining official and local and  inpline
object documentation together with compile-time contextual information and
type analysis results.

[Signature help](../reading/signature-help.md)
: Lists callable signatures and hints parameter names while you write arguments
in a function call. Your own method installations contribute signatures too,
type- and documentation-aware.

[Inlay hints](../reading/inlay-hints.md)
: Hints the predicted types (or bounds on type ranges in certain cocompletion) of
objects in your code, argument names, types or the restrictions on method
signatures possible to dispatch in current context.


[Rings and inline values](../reading/rings.md)
: For some algebraic structures `m2-ls` follows the semantics of symbols used
for indexing and refering to basis, generators, index tables and track some by
tracking some compile-time computable characteristic, like a bound on dimension.

[Folding](../reading/folding.md)
: Collapses multiline expressions, function bodies, comments, and strings using
  their source structure.


[Declaration, definition, implementation, and type definition](../navigating/go-to.md)
: Jump respectively to a name's first binding, its assignments, applicable method
  installations, or the definition of its inferred type. Available library sources
  extend these jumps into Core and imported packages.

[References](../navigating/references.md)
: Finds uses of the selected binding, respecting local scopes and shadowing.
  Globals are searched across the indexed workspace.

[Rename](../navigating/rename.md)
: Renames a user binding and its references together, including documentation
  links. A parameter rename leaves unrelated globals alone.

[Document symbols](../navigating/document-symbols.md)
: Builds a nested outline of bindings, functions, method installations, and ring
  generator families in the current file.

[Workspace symbols](../navigating/workspace-symbols.md)
: Searches names across indexed project files with a case-insensitive substring.

[Type hierarchy](../navigating/type-hierarchy.md)
: Browses direct parents and children of source and library types. Inheritance
  and metaclass relations remain distinct.

[Object hierarchies](../navigating/object-hierarchies.md)
: Reports the workspace's retained objects, namespaces, package dependencies,
  classes, and inheritance relations through an explicit inspection command.

[Document links](../navigating/document-links.md)
: Opens documentation references, package sources, and literal file loads.

[Workspace files](../navigating/workspace-files.md)
: Follows literal `load`, `needs`, and `input` dependencies, completes paths,
  watches changes on disk, and updates load strings during editor file moves.

[Completion](../writing/completion.md)
: Suggests package names, paths, types after `new`, option keys and values, and
  ordinary names when the prefix narrows the candidate list sufficiently.

[Diagnostics](../writing/diagnostics.md)
: Reports syntax, runtime, type, style, and package-documentation problems as you
  edit, including floating-point keys, option names, constructor result types,
  and declaration naming. Function parameters are exempt from naming checks.
  Every rule has a stable code and configurable suppression.

[Code actions](../writing/code-actions.md)
: Applies diagnostic fixes and expression rewrites. Suppression and library-code
  directives are available as explicit actions; automatic fix-all omits them.
  Simplifications include early-return null guards, mapped compositions, and
  redundant strict Boolean comparisons, forwarding callbacks, and partial
  applications. Each rewrite has its own configurable lint code.

[Formatting](../writing/formatting.md)
: Formats the whole document with configurable spacing, wrapping, indentation,
  and control-flow layout. The editor and `m2-fmt` share the formatter.

[Documentation comments](../beyond/documentation-comments.md)
: Brings nearby Markdown documentation into local hover and resolves `[[name]]`
  links for navigation and rename.

[Running code and tests](../beyond/runtime-commands.md)
: Runs a saved file, caller-supplied cell text, or package tests in a fresh M2
  process when explicitly requested.

[Command-line tools](../beyond/command-line.md)
: Provides formatting and linting outside the editor, including check modes for
  automated validation.

## Choose the next step

Use [configuration](../getting-started/configuration.md) for defaults and accepted
values, and [inline directives](../getting-started/inline-directives.md) for
per-file control. The [protocol reference](../capabilities.md) maps each editor
feature to its LSP request. Read [known limitations](../limitations.md) for the
boundaries of static analysis and workspace discovery.
