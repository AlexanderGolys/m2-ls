# Formatting

The formatter rewrites the whole document into one consistent layout:
indentation, spacing around operators and commas, where long expressions wrap,
and how control flow breaks across lines.

Expression simplifications are separate [code actions](code-actions.md#rewrites).
For example, formatting keeps `linked === true`; **Simplify Boolean comparison**
can replace it with `linked` when its type is known to be Boolean.

```m2,pane="Before"
normalize = (§parameter.declaration|v§,§parameter.declaration|limit§) -> (
if §parameter|v§ === §variable.readonly|null§ then 0 else if §parameter|v§ > §parameter|limit§ then §parameter|limit§ else §parameter|v§
)
total = §method.readonly|sum§ §function.defaultLibrary.readonly|apply§({1,2,3}, §parameter.declaration|i§ -> 2 * §parameter|i§+1)
```

```m2,pane="After"
normalize = (§parameter.declaration|v§, §parameter.declaration|limit§) -> (
    if §parameter|v§ === §variable.readonly|null§ then
        0
    else if §parameter|v§ > §parameter|limit§ then
        §parameter|limit§
    else §parameter|v§
)
total = §method.readonly|sum§ §function.defaultLibrary.readonly|apply§({1, 2, 3}, §parameter.declaration|i§ -> 2*§parameter|i§ + 1)
```

Format from the editor (`:lua vim.lsp.buf.format()` in Neovim), or from a
terminal and CI with [`m2-fmt`](../beyond/command-line.md#formatting) — both
produce byte-identical results.

## Control-flow layout { #control-flow }

[`controlFlowLayout`](../getting-started/configuration.md#formatting) decides
how an `if` that stands on its own line in a block is laid out. The same
function under each value:

```m2,pane="compact"
normalize = (§parameter.declaration|v§, §parameter.declaration|limit§) -> (
    if §parameter|v§ === §variable.readonly|null§ then 0 else if §parameter|v§ > §parameter|limit§ then §parameter|limit§ else §parameter|v§
)
```

```m2,pane="multiline"
normalize = (§parameter.declaration|v§, §parameter.declaration|limit§) -> (
    if §parameter|v§ === §variable.readonly|null§ then
        0
    else if §parameter|v§ > §parameter|limit§ then
        §parameter|limit§
    else
        §parameter|v§
)
```

```m2,pane="multilineCompactElse (default)"
normalize = (§parameter.declaration|v§, §parameter.declaration|limit§) -> (
    if §parameter|v§ === §variable.readonly|null§ then
        0
    else if §parameter|v§ > §parameter|limit§ then
        §parameter|limit§
    else §parameter|v§
)
```

```m2,pane="leadingKeywords"
normalize = (§parameter.declaration|v§, §parameter.declaration|limit§) -> (
    if §parameter|v§ === §variable.readonly|null§
    then 0
    else if §parameter|v§ > §parameter|limit§
    then §parameter|limit§
    else §parameter|v§
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

`operators.spacesAround` / `operators.compact`
: Which operators get a space on each side — `a + b`, `<< x` — and which are
  written tight — `a^b`, `-x`, `n!`. Each has a `binary` and a `unary` list;
  an operator in neither keeps the spacing you wrote. Operators spelled as
  words (`or`, `and`, `xor`, `not`, `SPACE`, and their qualified forms such
  as `Core$or`) are always spaced; listing them changes nothing. Listing an operator in one list is enough to take it out of the
  other's defaults:

  ```toml
  [formatting.operators.compact]
  binary = ["+", "-", "^", "_", "#"]   # a+b-c, x_i^2
  ```

`brackets.spacesInside` / `brackets.compact`
: Which brackets are padded as `{ 1, 2 }` and which are written `{1, 2}`,
  named `"{}"`, `"()"`, `"[]"` and `"<||>"`. Unlisted brackets keep what you
  wrote, and empty ones are always `{}`.

`softWrapWidth` / `hardLineWidth` (defaults `78` / `90`)
: The formatter prefers to wrap at the soft width and always wraps, where a
  break is possible, at the hard one. The outermost expression breaks first,
  after a comma or an operator, and the continuation is indented one level.

`indent` (default `"editor"`)
: `"editor"` indents as the editor asks with each formatting request,
  `"tab"` with tabs, and a number with that many spaces. `tabWidth` sets how
  wide a tab counts for the line widths; the editor's tab size when unset.

```m2,pane="Wrapped at the soft width"
result = §variable|someFunction§(§variable|firstArgument§, §variable|secondArgument§, §variable|thirdArgument§) +
    §variable|anotherFunction§(§variable|fourthArgument§, §variable|fifthArgument§)
```

```m2,pane="breakAfterSemicolon = true"
total = §method.readonly|sum§ §variable|L§;
scaled = 3*§variable|total§
```

## Redundant parentheses { #parentheses }

Parentheses that change nothing about how the code parses can be removed by
the **Remove redundant parentheses in file** source action, and, with
`redundantParentheses = "format"`, by every format:

```m2,pane="Before"
§variable|area§ = (§variable|w§*§variable|h§) + (§variable|a§ + §variable|b§)*§variable|c§
if (§variable|ready§) then §variable|go§(§variable|x§)
```

```m2,pane="After"
§variable|area§ = §variable|w§*§variable|h§ + (§variable|a§ + §variable|b§)*§variable|c§
if §variable|ready§ then §variable|go§ §variable|x§
```

Only pairs that are plainly redundant by where they stand are considered:

Around an atom
: A name, a literal, a list, array or sequence, or another pair: `(x)`,
  `({1, 2})`, `((a + b))`.

In a delimited place
: Anywhere keywords, commas, brackets, an assignment or the end of a
  statement already delimit the expression: `if (c) then`, `{(a + b), c}`,
  `x = (a + b)`.

Around a tighter operation
: An operand whose operator binds more tightly than the one it belongs to, or
  as tightly on the side it groups to: `(a*b) + c`, `a - (b*c)`,
  `(a + b) + c`, by the precedences of the Macaulay2 grammar.

Around a call's argument
: `f(x)` → `f x`, `f(x^2)` → `f x^2`, `sin(x)^2` → `sin x^2` (it squares inside
  the call either way). [`callParentheses = "keep"`](../getting-started/configuration.md#formatting)
  spares them.

Each candidate is then confirmed: the cell around it, parsed again without the
pair, must have exactly the syntax tree it had with the parenthesized
expression in place. So a pair stays whenever Macaulay2 would read the code
differently, as in `f(-1)`, where `f -1` subtracts. Where dropping a
parenthesis would glue two tokens together, a space takes its place:
`if(x)then` becomes `if x then`.

Three kinds of parentheses are never touched: those spanning lines, which keep
their content free of the newline rule; function bodies, `x -> (x + 1)`, which
can then later span lines or gain statements; and blocks holding statements,
`(a; b)`. A function's parameter list, `(x) -> x`, is no grouping at all:
unlike `x -> x`, it takes exactly one argument.

`"off"` hides the action; formatting then never touches parentheses either.

## Fine print

- Formatting covers the whole document; there is no range formatting.
- Comments stay where they are, attached to the code around them, and line
  endings (`\n` or `\r\n`) are preserved.
- A file with syntax errors is left untouched rather than half formatted;
  `m2-fmt` reports `cannot format source with syntax errors`.
- The formatter never changes what code means: it only moves whitespace and
  line breaks, and, with `redundantParentheses = "format"`, removes
  parentheses the parse does not need.
