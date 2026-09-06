# Hover, signatures, and type hints

These features answer three different questions without making you leave the
file:

- **Hover:** What is this name, and what does it do?
- **Signature help:** Which form of this call am I currently writing?
- **Type hints:** What can this expression evaluate to?

## Hover and signature help

Consider a local method alongside a builtin call:

```m2
successor = method(TypicalValue => ZZ)
successor ZZ := n -> n + 1

R = QQ[x, y]
I = ideal(x^2, y^2)
result = successor numgens I
```

Hover can show information for both local names and known library objects.
Inside a call, signature help follows the active argument and shows the
available method forms, including known options and result types.

Imported package names become available after their import, so the information
shown at a particular line follows what the file has made visible by then.

## Type hints

The default hints concentrate on places where the type adds information, such
as bindings and function results. Two settings can reveal progressively more:

- `expressionTypes` adds types for arguments and subexpressions, plus known call
  results.
- `allKnownTypes` also includes obvious literals and collections.

You may see compact forms such as:

- `RR | ZZ`: either of these types is possible.
- `ZZ?`: either `ZZ` or `null` is possible.
- `↑List`: some known subtype of `List`, without claiming one exact subtype.

The notation reflects uncertainty instead of hiding it. A literal can have an
exact type while the result of a call may only have a useful range.
