# Semantic highlighting

A syntax highlighter sees `area`, `rank` and `x` as three identical
identifiers. `m2-ls` knows that `area` is a method function you made, `rank` a
library method, and `x` a generator of a polynomial ring — and colors each by
what it is bound to, not by how it is spelled or where it stands.

```m2
R = §type.readonly|QQ§[§variable.declaration.algebraic|x§, §variable.declaration.algebraic|y§]                      -- R type · QQ type · x, y variable+algebraic
area = §function.readonly|method§()                   -- area method · method function
area §typeParameter.readonly|Matrix§ := §typeParameter.readonly|ZZ§ => §parameter.declaration|m§ -> §method.readonly|rank§ §parameter|m§  -- Matrix typeParameter · ZZ typeParameter · m parameter
twice = §parameter.declaration|n§ -> 2 * §parameter|n§                -- twice function
scan({1, 2}, §function.readonly|print§)               -- scan function+defaultLibrary · print function
G = §method.readonly|gb§(§variable|I§, §enumMember.option.readonly|Strategy§ => §enumMember.readonly|Linear§)     -- gb method · Strategy enumMember+option
stdio << "done" << §function.readonly|endl§           -- stdio variable · endl function
f = §modifier|symbol§ §enumMember|g§                      -- symbol modifier · g enumMember
typo + 1                          -- typo variable
needsPackage §namespace|"JSON"§               -- "JSON" namespace
load §namespace|"lib/helper.m2"§              -- "lib/helper.m2" namespace
```

Most tokens use standard LSP types. Embedded code uses custom `delimiter`
and `punctuation` types for group boundaries and separators, both falling
back to `operator` under `standardOnly`. Pattern escapes use the custom
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
| Parameter | `parameter` | a function parameter, or an input named on a documentation page, an option name that is not a protected symbol | `m` in `m -> rank m`, `verbose` in `{verbose => false} >> o -> …` |
| Property | `property` | a key rather than a binding: a member an access selects, a string hash key | `key` in `H.key`, `0` in `L_0`, `"x"` in `"x" => 1` |
| Namespace | `namespace` | a package, including package names in strings, and the file a `load`, `needs` or `input` call runs | `Core`, `"JSON"` in `needsPackage "JSON"`, `"lib/helper.m2"` in `load "lib/helper.m2"` |
| Symbol | `enumMember` | a name used as a `Symbol` rather than for its value | `Strategy`, `Linear`, `g` in `symbol g` |
| Domain or codomain | `typeParameter` | the types an installation is made on and returns | `Matrix`, `ZZ` in `area Matrix := ZZ => …` |
| Annotation | `decorator` | a documentation link | `TO`, `TOH`, `TO2` |
| Markup | `macro` | markup around documentation text | `{\tt`, `}`, `PARA`, `TT` |
| Modifier keyword | `modifier` | the quoting keywords | `symbol`, `local`, `global`, `threadVariable` |
| Keyword | `keyword` | language keywords, SimpleDoc section keywords, and document option names | `if`, `then`, `for`, `TEST`, `Key`, `Description` |
| String | `string` | string literals | `"done"`, `///…///` |
| Regular expression | `regexp` | special syntax within literal patterns of `match`, `regex`, `replace`, `select`, and `separate`; delimiters and plain text stay `string` | `+` in `replace(///a+///, "b", text)` |
| Escape sequence | `escapeSequence` | escaped characters in patterns, retaining their source spelling | `\d`, `\.`, `\\d` in quoted strings |
| Number | `number` | integer and float literals | `2`, `0.5` |
| Operator | `operator` | operators | `=`, `:=`, `->`, `<<`, `_` |
| Comment | `comment` | comments | `-- note` |
| Delimiter | `delimiter` | group boundaries in embedded documentation code | `(`, `)`, `[`, `]`, `{`, `}` |
| Punctuation | `punctuation` | separators in embedded documentation code | `,`, `;` |

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
| `algebraic` | | a ring generator or a known nonnumeric value living over a ring | `x`, an ideal, a module, a matrix |
| `option` | | an option key | `Strategy` in `Strategy => Linear` |
| `memberAccess` | | an operator selecting a part of its left operand | `_` in `S_1`, `.`, `.?`, binary `#` and `#?` |
| `example` | | code run as a documentation example | the lines under `Example` |
| `test` | | code inside a `TEST` block | the body of `TEST ///…///` |
| `strong` | | bold documentation text | `{\bf …}`, `BOLD`, a headline |
| `emphasis` | | italic documentation text | `{\em …}`, `{\it …}`, `EM` |
| `monospace` | | monospace documentation text | `{\tt …}`, `TT`, `CODE`, a `Pre` block |
| `url` | | the address of a web link | `\url{…}`, `HREF` |
| `file` | | a path to a file | `load "foo.m2"` |
| `directory` | | a path to a directory (reserved) | |
| `wikilink` | | the target of a wikilink | `[[ideal]]` |
| `tex` | | TeX mathematics | `$x^2$` |
| `injectedCode` | | code written inside a string | a `TEST` body, an example |
| `injectedName` | | a string naming an object not necessarily in scope | `needsPackage "Foo"` |
| `abstract` | <i class="fa-solid fa-check"></i> | standard fallback for `algebraic` and `nonsense`; otherwise reserved | `variable+abstract` in standard-only mode |
| `index` | | reserved for structural indices in known indexed generator names; currently not emitted | no source spans yet |
| `nonsense` | | reserved companion to `abstract`; currently not emitted | no source spans yet |

The complete list of `defaultLibrary` policies and their alias behavior is in
[Configuration](../getting-started/configuration.md#semantic-tokens). The
compiled policies describe the value; package policies describe the binding's
origin. `readonly` independently describes symbol protection. Local aliases and
shadows remain assignable, and `exportMutable` names remain unmarked.

## Standard-only compatibility { #standard-only }

By default the full vocabulary is available to themes. Enable
[`semanticTokens.standardOnly`](../getting-started/configuration.md#semantic-tokens)
when your editor or theme needs standard LSP classifications. The legend stays
stable; emitted tokens use these fallbacks:

| Full vocabulary | Standard-only output |
| --- | --- |
| `escapeSequence` type | `string` |
| `delimiter`, `punctuation` types | `operator` |
| All other token types | unchanged |
| `algebraic`, `nonsense` modifiers | `abstract` |
| `example`, `strong`, `emphasis`, `monospace`, `url`, `file`, `directory`, `wikilink`, `tex` modifiers | `documentation` |
| `option`, `memberAccess`, `index`, `test` modifiers | removed |
| `declaration`, `defaultLibrary`, `readonly`, `documentation`, `abstract` modifiers | unchanged |

Fallbacks combine without duplicating modifiers: a
`string+documentation+monospace` token becomes `string+documentation`.
An ideal classified as `variable+algebraic` becomes `variable+abstract`.
Compatibility mode sacrifices visual distinctions; it does not change name
resolution or type inference.


## Algebraic values { #algebraic }

Rings are type objects; their elements and the objects built over them are
values. Highlighting preserves that distinction:

```m2
R = §type.readonly|QQ§[§variable.declaration.algebraic|x§, §variable.declaration.algebraic|y§]
I = §method.readonly|ideal§(§variable.algebraic|x§^2, §variable.algebraic|y§)
S = §type|R§/§variable.algebraic|I§
M = §type|S§^2
A = §method.readonly|matrix§ {{§variable.algebraic|x§, §variable.algebraic|y§}}
f = §variable.algebraic|x§^2 + §variable.algebraic|y§
n = 3
```

Here `R` and `S` receive `type`. The inferred ideal `I`, module `M`,
matrix `A`, polynomial `f`, and generators `x` and `y` receive
`variable+algebraic`. Their binding introductions also carry `declaration`.
The numeric binding `n` remains a plain variable.

For known inferred classes, `algebraic` means that the catalog records an
installation of `ring` on that class or an ancestor. Numbers are excluded,
and type classification takes precedence. This supports package-defined
objects too, provided their type and `ring` installations are known.
Incomplete inference can leave a value as an ordinary variable.

## Subscripts { #subscripts }

`_` both spells objects and selects from them, and the highlighting tells the
two apart. A subscript that is one object of the kind of one of its parts is a
single token; every other access keeps its parts:

```m2
R = §type|RR_53§                   -- RR_53 type: one field of that precision
S = §type|R§[§variable.declaration.algebraic|x§, §variable.declaration.algebraic|y§]
1_S                         -- 1_S number: the number 1, in S
S_§variable.algebraic|1§                         -- S type · _ operator+access · 1 variable+algebraic
L = {§type|S§_§variable.algebraic|0§, §type|S§_§variable.algebraic|1§}
L_§property|0§                         -- L variable · _ operator+access · 0 property
L#§variable|i§                         -- # operator+access · i variable
```

A literal selector is colored by what it selects: a generator in `S_1`, a
member in `L_0`. The `memberAccess` modifier marks the operators that select — `_`,
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
Key                             -- keyword+documentation
  double                        -- function+documentation
Headline
  doubles a number              -- string+documentation+strong
Description
  §keyword.documentation|Example§
    §variable.documentation.example|double§ 3                    -- function+documentation+example
///
TEST ///
assert(§variable.test|double§ 2 == 4)           -- assert method+test, 2 number+test, == operator+test
///
```

The code of `TEST` blocks and documentation examples is analyzed as real code,
so its names resolve against the package exactly as they would when the tests
run. A plain variable named inside a string keeps the string's color instead of
dropping to the default text color.

SimpleDoc section names such as `Key`, `Description`, and `Example`
receive `keyword+documentation`. Hypertext link constructors such as `TO`
receive `decorator+documentation`; formatting constructors such as `TT`
receive `macro+documentation`. Both `@` delimiters of an inline expression
and the delimiters of TeX formatting receive markup tokens.

Quoted documentation references and styled strings include their quotation
marks in the highlight. For example, `TO "Matrix"` resolves the name
`Matrix`, but colors the entire `"Matrix"` literal as a type; `TT "code"`
marks the entire literal as `string+monospace`. TeX formatting such as
`{\tt code}` keeps its markup delimiters separate from its styled content.

Embedded code includes its group boundaries as `delimiter` and its commas
and semicolons as `punctuation`. These are emitted even when the editor
already highlights ordinary source syntax.

In `CannedExample` transcripts, `i1 :` prompts remain text while their
input bodies receive code tokens. An `o1 =` output and its continuation lines
remain text until the next input prompt.

References use local, catalog, and workspace information, including loaded
files. A supported client receives a semantic-token refresh after loaded-file
indexing finishes, replacing classifications requested before those definitions
were available.

## Syntax tokens and your editor's highlighter { #syntax }

Keywords, strings, numbers, operators and comments are also sent as tokens,
for editors that rely on the server alone. When the client announces
`augmentsSyntaxTokens` — it highlights syntax itself, for instance with a
Tree-sitter grammar — the server leaves those to it and sends only the tokens it
knows more about: names, roles such as an `memberAccess` operator or the literal it
selects with, and the modifier keywords `symbol`, `local` and `global`. Strings and comments that contain code or documentation are still
sent, so the code inside them is colored.

## Precedence { #precedence }

When several facts apply to one name, the most specific one wins:

1. **Its role in the source** — a ring generator, the domain or codomain of an
   installation, an option key, a quoted name after `symbol`, a
   package name, an access operator or the literal it selects with.
2. **A local binding**, by its inferred type: a parameter, a method function,
   a type.
3. **A library object**, by the class the catalog records.
4. **A definition in another workspace file.**
5. Otherwise, an expression name is a **variable**.

An installation head keeps the token its object has elsewhere and adds
`declaration`, so `area` in `area Matrix := …` is still a method.

## Fine print

- A function used as an `if` or `while` condition is shown as a plain variable:
  it is a value there, not a call.
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
link('@lsp.type.delimiter.macaulay2', '@punctuation.bracket')
link('@lsp.type.punctuation.macaulay2', '@punctuation.delimiter')
link('@lsp.type.escapeSequence.macaulay2', '@string.escape')
```

`:Inspect` on a name lists the groups applied to it. A modifier can be present
without a visible style: themes need their own bold, italic, or foreground
settings for custom documentation modifiers. Apply custom groups after the
colorscheme loads, and reapply them when switching colorschemes.

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
