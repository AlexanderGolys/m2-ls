# Macaulay2 that feels at home in your editor

`m2-ls` helps an editor understand the shape and meaning of Macaulay2 code.
Its most useful features are the ones that shorten exploration: hover over an
unfamiliar function, see its accepted arguments, jump to the relevant method,
rename a local binding safely, or apply a small Macaulay2-specific correction.

It is deliberately conservative. When the answer depends on running arbitrary
code, the server would rather show less than invent certainty.

## What you get

| While you are... | `m2-ls` can... |
| --- | --- |
| Discovering an API | Show documentation, signatures, option names, and known result types |
| Typing | Suggest packages, types after `new`, option keys, and a short list of unambiguous names |
| Reading code | Distinguish the roles of names, show inferred types, and connect matching pieces of syntax |
| Following a symbol | Jump to declarations, definitions, implementations, types, and library source |
| Changing names | Find references and rename bindings without confusing unrelated local names |
| Fixing code | Explain syntax and semantic problems and offer focused quick fixes |
| Reshaping a file | Format the document, fold meaningful regions, and browse document or workspace symbols |

## A small file to explore

Open this in an editor with `m2-ls` attached:

```m2
R = QQ[x, y]
I = ideal(x^2, x*y, y^3)

describeIdeal = J -> (
    G := gb J;
    (numgens source gens J, degree G)
)

describeIdeal I
```

Try hovering over `ideal` or `gb`, requesting signature help inside a call,
finding every use of `I`, and renaming `J`. The following chapters describe
what each feature is designed to reveal.
