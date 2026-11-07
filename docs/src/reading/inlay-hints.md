# Inlay type hints

Inlay hints write the inferred type of a value right into the line, as faded
text after it. The default is deliberately calm: bindings and selected expression results, and only where the type tells you something you could not read
off the code.

```m2
R = §type.readonly|QQ§[§variable.declaration.algebraic|x§, §variable.declaration.algebraic|y§]                     -- R: PolynomialRing   x: R   y: R
area = §function.readonly|method§()                  -- area: MethodFunction
area §typeParameter.readonly|Matrix§ := §parameter.declaration|m§ -> §method.readonly|rank§ §parameter|m§       --                       -> ZZ
G = §method.readonly|gb§ §method.readonly|ideal§(§variable.algebraic|x§^2, §variable.algebraic|y§)             -- G: GroebnerBasis
f = §modifier|symbol§ §enumMember|g§                     -- f: Symbol
x = if §variable|ready§ then 1 else 2       -- x: ZZ
```

Ring generators get the ring they belong to, with return hints enabled, a function shows what it returns
after its last expression (`-> ZZ`), and a binding is annotated where it is
first assigned or where its type changes — not on every reassignment.

Constructors preserve the requested type. In `new Span from Thing := (t,x) ->
{x}`, the constructor return hint is `Span`, reflecting the conversion performed
by `new`. A type whose metaclass inherits from `SelfInitializingType` or
`WrapperType` also constructs instances by application: `Span x` has type
`Span`. `HeaderType` supports this form with a list or sequence argument.

## Reading the notation

Types are printed as compactly as their certainty allows:

`ZZ`
: Exactly this type.

`RR | ZZ`
: One of these types.

`ZZ?`
: `ZZ` or `null`.

`↑List`
: Some subtype of `List` — maybe `List` itself, maybe a `VerticalList` —
  without claiming which.

`-> T`
: The result of a call or of a function's final expression.

A literal has an exact type; a call through a method function with several
installations often only has a range. The notation shows that uncertainty
instead of hiding it.

## Choosing the places

The [configuration](../getting-started/configuration.md#inlay-hints) separates
parameter names, type placement, and hide filters:

```toml
[inlayHints.argNames]
enabled = true
requireBracket = true
ignoreVariadic = true
maxLength = 15

[inlayHints.types]
enabled = true

[inlayHints.types.after]
call = true
endOfLine = true
endOfCell = true
assignment = true
brackets = ["parentheses"]
return = false
```

Names appear as `name:` immediately before argument values. With `argTypes`
enabled, `area(2)` can read `area(Number x: 2)`: the type precedes the name.
Names already spelled by their arguments are omitted. Type hints can be
switched off independently of argument names.

The `types.after` switches select calls, line ends, top-level cells,
assignments, delimiter forms, and returned values. `everywhere = true`
includes all expression positions, still subject to the filters below.

`brackets = ["parentheses"]` selects sequences such as `(2,2)`.
`brackets = ["holder"]` selects `(1)` and `((1,1))`: each holder evaluates to
its unique enclosed object. Holder parentheses are usually redundant, which
makes them useful as an explicit request for an inlay hint at that point.
Selected holder hints survive the literal/trivial filters and take precedence
rather than moving to the assignment or another automatic hint position.

`argNames.maxLength` truncates each parameter name independently. With a limit
of `5`, a long name becomes `long…:`; its domain type and trailing colon are
not truncated. The full name is available in the tooltip.

## Choosing what is said

```toml
[inlayHints.hide]
types = ["Thing", "Nothing"]
calls = ["toString", "net", "toList"]
nonExactMark = false
literal = true
trivial = true
sequential = true
beforeSemicolon = false
operatorCalls = true
```

`literal`
: Hide types directly stated by bracket-created collections, both string
  forms, integers, reals, booleans, `null`, lambdas, and explicit `new`
  expressions. Computed elements do not stop a collection from being literal.

`trivial`
: Hide types apparent from the producer. Literal forms are recognized now;
  broader constructor/conversion recognition is planned. For example,
  `matrix {{1},{1}}` is conceptually trivial, while only its bracket-created
  argument is literal.

`sequential`
: At a position with several consecutive type labels, keep only the final
  one: `FunctionClosure MarkdownBlock` becomes `MarkdownBlock`. This is not
  restricted to identical types. A closing delimiter between labels separates
  them.

`types`, `calls`
: Exclude the listed types or named calls. To show `Thing` or `Nothing`,
  remove it from `types`.

`nonExactMark`
: Hide only the `↑` marker, leaving the type visible.

`beforeSemicolon`
: Hide the result of a value discarded by `;`.

`operatorCalls`
: Hide parameter labels such as `left:` and `right:` beside operator
  operands. Keep ordinary expression type hints on the operands and result.
  Operator-name labels can be enabled with `operatorCalls = false` and
  `argNames.requireBracket = false`.

The [full configuration reference](../getting-started/configuration.md#inlay-hints)
lists every field's type, default, and interaction.

## Fine print

- A parameter whose domain is stated by a method installation gets no
  additional type annotation on its uses.
- A value's type is shown once when several enabled positions describe the
  same value.
- The editor decides whether hints are visible at all. In Neovim, run
  `:lua vim.lsp.inlay_hint.enable(true)`.
