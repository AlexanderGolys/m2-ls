# Completion that waits for context

Completion is intentionally selective. An empty expression could contain
almost anything, so `m2-ls` does not open an enormous list there. It offers
suggestions when the surrounding code or a sufficiently specific prefix makes
the likely choices small and useful.

## Package imports

Inside a recognized package-loading command, opening the string starts package
completion. Continuing to type narrows the list:

```m2
needsPackage "J"
```

At `J`, the suggestions include `JSON`. The same behavior is available in the
other recognized package-import forms.

## Types after `new`

After `new`, names are filtered to things that can be used as types:

```m2
LocalType = new Type
instance = new Loc
```

Completion after `Loc` can offer `LocalDictionary` and the locally declared
`LocalType`. A non-type name such as `Local` is omitted in this position.

## Options

When an argument looks like the beginning of an option key, completion uses the
options known for that callable:

```m2
G = gb(I, Str)
```

Request completion after `Str` to get `Strategy`. This is especially useful in
Macaulay2 because option keys are usually capitalized while ordinary local
variables usually are not. If an option has a documented set of values, those
can be suggested after the arrow as well.

## A short ending for an ordinary name

For ordinary names, completion begins after two typed characters and appears
only when the prefix has a few possible endings. For example, `Loc` is narrow
enough to offer `Local` and `LocalDictionary`.

This restraint is deliberate: completion helps finish a thought without
turning every expression into a search through the entire Macaulay2 library.
