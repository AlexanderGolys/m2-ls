# Document symbols

The document outline lists what the file defines, nested the way the code is:

```m2
R = QQ[x_0..x_3]
area = method()
area Matrix := m -> rank m
toString Tally := t -> "tally"
- String := s -> s
Point = new Type of List
f = L -> (
    g := x -> x + 1;
    apply(L, g)
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
