# Changelog

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
