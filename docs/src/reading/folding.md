# Folding

Folding ranges follow the structure of the code, not its indentation: every
multi-line block, list, function body and control-flow form can be collapsed,
and so can runs of comments and multi-line strings.

```m2
-- Helpers for building test ideals.     ┐
-- Each returns an ideal of R.            ┘ folds as a comment
makeIdeal = R -> (                       ┐
    gens := apply(3, i -> R_i^2);         │ folds as a block
    ideal gens                            │
    )                                     ┘
doc ///                                  ┐
Key                                       │ folds as a string
  makeIdeal                               │
///                                       §variable.documentation|┘§
```

Comment ranges are marked as comments, so an editor setting such as "fold all
comments" collapses exactly those. A long documentation string folds away as a
unit, which makes the code of a package file easy to scan.

SimpleDoc documentation also folds by section. Collapse an entire `Node`, or
fold `Key`, `Usage`, `Inputs`, `Outputs`, and other sections independently.
Nested sections such as `Description` → `Text` or `Example`, `Synopsis`, and
`Consequences` → `Item` have their own folds. Each fold keeps its keyword visible
and ends at its last nonblank content line, before the next section or closing
string delimiter. Empty sections do not produce folds.
