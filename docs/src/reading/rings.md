# Rings and their variables

A ring you write out is also a list of its variables, and Macaulay2 code
reaches into it by position or by name all the time. `m2-ls` reads the ring
the way Macaulay2 builds it, so a subscript is never just "something of type
`S`" — it is a particular variable, or a mistake.

```m2
S = QQ[x_0 .. y_2, z, w_(1,1)]
S_6                         -- S_6 = z
T = S[t]
T_1                         -- T_1 = x_0
T_9
-- ^ error[E14]: index 9 out of bounds 0 .. 8; this ring has 9 variables,
--   counting those of a polynomial coefficient ring
```

## What a subscript selects

By position
: `S_i` counts the ring's own variables first, then those of a polynomial
  coefficient ring. `T = S[t]` has `t` at `0` and the eight variables of `S`
  after it, so `T_1` is `x_0`.

By name
: `"z"_S`, `x_1_S` and `w_(1,1)_S` look the variable up by its name. A name
  must be one of the variables exactly: after `A = QQ[a, b_1]`, `"b_1"_A` is a
  variable but `"b"_A` is not — `b` names the family, not a variable.

Lookups chain: `T_1` is `x_0`, so `T_1_S` looks up `x_0` in `S`. The
selected variable is what inline values report and what the checks below
compare against; [inlay hints](inlay-hints.md) show its type, the ring it
lives in.

## Inline values { #inline-values }

Where the program determines the variable, the server answers
`textDocument/inlineValue` with it: `S_6 = z`, `T_1 = x_0`, `"b_1"_A = b_1`.
Editors that request inline values show them beside the expression; Neovim
does not request them on its own.

## Mistakes caught { #diagnostics }

Both mistakes are errors Macaulay2 raises only when the line runs. `m2-ls`
reports them as you type:

```m2
A = QQ[a, b_1]
A_2
-- ^ error[E14]: index 2 out of bounds 0 .. 1; this ring has 2 variables,
--   counting those of a polynomial coefficient ring
"b"_A
-- ^ error[E15]: variable not found in ring: `b`; lookups by name find only
--   the ring's variables, as its subscripts select them
b_2_A
-- ^ error[E15]: indexed variable not found in ring: `b_2`; …
```

A quotient keeps the variables of the ring it divides: after
`Q = S/ideal(z)`, `Q_40` is reported against the eight variables of `S`.

## How subscripts are colored { #highlighting }

A subscript that spells one object of the kind of one of its parts reads as a
single token; any other keeps its parts, each colored by what it is.

`1_S`, `3_R`
: The number `1` taken into `S`: one `number`.

`RR_53`
: The real field of that precision: one `type`.

`S_1`
: `S` stays a `type`, the `_` is an operator with the `access` modifier, and
  the `1` is colored as the generator it selects.

`L_0`, `L#0`
: An access into a value: the `0` is a `property`, like a field name.

Themes can fade the `access` operator so the eye skips to the parts that
matter; see [semantic highlighting](semantic-highlighting.md#subscripts).

## One index, one variable { #indexed }

Each index of an indexed family is its own variable. With the cursor on `z_1`
after `R = QQ[z_0 .. z_2]`, [document highlights](document-highlight.md) and
[references](../navigating/references.md) find every `z_1` and the range
`z_0 .. z_2` that declares it — never `z_0` or `z_2`.

## Fine print

- Only rings the program writes out are known: a constructor such as
  `QQ[x, y]` or `S[t]`, or a quotient of one, assigned to a name. A ring
  returned by a function, or with a computed range such as `u_0 .. u_n`, is
  known only by its type, so its subscripts get no value and no error.
- An index must be a literal. `S_i` with a computed `i` is an ordinary access.
- A name is looked up as written; a string with escapes is not compared.
