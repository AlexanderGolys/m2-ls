# Documentation comments

Ordinary comments become documentation when their placement has one clear
meaning. There is no special syntax to learn: write `--` comments directly
above a definition, and they show up in [hover](../reading/hover.md) wherever
the name is used — in this file or any other file of the workspace.

```m2
-- Computes the Hilbert series of [[R]] modulo [[I]].
--
-- The result is a rational function in `T`; see [[reduceHilbert]]
-- for a simplified form.
hilbertOf = (R, I) -> hilbertSeries(R / I)
```

Hovering `hilbertOf` shows the prose, rendered as Markdown below the code.
Emphasis, lists, headings and code spans are kept; a bare `--` line is a
paragraph break.

## What attaches, and what does not { #attachment }

Consecutive full-line comments directly above a definition document it. A
blank line in between breaks the attachment on purpose, and so do trailing
comments and comments above ordinary expressions:

```m2
-- This remains an ordinary note.

answer := 42            -- trailing comments are never documentation

-- Neither is this one: it sits above an expression, not a definition.
print answer
```

`---` and `-** … **-` are accepted as explicit documentation markers too; the
whole marker is stripped, so no stray `-` turns into a Markdown bullet.

## Links { #links }

`[[name]]` refers to an object. Inside the comment it behaves like code:
[hover](../reading/hover.md), [go to definition](../navigating/go-to.md),
[references](../navigating/references.md), [document highlights](../reading/document-highlight.md),
and [rename](../navigating/rename.md) all understand it, and the editor shows it
as a clickable [document link](../navigating/document-links.md). Renaming
`reduceHilbert` updates the link above.

## Examples { #examples }

Fenced Macaulay2 code inside a comment is highlighted as code in hover:

````m2
-- Return the unit ideal.
--
-- ```m2
-- R = QQ[x, y]
-- unitIdeal R
-- ```
unitIdeal = R -> ideal 1_R
````

## Packages and functions from inside { #blocks }

A block comment before `newPackage` documents the package itself:

```m2
-*
# WonderfulModules

Tools for building wonderful modules.
*-
newPackage("WonderfulModules")
```

A block comment at the very start of a function body documents that function:

```m2
normalizeInput = value -> (
    -* Normalize a value before dispatch. *-
    value
    )
```

A block comment inside an unrelated nested expression has no clear owner and
stays an ordinary comment.

## Fine print

- Classic `doc ///…///` and `document {…}` documentation keeps working: it is
  parsed, [checked](../writing/diagnostics.md#catalog) (`D` rules),
  [highlighted](../reading/semantic-highlighting.md#embedded), and shown in hover.
- Documentation comments are for reading in the editor; Macaulay2's own help
  system does not see them.
