# Documentation that lives with code

m2-ls lets ordinary-looking comments become documentation when their placement
has one clear meaning. This keeps casual comments casual: detached notes,
trailing comments, and comments above ordinary expressions are not indexed as
documentation.

## Document an item

Write consecutive full-line `--` comments directly above the definition. Use a
bare `--` when the Markdown needs a blank line.

```m2
-- Computes the answer with [[helper]].
--
-- The result is suitable for small examples.
answer := helper + 1
```

The prose appears when hovering `answer`. Markdown emphasis, lists, headings,
and code spans are preserved.

A blank source line breaks the attachment deliberately:

```m2
-- This remains an ordinary note.

answer := 42
```

## Link to an object

Write `[[name]]` to refer to a local object. The name behaves like a reference
in the editor: hover, go to definition, find references, highlights, and rename
all understand it.

```m2
-- Builds an ideal using [[preferredGenerators]].
makeIdeal := I -> ideal preferredGenerators I
```

In generated pages, a wiki link becomes a link when the target has its own
documentation page. Otherwise it remains readable code instead of leading to a
missing page.

## Add examples

Fenced Macaulay2 examples use the usual Markdown spelling. The generated book
highlights them with Tree-sitter.

````m2
-- Return the unit ideal.
--
-- ```m2
-- R = QQ[x, y]
-- unitIdeal R
-- ```
unitIdeal := R -> ideal 1_R
````

## Document a package or function from inside

A `-* ... *-` block before the package declaration documents the package:

```m2
-*
# WonderfulModules

Tools for building wonderful modules.
*-
newPackage("WonderfulModules")
```

A block at the beginning of a function body acts as inner documentation for
that function. A block in an unrelated nested expression has no clear owner and
remains an ordinary comment.

```m2
normalizeInput := value -> (
    -* Normalize a value before dispatch. *-
    value
    )
```

## Build the pages locally

From the m2-ls checkout, generate and build a local book for one file or a
directory:

```bash
m2-ls docs path/to/package --output target/package-docs
```

Open `target/package-docs/book/index.html` in a browser. Only packages and items
with accepted documentation blocks receive pages. Pass `--no-build` to generate
the mdBook sources without running mdBook.

The command uses the highlighting assets from the m2-ls `docs` directory. When
running it elsewhere, point `M2_LS_DOC_ASSETS` at that directory.

## Current limits

- Documentation is attached by position; a physical blank line intentionally
  turns an item block back into ordinary comments.
- Generated wiki links only target other documented pages. Editor navigation
  can still resolve an undocumented local object to its source.
- Existing `doc ///...///` documentation remains indexed for compatibility,
  but the new comment style is the simpler choice for new code.
