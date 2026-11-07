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

A method function you wrote lists all its installations, with the codomains
they declare or that inference deduces:

```m2
-- Rank of the matrix, or the integer itself.
area = §function.readonly|method§()
area §typeParameter.readonly|Matrix§ := §typeParameter.readonly|ZZ§ => §parameter.declaration|m§ -> §method.readonly|rank§ §parameter|m§
area §typeParameter.readonly|ZZ§ := §typeParameter.readonly|ZZ§ => §parameter.declaration|n§ -> §parameter|n§
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
