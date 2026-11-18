# Rename

Rename changes a binding and every reference to it in one edit, leaving
unrelated names with the same spelling alone.

```m2,pane="Before"
value = 10
double = §parameter.declaration|value§ -> 2 * §parameter|value§
print(§variable.readonly|value§ + §function|double§ §variable.readonly|value§)
```

```m2,pane="After: rename the parameter to n"
value = 10
double = §parameter.declaration|n§ -> 2 * §parameter|n§
print(§variable.readonly|value§ + §function|double§ §variable.readonly|value§)
```

The set of edited places is exactly what [references](references.md) finds:
a local renames within its file, a global across every workspace file, and
`[[name]]` links in documentation comments follow along. An input or output
named on a documentation page renames with the page's text naming it, its
`Usage` and `TT "x"` mentions (see [inputs and outputs](../reading/docs.md#items)).

Before anything changes, the editor asks the server whether the name can be
renamed and highlights its range; only names your code binds qualify. The new
name must be a valid Macaulay2 identifier — a letter followed by letters and
digits — otherwise the rename is refused instead of producing code that does
not parse.

To rename or move a file, use your editor's file tree: the loads that run it
follow along, see [workspace files](workspace-files.md#moving-and-renaming-files).

## Renaming a package { #packages }

A package your workspace declares renames from its `newPackage "Shapes"`
string, from a `needsPackage "Shapes"` naming it, or from the `Shapes` symbol
in a file that does either. One edit rewrites every file naming it — the
package-name strings, its symbol, its documentation keys — and, because
Macaulay2 finds package `Shapes` in `Shapes.m2` with its auxiliary files in
`Shapes/`, renames those too, rewriting the `load "Shapes/…"` calls that reach
them:

```text
Shapes.m2       →  Polygons.m2     newPackage "Polygons", load "Polygons/area.m2"
Shapes/area.m2  →  Polygons/area.m2
user.m2                            needsPackage "Polygons"; Polygons.Dictionary
```

Moving the file needs an editor that advertises file renames in workspace
edits, as Neovim and VS Code do; another editor gets an error saying so, and
you can rename the file first, then the package.

## Fine print

- Library names cannot be renamed: renaming `gb` or the package `JSON` in your
  files would silently break every call they stand for. Renaming a package no
  workspace file declares is refused with that reason.
- Inside a file the package loads, such as `Shapes/area.m2`, the bare `Shapes`
  symbol does not start a package rename; start it from `Shapes.m2` or an
  importer, which renames the fragment's uses as well.
- Strings that merely spell the name, other than documentation links, are not
  changed.
