# Signature help

While you type the arguments of a call, signature help shows every method the
callable has and highlights the argument you are on.

```m2
G = gb(ideal(x^2, y), |
```

With the cursor inside `gb( … )`:

```text
gb(Ideal) -> GroebnerBasis      ← active
gb(Module) -> GroebnerBasis
gb(Matrix) -> GroebnerBasis
```

It opens on `(` and refreshes on `,`, and the active parameter is the number of
arguments completed before the cursor. For a method function of your own, the
signatures are the installations in your code, with the codomains they declare;
for library and package callables they come from the catalog.

Macaulay2 calls do not need parentheses — `gb I` is a call too. Signature help
reacts to the argument list of any application whose arguments contain the
cursor, so `f (a, b)` and `f(a, b)` behave alike.

For the options a call accepts, start typing a capitalized name inside it:
[completion](../writing/completion.md#options) offers the option keys.

## Reading the active signature

The active parameter follows the argument position, counting completed
arguments before the cursor. The active signature is the first candidate
with enough parameter slots for that position, falling back to the first
signature when none has one. It is an editing aid; use
[hover](hover.md#the-signature-your-call-uses) for the signature selected using
inferred call argument types.

For a nested call, help belongs to the innermost enclosing call whose argument
contains the cursor. Move back into the outer argument list to inspect the
outer callable. A local binding takes precedence over a library callable with
the same spelling, so its signatures describe the function your code sees.

## Availability

Your editor can request help manually even when no trigger character was just
typed. Automatic popup behavior and signature cycling are client settings.
Missing callable metadata or an incomplete call can leave no signature to
show. Installed library methods may provide domain-only signatures when no
documented result type is available; absence of a displayed codomain does
not mean the call returns `null`.
