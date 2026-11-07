# Macaulay2 that feels at home in your editor

`m2-ls` is a language server for [Macaulay2](https://macaulay2.com/). It reads
your `.m2` files the way Macaulay2 would — scopes, method installations,
packages, ring generators — and turns that into hover documentation, jumps,
renames, type hints, diagnostics and formatting, without running your code during analysis. Explicit runtime commands can run
files, cells, and package tests when M2 is installed.

```m2
R = §type.readonly|QQ§[§variable.declaration.algebraic|x0§, §variable.declaration.algebraic|x1§, §variable.declaration.algebraic|y§]
--     ^^ Ring variable `x0` is a multi-character symbol, not an indexed
--        variable; use `x_0` so the generators form one indexed family

area = §function.readonly|method§()
area §typeParameter.readonly|Matrix§ := §parameter.declaration|m§ -> §method.readonly|rank§ §parameter|m§       -- area: method · Matrix: domain · rank: method

sizeLabel = §parameter.declaration|L§ -> (
    §variable.declaration|n§ := #§parameter|L§;
    if §variable|n§ then "nonempty" else "empty"
    -- ^ if condition must have type `Boolean`, but this expression has type `ZZ`
    )
```

## What each capability gives you

| While you are… | `m2-ls` can… | Chapter |
| --- | --- | --- |
| Discovering an API | show documentation, the signature matching your call, options and result types | [Hover](reading/hover.md), [Signature help](reading/signature-help.md) |
| Reading code | color names by what they are bound to and show inferred types inline | [Semantic highlighting](reading/semantic-highlighting.md), [Inlay type hints](reading/inlay-hints.md) |
| Working with rings | tell which variable each subscript selects, and catch the ones that select none | [Rings and their variables](reading/rings.md) |
| Typing | suggest packages, types after `new`, option keys and values | [Completion](writing/completion.md) |
| Fixing code | explain Macaulay2-specific mistakes, suggest method dispatch and shared typical values, and apply available fixes | [Diagnostics](writing/diagnostics.md), [Code actions](writing/code-actions.md) |
| Running code | execute files and cells or run package tests on request | [Runtime commands](beyond/runtime-commands.md) |
| Tidying | format the whole document with your layout preferences | [Formatting](writing/formatting.md) |
| Inspecting objects | browse source and library inheritance, and inspect namespace, class, and parent relations | [Type hierarchy](navigating/type-hierarchy.md), [Object hierarchies](navigating/object-hierarchies.md) |
| Following a name | jump to declarations, definitions, the installation a call dispatches to, library source | [Go to definition](navigating/go-to.md) |
| Changing a name | find every reference and rename it, scope-aware, across files | [References](navigating/references.md), [Rename](navigating/rename.md) |

## Built on facts, not guesses

Everything `m2-ls` knows comes from two places: a catalog of the Macaulay2
library downloaded automatically when available, and static analysis of your files. Macaulay2
itself is never required. When it is installed, navigation can additionally
follow names into its library source.

When an answer depends on what the code does at runtime, the server shows less
rather than invent certainty: a type it cannot prove stays a range like
`↑List`, a name it cannot resolve stays unresolved, a jump with no known source
returns nothing.

Start with [Installation and editor setup](getting-started/installation.md).
