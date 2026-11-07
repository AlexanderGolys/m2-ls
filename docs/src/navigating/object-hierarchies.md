# Inspecting object hierarchies

Run `m2-ls.printHierarchies` to inspect the objects and namespaces retained for
the current workspace. The report includes live buffers, indexed files they
load, package objects, and the library objects available to those files.

```m2
Meta = new §type.readonly|Type§ of §type.readonly|SelfInitializingType§
Entry = new §type|Meta§ of §type.readonly|BasicList§
entry = §type|Entry§ {}
```

These three values illustrate why the relations are separate:

| Object | `classOf` | `parentOf` |
| --- | --- | --- |
| `Meta` | `Core$Type` | `Core$SelfInitializingType` |
| `Entry` | `Meta` | `Core$BasicList` |
| `entry` | `Entry` | not a type-inheritance edge |

Subtype checks follow `parentOf`. A type's metaclass is its `classOf`, so
`Entry` is a subtype of `BasicList`; its class being `Meta` does not make it a
subtype of `Meta`. The same relation queries apply to source and library
objects. Unknown facts stay unknown.

## Namespaces and qualified names

The package tree shows dictionary ownership and bindings independently of type
inheritance. It includes private and public package dictionaries, the package
name dictionary, User dictionaries, and runtime lexical dictionaries.

| Name | Meaning |
| --- | --- |
| `P$name` | An exported name in package `P` |
| `User$name` | A workspace global in User |
| `Package$P` | A package object bound in the package-name dictionary |
| `Core$Package` | The class of package objects, defined in Core |

The report does not invent `Core$Core`, `Package$Package`, or `User$User` as
package aliases. The default `User$User` expression denotes an unresolved
symbol belonging to User, while the bare `User` denotes the package object.
`Package$User` is not synthesized as a package alias either.

An exported alias and its original name point to the same object. Internal
IDs appear in brackets so same-named objects remain distinguishable; these
IDs are not Macaulay2 qualified-name syntax.

`PackageExports` adds public dependency edges. Loading a forwarding package
loads those public namespaces transitively, without copying their names into
the forwarding package's dictionary. For example, forwarding JSON leaves
`JSON$toJSON` in JSON. Unknown dependency metadata is reported explicitly.

## Run the command

The standard LSP request is:

```json
{
  "command": "m2-ls.printHierarchies",
  "arguments": []
}
```

Send it through `workspace/executeCommand`. It returns the structured graph
and its readable `text`, and writes that text to the language-server output
log. It takes no arguments and does not execute Macaulay2 or change files.

In Neovim, this opens the text in a scratch buffer:

```lua
local client = assert(vim.lsp.get_clients({ name = "m2-ls", bufnr = 0 })[1])
client:exec_cmd({ command = "m2-ls.printHierarchies" }, { bufnr = 0 }, function(err, report)
  if err then
    vim.notify(err.message, vim.log.levels.ERROR)
    return
  end
  vim.cmd("new")
  vim.bo.buftype = "nofile"
  vim.bo.bufhidden = "wipe"
  vim.bo.swapfile = false
  vim.api.nvim_buf_set_lines(0, 0, -1, false, vim.split(report.text, "\n", { plain = true }))
end)
```

The JSON result contains `hierarchy.objects`, `hierarchy.namespaces`, named
exports and package dependency edges. Each dictionary includes its unqualified
`bindings`, actual `qualifiedBindings`, and owning package where applicable.
An object's separate `package` association preserves facts such as `package
Core === Core`, while its name is bound in the Package namespace and its class
is `Core$Package`.
The surrounding `packageLookup` and `imports` fields retain each file's
package environment and explicit import requests rather than assuming one
lookup order for every document.

For cursor-based inheritance browsing, use [Type hierarchy](type-hierarchy.md).
