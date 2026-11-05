# Code actions

Code actions turn a diagnostic into a one-keystroke fix, and offer a handful of
rewrites that make Macaulay2 code shorter or clearer. Ask for them with the
cursor on the code (`gra` in Neovim).

```m2,pane="Before"
value = if value =!= null then value else fallback
```

```m2,pane="After"
value = value ?? fallback
```

Every rewrite is formatted with your
[formatting settings](../getting-started/configuration.md#formatting) at the
indentation of its surroundings, so accepting one never leaves a line for the
formatter to clean up.

## Quick fixes { #quick-fixes }

Each of these fixes the diagnostic it is attached to:

**Use `:=` for method installation** — [`E04`](diagnostics.md#catalog)
: `g ZZ = n -> n` becomes `g ZZ := n -> n`.

**Use `=` for part assignment** — `X05`
: `L#0 := 5` becomes `L#0 = 5`.

**Add codomain annotation** — `T03`
: `f ZZ := n -> n + 1` becomes `f ZZ := ZZ => n -> n + 1`, with the type
  inference deduced. `new` installations do not need this annotation:
  construction sets the result class to the requested type.

**Capitalize option key** — `S01`
: `method(Options => {verbose => false})` becomes `{Verbose => false}`.

**Use `??` coalescence**, **Use `??=` coalescing assignment** — `S04`
: `if x === null then 0 else x` becomes `x ?? 0`; an `if` that only assigns a
  fallback, `if t === null then t = v`, becomes `t ??= v`.

**Rewrite as member access** — `X03`
: `L.1` becomes `L#1`.

## Rewrites { #rewrites }

These have no diagnostic; they appear when the cursor is on code they can
improve.

**Simplify unnecessary null branch**
: `if ready then value else null` → `if ready then value`, and
  `if linkStyles#?style then null else error(…)` →
  `if not linkStyles#?style then error(…)`.

**Simplify try**
: `try value then value` → `try value`;
  `try value then result else null` → `try value then result`.

**Simplify if condition**
: `if not (left == right) then …` → `if left != right then …`. A negation is
  only pushed inward when the result is genuinely simpler:
  `not linkStyles#?style` is left alone.

**Remove redundant parentheses**
: `if (ready) then value` → `if ready then value`.

**Flatten nested if into else-if chain**
: `if a then x else (if b then y else z)` → `if a then x else if b then y else z`.

**Convert to raw string**
: `"C:\\path\\to\\file"` → `///C:\path\to\file///`, whenever the value
  survives verbatim.

```m2,pane="Before"
label = n -> if n < 0 then "negative" else (if n == 0 then "zero" else "positive")
```

```m2,pane="After"
label = n -> if n < 0 then "negative" else if n == 0 then "zero" else "positive"
```

## Declaring library code { #primary-code }

Assigning to a protected name, `ZZ = 7` (`E13`), is a mistake in ordinary code
but exactly what the library's own source does. That error also offers:

**Analyze this file as library implementation code**
: Inserts `-- m2-ls: primaryCode` at the top of the file, which analyzes it
  [as the library](../getting-started/inline-directives.md#primary-code)
  rather than hiding the error. Whole directories are declared with
  [`sources.primaryCodePaths`](../getting-started/configuration.md#primary-code-paths).

Like the suppressions below, it is never part of **Fix all in file**.

## Silencing a diagnostic { #suppression }

Every `m2-ls` diagnostic under the cursor also offers two actions that write a
[suppression comment](diagnostics.md#suppression), listed after the real fixes:

**Disable S04 on this line**
: Inserts `-- m2-ls: disable-next-line S04` above the line, at its
  indentation.

**Disable S04 in this file**
: Inserts `-- m2-ls: disable S04` at the top of the file.
