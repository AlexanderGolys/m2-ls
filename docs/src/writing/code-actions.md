# Code actions

Code actions turn a diagnostic into a one-keystroke fix, and offer a handful of
rewrites that make Macaulay2 code shorter or clearer. Ask for them with the
cursor on the code (`gra` in Neovim).

```m2,pane="Before"
value = if §variable.readonly|value§ =!= §variable.readonly|null§ then §variable.readonly|value§ else §variable|fallback§
```

```m2,pane="After"
value = §variable.readonly|value§ ?? §variable|fallback§
```

Every rewrite is formatted with your
[formatting settings](../getting-started/configuration.md#formatting) at the
indentation of its surroundings, so accepting one never leaves a line for the
formatter to clean up.

## Quick fixes { #quick-fixes }

Each of these fixes the diagnostic it is attached to:

**Use `:=` for method installation** — [`E04`](diagnostics.md#catalog)
: `g ZZ = n -> n` becomes `g ZZ := n -> n`.

**Use `=` for part assignment** — `X05`
: `L#0 := 5` becomes `L#0 = 5`.

**Add codomain annotation** — `T03`
: `f ZZ := n -> n + 1` becomes `f ZZ := ZZ => n -> n + 1`, with the type
  inference deduced. `new` installations do not need this annotation:
  construction sets the result class to the requested type.

**Capitalize option key** — `S01`
: Changes `verbose` to `Verbose` in an option declaration or call, when
  capitalizing the first letter is enough to produce a PascalCase name.

**Use `??` coalescence**, **Use `??=` coalescing assignment** — `S04`
: `if x === null then 0 else x` becomes `x ?? 0`; an `if` that only assigns a
  fallback, `if t === null then t = v`, becomes `t ??= v`.
  A conditional update, `if stop =!= null then i = stop`, becomes
  `i = stop ?? i`. The forms without an `else` are offered only when the
  conditional's result is unused, preserving its missing-branch behavior.
  When every fallback path assigns the same existing variable, the assignment
  moves outside the chain: `if stop =!= null then i = stop else if ready then
  i = next else i = i + 1` becomes `i = stop ?? (if ready then next else i + 1)`.
  Statements before a branch's final assignment stay in that branch's block,
  so collecting a match or updating a table still happens only on that path.
  A branch declaring its own local target or lacking a final value keeps the
  original conditional.

**Omit `from 0`** — `S30`
: Removes a numeric `for` loop's explicit zero start: `for i from 0 to n do …`
  becomes `for i to n do …`. Comments in the removed clause are preserved.

**Inline null-tested local with `??`** — `S24`
: `v := g x; if v =!= null then y = v else y = 0` becomes `y = g x ?? 0`,
  folding the local into the statement. Offered over the binding and the
  conditional together, and only when nothing else uses the local.

**Use augmented assignment** — `S25`
: `prefix = prefix | "X"` becomes `prefix |= "X"`, for a symbol target and an
  operator that has an augmented form (`+ - * / // % ^ | & ++ ** …`).

**Rewrite as member access** — `X03`
: `L.1` becomes `L#1`.

## Rewrites { #rewrites }

These appear when the cursor is on code they can improve. Expression
simplifications each have their own diagnostic name and code. Each offers a
preferred quick fix; disabling its hint leaves the rewrite available as a
refactor. Redundant control parentheses have their own hint, `S03`. Raw-string
conversion is a refactor without a diagnostic.

**Simplify null-return guard** — `S11`
: An early-return guard, `if x === null then return y;`, becomes
  `x ?? return y;` when the conditional's result is unused and it has no
  `else` branch. For example, inside a function:

```m2,pane="Before"
if §variable|x§ === §variable.readonly|null§ then
    return §variable|y§;
```

```m2,pane="After"
x ?? return §variable|y§;
```

The return guard must test a symbol or an expression wrapped in `try`, since
`??` also catches errors while evaluating its left operand. A conditional
whose result is used stays unchanged: when `x` is non-null, the original
returns `null` while the shorter expression yields `x`.

**Simplify unnecessary null branch** — `S05`
: `if ready then value else null` → `if ready then value`, and
  `if linkStyles#?style then null else error(…)` →
  `if not linkStyles#?style then error(…)`.

**Simplify mapped composition** — `S12`
: `apply(rows, r -> blocksOf(contents r))` → `rows / blocksOf @@ contents`.
  The callback must forward its sole parameter through two unary calls.
  `/` maps over the input, and `@@` composes the functions in the same order.
  Composition binds more tightly, so the result means
  `rows / (blocksOf @@ contents)`.

Lowercase function names are eligible even when their types are unknown.
PascalCase callees are excluded as type constructors by convention, including
`SelfInitializingType` constructors. An inferred `Thing` does not suppress this
hint; the rewrite does not require proof of a `Function` type.
Shadowed `apply`, computed callees, extra arguments, and integer-count inputs
stay unchanged. Parentheses preserve grouping when the rewritten call is part
of a larger expression.

**Simplify Boolean comparison** — `S13`
: `linked === true or ready` → `linked or ready`, when `linked` is known to
  be Boolean. `linked === false` and `linked =!= true` become `not linked`;
  `linked =!= false` becomes `linked`. The literal may appear on either side.

Unknown and non-Boolean subjects keep their strict test. For example,
`null === true` is `false`, while a bare `null` is not a valid Boolean
condition. Comparisons using `==` or `!=` are left unchanged.

**Replace forwarding lambda with function** — `S14`
: `c -> inlineOf c` → `inlineOf`; `run -> inlineRun(run)` → `inlineRun`.
  Parentheses around the call or its argument are allowed. The callback must
  forward its sole parameter directly to a named lowercase function, with
  no extra arguments or statements.

**Replace lambda with composition** — `S28`
: `x -> foo bar x` → `foo @@ bar`; `x -> foo(bar(x))` has the same rewrite.
  Both calls must be unary and use lowercase function names independent of
  the parameter. Parentheses are preserved where an enclosing call or operator
  needs them. Fixed parameter lists, extra statements or arguments, computed
  callees, and comments that would be removed keep their lambda wrappers.
  Disable `composed-lambda` to hide the hint while retaining the refactor.

**Replace lambda with partial application** — `S18`
: `y -> f(x, y)` → `f_x`. The callback passes its sole parameter as the second
  argument after a fixed symbol. PascalCase callees, computed fixed arguments,
  reordered or repeated parameters, and extra statements keep their wrappers.

Each rule can be disabled independently. For example,
`-- m2-ls: disable forwarding-lambda` silences `S14` while keeping
`partial-application-lambda` (`S18`) enabled.

**Use self-initializing constructor call** — `S19`
: `new T from value` → `T value`, when the metaclass is known to inherit from
  `SelfInitializingType`, including source-defined metaclasses.

**Use header constructor call** — `S20`
: The same shorthand for a known `HeaderType`, with List or Sequence input.

**Use wrapper constructor call** — `S21`
: The same shorthand for a known `WrapperType`, with List or Sequence input.
  Scalar input keeps `new`: calling a wrapper with a scalar introduces a list
  around it and can change its contents.

These are three independent hints. Parentheses preserve the argument and the
surrounding expression's precedence. Constructions with an explicit `of`
clause keep their original form. Review custom call-method overrides: a custom
metaclass can normalize the argument differently from direct `new` construction.

**Simplify try** — `S15`
: `try value then value` → `try value`;
  `try value then result else null` → `try value then result`.

**Simplify if condition** — `S16`
: `if not (left == right) then …` → `if left != right then …`. A negation is
  only pushed inward when the result is genuinely simpler:
  `not linkStyles#?style` is left alone.

**Remove redundant parentheses**
: `if (ready) then value` → `if ready then value`.

**Flatten nested if into else-if chain** — `S17`
: `if a then x else (if b then y else z)` → `if a then x else if b then y else z`.

**Convert to raw string**
: `"C:\\path\\to\\file"` → `///C:\path\to\file///`, whenever the value
  survives verbatim.

```m2,pane="Before"
label = §parameter.declaration|n§ -> if §parameter|n§ < 0 then "negative" else (if §parameter|n§ == 0 then "zero" else "positive")
```

```m2,pane="After"
label = §parameter.declaration|n§ -> if §parameter|n§ < 0 then "negative" else if §parameter|n§ == 0 then "zero" else "positive"
```

## Declaring library code { #primary-code }

Assigning to a protected name, `ZZ = 7` (`E13`), is a mistake in ordinary code
but exactly what the library's own source does. That error also offers:

**Analyze this file as library implementation code**
: Inserts `-- m2-ls: primaryCode` at the top of the file, which analyzes it
  [as the library](../getting-started/inline-directives.md#primary-code)
  rather than hiding the error. Whole directories are declared with
  [`sources.primaryCodePaths`](../getting-started/configuration.md#primary-code-paths).

Like the suppressions below, it is never part of **Fix all in file**.

## Silencing a diagnostic { #suppression }

Every `m2-ls` diagnostic under the cursor also offers two actions that write a
[suppression comment](diagnostics.md#suppression), listed after the real fixes:

**Disable S04 on this line**
: Inserts `-- m2-ls: disable-next-line S04` above the line, at its
  indentation.

**Disable S04 in this file**
: Inserts `-- m2-ls: disable S04` at the top of the file.

**Remove redundant codomain annotation** — `S27`

Removes an installation’s `Type =>` annotation when it repeats the method’s
explicit `TypicalValue`, allowing the installation to inherit that value.

## SimpleDoc documentation placeholders

Place the cursor on a declaration and choose **Generate SimpleDoc documentation
for …** to insert a `doc /// … ///` placeholder immediately above it.
The action fills the documentation key and adds explanatory text for sections
that need your input.

The full templates translate Macaulay2's
[function](https://macaulay2.com/doc/Macaulay2/share/doc/Macaulay2/Macaulay2Doc/html/_function_spdocumentation_sptemplate.html),
[optional argument](https://macaulay2.com/doc/Macaulay2/share/doc/Macaulay2/Macaulay2Doc/html/_optional_spargument_spdocumentation_sptemplate.html),
[package](https://macaulay2.com/doc/Macaulay2/share/doc/Macaulay2/Macaulay2Doc/html/_package_spdocumentation_sptemplate.html), and
[overview](https://macaulay2.com/doc/Macaulay2/share/doc/Macaulay2/Macaulay2Doc/html/_overview_spdocumentation_sptemplate.html)
documentation templates into SimpleDoc syntax, preserving their section order.
The sections follow the item's role: types and method functions receive overview
nodes; ordinary functions and installed signatures receive usage, inputs, and
outputs; option nodes describe the option's input rather than a return value.
Only package pages include acknowledgements, contributors, references, and
subnodes. Function and method declarations with literal option tables also offer
separate actions using `[function, Option]`. Installations use signature keys
such as `(f, ZZ)`, parameter names from source, their declared domain types, and
the known codomain. Unknown types stay unspecified instead of being invented.

Each item has one template action. It includes explanatory prose, an example,
and a caveat, with consequences for callables and additional overview fields for
packages. Fill in or remove the guidance text before publishing documentation.

Load SimpleDoc in the package as usual, and move completed documentation to the
package's documentation section when needed: executable documentation references
may require their objects to have been defined first. These actions insert text
without loading packages or rearranging declarations, and require no diagnostic.
