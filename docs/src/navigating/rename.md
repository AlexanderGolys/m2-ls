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
`[[name]]` links in documentation comments follow along.

Before anything changes, the editor asks the server whether the name can be
renamed and highlights its range; only names your code binds qualify. The new
name must be a valid Macaulay2 identifier — a letter followed by letters and
digits — otherwise the rename is refused instead of producing code that does
not parse.

To rename or move a file, use your editor's file tree: the loads that run it
follow along, see [workspace files](workspace-files.md#moving-and-renaming-files).

## Fine print

- Library and package names cannot be renamed: renaming `gb` in one file would
  silently break every call it stands for.
- Strings that merely spell the name, other than documentation links, are not
  changed.
