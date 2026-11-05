# Semantic highlighting

A syntax highlighter sees `area`, `rank` and `x` as three identical
identifiers. `m2-ls` knows that `area` is a method function you made, `rank` a
library method, and `x` a generator of a polynomial ring — and colors each by
what it is bound to, not by how it is spelled or where it stands.

```m2
R = QQ[x, y]                      -- R type · QQ type · x, y variable+algebraic
area = method()                   -- area method · method function
area Matrix := ZZ => m -> rank m  -- Matrix typeParameter · ZZ typeParameter · m parameter
twice = n -> 2 * n                -- twice function
scan({1, 2}, print)               -- scan function+defaultLibrary · print function
G = gb(I, Strategy => Linear)     -- gb method · Strategy enumMember+option
stdio << "done" << endl           -- stdio variable · endl function
f = symbol g                      -- symbol modifier · g enumMember
typo + 1                          -- typo variable
needsPackage "JSON"               -- "JSON" namespace
load "lib/helper.m2"              -- "lib/helper.m2" namespace
```

Most tokens use standard LSP types. Pattern escapes use the custom
`escapeSequence` type; a theme can style this separately. The finer Macaulay2 distinctions travel as
modifiers, which a theme may style when it wants to.

## Token types { #types }

Each type has a Macaulay2 meaning and is sent under the LSP name in
the second column — that is the name your editor and theme see.

| Meaning | Sent as | Given to | Example |
| --- | --- | --- | --- |
| Type | `type` | any object whose class is a `Type`: builtin, package or your own | `ZZ`, `Matrix`, `R` in `R = QQ[x]`, `T = new Type` |
| Function | `function` | anything applied that is not a method function: closures, compiled functions, scripted functors, commands | `twice`, `print`, `apply`, `Ext`, `exit` |
| Method | `method` | a method function, made by `method()`, which dispatches to its installations | `area`, `det`, `gb`, `source` |
| Variable | `variable` | every other value | `G`, `stdio`, a local |
| Parameter | `parameter` | a function parameter, or an input named on a documentation page | `m` in `m -> rank m` |
| Property | `property` | a key rather than a binding: an option name that is not a protected symbol, a member an access selects | `verbose` in `{verbose => false} >> o -> …`, `key` in `H.key`, `0` in `L_0` |
| Namespace | `namespace` | a package, including package names in strings, and the file a `load`, `needs` or `input` call runs | `Core`, `"JSON"` in `needsPackage "JSON"`, `"lib/helper.m2"` in `load "lib/helper.m2"` |
| Symbol | `enumMember` | a name used as a `Symbol` rather than for its value | `Strategy`, `Linear`, `g` in `symbol g` |
| Domain or codomain | `typeParameter` | the types an installation is made on and returns | `Matrix`, `ZZ` in `area Matrix := ZZ => …` |
| Annotation | `decorator` | a SimpleDoc section keyword, a `document` option name, or a documentation link | `Key`, `Headline`, `TO` |
| Markup | `macro` | markup around documentation text | `{\tt`, `}`, `PARA`, `TT` |
| Modifier keyword | `modifier` | the quoting keywords | `symbol`, `local`, `global`, `threadVariable` |
| Keyword | `keyword` | language keywords | `if`, `then`, `for`, `TEST` |
| String | `string` | string literals | `"done"`, `///…///` |
| Regular expression | `regexp` | special syntax within literal patterns of `match`, `regex`, `replace`, `select`, and `separate`; delimiters and plain text stay `string` | `+` in `replace(///a+///, "b", text)` |
| Escape sequence | `escapeSequence` | escaped characters in patterns, retaining their source spelling | `\d`, `\.`, `\\d` in quoted strings |
| Number | `number` | integer and float literals | `2`, `0.5` |
| Operator | `operator` | operators | `=`, `:=`, `->`, `<<`, `_` |
| Comment | `comment` | comments | `-- note` |

Macaulay2 does not separate classes from types, so neither does highlighting:
there is no `class` token, only `type`.

## Functions versus methods { #methods }

In Macaulay2 a *method* is one installation on a domain — `det Matrix := …`
installs `det(Matrix)` — and the object owning the installations is a *method
function*. That object gets the `method` token; everything else that is applied
is a `function`. The decision follows the object's runtime class, exactly:

| Runtime class | Examples | Token |
| --- | --- | --- |
| `MethodFunction`, `MethodFunctionSingle`, `MethodFunctionBinary`, `MethodFunctionWithOptions` | `source`, `gcd`, `det`, `gb` | `method` |
| `FunctionClosure` | `x -> x + 1`, `print` | `function` |
| `CompiledFunction` from any package | `apply`, `scan` | `function` + `defaultLibrary` under the default policy |
| `CompiledFunctionClosure` | | `function`; marked by the `Compiled` policy |
| `ScriptedFunctor` | `id`, `Ext`, `HH` | `function` |
| `Command`, `Manipulator` | `exit`, `help`; `close`, `endl` | `function` |

The documented signatures do not matter: `apply` has typed signatures in the
documentation, but it is a compiled function, not a method function, so it stays
a `function`. A scripted functor such as `Ext^1(M, N)` dispatches through its
own subscript and superscript functions, so it is a `function` too.

For your own code the same rule applies to the inferred type: `area = method()`
makes `area` a method, `twice = n -> …` a function.

## Modifiers { #modifiers }

| Modifier | Standard | Meaning | Example |
| --- | :---: | --- | --- |
| `declaration` | <i class="fa-solid fa-check"></i> | where a name is bound, or what an installation installs on | `R =`, `n :=`, `x` in `QQ[x]`, `area` in `area Matrix :=` |
| `defaultLibrary` | <i class="fa-solid fa-check"></i> | a value selected by `semanticTokens.defaultLibrary` | by default, compiled functions such as `apply` and `scan` |
| `readonly` | <i class="fa-solid fa-check"></i> | a binding protected from reassignment, independently of value mutability | `ZZ`, a protected package export |
| `documentation` | <i class="fa-solid fa-check"></i> | inside documentation: a `doc` string, a `document` call, a [documentation comment](../beyond/documentation-comments.md) | everything inside `doc ///…///` |
| `algebraic` | | a generator made by an algebraic constructor | `x`, `y` in `QQ[x, y]` |
| `option` | | an option key | `Strategy` in `Strategy => Linear` |
| `access` | | an operator selecting a part of its left operand | `_` in `S_1`, `.`, `.?`, binary `#` and `#?` |
| `example` | | code run as a documentation example | the lines under `Example` |
| `test` | | code inside a `TEST` block | the body of `TEST ///…///` |
| `strong` | | bold documentation text | `{\bf …}`, `BOLD`, a headline |
| `emphasis` | | italic documentation text | `{\em …}`, `{\it …}`, `EM` |
| `monospace` | | monospace documentation text | `{\tt …}`, `TT`, `CODE`, a `Pre` block |
| `link` | | the address of a link | `\url{…}`, `HREF` |
| `math` | | TeX mathematics | `$x^2$` |

The complete list of `defaultLibrary` policies and their alias behavior is in
[Configuration](../getting-started/configuration.md#semantic-tokens). The
compiled policies describe the value; package policies describe the binding's
origin. `readonly` independently describes symbol protection. Local aliases and
shadows remain assignable, and `exportMutable` names remain unmarked.

With [`semanticTokens.standardOnly`](../getting-started/configuration.md#semantic-tokens)
only the four standard modifiers are sent, and `escapeSequence` falls back to
`string`. The former `command`, `file`,
`mutable`, and `unresolved` modifiers are not emitted. Unbound expression names
use `variable`.

## Subscripts { #subscripts }

`_` both spells objects and selects from them, and the highlighting tells the
two apart. A subscript that is one object of the kind of one of its parts is a
single token; every other access keeps its parts:

```m2
R = RR_53                   -- RR_53 type: one field of that precision
S = R[x, y]
1_S                         -- 1_S number: the number 1, in S
S_1                         -- S type · _ operator+access · 1 variable+algebraic
L = {S_0, S_1}
L_0                         -- L variable · _ operator+access · 0 property
L#i                         -- # operator+access · i variable
```

A literal selector is colored by what it selects: a generator in `S_1`, a
member in `L_0`. The `access` modifier marks the operators that select — `_`,
`.`, `.?`, binary `#` and `#?` — so a theme can fade them and let the base and
the selector stand out, as `x.1` reads in many Rust themes. Prefix `#L`, the
length, is a computation and stays a plain operator. More on how rings are
read in [Rings and their variables](rings.md).

## Code inside strings and documentation { #embedded }

Macaulay2 keeps a lot of code in strings, and highlighting follows it there:

```m2
export {"double"}               -- "double" colored as the function it exports
exportMutable {"verbosity"}     -- "verbosity" variable, without readonly
beginDocumentation()
doc ///
Key                             -- decorator+documentation
  double                        -- function+documentation
Headline
  doubles a number              -- string+documentation+strong
Description
  Example
    double 3                    -- function+documentation+example
///
TEST ///
assert(double 2 == 4)           -- assert method+test, 2 number+test, == operator+test
///
```

The code of `TEST` blocks and documentation examples is analyzed as real code,
so its names resolve against the package exactly as they would when the tests
run. A plain variable named inside a string keeps the string's color instead of
dropping to the default text color.

## Syntax tokens and your editor's highlighter { #syntax }

Keywords, strings, numbers, operators and comments are also sent as tokens,
for editors that rely on the server alone. When the client announces
`augmentsSyntaxTokens` — it highlights syntax itself, for instance with a
Tree-sitter grammar — the server leaves those to it and sends only the tokens it
knows more about: names, roles such as an `access` operator or the literal it
selects with, and the modifier keywords `symbol`, `local` and `global`. Strings and comments that contain code or documentation are still
sent, so the code inside them is colored.

## Precedence { #precedence }

When several facts apply to one name, the most specific one wins:

1. **Its role in the source** — a ring generator, the domain or codomain of an
   installation, an option key or value, a quoted name after `symbol`, a
   package name, an access operator or the literal it selects with.
2. **A local binding**, by its inferred type: a parameter, a method function,
   a type.
3. **A library object**, by the class the catalog records.
4. **A definition in another workspace file.**
5. Otherwise, an expression name is a **variable**.

An installation head keeps the token its object has elsewhere and adds
`declaration`, so `area` in `area Matrix := …` is still a method.

## Fine print

- A function used as an `if` or `while` condition is shown as a plain variable
  : it is a value there, not a call.
- A local `n := 5` is a variable, never a number — literal types do not leak
  into names.
- The `option` modifier applies to keys only; values retain their ordinary
  classification.

## Neovim { #neovim }

Neovim maps every token to highlight groups named after the wire names:
`@lsp.type.<type>.macaulay2`, `@lsp.mod.<modifier>.macaulay2` and
`@lsp.typemod.<type>.<modifier>.macaulay2`. Most colorschemes already style the
`@lsp.type.*` groups; the Macaulay2 modifiers are yours to style:

```lua
local function link(group, target)
  vim.api.nvim_set_hl(0, group, { link = target })
end

link('@lsp.typemod.variable.algebraic.macaulay2', '@constant')
link('@lsp.typemod.operator.access.macaulay2', '@punctuation.delimiter')
link('@lsp.mod.readonly.macaulay2', '@constant')
link('@lsp.mod.defaultLibrary.macaulay2', '@function.builtin')
link('@lsp.type.typeParameter.macaulay2', '@type.definition')
link('@lsp.type.decorator.macaulay2', '@attribute')
```

`:Inspect` on a name lists the groups applied to it.

Regular-expression highlighting applies to the first positional pattern argument
of the Core functions above. Replacement and subject strings keep their string
color, as does the single argument of `separate text`. A locally shadowed
function name does not assign pattern meaning to its arguments. Package names
receive namespace highlighting in package calls; `toString "_"` and `net "_"`
keep ordinary string highlighting.

## Regular-expression literals

In `match(///name[0-9]+\.m2///, text)`, the `///` delimiters and ordinary
characters remain strings. Brackets, the range hyphen, and `+` receive `regexp`;
`\.` receives `escapeSequence`. Inside a bracket expression, ordinary dots and
stars remain string text. Quoted patterns preserve the full spelling of their
escapes, so `"\\d"` highlights `\\d` as one escape sequence.

Only literal pattern arguments are refined. Replacement strings, input text,
and calls through a locally shadowed function keep their ordinary classification.
