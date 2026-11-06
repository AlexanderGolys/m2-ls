# m2-ls

[![crates.io](https://img.shields.io/crates/v/m2-ls.svg)](https://crates.io/crates/m2-ls)
[![GitHub release](https://img.shields.io/github/v/release/AlexanderGolys/m2-ls?sort=semver)](https://github.com/AlexanderGolys/m2-ls/releases/latest)
[![MSRV](https://img.shields.io/badge/MSRV-1.88-blue.svg)](https://www.rust-lang.org/)

**A language server for [Macaulay2](https://macaulay2.com/).** Hover docs,
go-to-definition, rename, type hints, formatting, and diagnostics that know
Macaulay2 — in any editor that speaks LSP, without running your code.

```m2
R = QQ[x0, x1, y]
--     ^^ Ring variable `x0` is a multi-character symbol, not an indexed
--        variable; use `x_0` so the generators form one indexed family

sizeLabel = L -> (
    n := #L;
    if n then "nonempty" else "empty"
    -- ^ if condition must have type `Boolean`, but this expression has type `ZZ`
    )
```

## Why use it

- **Explore without leaving the file.** Hover any builtin for its documentation,
  signatures, options, and result type; signature help follows you through the
  call.
- **Move around by meaning.** Jump to definitions in your workspace or in the
  installed Macaulay2 library, find references, and rename a binding without
  touching an unrelated one that shares its name. `load` paths open on a
  click, complete as you type, and follow files you move.
- **Catch Macaulay2-specific mistakes early.** Non-Boolean conditions, `=` where a
  method installation needs `:=`, installs on non-flexible operators, mismatched
  parallel assignments, and more, several with a quick fix.
- **See what the code is.** Semantic highlighting tells types, functions, and
  symbols apart; inlay hints show inferred types.
- **Keep it tidy.** A configurable whole-document formatter, folding, and
  document and workspace symbols.
- **Document code in comments.** `--` comments directly above a definition
  become its hover docs, and `[[name]]` inside them links to other definitions.

When a result depends on what the code does at runtime, `m2-ls` shows less
rather than guess.

## Install

Install from crates.io:

```sh
cargo install m2-ls
```

From a checkout, use `cargo install --path . --locked`. Cargo supports
installation options such as `--root` on Linux, macOS, and Windows.
Requires Rust 1.88 or newer.

Macaulay2 is optional. The server automatically downloads and caches a small
optional catalog from [M2Book](https://github.com/AlexanderGolys/M2Book/releases)
for builtin documentation and type information. No catalog setup is needed.
If it is unavailable, source analysis still works. Later starts retry
automatically, and an existing cache works offline.

## Connect your editor

Configure your editor to run `m2-ls` from `PATH`.

Neovim 0.11+:

```lua
vim.filetype.add({ extension = { m2 = 'macaulay2' } })

vim.lsp.config('m2_ls', {
  cmd = { 'm2-ls' },
  filetypes = { 'macaulay2' },
  root_markers = { 'M2LS.toml', '.git' },
})

vim.lsp.enable('m2_ls')
```

Any other LSP client: run `m2-ls` over stdio for `.m2` files and give it a
project root, which is the scope of workspace search and cross-file rename.
Navigation also enters the Macaulay2 library source of the loaded packages when
it can find them; use `macaulay2.prefix` for another installation and
`sources.packagePaths` for additional package directories.
Packages you write are read from source: importing `P` whose `P.m2` sits next
to the importing file makes only the names `P` exports visible, and the code of
each `TEST` block is analyzed like the session `check` runs it in.
Workspace indexing discovers `.m2` files below the editor's workspace roots
and builds their import graph. Package fragments inherit earlier package
bindings through their loaders; standalone files need explicit dependencies.
Unbound names used as callable, type, Boolean, or indexed values are errors.
File locals declared with `:=` remain private to their file.
Installed packages use the precomputed catalog;
opening their source does not index their dependency trees. When the catalog
is unavailable, imported installed packages are analyzed from source as well.
Core source opens
in `primaryCode` mode, without automatic imports of prelude packages.

## Configuration

Settings merge from `~/.m2-ls/M2LS.toml`, then the workspace root's `M2LS.toml`,
then explicit editor settings. On Windows, the global directory lives under
`%USERPROFILE%`. An empty project file can serve as an editor root marker,
alongside `.git`. Standalone tools use the nearest project configuration and
accept `--config` for explicit overrides.

Settings are grouped by what they control. The editor sends JSON under its
`m2-ls` settings section or as `initializationOptions`:

```json
{
  "m2-ls": {
    "diagnostics": {
      "emitAs": { "hint": ["unused-binding"] }
    },
    "formatting": { "softWrapWidth": 88 },
    "inlayHints": {
      "argNames": { "enabled": true, "requireBracket": true },
      "types": { "after": { "call": true } },
      "hide": { "types": ["Thing", "Nothing", "ZZ"] }
    }
  }
}
```

A [complete TOML example](tests/fixtures/configuration.toml) lists the defaults.

| Group | Purpose and defaults |
| --- | --- |
| `catalog` | `enabled = true`; set to `false` to skip catalog reads, downloads, and updates. `version = "0.1.*"` bounds updates to compatible M2Book releases. The server reads this group from `initializationOptions`; restart to change it. |
| `diagnostics` | Enabled by default; `disabled` lists rule names or codes. |
| `diagnostics.emitAs` | `error`, `warning`, `info`, `hint`, and `fixActionOnly` select publication categories; all lists default to empty. `fixActionOnly` retains quick fixes without publishing diagnostics. |
| `formatting` | Four-column indentation with tabs, soft wrapping at `78`, hard width `90`. `keepLineBreaks = true` preserves existing line breaks. |
| `inlayHints.argNames` | Enabled; requires bracketed arguments, ignores variadic parameter names, and limits names to `15` characters. |
| `inlayHints.types` | Enables type hints independently of argument names. `after` selects calls, line ends, cell ends, assignments, delimiters, and returns. Return hints default to off. |
| `inlayHints.hide` | Filters types, calls, literal and trivial results, adjacent type labels, and operator parameter labels. `nonExactMark` hides only the `↑` marker. |
| `semanticTokens` | `standardOnly` defaults to `false`; `defaultLibrary` defaults to `["CompiledFunction"]` and combines policy names and package names. |
| `macaulay2` | Optional `executable`, `prefix`, and `applicationDirectory` overrides. `runtimeDiscovery` defaults to `true`; disabling it prevents executable queries. |
| `sources` | `enabled = true`, extra `packagePaths = []`, `corePath = "auto"`, and `primaryCodePaths = []`. |
| `initialization` | `files = ["${applicationDirectory}/init.m2"]`; use `[]` for no initialization files. Relative paths are resolved against the first workspace folder. |

To try source analysis without the catalog, pass
`{"catalog": {"enabled": false}}` in the editor's `initializationOptions`.
This also ignores `M2_LS_CATALOG` and existing downloaded catalogs.

`catalog.version` accepts Cargo-style SemVer requirements, such as `0.1.*`,
`=0.1.1`, or `>=0.1.1, <0.2`. The highest published matching release is selected;
prereleases need an explicit prerelease comparator. Each bound has its own cache.
The default `0.1.*` excludes `0.2.0` and later breaking releases. A local catalog
selected by `M2_LS_CATALOG` bypasses release selection. The legacy environment
variable `M2_LS_CATALOG_VERSION` controls the standalone download command;
server and linter configuration uses `catalog.version`.

Without the catalog, builtin hover documentation, signatures, completion entries,
and recorded type and option information are unavailable unless the installed
sources provide them. Local definitions, navigation, syntax diagnostics,
formatting, and inference from source remain available. Imported installed
packages are analyzed from source, which can take longer than catalog lookups.

Except for `catalog`, changes apply to the running server. Each configuration message replaces the
whole configuration, so omitted fields return to their defaults.

Method call inference respects single-argument sequence dispatch and left
folding for binary methods. `T05` (`call-domain-mismatch`) warns when no known
method domain can accept the argument types; uncertain matches remain valid.

A comment can also silence rules in the source itself, for the whole file or
for the next line; the quick fixes of every diagnostic write these for you.
Without rules, a directive silences all of them.

```m2
-- m2-ls: disable unused-binding S04
-- m2-ls: disable-next-line ring-variable-naming
R = QQ[x0, x1]
```

Semantic tokens always use LSP-standard types. Additional distinctions are
modifiers, so themes can inherit the standard type's styling.

| Construct | Standard type | Additional modifiers |
| --- | --- | --- |
| Method functions (`det`, `source`, anything made by `method()`) | `method` | According to `defaultLibrary` policy |
| Other functions, scripted functors (`id`, `Ext`), commands and manipulators (`exit`, `endl`) | `function` | According to `defaultLibrary` policy |
| File values (`stdio`) | `variable` | |
| Ring generators | `variable` | `algebraic` |
| Access operators (`_` of `S_1`, `.`, `.?`, binary `#`, `#?`) | `operator` | `access` |
| Literal selectors (`0` of `L_0` or `L#0`) | `property` | |
| Installation domains and codomains (`ZZ`, `List` in `f ZZ := List => ...`) | `typeParameter` | |
| SimpleDoc section keywords, `document` option names, and links (`TO`, `TOH`, `TO2`) | `decorator` | |
| TeX delimiters and other hypertext constructors | `macro` | |
| Styled documentation text | `string` | `strong`, `emphasis`, `monospace`, `link`, or `math` |
| Option keys, including the `Limit` of `[f, Limit]` in documentation | `property`, or `enumMember` for a protected symbol | `option` |

Documentation calls, their tags and values, attached documentation comments,
and embedded code carry the standard `documentation` modifier. Examples,
including `CannedExample` and `EXAMPLE`, also carry `example`; `TEST` blocks
carry `test`. Embedded code keeps its normal token types. These contexts are
emitted even when the client also uses syntax highlighting.

In Neovim, each modifier has a separate highlight group, such as
`@lsp.typemod.function.readonly`, `@lsp.typemod.string.monospace`, or
`@lsp.typemod.variable.algebraic`. Multiple modifiers do not form a nested
highlight name. The standard base groups continue to apply.

Ring generators include `x` and `y` in `RR[x, y]`, or the whole `x_1` in
`QQ[x_0 .. x_2]`. The indexed table `x` itself does not carry `algebraic`.

A subscript spelling one object of the kind of one of its parts is a single
token: `1_S` is a `number`, `RR_53` a `type`. Other accesses keep their parts:
in `S_1` the ring `S` stays a `type`, the `_` is an `access` operator, and the
`1` is the generator it selects.

The runtime class decides `method` versus `function`, independently of the
call syntax or documented signatures. An installation head keeps its normal
type and adds `declaration`: the `det` in `det Matrix := ...` is still a
`method`. Symbolic heads such as `+` retain their operator type.

`semanticTokens.defaultLibrary` chooses the meaning of the standard
`defaultLibrary` modifier. Policy names are case-sensitive:

| Value | Marked bindings |
| --- | --- |
| `"CompiledFunction"` (default) | Values of class `CompiledFunction` or a subtype, including aliases in any package or local scope. |
| `"Compiled"` | Also includes `CompiledFunctionClosure` and its subtypes, such as compiled method dispatchers. |
| `"Core"` | Globals defined in `Core`, of any runtime class. |
| `"Prelude"` | Globals from the preloaded packages recorded by the library index. Extra packages loaded in a document do not extend this set. |
| `["Core", "Graphs"]` | Globals defined in the listed packages, including source packages in the workspace. |
| `"AllPackages"` | Globals defined in any visible indexed or source package. |
| `"None"` or `[]` | No bindings. |

Package policies follow the binding's defining package. Assigning a Core
function to a local variable keeps its compiled runtime class but gives the
local no Core membership. A compiled method dispatcher can have implementations
written in Macaulay2. Changes to the policy trigger a semantic-token refresh
without restarting the server.

The standard `readonly` modifier marks protected bindings known from the
library index or a package's `export` list. It describes protection against
reassignment, independently of whether the value itself is mutable. Local
aliases of protected globals and names listed by `exportMutable` remain
unmarked. Protection and compiled implementation are independent modifiers,
and both remain available under `semanticTokens.standardOnly`.
Assigning to a protected indexed symbol reports the `protected-assignment`
error (`E13`); assigning to a local that shadows it remains allowed.

For files implementing the library itself, such as Core, add the file property
`-- m2-ls: primaryCode` in a line comment. These files start without prelude
imports and allow definitions of protected symbols, without `readonly`
highlighting. Files in the discovered Core sources and in
`sources.primaryCodePaths` get this mode without the comment, and a
`protected-assignment` error offers adding it as a quick fix. Explicit package imports still take effect in source order.
Removing the property restores normal session analysis. TEST blocks and
documentation examples continue to use their separate session environments.

The `option` modifier applies to keys only. Values retain their ordinary
classification; for example, `Strategy => Linear` marks `Strategy` as an
option key and leaves `Linear` as a symbol. Names without a known binding use
plain variable tokens, without a separate unresolved modifier.

Formatting defaults are defined in [`FormattingSettings::default()`](src/settings.rs);
omitted settings use those values.

An invalid value only affects its own setting, which keeps its default; the
server reports what it ignored in a warning message.

## Troubleshooting

The LSP command `m2-ls.printHierarchies` returns the current workspace's package
and namespace tree, `classOf` relations, and `parentOf` inheritance edges, and
prints the report to the language-server output log. Invoke it through
`workspace/executeCommand` with no arguments. It includes source and library
objects, types, and packages, preserves alias identities, and records public
package dependencies separately from namespace ownership. Standard type
hierarchy requests use the same graph to navigate source and library types.

Actual diagnostic results and completed indexing counts are saved separately in
`~/.local/state/m2-ls/usage.jsonl` (`$XDG_STATE_HOME/m2-ls/usage.jsonl` when set).
The linter and server share this JSONL history; it contains findings, source
hashes and indexing summaries, without RPC traffic or test results.
`M2_LS_USAGE_LOG` overrides its path; `off` disables it. See
[Usage history](docs/src/beyond/command-line.md#usage-history) for fields and an
analysis example. Tests leave both result and operational logging disabled.

The server writes structured JSON lines to `/tmp/m2-ls.log`. Request errors
include the method, request ID, document URI, and error code. Operations taking
at least 100 ms are recorded automatically, with timing summaries every 30
seconds of activity and at shutdown. Summaries include count, total, mean, and
maximum wall time per operation; nested operations overlap.

Set `M2_LS_LOG=off` to disable operational logging, or `M2_LS_LOG=1` before starting the editor to record every operation's start
and finish, including parsing, inference, diagnostics, documentation, dependency
indexing, and queue waits. Set it to a file path to use that path instead.
Logs include paths and timing metadata, without document contents or full
request parameters. The current log and one rotated backup are bounded to
8 MiB each. Library source caches are updated on save, independently of edits
to open buffers.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

The server source code is dual licensed under [MIT](LICENSE-MIT) or
[Apache 2.0](LICENSE-APACHE), at your option.

The builtin catalog is downloaded separately and is not included in this crate
or its executables. It contains documentation and source excerpts from
Macaulay2, copyright The Macaulay2 Authors, under their
[GPL version 2 or version 3 terms](https://macaulay2.com/doc/Macaulay2/share/doc/Macaulay2/Macaulay2Doc/html/___Copyright_spand_splicense.html).
Individual packages retain their authors' copyright notices and licenses.
Downloading and displaying this material does not change its license.

### Explicit runtime commands

Invoke these through `workspace/executeCommand`, with one object in `arguments`.
They start `M2` from `PATH` in a fresh process, using the source file's directory
as the working directory. They run only when invoked; analysis never runs them.

| Command | Argument fields |
| --- | --- |
| `m2-ls.executeFile` | `uri`: local file URI; executes the saved file |
| `m2-ls.executeCell` | `uri`, `code`: selected cell source supplied by the caller |
| `m2-ls.runTests` | `uri`, `package`: loads that package from the named file and runs its tests |

Results contain `success`, `exitCode`, `stdout`, and `stderr`. Processes use normal
M2 initialization. Cells do not retain definitions between invocations. Package
installation and documentation generation are not currently implemented.


## Command-line tools

Installation also provides `m2-lint` and `m2-fmt`, using the same diagnostics
and formatter as the server:

```sh
m2-lint file.m2                 # Lint one file
m2-lint --deny-warnings file.m2 # Fail on warnings as well as errors
m2-fmt file.m2                  # Format one file in place
m2-fmt --check file.m2          # Check formatting without writing (CI)
m2-fmt --stdout file.m2         # Print the formatted file
cat file.m2 | m2-fmt -          # Format stdin to stdout
```

Each command takes exactly one file, or `-` alone to read stdin. The
`--config settings.json` option reads the same JSON settings as the server,
including `diagnostics` and `formatting`; invalid values fail the command. No
configuration file is loaded automatically.

Exit status is `0` on success, `1` for lint errors (also warnings with
`--deny-warnings`) or formatting differences with `--check`, and `2` for usage,
configuration, or I/O errors. Lint hints and information are printed but do not
fail the command. Diagnostic columns use one-based UTF-16 positions.

For a local build, use `cargo run --bin m2-lint -- file.m2` or
`cargo run --bin m2-fmt -- --check file.m2`; `cargo run` still starts the server.

The standalone tools also accept TOML configuration through `--config FILE`.
Settings files are not discovered automatically. `m2-fmt` does not need the
catalog or network access.
