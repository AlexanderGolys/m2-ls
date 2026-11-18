# Documentation

Read library help and your own package documentation without leaving the code.
`m2-ls` connects documentation to the same bindings used by navigation, so a
local function shadowing a library name gets its own description.

[Hover](hover.md)
: Ask what a name means at its use site. The response combines defining code,
  inferred type, applicable signatures, and documentation. For a library callable,
  the cached help page can include descriptions, methods, and examples.

[Signature help](signature-help.md)
: Keep the accepted domains and codomains visible while entering arguments.
  Local method installations and catalog signatures supply the alternatives.

[Documentation comments](../beyond/documentation-comments.md)
: Attach Markdown prose to a definition in your own source. References written
  as `[[name]]` become navigable links and follow binding renames.

[Documentation diagnostics](../writing/diagnostics.md#catalog)
: Check SimpleDoc sections and `document` calls while writing a package. The
  `D` rules cover missing or duplicate keys, invalid links, section structure,
  undocumented exports, and placement relative to package documentation setup.

## Inputs and outputs on a page { #items }

A page's `Inputs` and `Outputs` name what they describe: `n:ZZ` in SimpleDoc,
`"n" => ZZ` or a bare `"n"` in a `document` list. The page's `Usage` and its
monospace text — `TT "n"`, `{\tt n}`, `@TT "n"@` — usually name the same
items, wherever on the page they appear:

```m2
document {
    Key => (exp, RingElement),
    Usage => "exp x",
    Inputs => {"x"},
    Outputs => {{"the exponential of ", TT "x", ", provided ", TT "x", " is nilpotent"}}
    }
```

Each `x` here is the input: highlighted as a parameter (an output as a
variable), and go to definition, highlights, references and rename treat
them as one name. Monospace text only says *typewriter*, not *a name*, so
only text spelling exactly an item of its own page is linked; `TT "x^2"`
or a `TT "x"` on another page stays prose.

## Where the information comes from

Library prose and signatures come from the downloaded catalog. You can read
that documentation without a local M2 interpreter; its content reflects the
Macaulay2 release used to build the catalog. Installing library source adds
source navigation rather than replacing the catalog help pages.

Workspace definitions contribute their defining code and attached comments.
A name must be visible at the cursor: importing a package later in the file
does not make its documentation available earlier.

There is no separate documentation-generation or runtime documentation-query
command. Use [explicit runtime commands](../beyond/runtime-commands.md) to run
code or tests, and Macaulay2's own tools to build package documentation.
