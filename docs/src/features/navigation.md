# Navigation, references, and rename

Navigation follows the role a name plays in the program, including local
shadowing and source order. The same spelling in two unrelated scopes is not
automatically treated as the same symbol.

```m2
transform = method(TypicalValue => ZZ)
transform ZZ := value -> value + 1

applyTransform = value -> transform value
answer = applyTransform 41
```

From this example you can:

- jump from `transform` to its declaration;
- jump to the matching installed implementation;
- find the references to either `transform` or `value`;
- rename the local parameter without changing an unrelated `value` elsewhere;
- inspect the type hierarchy for a known type.

## Across a project

The server searches `.m2` files under the project root supplied by the editor.
Workspace symbol search finds top-level names, and definition, implementation,
reference, and rename requests can continue into other indexed files. Open,
unsaved buffers take precedence over the file on disk.

Hidden directories, `target`, and `node_modules` are skipped. If cross-file
results are missing, first check that the editor chose the intended project
root.

## Links in documentation

Wiki-link references in comments and documentation can become clickable links:

```m2
-- [[normalizeInput]] prepares a value before dispatch.
normalizeInput = value -> value
```

The link can lead to a local definition, a workspace symbol, or known library
documentation. External library source itself requires the optional source path
described in [Install and connect your editor](../setup.md#optional-library-source-jumps).
