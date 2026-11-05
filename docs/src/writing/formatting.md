# Formatting

The formatter rewrites the whole document into one consistent layout:
indentation, spacing around operators and commas, where long expressions wrap,
and how control flow breaks across lines.

Expression simplifications are separate [code actions](code-actions.md#rewrites).
For example, formatting keeps `linked === true`; **Simplify Boolean comparison**
can replace it with `linked` when its type is known to be Boolean.

```m2,pane="Before"
normalize = (v,limit) -> (
if v === null then 0 else if v > limit then limit else v
)
total = sum apply({1,2,3}, i -> 2 * i+1)
```

```m2,pane="After"
normalize = (v, limit) -> (
    if v === null then
        0
    else if v > limit then
        limit
    else v
)
total = sum apply({1, 2, 3}, i -> 2*i + 1)
```

Format from the editor (`:lua vim.lsp.buf.format()` in Neovim), or from a
terminal and CI with [`m2-fmt`](../beyond/command-line.md#formatting) — both
produce byte-identical results.

## Control-flow layout { #control-flow }

[`controlFlowLayout`](../getting-started/configuration.md#formatting) decides
how an `if` that stands on its own line in a block is laid out. The same
function under each value:

```m2,pane="compact"
normalize = (v, limit) -> (
    if v === null then 0 else if v > limit then limit else v
)
```

```m2,pane="multiline"
normalize = (v, limit) -> (
    if v === null then
        0
    else if v > limit then
        limit
    else
        v
)
```

```m2,pane="multilineCompactElse (default)"
normalize = (v, limit) -> (
    if v === null then
        0
    else if v > limit then
        limit
    else v
)
```

```m2,pane="leadingKeywords"
normalize = (v, limit) -> (
    if v === null
    then 0
    else if v > limit
    then limit
    else v
)
```

An `if` used as a value inside a single line — `n -> if n > 0 then 1 else 0` —
stays on that line under every layout until it outgrows the line width.

## Spacing and lines { #spacing }

`compactFactorOperators` (default `true`)
: Multiplication and division bind tighter than addition, and the default
  spacing shows it: `2*i + 1`. Set it to `false` for `2 * i + 1`.

`breakAfterSemicolon` (default `false`)
: Puts each `;`-separated expression on its own line.

`keepLineBreaks` (default `false`)
: Keeps every line break where you wrote it: only spacing within lines and
  indentation change, so the line widths and `controlFlowLayout` no longer
  apply.

`spaceInBraces` (default `null`)
: `true` pads lists as `{ 1, 2 }`, `false` writes `{1, 2}`, and `null` keeps
  what you wrote. Empty braces are always `{}`.

`softWrapWidth` / `hardLineWidth` (defaults `78` / `90`)
: The formatter prefers to wrap at the soft width and always wraps, where a
  break is possible, at the hard one. The outermost expression breaks first,
  after a comma or an operator, and the continuation is indented one level.

`indent`, `useTabs` (defaults `4`, `true`)
: The indentation width and whether to use tabs. Set either to `null` to
  follow what the editor sends with the formatting request.

```m2,pane="Wrapped at the soft width"
result = someFunction(firstArgument, secondArgument, thirdArgument) +
    anotherFunction(fourthArgument, fifthArgument)
```

```m2,pane="breakAfterSemicolon = true"
total = sum L;
scaled = 3*total
```

## Fine print

- Formatting covers the whole document; there is no range formatting.
- Comments stay where they are, attached to the code around them, and line
  endings (`\n` or `\r\n`) are preserved.
- A file with syntax errors is left untouched rather than half formatted;
  `m2-fmt` reports `cannot format source with syntax errors`.
- The formatter never changes what code means: it only moves whitespace and
  line breaks.
