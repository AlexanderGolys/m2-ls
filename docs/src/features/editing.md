# Formatting and editor structure

Several features make a large file easier to scan even when you are not asking
a direct question about one symbol.

## Formatting

The formatter handles the whole document. It normalizes indentation and
spacing, chooses safe places to wrap long expressions, and can reshape control
flow according to your preferred layout.

```m2
result = if condition then (
    firstStep value;
    secondStep value
) else fallback
```

You can choose spaces or tabs, set preferred and firm line widths, keep
multiplication compact or spaced, and decide whether a semicolon starts a new
line. Line endings are preserved.

## Semantic color

If the editor supports semantic highlighting, names can be colored by their
role rather than spelling alone. Types, functions, option keys, properties,
package names, locals, and builtins can therefore remain visually distinct.

## Symbols and folding

The document outline presents bindings, assignments, and functions as a
hierarchy. Workspace symbol search looks across the project. Folding covers
parsed blocks and runs of comments, so explanatory sections can be collapsed as
a unit.

Document highlights also connect related syntax under the cursor: uses of a
binding, matching delimiters, and the significant words of a control-flow form.
