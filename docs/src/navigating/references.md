# References

Find references lists every place that refers to the same thing as the name
under the cursor — scope-aware, so a different variable that happens to share
the spelling is never included.

```m2
value = 10                          -- global `value`
double = §parameter.declaration|value§ -> 2 * §parameter|value§         -- a parameter, unrelated to the global
print(§variable.readonly|value§ + §function|double§ §variable.readonly|value§)         -- both uses of the global
```

References on the global `value` return lines 1 and 3; on the parameter
`value`, only the two occurrences inside the lambda.

Local bindings
: A binding made in a function body or by `:=` in an inner scope stays in its
  file; its references are collected there alone.

Globals
: A top-level binding, or a name defined at top level in another file, is
  searched across the whole [workspace](go-to.md#workspace), skipping places
  where a local shadows it.

Documentation
: `[[name]]` links in [documentation comments](../beyond/documentation-comments.md)
  are references too. Quoted documentation page titles are also indexed:
  `TO`, `TOH`, `TO2`, `SeeAlso`, and `Subnodes` targets refer to matching `Key`
  entries across workspace files, in both SimpleDoc and hypertext documentation.
  Link display labels and ordinary prose strings are excluded. The include
  declaration flag controls whether `Key` entries are listed.

Indexed ring generators
: Each index is its own variable. After `R = RR[x, y_1, z_0 .. z_2]`,
  references on `z_1` are the range `z_0 .. z_2` that declares it and every
  `z_1` — never `z_0` or `z_2`. Document highlights follow the same rule.

The request's *include declaration* flag decides whether the binding itself is
listed.

## Fine print

- `x.k` means `x#(global k)`: the key is the symbol `k` itself, so it is not a
  reference to a local named `k`.
- An assignment `x = v` inside a nested function updates whatever local `x` is
  visible there, exactly as Macaulay2 does, so it counts as a reference to that
  local.
