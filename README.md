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
  touching an unrelated one that shares its name.
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

```sh
cargo install m2-ls
```

Requires Rust 1.88 or newer. Macaulay2 itself is optional: the builtin
catalog ships with the server.

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

## Connect your editor

Neovim 0.11+:

```lua
vim.filetype.add({ extension = { m2 = 'macaulay2' } })

vim.lsp.config('m2_ls', {
  cmd = { 'm2-ls' },
  filetypes = { 'macaulay2' },
  root_markers = { '.git' },
})

vim.lsp.enable('m2_ls')
```

Any other LSP client: run `m2-ls` over stdio for `.m2` files and give it a
project root, which is the scope of workspace search and cross-file rename.
Navigation also enters the Macaulay2 library source of the loaded packages when
it can find them; point `sources.paths` at an installation it does not find.
Packages you write are read from source: importing `P` whose `P.m2` sits next
to the importing file makes only the names `P` exports visible, and the code of
each `TEST` block is analyzed like the session `check` runs it in.

## Configuration

Every setting is optional and applies without a restart. Send them under the
`m2-ls` section of your client's settings (or as `initializationOptions`):

```json
{
  "m2-ls": {
    "diagnostics": { "disabled": ["unused-binding", "T02"] },
    "formatting": { "indent": 4, "hardLineWidth": 100 },
    "inlayHints": { "afterCall": true, "hideTypes": ["ZZ"] }
  }
}
```

| Setting | Default | Effect |
| --- | --- | --- |
| `diagnostics.enabled` | `true` | Shows or hides all diagnostics. |
| `diagnostics.disabled` | `[]` | Hides rules by name or code, e.g. `unused-binding` or `T02`. |
| `diagnostics.warningsAsHint`, `diagnostics.warningsAsInfo` | `[]` | Shows these rules' warnings as hints or information instead. |
| `formatting.indent`, `formatting.useTabs` | See source | Indentation used by the formatter. |
| `formatting.softLineWidth`, `formatting.hardLineWidth` | See source | Preferred and forced wrapping widths; `0` disables. |
| `formatting.controlFlowLayout` | See source | `compact`, `multiline`, or `multilineCompactElse`. |
| `formatting.compactFactorOperators` | See source | `2*x` instead of `2 * x`. |
| `formatting.breakAfterSemicolon` | See source | Starts the next statement on a new line. |
| `inlayHints.afterCall` | `false` | After each call with bracketed arguments, `f(x)`, `f()` or `f(x, y)` (not `f x`): the type it returns. |
| `inlayHints.afterLineEnd` | `true` | At each line end: the type of the last expression complete on it, so `2+` shows `2` and the next line `2` shows `2+2`. |
| `inlayHints.afterCell` | `true` | After each top-level value, as Macaulay2's `oN : T` line. |
| `inlayHints.duringAssignment` | `true` | After a name being assigned, where its type is first known or changes. |
| `inlayHints.argNames` | `false` | Before each argument, the name of the parameter it is passed to, as `f(x: 2)`. |
| `inlayHints.argTypes` | `false` | Before each argument, the type its parameter is declared for, as `f(Number 2)`; with `argNames`, `f(x: Number 2)`. A domain takes its subtypes, so it is never marked with `↑`. |
| `inlayHints.afterBrackets` | `false` | After every closing bracket; one closing call arguments shows the call's result. |
| `inlayHints.afterReturn` | `true` | After the value a function returns. |
| `inlayHints.everywhere` | `false` | After every expression. |
| `inlayHints.showLiteral` | `false` | Also shows types a literal states, as in `x = 3`. |
| `inlayHints.showUpperClosure` | `true` | Marks a type that also stands for its subtypes with `↑`, as `↑Ring`. |
| `inlayHints.showThing` | `false` | Also shows hints that say nothing, of values only known to be a `Thing`. |
| `inlayHints.showNothing` | `false` | Also shows hints of values known to be `null`. |
| `inlayHints.showRepeated` | `false` | Also shows types a nearby hint already shows, such as a binding's unchanged type. |
| `inlayHints.hideTypes` | `[]` | Types never shown, e.g. `["ZZ", "String"]`. |
| `inlayHints.hideAfterSemicolon` | `false` | Hides the hints of values a `;` discards. |
| `inlayHints.maxLabelLength` | `40` | Cuts longer labels, with the whole type in the tooltip; `0` disables. |
| `inlayHints.excludedCalls` | `[]` | Functions whose calls and arguments get no hints, e.g. `["print"]`. |
| `semanticTokens.standardOnly` | `false` | Drops the custom semantic token modifiers. Token types are always LSP-standard. |
| `semanticTokens.defaultLibrary` | `"CompiledFunction"` | Selects which bindings carry `defaultLibrary`; accepts a policy name or an array of package names. |
| `sources.enabled` | `true` | Lets navigation enter the Macaulay2 library source. |
| `sources.paths` | `[]` | Extra places to find it: an `M2` executable, an installation prefix (a build tree's `usr-dist` included), or a directory to add to Macaulay2's `path`. |

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

The `option` modifier applies to keys only. Values retain their ordinary
classification; for example, `Strategy => Linear` marks `Strategy` as an
option key and leaves `Linear` as a symbol. Names without a known binding use
plain variable tokens, without a separate unresolved modifier.

Formatting defaults are defined in [`FormattingSettings::default()`](src/settings.rs);
omitted settings use those values.

An invalid value only affects its own setting, which keeps its default; the
server reports what it ignored in a warning message.

## Troubleshooting

The server writes structured JSON lines to `/tmp/m2-ls.log`. Request errors
include the method, request ID, document URI, and error code. Operations taking
at least 100 ms are recorded automatically, with timing summaries every 30
seconds of activity and at shutdown. Summaries include count, total, mean, and
maximum wall time per operation; nested operations overlap.

Set `M2_LS_LOG=1` before starting the editor to record every operation's start
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

The builtin catalog `src/data/m2-index.jsonl` is derived from the Macaulay2
documentation and is distributed under the GNU General Public License, version 2
or version 3 ([GPL-2.0](LICENSE-GPL-2.0), [GPL-3.0](LICENSE-GPL-3.0)), following
Macaulay2's own terms. Documentation of individual Macaulay2 packages remains
under the terms chosen by their authors. Because the catalog is embedded in the
`m2-ls` executable, distributed binaries are also subject to the GPL.
