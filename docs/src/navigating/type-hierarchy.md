# Type hierarchy

Macaulay2 types form a tree rooted at `Thing`, and the type hierarchy shows any
type's place in it: its parent above, and every type deriving from it below.

```text
Thing
└─ BasicList
   └─ VisibleList
      ├─ List
      │  └─ VerticalList
      ├─ Array
      └─ Sequence
```

Invoke it on a type name — `List`, `Matrix`, `PolynomialRing`, or a type a
package defines — and expand supertypes or subtypes step by step. Each item
opens the type's definition when the library source is available, otherwise the
place you started from.

Source-defined and library types participate in the same hierarchy. A library
parent such as `BasicList` can list a source-defined child, and that child can
navigate back to its library parent. An alias selects the original type.
On an ordinary object, including a package object, preparation selects its
runtime class; on a type object, it selects the type itself.

The browser follows `parentOf` inheritance edges. Each item's detail states
both `classOf` and `parentOf` so a type's metaclass is not confused with its
parent. Library facts come from the downloaded catalog and work without
Macaulay2 installed. Unresolved facts produce no invented edges.

To inspect namespaces and both relations throughout the workspace, see
[Inspecting object hierarchies](object-hierarchies.md).

In Neovim: `:lua vim.lsp.buf.typehierarchy('supertypes')` or `'subtypes'`.
