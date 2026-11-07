# Document symbols

The document outline lists what the file defines, nested the way the code is:

```m2
R = §type.readonly|QQ§[§variable.declaration.algebraic|x_0§..§variable.declaration.algebraic|x_3§]
area = §function.readonly|method§()
area §typeParameter.readonly|Matrix§ := §parameter.declaration|m§ -> §method.readonly|rank§ §parameter|m§
toString §typeParameter.readonly|Tally§ := §parameter.declaration|t§ -> "tally"
- §typeParameter.readonly|String§ := §parameter.declaration|s§ -> §parameter|s§
Point = new §type.readonly|Type§ of §type.readonly|List§
f = §parameter.declaration|L§ -> (
    §function.declaration|g§ := §parameter.declaration|x§ -> §parameter|x§ + 1;
    §function.defaultLibrary.readonly|apply§(§parameter|L§, §function|g§)
    )
```

```text
R                  class      PolynomialRing
x                  variable   IndexedVariableTable
area               function   MethodFunction
area Matrix        method
toString Tally     method     toString Tally -> String
- String           method
Point              class      Type
f                  function   FunctionClosure
  g                function   FunctionClosure
```

Bindings
: Assignments and `:=` bindings, with their inferred type as detail when one
  is known. The symbol kind follows the value: a function or method function
  is a function, a type — rings included — a class, anything else a variable.

Installations
: Each method installation, named by its pattern — `area Matrix`,
  `toString Tally`, `Thing .. Thing`, `- String` — with the codomain when it is
  known.

Nesting
: Bindings made inside a function body sit beneath that function.

The outline is static: it lists what the document itself binds, not names that
loaded packages define or values created at run time. In Neovim, `gO` opens it
in the location list.
