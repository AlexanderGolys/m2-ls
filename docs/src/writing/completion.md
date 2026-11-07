# Completion

Completion in `m2-ls` waits until it has something precise to say. An empty
expression could be any of thousands of library names, so instead of opening a
wall of suggestions there, it answers in the places where the context narrows
the choice to a handful.

| Where you are | What is offered | Opens on |
| --- | --- | --- |
| Inside the package string of `needsPackage`, `loadPackage`, `debug`, `importFrom`, or a `PackageImports`/`PackageExports` list | package names | `"` |
| Inside the file name of `load`, `needs`, or `input` | `.m2` files and folders | `"`, `/` |
| After `new` | types only | typing |
| A capitalized name inside a call | that callable's option keys | typing |
| After `=>` of an option | the values documented for that option | `>` |
| Any name, two characters in | the endings, when there are at most four | typing |

## Packages { #packages }

```m2
needsPackage "J|
```

Offers `JSON` and the other packages starting with `J` — every package the
catalog or your workspace knows. The same works in `loadPackage`, `debug`,
`importFrom`, and in the `PackageImports => {…}` and `PackageExports => {…}`
lists of `newPackage`.

## Files { #files }

```m2
load "lib/|
```

Offers the `.m2` files and the folders in `lib/`, found the way `load` finds
them: relative to the directory of the file you are editing, in the home
directory after `~/`, or at an absolute name. Pick a folder and its contents
follow; folders reached through a symbolic link count as folders.

- Names are written the way the string can hold them: `we"ird.m2` completes
  as `we\"ird.m2` inside `"…"` and verbatim inside `///…///`, where folders
  come without their trailing `/`, which would close the string.
- Hidden entries appear once you type their leading `.`.
- A folder you may not read offers nothing.

## Types after `new` { #new }

```m2
LocalType = new §type.readonly|Type§
instance = new Loc|
```

Offers `LocalDictionary` and the locally declared `LocalType`; a name that is
not a type, such as `Local`, is left out.

## Options { #options }

```m2
G = gb(I, Str|
```

Offers `Strategy`. Option keys in Macaulay2 are capitalized while locals
usually are not, so a capital letter inside a call is the cue. After the arrow,
the values documented for that option follow:

```m2
G = gb(I, Strategy => |
```

## Short endings for ordinary names { #names }

From the second character on, an ordinary name is completed — but only when the
prefix leaves at most four possible endings. `Loc` offers `Local` and
`LocalDictionary`; `ma` stays quiet, because dozens of names start with it.
Candidates come from bindings in scope at the cursor, the library, imported
packages, and the keywords of the language.

If the builtin catalog is unavailable, visible definitions found in imported
package sources also supply ordinary name completions. These suggestions need
no catalog documentation; types and documented option values may be unavailable.

## Fine print

- What is in scope follows source order: a binding written further down is not
  offered, and a package's names appear only after the line importing it.
- Completion never inserts snippets or parentheses; it finishes the name.
