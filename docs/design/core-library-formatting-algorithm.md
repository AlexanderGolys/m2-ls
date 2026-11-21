# Formatting algorithm derived from the Macaulay2 Core library

This document describes, as an implementable algorithm, the formatting style of the
Macaulay2 Core library: the `.m2` sources in `M2/Macaulay2/m2/` of
[Macaulay2/M2](https://github.com/Macaulay2/M2). Every rule is backed by a
measurement over those files, and every rule that the corpus splits on carries an
explicit tie-break.

## 1. Corpus and method

* **Source:** `Macaulay2/M2`, branch `development`, commit `93409f5` (2026-11-20).
  The default branch `stable` (`a5ff8d7`, 2026-10-22) differs in 25 Core files by
  about 760 changed lines, and its numbers are within ±0.01 of the ones below.
* **Files:** all 98 `M2/Macaulay2/m2/*.m2` files, minus:
  * `exports.m2`, which is one generated-style `export { "...", ... }` list of
    1,385 strings and would dominate the comma and string statistics;
  * `freealgebras.m2`, `localring.m2` and `obsolete.m2`, which have fewer than 5
    code lines.

  That leaves **94 files, ~22,300 non-blank code lines**. None of them are
  documentation sources: the documentation nodes live in
  `M2/Macaulay2/packages/Macaulay2Doc/`, outside this directory. `document.m2`,
  `help.m2`, `html.m2` and `book.m2` implement the documentation *system*. They
  are ordinary code, so they are included.
* **Averaging:** every percentage is a **macro-average over files**: the share is
  computed in each file, then averaged, so every file has weight 1. Where it
  matters, the pooled count (`n`) is given too.
* **Lexing:** strings (`"..."` with escapes, and `///...///`), line comments
  (`-- ...`) and block comments (`-* ... *-`) are masked out before any
  operator, comma or bracket is measured. Indentation is measured in visual
  columns with tab stops every 8 columns.

The analysis scripts are in [`formatting-analysis/`](formatting-analysis/). See
[`formatting-analysis/README.md`](formatting-analysis/README.md) to reproduce the numbers.

## 2. Headline findings

| Property | Corpus (file average) | Rule chosen |
|---|---|---|
| Indentation model | "previous line's indent + width × net bracket change of the previous line" reproduces **93.4 %** of all line indents (median file 94.4 %) | §4.1, exactly this model |
| Indent width | dominant step is 5 columns in 47 files and 4 in 43 (2 use 2). A single fixed width reproduces 88.4 % of lines with 5 and 85.0 % with 4 | detect per file; fall back to **5** (§4.2) |
| Tabs | 89 % of lines indented ≥ 8 columns use *canonical Emacs tabification* (one TAB per 8 columns, then spaces); no line indented < 8 columns uses a tab | §4.3 |
| Closing `)` of a multi-line group | hugs the last line in 67 %; own line at **body** indent in 29 %; own line dedented to the opener's indent in only 4 % | hug (§5.2) |
| Closing `}` of a multi-line list | own line at **body** indent in 71 %, at the opener's indent in 7 %, hugging in 22 % | own line, body indent |
| Lines with a leading closer | not dedented in 84.5 % of them (dedenting matches 9 %) | never dedent |
| Line length | mean 45, p95 95, max ~138 columns; 10 % of lines > 80, 4 % > 100, 1.4 % > 120 | no hard limit; soft 100 (§6) |
| Blank lines | 97.9 % of blank runs are a single blank line | collapse runs to 1 |
| Top-level statements | preceded by code 53 %, a blank line 37 %, a comment 10 % | preserve; never insert |
| Trailing whitespace | 1.5 % of lines | strip |
| Final newline | present in 94 / 94 files | ensure |
| `:=` `=` `->` `=>` | spaced on both sides 99 / 98 / 99 / 96 % | always spaced |
| Comma | space after in 63 % (calls 68 %, lists 72 %, lambda parameters 49 %); never a space before (99.8 %) | §7.2 |
| `;` | newline after in 94 %, one space in 5 %, never a space before | §7.3 |
| Function call | `f(x)` 95 %, `f (x)` 5 % | no space |
| Inside brackets | `(x` 98 %, `{x` 88 % (ignoring newline) | no inner padding |

The Core library holds **two house styles**, and averaging splits most rules
between them:

| | "5-column" files (42, 9,674 lines) | "4-column" files (43, 8,733 lines) |
|---|---|---|
| Space after comma in `( )` | 45 % | 83 % |
| Space after comma in `{ }` | 56 % | 82 % |
| Multi-line `)` hugs | 62 % | 77 % |
| `-` spaced | 47 % | 68 % |
| Tab-indented lines | 39 % | 34 % |

The older Grayson/Stillman code is the 5-column style, written with the
historical Emacs `M2-indent-level` of 5. The newer code (for example `book.m2`,
2020, and the package, documentation and HTML machinery) uses 4 columns, which is
the current default of `M2-mode` in
[Macaulay2/M2-emacs](https://github.com/Macaulay2/M2-emacs). That mode implements
the §4.1 model literally, as `M2-next-line-indent-amount = (current-indentation) +
(M2-paren-change) * M2-indent-level`. **Per-file detection of the split rules
(§8) is therefore the best way to match the corpus**, and the fixed fallbacks
below are the average.

## 3. Invariants: what the formatter must never change

Macaulay2's grammar is newline-sensitive and its `;` is semantic, so these hold
before any style rule:

1. **Never add or remove `;`.** `(a; b)` returns `b` and `(a; b;)` returns
   `null`. The corpus has 266 `;)` sequences, all deliberate.
2. **Never split or join lines at bracket depth 0.** At top level, a newline
   ends a complete expression. Only change line breaks inside `()`, `{}`, `[]` or
   `<| |>`, or after a token that leaves the expression incomplete (a binary
   operator, `then`, `else`, `do`, `of`, `from`, `to`, `list`, `->`, `:=`, `=`,
   `,`).
3. **Never touch the text of strings.** That includes the lines of a multi-line
   `///...///` or `"..."` string, whose indentation is content. Do not re-indent a
   line that starts inside a string.
4. **Comment text is opaque.** The formatter may re-indent a comment-only line
   and normalise the whitespace before a trailing comment, but never the text
   after `--`. It must not convert between `--` and `-* *-` either.
5. **Applications change only when the AST stays the same.** `f x_1` and
   `f(x)_1` parse differently, so brackets may be added or removed around a call
   argument only where the parse is unchanged. m2-ls already does this through
   `redundantParentheses` / `callParentheses`, and the corpus agrees with it:
   when the argument is a single identifier, number or string and nothing
   postfix follows, the call is written juxtaposed (`f x`) in 98 % of cases
   (10,369 vs 130 `f(x)`). Only the space in `f (x)` is a separate style
   question (§7.4).

## 4. Indentation

### 4.1 The indentation model

For every line `L` that does not start inside a string:

```
P        := the nearest previous line that is not blank
Δ(P)     := (# of ( { [ opened on P) − (# of ) } ] closed on P)
            counted on P's code only (strings and comments masked out)
indent(L) := max(0, indent(P) + W × Δ(P))
```

with `W` the indent width (§4.2) and `indent(first line) = 0`.

What the model implies, and what the corpus confirms:

* **Closers are not dedented.** A line that begins with `)`, `}` or `)))` keeps
  the body's indentation, because the line *before* it had `Δ = 0` (84.5 % of
  leading-closer lines; dedenting reproduces only 9 %). The drop happens on the
  line *after* the closer.
* **Keyword continuation lines get no extra indent.** A line that starts with
  `then`, `else`, `or`, `and` or `<<` follows the model unchanged (92.3 %).
  `else` therefore lands at the indent of the line that holds its `if`, which
  the corpus does in 98 % of multi-line `else`s.
* **A line that opens two groups indents twice.** `f(x, y -> (` is `Δ = 2`, so
  the next line is `+2W`, which is the 17 % of "opener + 8/10" continuations.
* **A line that ends with an operator or a comma and no open bracket does not
  indent.** After a line ending in `,` 99 % of next lines keep the indent, after
  `;` 98 %, after `:=` 97 %, after `=` 90 % and after `or`/`and` 87 %.
* **Comment-only lines** have `Δ = 0` and are indented like code.
* Lines inside a multi-line string are left alone, and the next code line uses
  the last code line as `P` (masked string content counts as no brackets).

Known exceptions, deliberately not modelled (together < 7 % of lines):

* `then` at the end of a line, with the then-branch on the next line, is followed
  by a +2 hanging indent in 47 % of the 100 cases and by no indent in 25 %. Use
  the model (no indent), because the +2 is hand-made and the sample is small.
* Hand-aligned columns, such as tables of `("XC", 90),  ("L", 50)` in `book.m2`
  or arguments aligned under an opening parenthesis (5 % of continuations).
  §8 preserves these.
* Files that mix widths 4 and 5, which are most of the off-by-one mismatches.

### 4.2 Indent width `W`

1. **Detect:** over the file, count the positive indent increases between
   consecutive non-blank code lines. If one of 2, 4 or 5 accounts for at least 60 %
   of the increases of 2, 4 or 5, and there are at least 3, use it. Per-file
   detection reproduces 93.4 % of lines, against 88.4 % for the best fixed width.
2. **Fallback:** `W = 5`. It is the most common dominant step (47 vs 43 files) and
   it reproduces more lines as a fixed setting (88.4 % vs 85.0 %).
3. **Tie-break when a project prefers the modern style:** `W = 4`, the current
   `M2-mode` default and the style of the newer Core files. This is the one place
   where the "average" and the "direction of travel" disagree, so it should be a
   setting.

### 4.3 Writing the indentation

Emit an indent of `c` columns as `⌊c/8⌋` TAB characters followed by `c mod 8`
spaces. 88.8 % of the lines indented ≥ 8 columns are written exactly like that,
6.9 % use spaces only, and 4.4 % mix them some other way. No line indented
< 8 columns starts with a TAB.

Line widths are measured with TAB = 8 columns. A spaces-only option is
reasonable for new code, because the current `M2-mode` sets
`indent-tabs-mode nil`, but it differs from 35 % of the indented lines in the
corpus.

## 5. Layout of multi-line constructs

These rules decide where breaks go when the formatter creates them, and §4
indents the result. When breaks are kept as written (the faithful default, §6),
only §4 and §7 apply.

### 5.1 Function bodies

```m2
name = (a,b) -> (
     stmt1;
     stmt2;
     result)
```

* `-> (` ends the line in 68 % of `-> (` uses (1,342 of 1,960). The other 32 %
  start the body on the same line: `x -> ( s := ""; ...)`. Break after `-> (` when
  the body has more than one statement, or when the one-line form exceeds the
  soft width.
* One statement per line. The newline follows `;` in 94 %.
* The last expression **hugs** the closing `)` (§5.2). There is no `;` before the
  `)` unless the source has one (§3.1).
* `->` at the end of a line, with the body unparenthesised on the next line, is
  rare (60 cases, 1.3 %). Do not create it.

### 5.2 Closing brackets

| Group | Rule | Corpus |
|---|---|---|
| `(` at end of line (body, argument block) | `)` hugs the last element: `last)` or `last));` | 67 % hug, 29 % own line at body indent, 4 % own line at the opener's indent |
| `(` with content after it on the same line | `)` hugs | 92 % |
| `{` at end of line (option and hash-table lists) | `}` on its own line, at **body** indent (the §4.1 result) | 71 % own line at body indent, 7 % at the opener's indent, 22 % hug |
| `{` with content after it | `}` hugs | 87 % |

When an own-line `}` is emitted, any trailing `;`, `)` or `,` follows it on the
same line, for example `},`. A multi-line list ends **without** a trailing comma
before an own-line `}` (142 vs 27).

```m2
newpkg := new Package from {
     "pkgname"         => pkgname,
     symbol Options    => opts,
     "close hook"      => hook
     }
```

### 5.3 `if` / `then` / `else`

1. **Single line** when it fits: `if C then X else Y`. 49 % of all `else`s are on
   the line of their `if`, and `then` is on the `if`'s line in 87 % of cases.
2. **Multi-line:** keep `if C then X` (or `if C then (`) on the first line and
   start the `else` part on a new line at the `if` line's indent (§4.1 gives this
   automatically).
3. **`else if`** always stays together on one line (444 of 444).
4. **`then` on its own line** only when the condition itself spans lines (16 % of
   `then`s). It sits at the `if` line's indent, and the condition's
   continuation lines start with `or`/`and` at the same indent:

   ```m2
   if raw cover src =!= source f
   or raw cover tar =!= target f
   then f = rawMatrixRemake2(...);
   ```
5. **Parenthesised branches** use `then (` / `else (` at the end of the line, with
   the body at +W and the `)` hugging, and the next `else` on a new line. `) else (`
   on one line is rare (3.5 % of multi-line `else`s). Do not create it.

   ```m2
   if R =!= S then (
        R' := ...;
        notToSameRing(class M,class N))
   else (M,N))
   ```

### 5.4 Loops and `try`

`for i from a to b do (` with the body at +W and the `)` hugging is 56 % of
`do`s; `do X` inline is 40 %. Break the same way as §5.1. `while C do (` and
`scan(L, x -> (` follow the same shape, and the `scan` form opens 2 groups (+2W
under §4.1).

### 5.5 Long expressions

When a line must be broken (§6), break in this order of preference, which is the
order of frequency at line ends and starts in the corpus:

1. after an opening `(` / `{` of a call or list (12.4 % of all lines end with
   `(`, and 1.2 % with `{`);
2. after a `,` at the outermost bracket level of the line (6.6 % of lines);
3. before `else` (4.4 % of lines start with `else`), then before `then` (1.9 %),
   then before `or` / `and` / `<<` / `|`;
4. after `:=` / `=` in an assignment (2.5 % of lines end with `:=`). Most of these
   are stacked method installations `f Type :=` (§8.2), not breaks of long lines.

Never break before a `,` or `;`, never break between a function and its
juxtaposed argument, and never break after a binary arithmetic operator unless
nothing else applies. In the corpus, lines ending in `+ - * ^ .` total < 0.1 %.

## 6. Line width

There is no line-length convention to enforce: p95 is 95 columns and 4 % of
lines exceed 100.

* **Default: keep the line breaks as written** and apply only §4 and §7. This is
  the setting that leaves the most corpus lines unchanged.
* When wrapping is enabled: soft width **100** (just above the p95 of 95), no hard
  width. Wrap only lines over the soft width, using §5.5. A width of 80 would
  rewrap 10 % of the corpus's lines.

## 7. Horizontal spacing

### 7.1 Operators

Spacing means "one space on each side" (*spaced*) or "no space" (*tight*). The
measurement counts only occurrences with both neighbours on the same line and
symmetric spacing.

**Always spaced (≥ 85 %):**

| Operator | spaced | n |
|---|---|---|
| `:=` | 99 % | 6,524 |
| `->` | 99 % | 5,470 |
| `=` | 98 % | 3,234 |
| `===` | 98 % | 1,077 |
| `=!=` | 99 % | 565 |
| `=>` | 96 % | 2,344 |
| `==` | 96 % | 479 |
| `!=` | 97 % | 114 |
| `<-` | 100 % | 28 |
| `<<` | 99 % | 398 |
| `@@` | 97 % | 146 |
| `\` (apply) | 93 % | 143 |
| `++` | 92 % | 34 |
| `?` | 92 % | 83 |
| `**` | 90 % | 128 |
| `\|\|` | 90 % | 28 |
| `<` | 90 % | 199 |
| `<=` / `>=` | 95 % / 89 % | 77 / 46 |
| `%` | 89 % | 92 |
| `\|` | 87 % | 778 |
| `>` | 85 % | 274 |
| `..<` | 80 % | 29 |
| word operators `and or xor not` | always | |

**Always tight (≥ 85 %):**

| Operator | tight | n |
|---|---|---|
| `#` (index `x#i`) | 100 % | 2,439 |
| `.` / `.?` / `#?` | 100 % | 2,207 / 327 / 294 |
| `_` | 89 % | 638 |
| `^` | 85 % | 299 |
| prefix `-x` | 92 % | 283 |
| prefix `#x` | 89 % | 668 |

**Split. Context decides, with tie-breaks:**

| Operator | spaced overall | Rule |
|---|---|---|
| `*` | 76 % | spaced, tight inside an index (below) |
| `//` | 69 % | spaced, tight inside an index |
| `+` | 66 % | spaced, tight inside an index |
| `-` (binary) | 59 % | spaced, tight inside an index |
| `/` | 57 % | spaced, tight inside an index |
| `..` | 54 % | spaced (`0 .. n-1`). Weakest rule in the table, so detect per file (§8) |
| `:` | 36 % spaced | tight (`R^1:...`) |

The **index context** is an arithmetic operator inside a bracket opened directly
after `_`, `#`, `^` or `.` (for example `x#(i+1)` or `x_(n-1)`). There, `+ - * /`
are tight in 77–83 % of cases. When one operand is itself subscripted
(`x_i+y_j`), the corpus splits about 50/50, so keep it as written.

For `+ - * /` where both operands are plain atoms (`n-1`, `2*x`), the corpus is
spaced 66 % and tight 34 % in both house styles, so they are spaced by default.
This **differs from `compactFactorOperators = true`** in the current m2-ls
defaults, which is why §11 drops that key.

### 7.2 Commas

* Never a space before a comma (99.8 %).
* After a comma, either a newline or exactly one space, except in lambda
  parameter lists:

| Context | space after `,` (file average / median) | n | Rule |
|---|---|---|---|
| call arguments `f(a, b)` | 68 % / 68 % | 7,709 | space |
| other sequences `(a, b)` | 67 % / 71 % | 1,665 | space |
| lists `{a, b}` | 72 % / 88 % | 1,101 | space |
| arrays `[a, b]` | 100 % | 23 | space |
| method signatures `f(ZZ, Ring) :=` | 58 % / 57 % | 1,344 | space |
| lambda parameters `(x,y) ->` | **49 % / 40 %** | 2,520 | detect per file; fallback **tight** |

The lambda-parameter tie is the 5-column style (`(M,N) -> ...`) against the
4-column style (`(M, N) -> ...`). The fallback follows the median file (tight).
With per-file detection of the comma style (§8), all comma contexts in a file
follow that file's majority.

* Extra spaces after a comma that align columns (352 occurrences, mostly tables)
  are preserved (§8.2).

### 7.3 Semicolons

* No space before `;` (99.9 %).
* After `;`: a newline when the statements are on separate lines, otherwise one
  space (`a; b`, 96 % of same-line cases).
* `;` directly before `)` (`b;)`) is kept (§3.1).

### 7.4 Brackets and keywords

* No space after an opening bracket or before a closing one, for `()` (98 %),
  `{}` (88 %) and `[]` (89 %). Empty brackets are `()` and `{}`.
* No space between a function name and `(` in a call: `f(x)` (95 %). Prefer
  `f x` over `f(x)` for a bare atom argument wherever that leaves the AST the same
  (§3.5).
* Exactly one space between a keyword and a following `(`: `then (`, `else (`,
  `do (`, `if (`, `while (`, `not (`, `return (`, `list (` (95–100 %).
* `symbol =` and similar `symbol <op>` forms are left exactly as written.

## 8. Things to preserve rather than normalise

### 8.1 Per-file detection

For the rules the corpus splits on (majority < 60 % across files), the closest
reproduction is to measure the file being formatted and follow its own majority,
with the fallback above when the file has fewer than 3 samples:

| Rule | Detection key | Fallback |
|---|---|---|
| indent width | dominant positive indent step among 2/4/5 | 5 |
| comma spacing | share of `, ` vs `,x` in `( )` | space, tight for lambda params |
| `+` `-` `/` `..` spacing | share spaced per operator | spaced |
| hugging vs own-line `)` | share among multi-line `(` groups | hug |

### 8.2 Alignment padding

43 % of adjacent lines that both contain `:=`, `=` or `=>` have them in the same
column, and 17 % reach that alignment with extra padding. Overall, 8.4 % of
`:=`/`=>` are preceded by two or more spaces. This is deliberate tabular style:

```m2
isDocumentableThing    String :=
isDocumentableThing  Sequence := key -> false
isDocumentableThing   Nothing :=
```

Rule: if an operator is preceded by more than one space, **and** the same
operator is at the same column on the previous or next non-blank line, keep the
padding. Otherwise collapse to one space. The same check applies to padding
after commas and before trailing comments. Never introduce alignment.

## 9. Comments and file structure

* **Line comments:** `-- text` with one space (66 % of comment lines). `--text`
  without a space is 1.7 %, and a bare `--` is 8 %, used as a visual spacer.
  Comment text is not reflowed or rewritten (§3.4).
* **Trailing comments:** separated from code by one space (57 %), a TAB (29 %) or
  aligned padding (10 %). Normalise to one space unless the §8.2 alignment check
  applies.
* **Block comments** `-* ... *-` are 1.6 % of comment lines, mostly copyright
  headers. Leave them untouched.
* **Section banners:** a line of exactly **77** `-` (655 of 687 rule lines, 95 %), a
  `-- Title` line, then the 77-dash line again. A formatter may normalise
  rule-only comment lines of 60–90 dashes to 77.
* **Header:** 73 of 94 files open with a copyright comment, and 90 have `needs
  "..."` lines near the top. These are content, not formatting.
* **Footer:** 70 of 94 files end with the Emacs block

  ```m2
  -- Local Variables:
  -- compile-command: "make -C $M2BUILDDIR/Macaulay2/m2 "
  -- End:
  ```

  Keep it, and do not add it.
* **Blank lines:** collapse runs of 2 or more blank lines to 1 (97.9 % of runs are
  already 1). Remove blank lines directly after an opening bracket (only 170 of 2,636
  end-of-line openers, 6 %, are followed by one) and directly before an own-line
  closer (18 cases). Never insert blank lines, because 53 % of top-level
  definitions directly follow the previous one with no blank line.
* **Whitespace:** strip trailing whitespace and end the file with exactly one
  newline.

## 10. The algorithm as pseudocode

```
format(file):
  toks   := lex(file)                     # strings, comments, ///…/// opaque
  W      := detectIndentWidth(toks) or 5          # §4.2
  style  := detectSplitRules(toks)               # §8.1, with fallbacks
  if wrapping enabled:
      for each line longer than 100 columns:      # §6
          break at the first available point in §5.5 order,
          only inside brackets or after an incomplete-expression token   # §3.2
  # horizontal pass (inside each line, code tokens only)
  for each adjacent token pair (a, b) on the same line:
      gap := spacing(a, b, context, style)       # §7 tables
      if gap == ONE and existing gap > 1 and isAligned(a or b):   # §8.2
          keep existing gap
      else
          set gap
  # vertical pass
  prevIndent, prevDelta := 0, 0
  for each line L:
      if L starts inside a string: copy verbatim; continue        # §3.3
      if L is blank: emit at most one blank in a row; continue    # §9
      c := max(0, prevIndent + W * prevDelta)                      # §4.1
      emit tabs(c div 8) + spaces(c mod 8) + strip(L)              # §4.3
      prevIndent, prevDelta := c, netBracketChange(L)
  strip trailing whitespace; ensure single final newline
```

`spacing(a, b, ...)`, by precedence (first match wins):

1. inside a string or comment → unchanged;
2. either side is a newline → no horizontal space;
3. `a` is an opening bracket or `b` a closing one → none (§7.4);
4. `b` is `,` or `;` → none;
5. `a` is `,` → one space, or none in a lambda parameter list or a file whose
   detected comma style is tight (§7.2);
6. `a` is `;` → one space;
7. `a` is a keyword and `b` is `(` → one space;
8. `a` is an identifier and `b` is the `(` of its call → none. Whether the
   brackets exist at all is decided earlier, by the AST-preserving removal of
   redundant parentheses (§3.5);
9. binary operator → the §7.1 table, with the index-context override;
10. prefix operator → none between operator and operand;
11. word operators and keywords → one space;
12. anything else → as written.

## 11. A `formatting.*` settings schema derived from this algorithm

This schema replaces the current `formatting.*` keys. Every key corresponds to
one rule above, and its default is the Core-faithful choice. A key whose rule
the corpus splits on accepts `"detect"`, which applies §8.1 to the document
being formatted. `redundantParentheses` and `callParentheses` are kept as they
are: they are AST-level transforms, and §3.5 shows that the corpus agrees with
them.

### 11.1 Schema with defaults

```toml
[formatting]
profile = "core"            # "core" | "modern": a preset for every key below (§11.2);
                            # keys set explicitly override the preset

# --- detection (§8.1) -------------------------------------------------------
[formatting.detect]
minSamples = 3              # fewer samples in the document -> use the fallback
threshold  = 0.6            # share the document's majority must reach

# --- indentation (§4) -------------------------------------------------------
[formatting.indent]
width         = "detect"    # "detect" | 2 | 4 | 5 | any positive integer
fallbackWidth = 5           # used when width = "detect" finds no majority
whitespace    = "emacsTabs" # "emacsTabs": one TAB per tabWidth columns, then spaces
                            # "spaces": spaces only
                            # "tabs": one TAB per level (never used in the corpus)
tabWidth      = 8           # columns per TAB, for emacsTabs and for line widths
closers       = "body"      # "body": a line starting with ) } ] keeps the body indent (§4.1)
                            # "opener": dedent it to the opener line's indent

# --- line breaks and width (§5, §6) ----------------------------------------
[formatting.lines]
breaks          = "keep"    # "keep": only indentation and spacing change
                            # "wrap": also break lines longer than softWidth (§5.5)
                            # "normalize": also apply the §5 layouts to every construct
softWidth       = 100       # integer or null; only read when breaks != "keep"
hardWidth       = null      # integer or null; never wrap past it, even inside a construct
maxBlankLines   = 1         # longer runs of blank lines collapse to this
trimBlankLinesInBrackets = true   # drop blank lines just after ( { and just before ) }
finalNewline    = true
trimTrailingWhitespace = true

# --- multi-line layout (§5) -------------------------------------------------
# These are read only when lines.breaks != "keep".
[formatting.layout]
closeParen      = "hug"     # "hug" | "ownLine": the ) of a multi-line ( group
closeBrace      = "ownLine" # "ownLine" | "hug": the } of a multi-line { list
trailingComma   = false     # add a , before an own-line } (the corpus: no, 142 vs 27)
functionBody    = "breakWhenMultiStatement"   # "always" | "breakWhenMultiStatement" | "keep"
statements      = "onePerLine"                # "onePerLine" | "keep": the newline after ;
singleLineIf    = "whenFits"  # "whenFits" | "never" | "keep"
elsePlacement   = "ownLine"   # "ownLine": else starts a line at the if line's indent
                              # "afterClose": ) else ( on one line
thenPlacement   = "withCondition"  # "withCondition": then ends the condition's last line
                                   # "ownLine": then starts a line when the condition spans lines
conditionBreaks = "beforeOperator" # "beforeOperator": or/and start the continuation lines
                                   # "afterOperator": or/and end them

# --- horizontal spacing (§7) ------------------------------------------------
[formatting.spacing]
comma             = "detect"    # "detect" | "space" | "tight"; fallback "space"
lambdaParamComma  = "detect"    # "detect" | "space" | "tight"; fallback "tight"
                                # "sameAsComma" follows comma
semicolon         = "space"     # "space" | "tight": a ; followed by more code on the same line
bracketsInside    = "tight"     # "tight" | "space" | "keep": (x) vs ( x ), likewise {} and []
callParen         = "tight"     # "tight" | "keep": f(x) vs f (x)
keywordParen      = "space"     # then ( / else ( / do ( / if ( ...: always one space
indexContextTight = true        # + - * / are tight inside brackets after _ # ^ . (x#(i+1))
trailingComment   = "oneSpace"  # "oneSpace" | "keep": the gap before a trailing --
preserveAlignment = true        # keep padding that aligns with a neighbouring line (§8.2)

[formatting.spacing.operators]
spaced = [":=", "=", "->", "=>", "<-", "==", "===", "=!=", "!=", "<", ">", "<=", ">=",
          "<<", ">>", "@@", "\\", "++", "?", "**", "||", "%", "|", "..<", "&"]
tight  = ["#", ".", ".?", "#?", "_", "^", ":"]
detect = ["+", "-", "*", "/", "//", ".."]   # majority of the document; fallback "spaced"
# Operators in none of these lists keep their spacing as written. Word operators
# (and or xor not) are always spaced; prefix operators are always tight to their
# operand (-x, #x).

# --- comments (§9) ----------------------------------------------------------
[formatting.comments]
bannerWidth = 77            # integer or null: rule-only comment lines of 60–90 dashes
                            # become this many dashes

# --- AST-level transforms (unchanged) --------------------------------------
redundantParentheses = "action"   # "off" | "action" | "format"
callParentheses      = "remove"   # "remove" | "keep"
```

### 11.2 Presets

`profile` sets every default at once. `"core"` is the table above. `"modern"`
is the 4-column house style of the newer Core files, combined with the current
`M2-mode` editor settings:

| Key | `core` | `modern` | Why the two differ |
|---|---|---|---|
| `indent.width` | `"detect"` | `4` | 4-column files and `M2-indent-level 4` |
| `indent.fallbackWidth` | `5` | `4` | as above |
| `indent.whitespace` | `"emacsTabs"` | `"spaces"` | `M2-mode` sets `indent-tabs-mode nil` |
| `lines.breaks` | `"keep"` | `"wrap"` | a new codebase can afford rewrapping |
| `spacing.comma` | `"detect"` | `"space"` | 83 % in 4-column files |
| `spacing.lambdaParamComma` | `"detect"` | `"sameAsComma"` | 4-column files space them |
| `spacing.operators.detect` | `+ - * / // ..` | (empty; all spaced) | 68–85 % spaced in 4-column files |

Every other key has the same value in both presets.

### 11.3 Design notes

* **One key per rule, no combined enums.** The old `controlFlowLayout` merged
  four independent choices into one value. Those are now `singleLineIf`,
  `elsePlacement`, `thenPlacement` and `conditionBreaks`, because the corpus
  measures each one separately (§5.3).
* **`"detect"` is a value, not a separate switch.** Only the five rules with a
  file-level split accept it, so a user can pin any one of them without turning
  detection off for the others.
* **`lines.breaks = "keep"` is the default** because it changes the fewest
  corpus lines. `"wrap"` and `"normalize"` are opt-in. The old
  `keepLineBreaks` / `breakAfterSemicolon` pair is now `lines.breaks` plus
  `layout.statements`.
* **No factor-operator special case.** `compactFactorOperators` is dropped. The
  corpus spaces `2 * x` (66 % for atom × atom), and the one context where it is
  tight, inside an index, is `indexContextTight`.
* **Operator lists name the exceptions to "keep as written"**, as the old
  `spacesAround` / `compact` lists did. The defaults are now the measured ones
  (§7.1), and `@@` moves to `spaced`.

### 11.4 Migration from the current keys

| Current key | Becomes |
|---|---|
| `indent = "editor"` / `"tab"` / N | `indent.width = N` with `indent.whitespace`; `"tab"` → `whitespace = "tabs"` |
| `tabWidth` | `indent.tabWidth` |
| `softWrapWidth` / `hardLineWidth` | `lines.softWidth` / `lines.hardWidth` (`lines.breaks = "wrap"` if either is set) |
| `keepLineBreaks = true` | `lines.breaks = "keep"` |
| `breakAfterSemicolon` | `layout.statements = "onePerLine"` / `"keep"` |
| `controlFlowLayout` | `layout.singleLineIf`, `elsePlacement`, `thenPlacement` |
| `compactFactorOperators` | removed (`spacing.indexContextTight` covers the measured case) |
| `operators.spacesAround` / `operators.compact` | `spacing.operators.spaced` / `tight` |
| `brackets.spacesInside` / `brackets.compact` | `spacing.bracketsInside` |
| `redundantParentheses`, `callParentheses` | unchanged |

## 12. Fidelity and caveats

* With per-file width detection, the indentation model alone reproduces 93.4 % of
  existing line indents (file average; 92.6 % pooled). With a fixed width it
  reproduces 88.4 % (W = 5) or 85.0 % (W = 4). The remainder is mostly files that
  mix 4 and 5, hand alignment, and `then`-at-end-of-line hanging indents.
* The operator and comma figures count same-line, symmetric occurrences. Unary
  versus binary is decided by the previous token, so a few prefix uses after `)`
  are counted as binary.
* "Averaged over files" gives a 6-line file the same weight as `expressions.m2`
  (958 code lines). The pooled numbers (`n`) move no rule across its threshold,
  except the indent width, where pooling slightly favours 5 as well.
