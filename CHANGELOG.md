# Changelog

## 2.6.2 - 2026-11-12

- Show computed values as inline values: exact `ZZ`/`QQ` arithmetic,
  comparisons and logic, `numgens`/`dim`/`char` of polynomial rings over
  `ZZ`, `QQ`, `ZZ/p` and `GF q`, the shapes of matrices written out and the
  generator counts of ideals. Only names assigned exactly once are followed.
- Track which ring an expression lives in through coefficient rings and
  quotients, show it inline (`… in C`), and follow `use R` rebinding the names
  of `R`'s variables in source order.
- Add E23 `matrix-shape-mismatch` and E24 `division-by-zero` errors, the S22
  `predicate-typical-value` warning, and S23 `predicate-function-closure`,
  S24 `inline-null-binding`, S25 `prefer-augmented-assignment` and S26
  `constant-condition` hints; S24 and S25 come with rewrites.
- Report as E01 the installations the interpreter never calls because it
  evaluates the operator itself for those native operands, as `ZZ + QQ := …`.
- Accept `return` inside loop bodies, `if` and `try` at any depth within a
  function; E08 no longer reports it as outside a function body.
- Limit S08 to type names and declared optional arguments, and accept any
  declared typical value for S10.
- Add the `ring`, `module`, `morphism`, `complex`, `functor`, `structure` and
  `space` kind modifiers and the `documentTag` modifier. Codomain annotations
  are `decorator`s and interpreter tables are `struct`s. Rename the `access`
  and `math` modifiers to `memberAccess` and `tex`, split `link` into `url`,
  `file`, `directory` and `wikilink`, and add `injectedCode` and
  `injectedName`. Unprotected option names are parameters.
- Leave a package's exports unprotected inside its own source.
- Load Core for library implementation files, which report syntax errors only
  and get no code actions or formatting. Hover describes the catalog object
  for names the library assigns itself, and go-to-definition falls back to the
  installed source location for objects no assignment defines.
- Stop counting quoted names as references to the binding they spell in
  highlights and renames.
- Highlight documentation keys, `TO2` link text and example option names more
  precisely, and report unresolved documentation references as D05 errors in
  any file.

## 2.6.1 - 2026-11-06

- Refuse to index a repository root holding more than 100,000 entries,
  logging the refusal instead of scanning it. `m2-lint` treated the nearest
  ancestor with a `.git` as the repository, so a `.git` in the home
  directory made it scan everything below and hang on every input.
- Report a framed source slice that parses into more than one cell as a
  parse error at the surplus cell, instead of aborting the process.

## 2.6.0 - 2026-11-06

- Discover repository source files and resolve package fragments through their
  unopened entry points and preceding includes. Keep file locals private and
  preserve distinct contexts when multiple packages include the same file.
- Add E22 `unresolved-value` errors for unbound callable, constructor, Boolean,
  and indexed-receiver names. Standalone fragments must declare or explicitly
  import their dependencies; arbitrary callers no longer supply ambient globals.
  Intentional symbolic uses remain valid.
- Restrict cross-file references and renames to shared visible bindings. Refresh
  source contexts after edits and file operations without restoring deleted
  definitions or publishing duplicate diagnostics.
- Load global `~/.m2-ls/M2LS.toml` and workspace-root `M2LS.toml`, with explicit
  editor settings taking precedence. Reload watched configuration files and
  support the Windows user profile without requiring `HOME`. Standalone tools
  discover the nearest project configuration and accept `--config` overrides.
- Add independent S19 `self-initializing-constructor`, S20 `header-constructor`,
  and S21 `wrapper-constructor` hints and code actions for `new T from value`.
  Header and wrapper shorthand requires List or Sequence input; rewrites retain
  grouping and comments.
- Preserve source object identities during context-only reanalysis and recognize
  the mutable runtime `currentPackage` binding when absent from the catalog.
- Document import visibility, configuration precedence, unresolved-value errors,
  and constructor shorthand in the user guide.

## 2.5.1 - 2026-11-05

- Add simplification hints and code actions for early-return null guards,
  mapped function compositions, strict comparisons of Boolean values with
  `true` or `false`, forwarding callbacks, and fixed-first-argument partial
  applications. Keep rewrites available as refactors when their hints
  are suppressed, and preserve expression grouping.
- Give each expression rewrite its own lint name and code. S05 now identifies
  `unnecessary-null-branch`; use the individual S11–S18 selectors instead of
  the former `simplifiable-expression` selector for other simplifications.
- Add E21 `float-key`, S08 `naming-convention`, S09 `bound-option-key`, and
  S10 `constructor-typical-value`. Exempt function parameters from naming
  checks so type arguments such as `T` remain valid.
- Check option names and unbound symbolic values in declarations and calls;
  keep bound default values and ordinary hash-table entries outside these
  naming checks.
- Refine semantic highlighting for embedded documentation and class roles,
  document custom-token fallbacks, and refresh semantic tokens after loaded
  dependencies are indexed.
- Expand the user guide with lint examples, simplification requirements, and
  the distinction between formatting and expression rewrites.

## 2.5.0 - 2026-11-05

- Add grouped TOML and JSON configuration for hint placement, argument names,
  hide filters, diagnostic severities, catalog selection, source discovery,
  and multiple initialization files. Keep earlier settings as compatibility
  aliases. Use tabs by default when formatting.
- Refine inlay hints with separate sequence and holder settings, explicit
  holder requests, per-parameter name limits, operator argument labels, and
  filtering of consecutive labels.
- Add E19 `manual-type-dispatch` for repeated class tests of a parameter,
  E20 `list-record-shape` for fixed-size list validation, and S07
  `shared-method-codomain` for methods with at least five installations and
  more than 75% sharing an explicit result annotation.
- Keep regex delimiters and ordinary text as strings, highlight only regex
  punctuation as `regexp`, and classify escapes as `escapeSequence`.
  `semanticTokens.standardOnly` falls back to strings for escape sequences.
- Add a user guide covering editor setup, configuration, diagnostics, runtime
  commands, and navigation. Install through Cargo on all supported platforms.
- Record lint results and indexing summaries in local JSONL usage history,
  separate from operational logs, with path overrides and opt-out settings.
  Tests disable both file writers.

## 2.4.0 - 2026-11-05

- Share one workspace hierarchy between type navigation and the new
  `m2-ls.printHierarchies` command. Include source and library objects, packages,
  lexical dictionaries, class relations and inheritance; preserve alias
  identities and distinguish package dependencies from namespace ownership.
- Add explicit `m2-ls.executeFile`, `m2-ls.executeCell` and `m2-ls.runTests`
  commands. Each runs in a fresh Macaulay2 process and returns its exit status,
  standard output and standard error.
- Preserve resolved type identities across aliases, shadowing and independent
  source snapshots, keeping inference and hierarchy navigation consistent.
- Infer constructor installation parameters and return hints using the
  requested type object and constructed class.
- Highlight regular-expression patterns passed to builtin functions using the
  standard `regexp` semantic token type.

## 2.3.0 - 2026-11-03

- Run analysis, diagnostics, editor queries, formatting and folding on one
  typed syntax tree per document revision, with unchanged cells reused between
  edits. Output, settings and idempotence are unchanged.

## 2.2.0 - 2026-10-29

- Keep installed package dependencies out of the workspace index when opening
  their source files. User-package dependencies continue to be indexed.
- Follow the files named by `load`, `needs` and `input`, looked up the way
  `load` looks them up (beside the loading file, then along `path`; `~/`,
  `./`, `../` and absolute names; escapes read; symbolic links resolved by the
  operating system): highlight the names as namespaces, open them as document
  links and with go to definition, and complete `.m2` files and folders inside
  the name, escaped for the string they go into.
- Warn about a load that cannot run (E17 `unloadable-file`): no file where
  `load` looks, a directory, a file or directory without permission, or an
  empty name. Unsaved documents, sockets and pipes are not judged.
- Support workspace folders and file operations: renaming or moving a file or
  folder rewrites the loads that run it, in every known file and every file of
  the workspace folders mentioning it, including the moved files' own relative
  loads, keeping each name's style and delimiters; creating, deleting,
  renaming and changing the permissions of files, in the editor or on disk,
  keeps the workspace index and the E17 warnings current; closing a workspace
  folder drops its closed files from the index.
- Select the init file every document starts with by the new
  `sources.initFile` setting: `true` for the discovered `init.m2`, `false` for
  none, or a file; a file opts out or picks another with
  `-- m2-ls: initFile none` or `-- m2-ls: initFile <path>`. Its globals are
  known only in the documents starting with it, never in TEST blocks or
  documentation examples; it is read again when it changes on disk. A
  configured file that cannot be read is reported by a message, one a
  directive names by E18 `unavailable-init-file`; code in it that does not
  parse is published as the init file's own errors, and its other findings
  only when it lies in a workspace folder.
- Analyze files in the discovered Core sources, and in the directories listed by
  the new `sources.primaryCodePaths` setting, as library implementation code
  (`primaryCode`) without a directive; the setting also covers library
  sources on a machine without Macaulay2. `m2-lint` applies the same rule.
- Offer adding `-- m2-ls: primaryCode` as a quick fix for assignments to
  protected symbols (E13), and name it in the error message.
- Download the builtin catalog from M2Book releases and cache it instead of
  embedding it in the executables or shipping it in the crate. Each download is
  verified against the checksum published with it. First use needs network access;
  `M2_LS_CATALOG` selects an existing file for offline use. Macaulay2 and M2Book
  are not runtime dependencies.
- Add `m2-ls --download-catalog` to prepare the catalog before opening an editor.
- Add `install.sh` to install the tools and automatically prepare the catalog.
  The server checks for a new M2Book release in the background and uses its
  catalog from the next start; server upgrades reuse the cache.
- Report option names a called function does not accept as errors (E16), with
  the closest accepted name as a quick fix, and option values outside the
  documented types or accepted symbols as warnings (T06). Functions declaring a
  literal option list in source are checked now; library functions once the
  catalog records their option tables.
- Add formatting settings `keepLineBreaks` (keep line breaks as written),
  `spaceInBraces` (`{ b }` or `{b}`), and the `leadingKeywords` control-flow
  layout, which starts `then` and `else` on lines under their `if`.
- Report redundant control parentheses (S03) and simplifiable conditionals
  (S05) as hints again, naming the shorter form. Their rewrites are the hints'
  quick fixes; turned off, the rewrites remain available as refactors.
- Declare the server crate under MIT or Apache-2.0. The separately downloaded
  documentation and source excerpts retain their original licenses.

## 2.1.2 - 2026-10-28

- Show inferred argument types in `call-domain-mismatch` (T05) diagnostics,
  including the failing pair in chained binary method calls.
- Add a `Fix all in file` code action for fixes and simplifications throughout
  the document. Overlapping proposals may need another run.
- Avoid duplicate ring subscript diagnostics and inline values for parenthesized
  expressions and function bodies.
- Accept deprecated CLI configuration keys with a warning instead of rejecting
  the configuration.

## 2.1.1 - 2026-10-27

- Document highlights on `new`, `of` or `from` light up all the keywords of
  the `new` expression.

## 2.1.0 - 2026-10-27

- Inlay hint settings are independent keys, each with one name:
  `afterCall`, `afterLineEnd`, `afterCell`, `duringAssignment`, `argNames`,
  `argTypes`, `afterBrackets`, `afterReturn`, `everywhere`, `showLiteral`,
  `showUpperClosure`, `showThing`, `showNothing`, `showRepeated`, `hideTypes`,
  `hideAfterSemicolon`, `maxLabelLength` and `excludedCalls`. The former
  `expressionTypes`, `allKnownTypes`, `calls` and `unknownTypes` keys are
  deprecated and read as their replacements, with a warning.
- Line-end hints show the last expression complete on every line, so `2 +`
  shows `2` and the line completing it the whole sum.
- Parameter hints before arguments, `f(x: Number 2)`, from local method
  installations, local functions and library signatures.
- `afterBrackets` hints only parenthesized values such as `(2)`, which
  evaluate to what they hold, not collections or call arguments.
- Hints giving the type of the same value are shown once: an assignment's
  hint replaces the cell and line-end hints of its value, and a cell's
  replaces its line-end hint.
- Rings the program writes out are known values: inline values show what a
  subscript selects, `S_6 = z`, and new errors report a subscript past a
  ring's variables (`ring-index-out-of-bounds`, E14) and a lookup of a
  missing variable by name (`ring-variable-not-found`, E15).
- Assigning to a protected indexed symbol reports `protected-assignment`
  (E13). The `-- m2-ls: primaryCode` file property analyses library sources
  such as Core without prelude imports.
- Subscripts keep their parts: in `S_1` the ring `S` stays a type, the `_`
  carries the new `access` modifier, as do `.`, `.?`, binary `#` and `#?`,
  and the `1` reads as the generator it selects; a literal selector such as
  the `0` of `L_0` is a property. `1_S` and `RR_53` stay single tokens.

## 2.0.0 - 2026-10-26

- **Breaking:** semantic tokens now use only LSP-standard token types; Macaulay2
  distinctions (commands, ring generators, documentation, examples, tests,
  options, mutable exports) are carried as modifiers, so themes inherit the
  standard styling. `semanticTokens.standardOnly` now only drops the custom
  modifiers.
- **Breaking:** license the server source under MIT or Apache-2.0; the builtin
  catalog stays under GPL-2.0-only or GPL-3.0-only.
- Silence diagnostics in the source with `-- m2-ls: disable` and
  `-- m2-ls: disable-next-line` directives, written by every diagnostic's
  quick fixes.
- Add `inlayHints.calls` and `inlayHints.unknownTypes` settings for call-result
  and uninformative type hints.
- Write structured JSON logs with request errors and operation timings to
  `/tmp/m2-ls.log`; `M2_LS_LOG` enables full tracing or picks the log path.
- Retain the typed syntax tree per document cell and build navigation from it,
  on the `m2-syn` 0.7 parser.
- Fix ring generator tracking and generalize application precedence; improve
  hover, completion, signature help, formatting, and package documentation
  analysis.

## 1.3.0 - 2026-10-20

- Add `m2-lint` and `m2-fmt` command-line tools, including formatter-backed
  quick fixes and stdin support.
- Analyze user package sources and package `TEST` blocks in their package
  session; navigate into indexed library sources.
- Improve diagnostics, name resolution, semantic tokens, formatting, and
  package documentation analysis.
- Rework settings as plain configuration structs and improve server lifecycle,
  indexing, and installation checks.
- License the project under MIT or GPL-2.0-only or GPL-3.0-only terms.
