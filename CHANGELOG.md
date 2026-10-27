# Changelog

## Unreleased

- **Breaking:** inlay hint settings are independent keys, each with one name:
  `afterCall`, `afterLineEnd`, `afterCell`, `duringAssignment`, `argNames`,
  `argTypes`, `afterBrackets`, `afterReturn`, `everywhere`, `showLiteral`,
  `showUpperClosure`, `showThing`, `showNothing`, `showRepeated`, `hideTypes`,
  `hideAfterSemicolon`, `maxLabelLength` and `excludedCalls`. The former
  `show`, `callForm`, `literalTypes`, `repeatedTypes`, `unknownTypes`,
  `hiddenTypes`, `expressionTypes`, `allKnownTypes` and `calls` keys are no
  longer read.
- Line-end hints show the last expression complete on every line, so `2 +`
  shows `2` and the line completing it the whole sum.
- Parameter hints before arguments, `f(x: Number 2)`, from local method
  installations, local functions and library signatures.

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
