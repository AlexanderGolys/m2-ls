# Diagnostics

Documentation examples (`Example`, `CannedExample`, and hypertext `EXAMPLE` code)
do not publish lint diagnostics. They remain analyzed for highlighting and
navigation. Ordinary source and `TEST` blocks still report diagnostics.

Macaulay2 accepts a lot of code that does not do what it says. A misspelled
name becomes a symbol, a method installed with `=` is rejected only when that
line runs, a non-`Boolean` condition fails deep inside a computation. `m2-ls`
reports these while you type, and its messages say what to write instead.

```m2
R = §type.readonly|QQ§[§variable.declaration.algebraic|x0§, §variable.declaration.algebraic|x1§]
--     ^^ hint[S06]: Ring variable `x0` is a multi-character symbol, not an
--        indexed variable; use `x_0` so the generators form one indexed family

g = §function.readonly|method§()
g §type.readonly|ZZ§ = §parameter.declaration|n§ -> §parameter|n§
-- ^ error[E04]: Installing a method on `g` must use `:=`, not `=`: M2 rejects
--   this ("no method for storing values of function g"). Use `:=`.

sizeLabel = §parameter.declaration|L§ -> (
    §variable.declaration|n§ := #§parameter|L§;
    if §variable|n§ then "nonempty" else "empty"
    -- ^ warning[T02]: if condition must have type `Boolean`, but this
    --   expression has type `ZZ`
    )
```

Many diagnostics come with a [code action](code-actions.md) that applies the
fix. The same checks run outside the editor through
[`m2-lint`](../beyond/command-line.md).

## Codes

Every diagnostic has a stable code and a stable name; settings and suppression
comments accept either. The letter says what kind of problem it is:

`X` — structure
: Source that is malformed, or assignments Macaulay2 cannot carry out.

`E` — runtime
: Runtime concerns and design patterns worth reviewing; some are valid code
  with a more idiomatic alternative.

`T` — types
: Type errors found by inference.

`S` — style
: Hints toward idiomatic Macaulay2.

`D` — documentation
: Package documentation Macaulay2 rejects or warns about when installing.

Codes are never reused: a retired diagnostic's code stays retired, so a
setting written for one release never silently means something else in the
next.

## The catalog { #catalog }

| Code | Name | Severity | Reported for |
| --- | --- | --- | --- |
| `X01` | `syntax-error` | error | source the grammar cannot parse: `x = (1 + ]` |
| `X02` | `missing-node` | error | a token the grammar expected, such as an `if` ending after `then` |
| `X03` | `ambiguous-float-member-access` | warning | `L.1`, which is `L` applied to the float `.1`; fix: `L#1` |
| `X04` | `multiple-assignment-targets` | error | a parallel-assignment target that cannot be assigned: `(a, 1) = (2, 3)` |
| `X05` | `colon-equal-part-assignment` | error | `L#0 := 5`; parts are assigned with `=` |
| `X06` | `parallel-assignment-arity` | error | `(a, b) = (1, 2, 3)` |
| `E01` | `install-no-effect` | warning | an installation nothing dispatches to: on a plain function, the binary `??`, a `<-` method on anything but one non-symbol type, or an operator the interpreter [evaluates itself](#native-operands) for those operands: `ZZ + QQ := …` |
| `E02` | `operator-not-flexible` | error | an installation on an operator users cannot extend: `ZZ === ZZ := …` |
| `E03` | `install-arity` | error | `g(ZZ, ZZ) := (a, b, c) -> a` |
| `E04` | `install-needs-colon-equals` | error | `g ZZ = n -> n`; installations use `:=` |
| `E06` | `protect-computed-symbol` | warning | `protect getSymbol "x"`, protecting a value known only at run time |
| `E08` | `invalid-control-transfer` | error | `return`, `break` or `continue` outside the function or loop it needs |
| `E09` | `explicit-install-required` | error | `(x <- ZZ) := …`; `<-` is overloaded only through `installMethod` |
| `E11` | `install-option-form` | error | an installed function whose shape does not match how the method passes options |
| `E12` | `install-unreachable-options` | warning | `{Limit => 1} >> o -> …` installed on a method without options, so the defaults always apply |
| `E13` | `protected-assignment` | error | assigning to a protected indexed symbol, such as `ZZ = 7`; local shadows remain assignable, and [library code](../getting-started/inline-directives.md#primary-code) may define them |
| `E14` | `ring-index-out-of-bounds` | error | a subscript past the variables of a ring the program writes out: `A_2` after `A = QQ[a, b_1]` |
| `E15` | `ring-variable-not-found` | error | a lookup by name of a variable the ring lacks: `"b"_A`, `b_2_A`; see [Rings](../reading/rings.md#diagnostics) |
| `E16` | `unknown-option` | error | an option name the called function does not accept, which Macaulay2 rejects: `f(1, Verbos => true)`; fix: the closest accepted name |
| `E17` | `unloadable-file` | warning | a `load`/`needs`/`input` name `load` cannot run: no file where it looks, a directory, a file it may not read, or an empty name: `load "no-such-file.m2"`; see [Workspace files](../navigating/workspace-files.md#unloadable) |
| `E18` | `unavailable-init-file` | warning | an [`initFile` directive](../getting-started/inline-directives.md#init-file) naming a file that cannot be read: `-- m2-ls: initFile no-such-init.m2`; the file starts without an init file |
| `E19` | `manual-type-dispatch` | warning | three or more `instance` tests of the same function parameter anywhere in its body; consider method specializations |
| `E20` | `list-record-shape` | warning | a list-family `instance` check combined with a fixed length above one; consider a hash-table-based record type |
| `E21` | `float-key` | error | a floating-point key or index, including a key inferred to have floating-point type; approximate equality makes lookup unreliable |
| `E22` | `unresolved-value` | error | an unbound callable, constructor type, Boolean operand or condition, or indexed receiver; declare or explicitly import the value |
| `E23` | `matrix-shape-mismatch` | error | a sum, difference or product of matrices written out whose dimensions cannot combine: `A * A` for a 2×3 `A` |
| `E24` | `division-by-zero` | error | a number divided by an exact zero written out: `1/0`, `1.5/0`; `1/0.` is valid and gives infinity |
| `T01` | `parallel-assignment-type` | error | `(a, b) = 5` |
| `T02` | `condition-type` | warning | an `if` or `while` condition known not to be `Boolean` |
| `T03` | `install-codomain-missing` | hint | an installation other than `new` without a codomain that inference can deduce; fix: insert `ZZ =>` |
| `T04` | `install-codomain-mismatch` | warning | a declared codomain contradicting the inferred result: `g ZZ := String => n -> n + 1` |
| `T05` | `call-domain-mismatch` | warning | no known method domain can accept the inferred argument types; uncertain matches remain valid |
| `T06` | `option-value-mismatch` | warning | an option value of none of the documented types, or a symbol outside the values the option accepts |
| `S01` | `option-key-convention` | hint | an option name or unbound symbol value that does not use PascalCase in an option declaration or a call |
| `S02` | `unused-binding` | warning | a local that is never read; names starting with `_` are exempt |
| `S03` | `redundant-control-parentheses` | hint | parentheses the keyword already delimits: `if (n > 0) then` |
| `S04` | `prefer-coalescence` | hint | a null test `??` says directly: `if x === null then 0 else x` |
| `S05` | `unnecessary-null-branch` | hint | an `if` with a removable `null` branch |
| `S06` | `ring-variable-naming` | hint | a multi-character ring variable such as `x0` instead of `x_0` |
| `S07` | `shared-method-codomain` | hint | at least five method installations, with more than 75% explicitly annotating the same result type; consider declaring a shared `TypicalValue` |
| `S08` | `naming-convention` | hint | a type name or a declared optional argument (`Options => {…}`, `{…} >> f`) outside PascalCase or uppercase |
| `S09` | `bound-option-key` | warning | an option key whose existing source binding is evaluated instead of supplying the name as a symbol; bound values are allowed |
| `S10` | `constructor-typical-value` | hint | an installation of a method named after a known type, such as `matrix` or `toMatrix`, declaring no typical value at all |
| `S11` | `null-return-guard` | hint | an unused `if x === null then return y` guard expressible as `x ?? return y` |
| `S12` | `mapped-composition` | hint | `apply(rows, x -> f(g(x)))` expressible as `rows / f @@ g` |
| `S13` | `redundant-boolean-comparison` | hint | a known Boolean strictly compared with `true` or `false` |
| `S14` | `forwarding-lambda` | hint | `c -> inlineOf c` or `c -> inlineOf(c)` expressible as `inlineOf` |
| `S15` | `simplifiable-try` | hint | a `try` with a repeated success value or redundant `null` fallback |
| `S16` | `simplifiable-if-condition` | hint | an `if` condition with an invertible comparison or cancellable double negation |
| `S17` | `flattenable-if-chain` | hint | nested conditionals that can become an `else if` chain |
| `S18` | `partial-application-lambda` | hint | `y -> f(x, y)` expressible as `f_x`, with a fixed symbol as the first argument |
| `S19` | `self-initializing-constructor` | hint | `new T from value` expressible as `T value` for a known `SelfInitializingType` |
| `S20` | `header-constructor` | hint | `new T from values` expressible as `T values` for a known `HeaderType`, with List or Sequence input |
| `S21` | `wrapper-constructor` | hint | `new T from values` expressible as `T values` for a known `WrapperType`, with List or Sequence input |
| `S22` | `predicate-typical-value` | warning | a predicate (`is` and an uppercase letter, as in `isPrime`) whose `method()` declaration does not set `TypicalValue => Boolean`, or an installation of one that annotates another result type |
| `S23` | `predicate-function-closure` | hint | a predicate defined as a function closure rather than as a method |
| `S24` | `inline-null-binding` | hint | `v := e; if v =!= null then y = v else y = w` expressible as `y = e ?? w`, dropping the local |
| `S25` | `prefer-augmented-assignment` | hint | `prefix = prefix \| "X"` expressible as `prefix \|= "X"`, for a symbol target and an operator with an augmented form |
| `S26` | `constant-condition` | hint | an `if` or `while` condition computed from literals alone, as `if 2^10 > 1000`, so one branch never runs |
| `D01` | `unknown-documentation-section` | error | a SimpleDoc keyword or `document` option that does not exist there |
| `D02` | `malformed-documentation` | error | documentation SimpleDoc or `document` rejects: a repeated section, a multi-line headline, `Inputs` without `Usage`, … |
| `D03` | `missing-documentation-key` | error | a documentation page without a `Key` |
| `D04` | `long-headline` | warning | a headline over 100 characters |
| `D05` | `undefined-documentation-key` | error | a key, `@TO@` link or `SeeAlso` entry nothing defines — `endPackage` would reject it |
| `D06` | `duplicate-documentation` | error | a key documented twice, or both documented and listed in `undocumented` |
| `D07` | `undocumented-export` | warning | an exported symbol without documentation in a package documented in the same file |
| `D08` | `documentation-outside-package` | error | documentation after `endPackage`, where no package can hold it |
| `D09` | `documentation-before-begin` | error | `doc` before `beginDocumentation()`, which is what loads it |

Unused bindings are also tagged *unnecessary*, which most editors render by
fading the name rather than underlining it.

## The installation checks { #installations }

The `E` rules around method installation are the ones that save the most
debugging, because Macaulay2 itself fails late or not at all:

```m2
f = §parameter.declaration|x§ -> §parameter|x§
f §typeParameter.readonly|ZZ§ := §parameter.declaration|n§ -> §parameter|n§
-- E01: Installing a method on `f` has no effect: `f` is not a method function.
--      Define it with `f = method()` to make method installations take effect.

opts = §function.readonly|method§(§enumMember.option.readonly|Options§ => {§enumMember.option.readonly|Limit§ => 1})
opts §typeParameter.readonly|QQ§ := {} >> §parameter.declaration|o§ -> §parameter.declaration|x§ -> §parameter|x§
-- E11: `opts` passes its options as a table to the installed function, so a
--      function declaring options with `>>` never receives them and the call
--      fails. Install `o -> x -> …`.

plain = §function.readonly|method§()
plain §typeParameter.readonly|ZZ§ := {§enumMember.option.readonly|Limit§ => 1} >> §parameter.declaration|o§ -> §parameter.declaration|n§ -> §parameter|n§
-- E12: `plain` has no options, so calls cannot pass any and the defaults
--      declared with `>>` always apply.
```

### Operators the interpreter evaluates itself { #native-operands }

`E01` also covers installations that Macaulay2 accepts, lists in `methods`, and
never calls. Before it looks up any method, the interpreter computes some
operator calls on native values directly, so a method installed for exactly
those operand classes is dead:

| Operator | Operand classes the interpreter handles itself |
| --- | --- |
| `+ - * /` | any two of `ZZ QQ RR RRi CC CCi` |
| `^` | the same pairs, except `CC ^ RRi` |
| `==` | the same pairs, except `CC == RRi`; any real interval on the left; two Booleans, symbols, strings or nets |
| `?` | most number pairs (`CCi` never on the left), any real interval on the left; strings and nets, symbols, `null` |
| `//` `..` `..<` | two integers |
| `and` `or` | two Booleans |
| unary `-` `+` | a native number |
| `not`, prefix `??` | a Boolean, `null` |

The binary `??` and `<-` are the other two dead installations, with their own
messages. Everything else dispatches normally: ring elements of polynomial
rings, matrices, ideals, lists, your own types, and every other operator
(`**`, `%`, `|`, `<<`, `_`, …). Installing for `Number` or another ancestor
works too, since only the exact native pair is answered first. The fix is a
type of your own, for example a wrapper around the value. The table is checked
against the real interpreter by the probe in `tests/native_dispatch`.

`S24` is the neighbour of `S04` for the pattern with an intermediate local. It
reports the binding and the conditional together and is not reported twice by
`S04`. `??` also falls back when its left side raises an error, which only
matters if the code sits inside a `try` that relied on that error escaping.

Where a finding points at another place — `f` defined as a plain function on
an earlier line — the diagnostic carries it as related information, which
editors show as a clickable note.

## Naming, options, and constructors { #naming }

`S08` checks that type names and declared optional arguments use PascalCase
or uppercase. Function and local names are free, since rings, modules and
other mathematical objects keep the names their notation gives them. Function
parameters, ring generators and ordinary global values are also outside this
check. Leading underscores are allowed.

```m2
convert = §parameter.declaration|T§ -> §parameter.declaration|value§ -> new §parameter|T§ from §parameter|value§
```

Option names and unbound symbolic defaults use PascalCase (`S01`), both where
options are declared and where they are passed to a call:

```m2
render = §function.readonly|method§(§enumMember.option.readonly|Options§ => {§property.option.readonly|Layout§ => §variable|Compact§})
```

A source-bound option key produces `S09`: its value would be evaluated instead
of supplying the key as a symbol. A bound default value is allowed:

```m2
defaultLimit = 10
render = §function.readonly|method§(§enumMember.option.readonly|Options§ => {§enumMember.option.readonly|Limit§ => §variable|defaultLimit§})
```

Ordinary hash-table entries are not option declarations. `S01` leaves known
library symbols and bound values alone.

`S10` recognizes conversion methods named after known types, such as `widget`
or `toWidget` for `Widget`. Declare the result with `TypicalValue => Widget`
on the method or a `Widget =>` annotation on each installation. Any declared
typical value satisfies the hint, so installations may narrow or differ from
the method's, for instance to a subclass:

```m2
Widget = new §type.readonly|Type§ of §type.readonly|HashTable§
toWidget = §function.readonly|method§(§enumMember.option.readonly|TypicalValue§ => §type|Widget§)
toWidget §typeParameter.readonly|ZZ§ := §parameter.declaration|n§ -> new §type|Widget§ from {§property.option|"value"§ => §parameter|n§}
```

## Floating-point keys { #float-keys }

`E21` reports floating-point keys in pairs and indexed access with `#` or
`#?`. This includes a key whose inferred type is floating point, even when
the literal is assigned on another line:

```m2
key = 1.5
table#§variable|key§
```

Use exact values or another stable identifier for keys. A floating-point value
stored under a key is allowed, as in `hashTable {Weight => 1.5}`.

## Silencing a diagnostic { #suppression }

Settings turn rules off everywhere
([`diagnostics.disabled`](../getting-started/configuration.md#diagnostics)).
For one place, write a directive comment in the file:

```m2
-- m2-ls: disable S04 unused-binding
-- m2-ls: disable-next-line ring-variable-naming
R = §type.readonly|QQ§[§variable.declaration.algebraic|x0§, §variable.declaration.algebraic|x1§]
```

`-- m2-ls: disable …`
: Silences the listed rules in the whole file, wherever the comment stands.

`-- m2-ls: disable-next-line …`
: Silences them on the following line only.

Rules are named by code or name, as in the settings; a directive that names
none silences every rule. You rarely need to type these:
every diagnostic offers **Disable … on this line** and **Disable … in this
file** as [code actions](code-actions.md#suppression).

See the [complete inline directive reference](../getting-started/inline-directives.md)
for separators, unknown selectors, blank lines, cumulative scope, limitations,
and the separate `primaryCode` analysis property.

## Fine print

- Diagnostics are recomputed on every change and published for the version of
  the document they were computed from.
- Type diagnostics fire only on what inference can prove. A condition whose
  type is unknown is not reported; one known to be `ZZ` is.
- Documentation rules (`D`) apply to `doc ///…///` strings and `document {…}`
  calls in package files.

### Prefer method dispatch for class-specific behavior

`E19` highlights the third `instance` test of the same function parameter
anywhere in its body. Checks in compound conditions, nested expressions, and
separate statements all count:

```m2
renderLabel = §parameter.declaration|x§ -> if §function.defaultLibrary.readonly|instance§(§parameter|x§, §type.readonly|String§) then §parameter|x§
    else if instance(x, ZZ) then toString x
    else if instance(x, List) then toString x
    else "other"
```

Separate method specializations keep each class's behavior together and let new
classes extend the operation:

```m2
renderLabel = §function.readonly|method§()
renderLabel §typeParameter.readonly|String§ := §decorator.readonly|String§ => §parameter.declaration|x§ -> §parameter|x§
renderLabel §typeParameter.readonly|ZZ§ := §decorator.readonly|String§ => §parameter.declaration|x§ -> §method.readonly|toString§ §parameter|x§
renderLabel §typeParameter.readonly|List§ := §decorator.readonly|String§ => §parameter.declaration|x§ -> §method.readonly|toString§ §parameter|x§
renderLabel §typeParameter.readonly|Thing§ := §decorator.readonly|String§ => §parameter.declaration|x§ -> "other"
```

This warning is advisory. Review inheritance and branch order before changing a
chain: methods choose the most specific domain, whereas a chain chooses the first
matching test. Repeated tests of the same class, negated tests, and validation
guards count toward the threshold, as do tests with computed class expressions.
Only a direct reference to a function parameter as the first argument counts;
tests of globals, locally derived values, or computed subjects such as `x#0`
do not. Shadowed `instance` functions are excluded. Each parameter is counted
independently using its binding, so parameters with the same name in separate
functions do not combine. A closure testing an enclosing function's parameter
contributes to that parameter's count. Each parameter receives at most one warning.

### Prefer named fields for records

`E20` suggests a hash-table-based type when a condition checks both list
membership and a fixed multi-field length:

```m2
if not §function.defaultLibrary.readonly|instance§(§variable|task§, §type.readonly|VisibleList§) or #§variable|task§ != 2 then
    §function.defaultLibrary.readonly|error§ "task needs two fields"
```

A fixed number of positional entries can indicate a record whose fields would
be clearer with names. Consider making a struct-like type based on hash tables
instead of a list. This is advisory: fixed-length lists can also represent
legitimate mathematical or external data formats.

The check recognizes `List`, `BasicList`, and `VisibleList`, combined through
`and` or `or`, including negated tests. Length comparisons must use `==` or `!=`
and an integer literal greater than one, in either operand order. Both tests
must refer to the same written subject. Subjects can be parameters, local or
global values, or expressions such as `task#0`. Tests of lengths zero or one,
variable lengths, ordering comparisons, and unrelated subjects stay quiet.

### Share a method's usual result type

`S07` suggests setting `TypicalValue` in a locally created method's declaration
when at least five effective installations exist and more than three quarters
explicitly annotate the same codomain. For example, four `String` annotations
out of five installations qualify; six out of eight do not.

```m2
renderLabel = §function.readonly|method§(§enumMember.option.readonly|TypicalValue§ => §type.readonly|String§, §enumMember.option.readonly|Options§ => {§property.option|Width§ => 80})
```

Keep the declaration's existing options and differing per-method codomain
annotations. Those annotations override the shared typical value. Unannotated
installations count toward the total but not toward the annotated majority.
Inferred codomains do not count as explicit annotations. Reinstalling a domain
counts only its effective installation. A declaration already specifying a
`TypicalValue` receives no hint. This whole-document hint is refreshed on save.

## Resolved values and symbolic names

`E22` requires a definition where syntax needs a value: the function in a call,
the class after `new`, a bare `if`/`while` condition, a Boolean operand, or the
receiver of indexed access. Parameters and declared values remain valid even
when their types are unknown. Option keys, enum-like symbols, and quoted symbols
do not become missing-value errors.

Package fragments use their declared package's include order, rather than an
arbitrary caller's globals. See [package context](../navigating/workspace-files.md#package-context).
An unresolved use is an error even though the runtime can initially represent
an unknown name as a Symbol.
