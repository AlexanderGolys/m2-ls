# Modifiers

Modifiers let your theme show more than an object's category. A ring generator
and a local counter both use the `variable` token type, but the generator also
has `algebraic`. A method installation keeps its `method` type and adds
`declaration` at the installation head.

## Binding and source roles

`declaration`
: Marks the place a binding is introduced, including ring variables and method
  installation heads. Use it to make definitions stand out among their uses.

`defaultLibrary`
: Marks objects selected by `semanticTokens.defaultLibrary`. The default policy
  selects compiled functions. Other policies can select package bindings;
  consult the [policy reference](../getting-started/configuration.md#semantic-tokens)
  for accepted values and alias behavior.

`libraryConst`, `libraryConfigParam`
: Mark indexed library bindings exported as protected or mutable, respectively.
  For example, `ZZ` carries `libraryConst`, while `allowableThreads` carries
  `libraryConfigParam`. Protection describes reassignment of the binding,
  independently of whether its value can be mutated. Local aliases and shadows
  carry neither modifier. Both are independent of `defaultLibrary` and are
  removed in standard-only mode.

`readonly`
: Marks a protected binding. This describes reassignment of the name, rather
  than whether the value itself can be mutated. Local shadows and mutable
  exports do not acquire protection merely by sharing a spelling. Inside a
  package's own source, including the files that declare it, its exports are
  not protected: the package manager protects them only when it links the
  package for its users.

`algebraic`
: Marks ring generators and inferred nonnumeric values whose classes have
  a known `ring` installation, including ideals, modules, and matrices.
  Rings themselves remain `type`. See [algebraic values](semantic-highlighting.md#algebraic).

`ring`, `module`, `morphism`, `complex`, `functor`, `structure`, `space`
: Mark a whole algebraic or geometric object with its kind, on top of its
  usual token type: a ring is a `type`, a functor a `function`, the others
  variables. See [kinds](semantic-highlighting.md#kinds).

`optionalParameter`
: Marks an option key, which is a `property`. Only a name denoting a
  `Symbol` is a key: a protected symbol such as `Strategy`, or a name nothing
  binds. A string, or a name bound to a function or variable, is a value and
  keeps its own token. Inside a function that receives an option table,
  `{…} >> o -> …` or an installation of a method declaring `Options`, the keys
  read from the table, `o.verbose` and `o#verbose`, carry it too. Its value
  retains its own token classification.

`memberAccess`
: Marks part-selection operators: `_`, `.`, `.?`, binary `#`, and `#?`.
  A theme can deemphasize them so the base and selected member stand out.
  Prefix `#`, which computes a length, has no `memberAccess` modifier.

## Documentation and embedded code

`documentation`
: The standard-only fallback for custom documentation style modifiers.
  Prose uses the `docs` token type; embedded code does not inherit a blanket
  documentation modifier, so its own token styles remain visible.

`documentTag`
: Marks the parts of a documentation tag: the key a `Key` entry documents or a
  `TO`, `TOH` or `TO2` link names, as in `(f, ZZ)`, `[f, Opt]` or
  `(symbol +, ZZ, ZZ)`. Every `TO`, `TOH` or `TO2` target carries it, whether
  it names an object, a package, or only a page title. The text of a `TO2`
  link carries it too, as the text of a link to its tag.

`example`
: Marks code in a documentation example. The example's names still receive
  their ordinary semantic categories.

`test`
: Marks code inside a `TEST` block, independently of its token type.

`injectedCode`
: Marks code written inside a string and analysed as code: the body of a
  `TEST` block and documentation examples.

`injectedName`
: Marks a string that names an object which need not be in scope, such as
  the package in `needsPackage "Foo"`.

`strong`, `emphasis`, `monospace`
: Mark bold, italic, and code-style documentation text respectively. This
  includes supported markup commands and documentation section roles.

`url`
: Marks the address in supported documentation link markup, `\url{…}` and
  `HREF`, the text a `TO2 {key, "text"}` link shows, and the title of a page
  linked by a string that names no symbol, `TO "page title"`.

`file`
: Marks a path to a file, such as the argument of `load`.

`directory`
: Marks a path to a directory. Registered in the legend but currently has
  no emission rule.

`wikilink`
: Marks the target of a `[[wikilink]]` in a documentation comment.

`tex`
: Marks TeX mathematics in documentation.

## Reserved modifiers

`index`
: Reserved for structural indices in known indexed generator names. It is
  registered in the legend but currently has no emission rule; indexed
  generators still receive their existing combined spans.

`abstract`, `nonsense`
: `abstract` is standard LSP vocabulary and the standard-only fallback for
  `algebraic` and `nonsense`. The custom companion `nonsense` has no
  automatic emission rule.

## Editor support and configuration

`declaration`, `defaultLibrary`, `readonly`, `documentation`, and `abstract`
are standard LSP modifiers. The remaining modifiers are extensions.
Compatibility mode maps some extensions to standard modifiers and removes
others; consult the complete [standard-only mapping](semantic-highlighting.md#standard-only).

Modifiers combine with token types. In Neovim, for example,
`@lsp.typemod.variable.algebraic.macaulay2` styles algebraic variables, while
`@lsp.mod.readonly.macaulay2` applies to protected bindings of any category.
See the [highlight setup example](semantic-highlighting.md#neovim) and inspect
a token in your editor before choosing a group to style. A modifier can be
present even when your theme gives it no visible distinction.
