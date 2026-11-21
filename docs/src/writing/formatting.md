# Formatting

The formatter lays a file out the way the Macaulay2 Core library is written:
how far a bracket indents, where closing brackets go, which operators get
spaces. Each rule is one choice in your settings, applied the same way to
every file.

By default it changes indentation and spacing within lines, and keeps every
line break where you wrote it:

```m2,pane="Before"
normalize = (v,limit) -> (
if v === null then 0
else if v > limit then limit else v
  )
total = sum apply({1,2,3}, i -> 2 * i+1)
scaled = apply(total, x -> x#(i + 1))
```

```m2,pane="After"
normalize = (v,limit) -> (
     if v === null then 0
     else if v > limit then limit else v
     )
total = sum apply({1, 2, 3}, i -> 2 * i + 1)
scaled = apply(total, x -> x#(i+1))
```

Format from the editor (`:lua vim.lsp.buf.format()` in Neovim), or from a
terminal and CI with [`m2-fmt`](../beyond/command-line.md#formatting); both
produce byte-identical results. Every key below is listed in the
[configuration reference](../getting-started/configuration.md#formatting).

Expression simplifications are separate [code actions](code-actions.md#rewrites).
For example, formatting keeps `linked === true`; **Simplify Boolean comparison**
can replace it with `linked` when its type is known to be Boolean.

## Indentation { #indentation }

Each line is indented from the line above it: one width further for every
bracket that line leaves open, one width back for every bracket it closes.

```m2
f = (a,b) -> (
     if a then (
          b)
     else a)
```

That one rule gives the layout of Core:

- A line starting with `)` or `}` keeps the indentation of the body it
  closes; the line after it moves back. `indent.closers = "opener"` moves the
  closer back instead, to the line that opened it.
- A line that opens two brackets, as `scan(L, x -> (`, indents the next one
  twice.
- `then`, `else`, `or` and a line after `:=` or a trailing operator get no
  extra indentation: an `else` lands under its `if`.
- Lines inside a multi-line string or block comment are left exactly as they
  are.

A bracket indents by `indent.width` columns, 5 by default, as Emacs `M2-mode`
long did. The indentation is written as Emacs writes it: a tab for every
`indent.tabWidth` columns, 8, then spaces. `indent.whitespace = "spaces"`
writes spaces only, and `width` equal to `tabWidth` gives one tab per bracket.

## Spacing { #spacing }

Within a line, every gap between two tokens gets the first rule that applies:

1. In a string or comment: untouched.
2. Before a trailing comment: one space.
3. Just inside brackets: none, `(x)`, unless `spacing.bracketsInside` pads
   that kind of pair, as `{ 1, 2 }`; empty brackets close up as `{}`.
4. Before `,` and `;`: none.
5. After `,`: one space, `f(a, b)`, or none under `spacing.comma = "tight"`;
   nothing beside an empty slot, `f(a,,b)`. Lambda parameters are tight,
   `(x,y) ->`, as `spacing.lambdaParamComma` says.
6. After `;` followed by more code: one space.
7. Between a keyword and a bracket: one space, `then (`.
8. Between a function and its parenthesized argument: none, `f(x)`, also in
   method installations, `f(ZZ,Ring) := …`.
9. Around a binary operator: as `spacing.operators` lists it, with `+ - * /`
   tight inside an index, `x#(i+1)`.
10. Between a prefix operator and its operand: none, `-x`, `#L`, unless
    `spacing.operators.spacedPrefix` lists it, `<< x`.

Anything else keeps the spacing you wrote.

The operator lists say which operators are spaced (`a := b`, `a + b`) and
which are tight (`a^b`, `x_i`, `L#0`); one in neither list keeps the spacing
you wrote. Operators spelled as words,
`and`, `or`, `not`, are always spaced. Closing up a gap never runs two tokens
together: `L#0 .?key` keeps its space, since `0.` would read as a number.

Padding of more than one space stays when it lines a token up with the same
token on the line above or below:

```m2
isDocumentableThing    String :=
isDocumentableThing  Sequence := key -> false
```

Rule-only comment lines of 60 to 90 dashes become `comments.bannerWidth`
dashes, 77. Runs of blank lines shrink to one, and blank lines just after an
opening bracket or before a closing one go.

## Line breaks { #line-breaks }

`lines.breaks` decides whether the formatter may move line breaks.

`"keep"` (default)
: Line breaks stay where they are; only indentation and spacing change.

`"wrap"`
: Lines longer than `lines.softWidth` (100) are broken, at the best place
  they offer: after an opening `(` or `{`, then after the outermost commas,
  then before `else`, `then`, `or`, `and`, `<<` or `|`, then after `:=` or
  `=`, and only as a last resort after another operator.

  ```m2
  result = someFunction(
       firstArgument, secondArgument,
       thirdArgument)
  ```

`"normalize"`
: Also lays out every multi-line construct the way Core does, as the
  `layout` keys say. A function body with several statements breaks after
  `-> (`, one statement per line, and its `)` hugs the last one. A multi-line
  `{` list closes with `}` on its own line. An `if` that fits on one line is
  joined into one; otherwise `else` starts a line under its `if`, and
  `else if` stays together.

```m2,pane="Before"
f = x -> ( y := x + 1; z := y * 2; if z > 10
  then z
  else y)
opts = {
  Strategy => Linear,
  Limit => 10}
```

```m2,pane="normalize"
f = x -> (
     y := x + 1;
     z := y * 2;
     if z > 10 then z else y)
opts = {
     Strategy => Linear,
     Limit => 10
     }
```

## Profiles { #profiles }

`profile = "core"` (default) is the layout described above. `profile =
"modern"` is the four-column style of the newer Core files and the current
Emacs `M2-mode`: width 4 with spaces, and long lines wrapped. A key you set
yourself wins over the profile:

```toml
[formatting]
profile = "modern"

[formatting.indent]
width = 2
```

[Profiles](formatting-profiles.md) lists every value each one sets.

## Redundant parentheses { #parentheses }

Parentheses that change nothing about how the code parses can be removed by
the **Remove redundant parentheses in file** source action, and, with
`redundantParentheses = "format"`, by every format:

```m2,pane="Before"
§variable|area§ = (§variable|w§ * §variable|h§) + (§variable|a§ + §variable|b§) * §variable|c§
if (§variable|ready§) then §variable|go§(§variable|x§)
```

```m2,pane="After"
§variable|area§ = §variable|w§ * §variable|h§ + (§variable|a§ + §variable|b§) * §variable|c§
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
  line breaks, evens out the dashes of section rules, and, with
  `redundantParentheses = "format"`, removes parentheses the parse does not
  need. It parses its own result once more and leaves the file as it is if
  the syntax came out different, logging why.
- Formatting is a fixpoint: formatting a formatted file changes nothing.
