# Configuration

Settings are grouped by what they control: diagnostic publication, formatting,
hint contents and placement, highlighting, installation discovery, source
lookup, initialization, workspace indexing, and operational logging. Every field is optional; an omitted field takes
its default.

## Complete default configuration { #defaults }

The same groups work in TOML files and in the JSON your editor sends. This is
the server's own defaults file, compiled in as the bottom configuration layer;
a key your configuration leaves out takes the value written here:

```toml
{{#include ../../../src/settings/defaults.toml}}
```

## Delivery, scope, and updates { #delivery }

The editor sends settings under `m2-ls`, as `initializationOptions` at startup
or `workspace/didChangeConfiguration` later. A bare settings object is also
accepted. The legacy `macaulay2` section wrapper is recognized when it contains
server settings; the new `macaulay2` group contains installation paths.

For example, in Neovim:

```lua
vim.lsp.config('m2_ls', {
  cmd = { 'm2-ls' },
  filetypes = { 'macaulay2' },
  root_markers = { 'M2LS.toml', '.git' },
  settings = {
    ['m2-ls'] = {
      inlayHints = {
        argNames = { enabled = true, requireBracket = true },
        hide = { types = { 'Thing', 'Nothing', 'ZZ' } },
      },
    },
  },
})
```

Settings are merged in this order, with later values taking precedence:

1. The [default configuration](#defaults), compiled into the server, so it
   works with no configuration file at all.
2. Global `~/.m2-ls/M2LS.toml`.
3. `M2LS.toml` in the editor's workspace root.
4. Explicit settings sent by the editor.

Tables merge recursively; arrays and individual values replace earlier values.
Omitted fields use the previous layer, then the built-in defaults. Each editor
configuration message replaces the editor layer. Project file changes reload
settings and refresh affected diagnostics, hints, and highlighting.

On macOS and Linux, the global directory is `.m2-ls` under your home directory.
On Windows, it is `%USERPROFILE%\.m2-ls`; no `HOME` environment variable is
required. If the operating system cannot supply a home directory, global settings
are skipped. Local and editor settings still work.

The editor chooses the workspace root. Configure `M2LS.toml` and `.git` as root
markers; an empty `M2LS.toml` is enough to mark a project. The server uses the
workspace folders or root URI the editor sends and does not search parent Git
repositories itself. With several workspace roots in one server instance, the
lexically first root supplies the shared project configuration; use separate
server instances when projects need different settings.

The standalone [`m2-fmt` and `m2-lint`](../beyond/command-line.md#settings)
read the global file and the nearest ancestor `M2LS.toml` of the input file
(or the current directory for stdin). `--config settings.toml` or
`--config settings.json` adds the highest precedence layer. TOML uses the same
groups as editor settings, without requiring an outer `m2-ls` table.

## Invalid values { #invalid }

Invalid fields are reported in one warning and replaced with their defaults.
Valid siblings remain in effect, and invalid list members are removed without
removing valid members. Unknown keys inside settings groups are reported;
unrelated top-level editor-extension keys are ignored.

Standalone tools use the same validation, but reject invalid configuration
instead of continuing silently. Environment variables are read from the running
process; restart it after changing its environment.

## Builtin catalog { #catalog }

`catalog.enabled` defaults to `true`. Set it to `false` to try analysis using
only source code; builtin documentation and recorded signatures and types
will be missing where source analysis cannot supply them.

`catalog.version` defaults to `"0.1.*"`, limiting automatic updates to that
release series. It accepts Cargo-style SemVer requirements, including an exact
pin such as `"=0.1.1"`.

Send these settings in `initializationOptions` and restart the server to change
them. Other settings can be updated while the server is running.

## Diagnostics { #diagnostics }

`diagnostics.enabled` (boolean, default `true`)
: Enables diagnostic rules. `false` disables their publication and quick fixes.

`diagnostics.disabled` (array of rule selectors, default `[]`)
: Disables selected rules by code or name, such as `T03` or `unused-binding`.
  See the [diagnostic catalog](../writing/diagnostics.md#catalog).

`diagnostics.errorCatchingRewrites` (boolean, default `false`)
: Also suggests rewrites that turn an error into a fallback value, reported as
  [`S32 error-catching-rewrite`](../writing/diagnostics.md#error-catching).
  `??` yields its fallback when its left side fails, so
  `if h.k =!= null then h.k else v` becomes `h.k ?? v`, which yields `v`
  where the original fails because `h` has no key `k`. Off, neither the hint
  nor its fix is offered, and code is never rewritten this way.

### Publication categories { #emit-as }

Each list in `diagnostics.emitAs` defaults to `[]`:

`error`, `warning`, `info`, `hint`
: Publish the selected rules at that severity, independently of their original
  severity. Rules not listed keep their original severity.

`fixActionOnly`
: Keep the selected rules' quick fixes available without publishing their
  diagnostics. This differs from `disabled`, which disables the rule's fixes too.

If a rule appears in several severity lists, the order is `error`, `warning`,
`info`, then `hint`. `fixActionOnly` suppresses publication regardless of a
severity assignment; `disabled` and source suppression take precedence over both.

```toml
[diagnostics.emitAs]
hint = ["unused-binding"]
fixActionOnly = ["install-codomain-missing"]
```

## Formatting { #formatting }

`formatting.indent` (`"editor"`, `"tab"`, or a number of spaces, default `"editor"`)
: One indentation level. `"editor"` follows the editor's `insertSpaces` and
  `tabSize`, and indents with tabs in standalone tools; `"tab"` always uses a
  tab. JSON's `null` means `"editor"`. The 2.7.0 key `useTabs` is still read,
  with a deprecation warning: `true` as `"tab"` with the old `indent` as
  `tabWidth`, `false` as that many spaces.

`formatting.tabWidth` (unsigned integer, unset by default)
: Columns a tab counts as when measuring line widths. Unset uses the
  editor's `tabSize`, or `4` in standalone tools.

`formatting.softWrapWidth` (unsigned integer or `null`, default `78`)
: Preferred line wrapping width. `0` or `null` disables this target.

`formatting.hardLineWidth` (unsigned integer or `null`, default `90`)
: Wrap at a legal break when this width is exceeded. `0` or `null` disables
  this limit. An enabled soft target is clamped to an enabled hard width.
  Neither width splits an indivisible token or rewrites a literal's contents.

`formatting.controlFlowLayout` (string, default `"multilineCompactElse"`)
: One of `"compact"`, `"multiline"`, `"multilineCompactElse"`, or
  `"leadingKeywords"`. See the [layout examples](../writing/formatting.md#control-flow).

`formatting.compactFactorOperators` (boolean, default `true`)
: Write factor operations as `2*x` instead of `2 * x`.

`formatting.breakAfterSemicolon` (boolean, default `false`)
: Start the next semicolon-separated expression on a new line.

`formatting.keepLineBreaks` (boolean, default `false`)
: Preserve existing line breaks and change only spacing and indentation.
  Wrapping widths, conditional expansion, and `breakAfterSemicolon` have no
  effect in this mode.

`formatting.redundantParentheses` (string, default `"action"`)
: Parentheses the code parses the same without, as in `(a*b) + c`. `"action"`
  offers the **Remove redundant parentheses in file** source action, `"format"`
  also removes them whenever the document is formatted, and `"off"` does
  neither. See [redundant parentheses](../writing/formatting.md#parentheses).

`formatting.callParentheses` (string, default `"remove"`)
: Whether redundant parentheses around a call's argument go too: `"remove"`
  turns `f(x)` into `f x`, `"keep"` leaves calls as written.

`formatting.operators.spacesAround.binary`, `formatting.operators.compact.binary` (lists of operators)
: Binary operators written `a + b` and those written `a^b`. By default
  comparisons, logic, sums, assignments, arrows and the factor operators
  `*`, `/`, `%`, `**` are spaced (the factors then compacted by
  `compactFactorOperators`); powers, subscripts, `@`, `·`, `#` and `#?` are
  compact. The complete lists are in the [default configuration](#defaults).

`formatting.operators.spacesAround.unary`, `formatting.operators.compact.unary` (lists of operators)
: Prefix and postfix operators, spaced or compacted on their operand's side:
  `<< x` is spaced by default, while signs, `#`, prefix comparisons and every
  postfix operator (`n!`, `M^*`, `f(*)`) are compact.

`formatting.brackets.spacesInside`, `formatting.brackets.compact` (lists of `"{}"`, `"()"`, `"[]"`, `"<||>"`, default `[]`)
: Brackets padded as `{ x }` and those written `{x}`. `"()"` covers
  parentheses, call arguments, parameter lists and parallel assignments.
  Empty brackets are always closed up, as `{}`.

Operators and brackets listed in neither list keep their spacing as written.
Each list given replaces its default; a list left unset is its default minus
whatever the other list names, so `compact.binary = ["+"]` alone takes `+`
out of the spaced operators and leaves the other defaults alone. An entry that
is not an operator of that arity, or not a bracket pair, is reported and
ignored. The 2.7.0 key `spaceInBraces` is still read, as `"{}"` listed under
`spacesInside` (`true`) or `compact` (`false`), with a deprecation warning.

```toml
[formatting.operators.compact]
binary = ["+", "-"]          # a+b, but `^` falls back to as-written
[formatting.operators.spacesAround]
unary = ["<<", "!"]          # n !
[formatting.brackets]
spacesInside = ["{}"]        # { 1, 2 }
compact = ["()", "[]"]       # f(x), [1, 2]
```

## Inlay hints { #inlay-hints }

Enable [inlay hints](../reading/inlay-hints.md) in the editor as well. The
settings below choose their contents and positions; the editor chooses font
and color.

### Argument names { #argument-names }

`inlayHints.argNames.enabled` (boolean, default `true`)
: Show a parameter's name directly before its argument value, followed by `:`.
  For example, `count: 3`. A name already spelled by its argument is omitted.

`inlayHints.argNames.requireBracket` (boolean, default `true`)
: Restrict names to calls with bracketed arguments, such as `f(3)`.

`inlayHints.argNames.ignoreVariadic` (boolean, default `true`)
: Omit variadic parameter-name hints.

`inlayHints.argNames.maxLength` (unsigned integer or `null`, default `15`)
: Truncate each parameter name independently, adding `…` within the limit.
  The trailing colon and the parameter's type do not count toward it. The full
  name remains in the tooltip. `0` or `null` removes the limit.

`inlayHints.argTypes` (boolean, default `false`)
: Show the parameter's declared domain type. With names enabled, the type
  precedes the name: `ZZ count: 3`. Domains are not marked with `↑`, because a
  domain already accepts its subtypes.

### Type placement { #type-placement }

`inlayHints.types.enabled` (boolean, default `true`)
: Enable type hints independently of argument-name hints.

Each key below belongs to `inlayHints.types.after`:

`call` (boolean, default `true`)
: After a call with bracketed arguments.

`endOfLine` (boolean, default `true`)
: After the last expression completed on each line.

`endOfCell` (boolean, default `true`)
: After the final value of each top-level cell.

`assignment` (boolean, default `true`)
: After the name being assigned, where its type is first known or changes.

`brackets` (array of delimiter names, default `["parentheses"]`)
: Select delimiters after which types are displayed. Accepted names are
  `"parentheses"`, `"array"`, `"list"`, `"angle bar"`, and `"holder"`.
  `"parentheses"` selects sequence literals such as `(2,2)`; `"holder"` selects
  a single held object, such as `(1)` or `((1,1))`. A holder has the value and
  type of its unique enclosed object. Selected holder hints are explicit
  requests and survive literal/trivial filtering, so redundant parentheses can
  request a type hint at a chosen point. That request takes precedence over
  assignment, return, cell, and line-end placement, so the hint stays after the
  holder. Call arguments are handled separately.

`return` (boolean, default `false`)
: After the value a function returns.

`inlayHints.everywhere` (boolean, default `false`)
: Include every expression, subject to the type-enable switch and hide filters.

### Hide filters { #hide-filters }

These keys belong to `inlayHints.hide`:

`types` (array of type names, default `["Thing", "Nothing"]`)
: Omit hints of the listed types. Replace the list to show either of these.

`calls` (array of function names, default `["toString", "net", "toList"]`)
: Omit hints for the selected calls and their arguments.

`nonExactMark` (boolean, default `false`)
: Omit only the `↑` marker; the type itself remains visible.

`literal` (boolean, default `true`)
: Omit types directly stated by literal forms: bracket-created collections,
  ordinary and raw strings, integers, reals, booleans, `null`, lambdas, and
  explicit `new` expressions. A bracket-created collection is a literal even
  when its elements are computed. `matrix {{1},{1}}` is a call, not a literal.

`trivial` (boolean, default `true`)
: Omit results whose type is apparent from their producer. Literal forms are
  recognized along with conventional constructors (`widget` for `Widget`),
  `toType` conversions, wrapper/header/self-initializing type application,
  loops, and `if` expressions. Named constructors require inferred evidence
  that their result belongs to the named type. Inner computations retain
  their hints. `matrix {{1},{1}}` and `toString ZZ` are trivial calls,
  while their calls are not literals.

`sequential` (boolean, default `true`)
: When multiple type labels appear consecutively at one source position, keep
  only the final label, even when their types differ. For example,
  `FunctionClosure MarkdownBlock` becomes `MarkdownBlock`. A closing delimiter
  between hints separates them.

`beforeSemicolon` (boolean, default `false`)
: Omit the result hint for a value discarded by a following `;`.

`operatorCalls` (boolean, default `true`)
: Omit parameter labels such as `left:` and `right:` on operator operands.
  Expression type hints, including those on operands and the operator result,
  remain available. To enable operator names, set this to `false` and
  `argNames.requireBracket` to `false`.

### Previous configuration keys { #deprecated-inlay-hints }

The previous flat hint keys are accepted for compatibility. Grouped settings
win when both are present. Use `types.after` for placement and `hide` for
filters; `argNames` is now a group rather than a boolean. `softLineWidth` is
accepted as an alias for `softWrapWidth`; older diagnostic warning-severity
lists remain accepted, while `emitAs` supports every severity.

The earlier `expressionTypes`, `allKnownTypes`, `calls`, and `unknownTypes`
keys also remain accepted with a deprecation warning. New configurations
should use the groups above.

## Semantic tokens { #semantic-tokens }

`semanticTokens.standardOnly` (boolean, default `false`)
: Emit standard LSP vocabulary: map pattern escapes to `string`, embedded
  delimiters and punctuation to `operator`, algebraic values to `abstract`,
  and documentation styles and examples to `documentation`. Other custom
  modifiers are removed. See the [complete fallback table](../reading/semantic-highlighting.md#standard-only).
  This does not disable [semantic highlighting](../reading/semantic-highlighting.md).

`semanticTokens.defaultLibrary` (array of selectors, default `["CompiledFunction"]`)
: Combine named policies and package names. A policy string is also accepted.
  An empty array selects no bindings.

| Selector | Values marked with `defaultLibrary` |
| --- | --- |
| `CompiledFunction` | Compiled-function values, including aliases. |
| `Compiled` | Compiled functions and compiled function closures. |
| `Core` | Bindings defined in Core. |
| `Prelude` | Bindings from the catalog's preloaded packages. |
| `AllPackages` | Globals from any known package. |
| `None` | No bindings. |
| Any other package name | Bindings belonging to that package. |

For example, `["CompiledFunction", "Graphs"]` selects both categories.
Package policies use the binding's origin; assigning a Core value to a local
name does not make that local a Core binding. `readonly` is independent and
records protection of a symbol, not mutability of its current value.

## Macaulay2 installation { #macaulay2 }

Paths are discovered automatically unless overridden. A leading `~` expands
to the home directory. Prefer absolute paths after expansion.

`macaulay2.executable` (path string, omitted by default)
: Select another `M2` executable instead of the first one found on `PATH`.

`macaulay2.prefix` (path string, omitted by default)
: Select an installation prefix, including a build tree's `usr-dist`.
  This takes precedence over a prefix inferred from the executable.

`macaulay2.applicationDirectory` (path string, omitted by default)
: Override the user application directory used for packages and initialization.
  Otherwise this is `~/.Macaulay2`, or
  `~/Library/Application Support/Macaulay2` on macOS.

`macaulay2.runtimeDiscovery` (boolean, default `true`)
: Permit querying the executable for its installation prefix when filesystem
  inspection is insufficient. `false` prevents such queries. Explicit paths
  and filesystem discovery still work; a usable executable is not required.

## Library sources { #sources }

`macaulay2` selects an installation; `sources` selects the source search paths.
Neither group imports a package into your document.

`sources.enabled` (boolean, default `true`)
: Enable library-source navigation and source-based fallback analysis. This
  does not disable the builtin catalog or ordinary workspace navigation.

`sources.packagePaths` (array of directory paths, default `[]`)
: Extra package directories, searched before environment-provided, user, and
  installed package directories. Entries are searched in order and duplicates
  and nonexistent directories are omitted.

`sources.corePath` (`"auto"`, a directory path, or a boolean, default `"auto"`)
: `"auto"` discovers the installation's Core sources; a path selects a Core
  source directory; `false` disables Core-source discovery. `true` is also
  accepted for automatic discovery.

`sources.primaryCodePaths` (array of directory paths, default `[]`)
: Treat these directories and their subdirectories as
  [library implementation code](inline-directives.md#primary-code), in addition
  to the selected Core sources. This still applies with `sources.enabled = false`.

The source directories also supply the fallback search path for
[`load`, `needs`, and `input`](../navigating/workspace-files.md#lookup).

### Library implementation code { #primary-code-paths }

A file uses the `primaryCode` analysis mode when it declares the directive,
lies in the selected Core source directory, or lies in `primaryCodePaths`.
Symbolic links are resolved when checking directories. These files start
without the ordinary prelude or initialization files, and may define protected
library names. Changing paths reanalyzes affected open documents.

## Initialization files { #init-file }

`initialization.files` (array of paths, default `["${applicationDirectory}/init.m2"]`)
: Files whose globals and imported packages are known before each ordinary
  document. An empty list starts without initialization, like `M2 -q`.

`${applicationDirectory}/` expands to the discovered or configured application
directory; `~` expands to the home directory. Relative paths use the first
workspace folder, or the current directory in `m2-lint`.

```toml
[initialization]
files = ["${applicationDirectory}/init.m2", "project-init.m2"]
```

The default application-directory file may be absent. An explicitly selected
file that cannot be read produces a warning. Files are read once and reloaded
when saved or changed on disk; affected sessions are restarted and diagnostics
are republished. Each selected file's definitions support navigation.

A document's [`initFile` directive](inline-directives.md#init-file) replaces
the configured list for that document; `initFile none` selects no files.
Initialization files never start with themselves. TEST blocks, documentation
examples, and `primaryCode` files use their separate session environments.

Parse errors inside initialization files are always reported for those files.
Other findings are published when a selected file belongs to the workspace;
opening the file shows all its diagnostics as for any ordinary buffer.

## Environment variables { #environment }

| Variable | Effect |
| --- | --- |
| `M2_LSP_SOURCE_PATH` | Additional package directories, separated with the platform's `PATH` separator. Searched after configured package directories and before user and installed packages. |
| `PATH` | Locates the first `M2` executable in an absolute directory entry. Relative entries are ignored. |
| `HOME` | Locates the user's Macaulay2 application directory, packages, and `init.m2`; also supplies leading-`~` expansion. On Windows, `USERPROFILE` takes precedence over `HOME`. |
| `M2_LS_LOG` | `off` disables operational logging. Otherwise, setting it enables verbose structured logging: `1`, `true`, or an empty value keeps `/tmp/m2-ls.log`; any other value is the log path. |
| `M2_LS_USAGE_LOG` | Overrides the local usage-history path; `off` or an empty value disables it. See [usage history](../beyond/command-line.md#usage-history). |
| `XDG_STATE_HOME` | Supplies the directory containing `m2-ls/usage.jsonl`; defaults to `~/.local/state` when unset. |

With `M2_LS_LOG` unset, errors and operations taking at least 100 ms are still
logged. Each log is bounded to 8 MiB with one rotated backup; for the default
path the backup is `/tmp/m2-ls.log.1`. Timing summaries are written every
30 seconds of activity and at shutdown. Changing a log path also enables
verbose logging; values such as `0` are treated as paths, not as an off switch.
Logs contain paths, timings, and error metadata rather than document contents
or full request parameters.

## Workspace indexing { #workspace }

`workspace.indexFileLimit` (integer, default `500`)
: How many `.m2` files of its workspace folder opening a document indexes,
  besides the files it loads, which are always indexed. Files nearest to the
  document come first: its own directory, then each parent's remaining files
  up to the workspace folder. The default holds Core's sources (121 files) or
  `Macaulay2Doc` (386) whole, while a folder of all 300 installed packages
  stops after the 500 nearest files instead of indexing all of them. `0`
  indexes only what the documents load. Indexing costs roughly 2 MB of memory
  per file; a walk the limit cuts short is logged at `info` as
  `repository_scan_truncated`. A change applies to documents opened
  afterwards.

`workspace.scanEntryLimit` (integer, default `100000`)
: How many directory entries, files and directories alike, one walk visits
  before it stops, however few sources it has found. It bounds the walk when
  the workspace folder is a home directory or a whole filesystem, where
  `.m2` files are too sparse for `indexFileLimit` to stop it.

```toml
[workspace]
indexFileLimit = 100   # only the 100 nearest files
scanEntryLimit = 20000
```

## Operational logging { #logging }

`logging.level` (string, default `"info"`)
: Controls operational logging: `off`, `error`, `warn`, `info`, `debug`, or
  `trace`. Errors retain failed operations; `info` adds ordinary events, slow
  operations, and timing summaries. `debug` includes fast timings; `trace`
  also records operation starts. Changes apply during the session. This
  setting does not change source diagnostics or the usage log.

The default operational log is `/tmp/m2-ls.log`. `M2_LS_LOG` can select its
path; `M2_LS_LOG=off` disables the writer regardless of settings. The legacy
verbose mode (`M2_LS_LOG=1`, `true`, or a path) also enables detailed timings
within the configured level. CLI tools accept the same `[logging]` group
through their configuration files.
