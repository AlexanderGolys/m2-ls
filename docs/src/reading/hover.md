# Hover

Rest the cursor on a name and ask "what is this?". Hover answers with the code
the name stands for, a one-line summary of what kind of object it is, and its
documentation — for a library function, the whole Macaulay2 help page.

Hovering `gb` in `G = gb ideal(x^2, y)` shows:

````markdown
```macaulay2
gb(Ideal) -> GroebnerBasis
```
`MethodFunctionWithOptions` · package `Core`

---

Compute a Gröbner basis

- **Result type:** GroebnerBasis
- …

## Description
See Gröbner bases for more information and examples.
…
## Methods
- `gb(Ideal)`
- `gb(Module)`
- `gb(Matrix)`

## Examples
…
````

## The signature your call uses

The first block is not the first method in the documentation — it is the one
*this call* dispatches to. `gb` has methods for `Ideal`, `Module` and `Matrix`;
the argument here is an `Ideal`, so that signature leads. When the argument's
type is only partly known, the remaining candidates follow under
**Also possible for this call**.

The same works for operators. Hovering the `+` of `s = 1 + 2` shows
`ZZ + ZZ -> ZZ`, the operator's documentation, its forms (binary, prefix) and
whether user code may install methods on it.

## Your own code

Local names hover with their defining code, their inferred type and their role:

```m2
f = §parameter.declaration|L§ -> (
    §variable.declaration|total§ := §method.readonly|sum§ §parameter|L§;    -- hover: `total := sum L`, Thing · user-defined binding
    §variable|total§
    )
```

A constructor named after a type makes that type whatever it is given, even
when the argument's own type is unknown: `ideal`, `monomialIdeal`,
`matrix`, `module`, `ring`, `monoid`, `net`, `vector` and `complex`. So the
whole chain below is typed:

```m2
R = QQ[x, y, z]
I1 = ideal R_*    -- Ideal
I = I1^3          -- Ideal
Q := R/I          -- QuotientRing
```

The output variables `oo`, `ooo`, `oooo` and `o1`, `o2`, … are typed as the
Macaulay2 REPL would leave them when running the file line by line. Every
top-level input is an output, even when a `;` mutes it, and each `;`-ended
statement takes its own number. A `null` value binds no `oN`. It also leaves
`oo` as it was, though it still moves `oo` to `ooo` and `ooo` to `oooo`:

```m2
1
"a";
null
c = oooo    -- ZZ: null moved 1 from ooo to oooo, and left "a" in oo
d = o3      -- Symbol: null bound nothing to o3
e = o2      -- String: the muted "a" is output 2
f = () -> oo    -- Thing: a function reads oo when it is called
```

A method function you wrote lists all its installations, with the codomains
they declare or that inference deduces:

```m2
-- Rank of the matrix, or the integer itself.
area = §function.readonly|method§()
area §typeParameter.readonly|Matrix§ := §decorator.readonly|ZZ§ => §parameter.declaration|m§ -> §method.readonly|rank§ §parameter|m§
area §typeParameter.readonly|ZZ§ := §decorator.readonly|ZZ§ => §parameter.declaration|n§ -> §parameter|n§
```

Hovering `area` anywhere shows `area = method()`, then **Methods**
`area(Matrix) -> ZZ` and `area(ZZ) -> ZZ`, and below a rule the comment above
the definition, rendered as Markdown — see
[Documentation comments](../beyond/documentation-comments.md). Hovering an
installation line itself pins that installation and lists the others.

Names defined in another file of the workspace hover with the code that
defines them there and their documentation page.

## Fine print

- What is in scope follows source order: a package's names gain hover
  information from the line that imports it on.
- A key such as the `value` in `output.value` is the symbol `value`, not the
  function or a local of that name, so it has no hover.
- Library documentation comes from the downloaded and cached catalog, so it matches the
  Macaulay2 release the catalog was generated from, not necessarily the one you
  have installed.
