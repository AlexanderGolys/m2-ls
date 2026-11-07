# Document highlights

Leave the cursor on something and every related piece of the file lights up.
What counts as related depends on what is under the cursor.

```m2
total = 0
scan(§variable|L§, §parameter.declaration|value§ -> §variable.declaration|total§ = §variable|total§ + §parameter|value§)
f = §parameter.declaration|value§ -> if §parameter|value§ > 0 then §parameter|value§ else -§parameter|value§
```

On a name
: every occurrence of the same binding, scope-aware. With the cursor on the
  `value` of line 2, only that lambda's `value`s light up — the parameter of
  `f` is a different variable. The write (`total =`) and the reads are told
  apart, so editors can shade them differently.

On a keyword
: the other keywords of the same construct: `if`/`then`/`else`,
  `for`/`in`/`from`/`to`/`list`/`do`, `while`/`do`, `try`/`then`/`else`.
  A nested construct keeps its own set.

On `return`, `break` or `continue`
: the function or loop it leaves, together with every other exit from it.
  Resting on the `->` of a function or on the keyword of a loop highlights all
  its exits — handy for spotting an early `return` in a long body.

On an indexed ring generator
: that one variable: every access to the same index and the range in the
  ring constructor declaring it, so `z_1` lights up `z_0 .. z_2` but never a
  `z_0`. See [Rings](rings.md#indexed).

On a package name
: every mention of that package, including the string in `needsPackage "JSON"`
  — even before the package is loaded.

On a `;`
: what resting on the expression before it would highlight, so you can park
  the cursor at the end of a line.

A library name that the file never rebinds highlights all its uses; a local
that shadows it is a different symbol and stays dark.
