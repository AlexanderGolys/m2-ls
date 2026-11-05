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
