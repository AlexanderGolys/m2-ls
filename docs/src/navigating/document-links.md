# Document links

Some text in a Macaulay2 file is really a pointer: a `[[name]]` link in a
comment, the package named in `needsPackage "Graphs"`, or the file named in
`load "lib/helper.m2"`. The server marks all of them as links, so the editor
can underline them and open their target on a click.

```m2
-- Normalizes the input before [[§property.documentation|dispatch§]] sees it.
normalize = §parameter.declaration|x§ -> §parameter|x§

needsPackage §namespace|"Graphs"§
loadPackage(§namespace|"Normaliz"§, §enumMember.option.readonly|Reload§ => §variable.readonly|true§)
load §namespace|"lib/helper.m2"§
```

`[[dispatch]]`
: Opens the definition of `dispatch` — in this file, another workspace file,
  or the library.

`"Graphs"`, `"Normaliz"`
: Open the package's source file. Every package-loading form counts:
  `needsPackage`, `loadPackage`, `debug`, `importFrom`, and the
  `PackageImports` and `PackageExports` lists of `newPackage`.

`"lib/helper.m2"`
: Opens the file the call runs, found the way `load` finds it; the same goes
  for `needs` and `input`. See [workspace files](workspace-files.md).

Targets are resolved only when the editor asks for one, by the same machinery
as [go to definition](go-to.md), and open at the exact line and column. A
package link needs the package's source on disk; see
[library sources](../getting-started/installation.md#library-sources-optional).
