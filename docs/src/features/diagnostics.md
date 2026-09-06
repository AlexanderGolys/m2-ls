# Diagnostics and quick fixes

Diagnostics cover syntax problems, likely mistakes, and a small number of
Macaulay2 idioms where the intended correction is unusually clear. Many of
them carry an action that can apply the change directly.

## Assignment or method installation?

Part assignment uses `=`:

```m2
values#0 := 1
```

Here the server can offer **Use `=` for part assignment**.

Installing a method uses `:=`:

```m2
f = method()
f ZZ = value -> value
```

Here it can offer **Use `:=` for method installation**. The distinction is
small on the page but changes the meaning of the code, so this is a particularly
valuable correction.

## Macaulay2 conventions

Option keys are conventionally capitalized:

```m2
G = gb(I, strategy => 4)
```

The corresponding action capitalizes `Strategy`.

Other actions include adding a missing method result annotation, protecting the
symbol itself rather than its current value, converting a troublesome string to
a raw string, removing redundant condition parentheses, and reshaping nested
conditionals into an `else if` chain.

## Smaller equivalent expressions

The server recognizes a few verbose patterns with direct Macaulay2 forms:

```m2
value = if value === null then fallback else value
candidate = if result =!= null then result else fallback
```

It can offer the coalescing forms `value ??= fallback` and
`result ?? fallback` respectively.

Diagnostics can be disabled globally or one rule at a time. See
[Configuration](../configuration.md#diagnostics) for examples.
