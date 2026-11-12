# Rings and their variables

A ring you write out is also a list of its variables, and Macaulay2 code
reaches into it by position or by name all the time. `m2-ls` reads the ring
the way Macaulay2 builds it, so a subscript is never just "something of type
`S`" — it is a particular variable, or a mistake.

```m2
S = §type.readonly|QQ§[§variable.declaration.algebraic|x_0§ .. §variable.declaration.algebraic|y_2§, §variable.declaration.algebraic|z§, §variable.declaration.algebraic|w_(1,1)§]
S_§variable.algebraic|6§                         -- S_6 = z
T = §type|S§[§variable.declaration.algebraic|t§]
T_§variable.algebraic|1§                         -- T_1 = x_0
T_§variable.algebraic|9§
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

## Computed values { #computed-values }

Inline values also show what an expression certainly evaluates to, computed
without running Macaulay2. Only what is cheap and certain is evaluated:

```m2
n = 5
2^10 + n                     -- = 1029
7 // 2 * 3                   -- = 9
(-7) % 3                     -- = 2 (the remainder is never negative)
binomial(10, 3)              -- = 120
4/2 === 2                    -- = false (4/2 is a QQ value)
R = QQ[x, y, z]              -- QQ[x, y, z] = 3 variables, char 0, dim 3
dim R                        -- = 3
char (ZZ/101)                -- = 101
M = matrix {{1, 2, 3}, {4, 5, 6}}
M * transpose M              -- = 2×2 matrix
ideal(x^2, y)                -- = ideal with 2 generators
```

- **Exact numbers and logic.** `ZZ` and `QQ` arithmetic, comparisons, `and`,
  `or`, `not`, and `binomial`, `gcd`, `lcm`, `abs`, `max` and `min`. The
  interpreter computes these natively and ignores installed methods, so the
  result cannot depend on the rest of the program. Division by zero, a
  result beyond 128 bits and an exponent above 4096 are not evaluated.
- **Names.** A name is followed to its value only when the program assigns
  it exactly once, so the value cannot depend on control flow. `m = 2` and
  `m = m + 1` leave `m` unknown.
- **Rings.** `numgens`, `dim` and `char` of `ZZ`, `QQ`, `ZZ/p`, `GF q` and
  polynomial rings over them (a polynomial ring adds its variables to the
  dimension of its coefficient ring). A quotient by an ideal, `RR` and `CC`
  stay unknown.
- **Matrices and ideals.** A matrix written out has its dimensions, `numRows`,
  `numColumns`, `transpose`, sums and products follow; an ideal written out
  has its generator count.

A bare literal or name shows nothing, since it would only repeat itself.

## Mistakes caught { #diagnostics }

Both mistakes are errors Macaulay2 raises only when the line runs. `m2-ls`
reports them as you type:

```m2
A = §type.readonly|QQ§[§variable.declaration.algebraic|a§, §variable.declaration.algebraic|b_1§]
A_§variable.algebraic|2§
-- ^ error[E14]: index 2 out of bounds 0 .. 1; this ring has 2 variables,
--   counting those of a polynomial coefficient ring
"b"_§type|A§
-- ^ error[E15]: variable not found in ring: `b`; lookups by name find only
--   the ring's variables, as its subscripts select them
b_§property|2§_§type|A§
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
: `S` stays a `type`, the `_` is an operator with the `memberAccess` modifier, and
  the `1` is colored as the generator it selects.

`L_0`, `L#0`
: An access into a value: the `0` is a `property`, like a field name.

Themes can fade the `memberAccess` operator so the eye skips to the parts that
matter; see [semantic highlighting](semantic-highlighting.md#subscripts).

## One index, one variable { #indexed }

Each index of an indexed family is its own variable. With the cursor on `z_1`
after `R = QQ[z_0 .. z_2]`, [document highlights](document-highlight.md) and
[references](../navigating/references.md) find every `z_1` and the range
`z_0 .. z_2` that declares it — never `z_0` or `z_2`.

## Which ring a name denotes { #use }

Constructing a ring binds the names of its variables, and a later ring with the
same names takes them over. `use R` rebinds the names of `R`'s own variables to
`R`'s generators again, exactly as constructing it did; the variables of its
coefficient rings keep the names they have. `m2-ls` follows both in source
order, so after

```m2
R = QQ[x, y]
S = QQ[x]
x         -- an element of S
use R
x         -- an element of R
```

the type of `x` changes where `use R` runs. Each ring keeps all its own names
whatever the global names currently denote, which is why `"x"_R` still finds
`R`'s generator after `S` took the name `x`.

## Which ring an expression lives in { #lineage }

Every ring construction makes a new ring: `QQ[x] === QQ[x]` is false, and so
is `A/I === A/I`. Elements of two constructions never mix, even when the
constructions are written the same. What does mix is a ring with the rings it
is built over, its coefficient rings and the ring it is a quotient of. For

```m2
A = ZZ[x, y]
B = A[y, z]/ideal(y)
C = B[x, z]/ideal(x - z)
(1 + x - z) * y
```

the last line lives in `C`, and the inline value says so: `(1 + x - z) * y in C`.

A number joins a ring when it promotes to the field the ring is built on, so
`ZZ[x]` rejects `1/2`, `QQ[x]` accepts it but rejects `1.5`, and `RR[x]`
accepts both but not `ii`. Two distinct rings, `RR[x]` and `CC[x]` included,
never mix. Wherever the analysis cannot tell which ring a name denotes, because
the name is assigned twice or the ring is built in a loop, it shows nothing.
Inside the quotient clause of `B[x, z]/ideal(x - z)` the generators already
belong to `C`, though in the interpreter they still generate the ring before
the quotient.

## Fine print

- Only rings the program writes out are known: a constructor such as
  `QQ[x, y]` or `S[t]`, or a quotient of one, assigned to a name. A ring
  returned by a function, or with a computed range such as `u_0 .. u_n`, is
  known only by its type, so its subscripts get no value and no error.
- An index must be a literal. `S_i` with a computed `i` is an ordinary access.
- A name is looked up as written; a string with escapes is not compared.
