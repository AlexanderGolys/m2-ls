# Implementation plan: Core-faithful formatter and the new `formatting.*` schema

This plan is for Claude Code, run locally in a checkout of `AlexanderGolys/m2-ls`.
It implements the spec in
`docs/design/core-library-formatting-algorithm.md` (the same content as the
"Core library formatting algorithm" doc). The spec's section numbers (§n) are
used throughout. Read the spec in full before starting.

## Ground rules

* Work on a feature branch. Make one commit per phase, and keep each phase green.
  Before each commit, run the checks from `CONTRIBUTING.md`:
  `cargo fmt -- --check`,
  `cargo clippy --workspace --all-targets --all-features -- -D warnings` and
  `cargo test`.
* While fixing a failure, re-run only the failing tests
  (`cargo test <name>`), and run the full suite once per phase.
* Never change semantics. Every new test that formats code also asserts that the
  output parses to the same AST as the input. Add a helper for this in phase 0
  if one does not already exist (§3).
* The spec's numbers are measurements. Do not "improve" a default away from them.
  If something seems wrong, stop and ask.
* Ask before pushing, and before opening a PR.

## Orientation: where things are today

| Concern | Location |
|---|---|
| Settings struct and docs comments | `src/settings.rs` (`FormattingSettings`, `Indent`, `upgrade_deprecated_key*`) |
| Defaults | `src/settings/defaults.toml` (`[formatting]`) |
| Operator and bracket lists | `src/settings/spacing.rs` |
| Formatter pipeline | `src/capabilities/formatting.rs`: `formatted_layout` runs the passes `Spacing → LeadingOperators → Options → EmptyBodies → ControlFlow → Indent → Wrap → Closers → Indent` |
| Indentation | `reindent_from_syntax` + `TreeIndentLayout` (depth × `options.indent`; dedents leading closers via `leading_closer_rows`) |
| Whitespace editing | `src/capabilities/formatting/layout.rs` (`LayoutSource`) |
| CLI | `src/bin/m2-fmt.rs` → `src/cli.rs` (`Tool::Formatter`) |
| User docs | `docs/src/writing/formatting.md`, `docs/src/getting-started/configuration.md`, `README.md` |
| LSP settings tests | `tests/pipeline/lsp.rs`, unit tests in `src/settings.rs` |

There are two structural gaps between the current formatter and the spec, and
they drive most of the work:

1. Indentation is **level-based**: it emits depth × one indent unit and dedents
   closers. The spec's is **column-based**:
   `indent(L) = indent(P) + W × Δ(P)`. Closers are not dedented, and indentation
   is written as a TAB per 8 columns.
2. The default layout puts `)` on its own line at the opener's indent and uses
   `then`/`else` blocks. The spec has `)` hug the last line, a `}` of a
   multi-line list on its own line at body indent, and `else` on a new line at
   the `if` line's indent.

## Phase 0: corpus fidelity harness (tests only)

Goal: a number that says how close the formatter is to the Core library. Every
later phase must raise it or leave it unchanged.

1. Add `tests/core_fidelity.rs`, marked `#[ignore]` so CI does not need the
   corpus. It should:
   * take a directory from `M2_CORE_DIR` (the `M2/Macaulay2/m2` folder of a
     Macaulay2/M2 checkout on `development`);
   * skip `exports.m2` and the files with fewer than 5 code lines
     (`freealgebras.m2`, `localring.m2`, `obsolete.m2`);
   * run `format_document_with_options` with the profile under test;
   * report, per file and macro-averaged, the share of non-blank lines that come
     out byte-identical, and the share whose leading whitespace is unchanged;
   * assert that every output reparses to the same AST.
2. Record today's numbers with the current defaults in the test's doc comment.
   This is the baseline.
3. Add a small unit helper `assert_same_ast(input, output)` for the phase tests.

Done when: `M2_CORE_DIR=… cargo test --test core_fidelity -- --ignored --nocapture`
prints the table.

## Phase 1: the new settings schema, with no behaviour change yet

Implement §11.1 as data only. The formatter keeps reading the old fields through
an adapter, so this phase does not change any output.

1. In `src/settings.rs`, replace the flat `FormattingSettings` with nested groups:
   `profile`, `detect`, `indent`, `lines`, `layout`, `spacing` (with
   `spacing.operators`), `comments`, plus the unchanged `redundantParentheses`
   and `callParentheses`. Use the key names, value spellings and defaults from
   §11.1 exactly. Add `deny_unknown_fields` and doc comments in the existing
   style, one line per accepted value.
2. Model `"detect"` as an enum variant, for example
   `enum Detectable<T> { Detect, Fixed(T) }`, on `indent.width`,
   `spacing.comma`, `spacing.lambdaParamComma` and `spacing.operators.detect`.
3. Presets (§11.2): resolve `profile` first, then overlay explicitly set keys.
   Put the preset tables in `defaults.toml` as `[formatting.profiles.core]` and
   `[formatting.profiles.modern]`, or in code, whichever fits the existing
   loader better.
4. Migration (§11.4): extend `upgrade_group` for `FormattingSettings`, using the
   existing `upgrade_deprecated_key` machinery, so every old key keeps working
   and reports a deprecation `SettingProblem` naming its replacement.
   `compactFactorOperators` has no replacement. Accept it, ignore it, and report
   it as removed.
5. Change `FormatOptions::from(&FormattingSettings)` to read the new fields and
   produce today's `FormatOptions`. Behaviour is unchanged in this phase, but
   values with no old equivalent are stored for the later phases.
6. Tests: one deserialisation test per key and value, preset resolution, every
   old key upgrading to its new one, and `deny_unknown_fields` errors.

Done when: all existing formatting tests pass unchanged and the fidelity number
is unchanged.

## Phase 2: per-file detection (§8.1)

1. Add `src/capabilities/formatting/detect.rs` with
   `fn detect_style(syntax: &SyntaxSnapshot, settings) -> DetectedStyle`, which
   walks tokens (code only):
   * indent width: the positive indent steps between consecutive non-blank code
     lines (visual columns, TAB = `tabWidth`). Pick whichever of 2, 4 or 5 holds
     ≥ `detect.threshold` of them, with at least `detect.minSamples`; otherwise
     use `indent.fallbackWidth`;
   * comma spacing: the share of `, ` against `,x` inside `( )`, and separately
     inside lambda parameter lists (a `( … )` directly followed by `->`);
   * for each operator in `spacing.operators.detect`, the share of spaced against
     tight occurrences, counting only same-line symmetric ones.
2. Resolve every `Detect` into a concrete value once per format call, and pass
   the resulting `DetectedStyle` through `FormatOptions`.
3. Tests: one fixture per house style, a 5-column file with `(a,b)` and a
   4-column file with `(a, b)`, asserting the detected values. Also test that a
   file with too few samples gets the fallbacks.

## Phase 3: column-based indentation (§4)

This phase has the biggest fidelity gain.

1. Replace the depth-based computation in `reindent_from_syntax` /
   `TreeIndentLayout` with the §4.1 recurrence:
   * `P` is the previous non-blank line that does not start inside a string or
     comment block;
   * `Δ(P)` is opening brackets minus closing brackets on `P`, counted on code
     tokens only. Use the syntax tree's bracket tokens, not characters;
   * `indent(L) = max(0, indent(P) + W × Δ(P))`;
   * keep the existing `protected_ranges` logic, so lines inside multi-line
     strings stay untouched (§3.3).
2. `indent.closers = "body"` drops the `leading_closer_rows` dedent, and
   `"opener"` keeps today's behaviour.
3. Write the indentation according to `indent.whitespace`. `"emacsTabs"` emits
   `⌊c/tabWidth⌋` TABs + `c mod tabWidth` spaces, `"spaces"` emits `c` spaces,
   and `"tabs"` emits one TAB per level (the old `indent = "tab"`).
4. Check that `folding_ranges` still works, since it reuses the indent layout,
   and keep its tests green.
5. Tests: the §4.1 bullets as fixtures (closer lines, `else`/`then`/`or` lines,
   a two-opener line indenting twice, `,`-ended lines without an open bracket,
   comment-only lines, multi-line strings).

Done when: fidelity (leading whitespace unchanged) reaches about 93 % with
`profile = "core"` and `lines.breaks = "keep"`. The spec's model reaches 93.4 %.

## Phase 4: horizontal spacing (§7, §8.2)

1. In `normalize_whitespace` / `OperatorGaps`, apply the §10 `spacing(a, b)`
   precedence list exactly, driven by `spacing.*` and `DetectedStyle`.
2. Replace the defaults with the §11.1 operator lists. Remove the factor-operator
   special case and add the index-context override, where `+ - * /` are tight
   inside a bracket opened directly after `_ # ^ .`.
3. Alignment preservation (§8.2): when the gap before an operator, after a comma
   or before a trailing comment is more than 1 space, and the same token sits in
   the same visual column on the previous or next non-blank line, keep it.
4. Trailing comments: normalise the gap to one space, unless it is aligned.
   Banner normalisation: rule-only comment lines of 60–90 dashes become
   `comments.bannerWidth` dashes.
5. Tests: every row of the §7.1, §7.2 and §7.4 tables, the alignment example in
   §8.2, and the index context.

## Phase 5: line structure (§5, §6, §9)

All of this runs only when `lines.breaks != "keep"`, except blank-line handling,
trailing whitespace and the final newline, which always run.

1. Blank lines: collapse runs to `lines.maxBlankLines`. With
   `trimBlankLinesInBrackets`, drop blank lines right after an end-of-line opener
   and right before an own-line closer. Never insert blank lines.
2. Closers (`FormatPass::Closers`): `layout.closeParen = "hug"` joins an own-line
   `)` onto the previous line. That is safe because it is inside brackets (§3.2).
   `layout.closeBrace = "ownLine"` puts the `}` of a multi-line `{` group on its
   own line, with trailing punctuation kept on it (`},`). The default for
   `layout.trailingComma` is false.
3. Control flow (`format_control_flow`): replace `controlFlowLayout` with
   `singleLineIf`, `elsePlacement`, `thenPlacement` and `conditionBreaks`, as
   described in §5.3. Never emit `) else (` unless `elsePlacement = "afterClose"`.
4. Function bodies and statements: `layout.functionBody` and `layout.statements`
   (§5.1).
5. Wrapping (`wrap_long_lines` / `select_line_breaks`): when `lines.breaks =
   "wrap"`, re-rank break candidates in the §5.5 order:
   1. after an opening `(` or `{`;
   2. after an outermost `,`;
   3. before `else`, `then`, `or`, `and`, `<<` or `|`;
   4. after `:=` or `=`;
   5. last of all, after an arithmetic operator.

   The soft width is the default 100, and the hard width is off by default.
6. Tests: one fixture per §5 subsection showing input and expected output, in the
   spec's own examples, plus an AST-equality assertion for each.

## Phase 6: documentation and release notes

1. Rewrite `docs/src/writing/formatting.md` around the new keys. Show the `core`
   and `modern` presets with before and after panes, keeping the existing pane
   syntax.
2. Update `docs/src/getting-started/configuration.md` and `README.md`, including
   a migration table copied from §11.4.
3. Add a `CHANGELOG.md` entry covering the new schema, the deprecated keys, the
   behaviour changes (closers, tabs, operator spacing) and the fidelity number.
4. Put the final fidelity table from phase 0 at the end of the spec, in its §12.

## Final acceptance

* `cargo fmt -- --check`, clippy with `-D warnings` and `cargo test` are clean.
* The fidelity harness, with `profile = "core"`, reports:
  * with `lines.breaks = "keep"`, ≥ 93 % of lines with unchanged leading
    whitespace, and the per-line byte-identical share reported next to it;
  * no AST changes on any of the 94 corpus files.
* Formatting is idempotent on the corpus: running `m2-fmt` twice gives the same
  output as running it once.
* The old settings keys still load with a deprecation notice.
