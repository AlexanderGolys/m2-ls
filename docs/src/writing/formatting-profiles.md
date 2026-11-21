# Formatting profiles

A profile is a fixed preset of [formatting](formatting.md) settings, chosen
with `formatting.profile`. There are two, and settings files cannot add
more. Every key a profile leaves alone keeps its default, and every key your
configuration sets wins over the profile.

## `core` { #core }

The default: the layout of the Macaulay2 Core library. Its values are the
`[formatting]` defaults themselves, which this is, verbatim:

```toml
{{#include ../../../src/settings/defaults.toml:formatting}}
```

## `modern` { #modern }

The four-column style of the newer Core files and of the current Emacs
`M2-mode`. It sets these keys and keeps every other one from `core`:

```toml
{{#include ../../../src/settings/profiles/modern.toml}}
```

A `profile = "modern"` file thus indents a bracket by four spaces, wraps
lines longer than 100 columns, and spaces lambda parameters like other
commas, `(x, y) ->`.
