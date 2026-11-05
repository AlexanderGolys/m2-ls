# Go to definition and friends

Four jumps, four different questions. In Macaulay2 they genuinely differ: a
name can be assigned many times, and a method function's behavior lives in
installations scattered across files.

```m2
area = method()                 -- ① declaration and definition of `area`
area Matrix := m -> rank m
area ZZ := n -> n               -- ③ what `area 3` dispatches to
counter = 0                     -- ② declaration of `counter`
counter = counter + 1           -- ② another definition
r = area 3
```

Go to declaration
: Where the name was first bound — for `counter`, the line `counter = 0`. A
  name nothing defines is declared where Macaulay2 first reads it, as it
  becomes a global symbol there.

Go to definition
: Every assignment that defines it: both `counter` lines. For a library name,
  the line in the package source that defines it.

Go to implementation
: The installations a method function dispatches to. On `area` in `area 3`
  it is `area ZZ := …` — the installation for the argument's type — and on
  `area` alone, every installation. Library methods jump to their
  installations in Core and the loaded packages.

Go to type definition
: The definition of the value's type: on `M` after `M = matrix {{1}}`, the
  definition of `Matrix`; on an instance of your own `T = new Type of List`,
  the line defining `T`.

## Across files { #workspace }

The jumps cross into every file the workspace knows: the files open in the
editor, every file they load with `load`, `needs` or `input`, every user
package they import, transitively, and your `init.m2`. Files loaded this way
need not be open. Unsaved buffers take precedence over their file on disk, and
changes made outside the editor are picked up by the file watcher.

On the file name of a `load`, `needs` or `input` call, go to definition opens
the file, found the way `load` finds it; see
[workspace files](workspace-files.md).

Packages follow Macaulay2's visibility rules: a global defined inside a
`newPackage` section belongs to the package; other files see it only if it is
exported.

## Into the Macaulay2 library { #library }

When the library source is available ([setup](../getting-started/installation.md#library-sources-optional)),
definition, declaration, type definition and implementation continue into it:
`gb` jumps to where Core defines it, `Matrix` to its type definition, and
implementation on `det` lists its installations across the loaded packages.
Inside a library file, the package's private names resolve too. A package's
files are read the first time a jump needs them.

> [!NOTE]
> Functions compiled into the interpreter, such as `apply`, have no Macaulay2
> source. Their jumps return nothing rather than a guessed location.

## Fine print

- Without library source, jumps into the library return no location; hover,
  signatures and types keep working from the downloaded and cached catalog.
- [`sources.enabled = false`](../getting-started/configuration.md#sources)
  keeps navigation inside your own files.
