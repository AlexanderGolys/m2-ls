# Inline directives

The server recognizes four comment commands. They work in the editor and,
where relevant, in `m2-lint`. They are ordinary comments to Macaulay2 and do
not change how the interpreter runs the file.

| Directive | Scope | Purpose |
| --- | --- | --- |
| `-- m2-ls: disable [rules]` | Entire file | Suppresses selected diagnostics. |
| `-- m2-ls: disable-next-line [rules]` | Immediately following physical line | Suppresses selected diagnostics starting on that line. |
| `-- m2-ls: primaryCode` | Entire file | Analyzes library implementation code without automatic prelude imports or symbol-protection checks. |
| `-- m2-ls: initFile none` or `-- m2-ls: initFile <path>` | Entire file | Starts the file without an init file, or with another one than the configured. |

Commands and rule names are case-sensitive. Whitespace between `--` and
`m2-ls:` is optional. Use a separate line comment for each command. There is
no general `set` command, arbitrary JSON-in-comments syntax, include command,
or formatting-off directive.

## Diagnostic suppression { #suppression }

Selectors are stable diagnostic codes or names from the
[diagnostic catalog](../writing/diagnostics.md#catalog). Spaces, tabs, and
commas separate them, and codes and names can be mixed:

```m2
-- m2-ls: disable S04, unused-binding
-- m2-ls: disable-next-line S06
R = QQ[x0, x1]
```

The first comment hides `prefer-coalescence` and `unused-binding` throughout
the file. The second hides `ring-variable-naming` only on the `R` line.

### Whole-file suppression

```m2
-- m2-ls: disable protected-assignment
ZZ = 7
```

A `disable` comment applies to the whole file regardless of its position,
including code before it. Repeated directives accumulate: each adds to the
set of hidden rules. There is no corresponding `enable` command and no
bounded disable/enable region. Remove the directive or narrow it to
`disable-next-line` to restore diagnostics elsewhere.

### Following-line suppression

```m2
-- m2-ls: disable-next-line ring-variable-naming
R = QQ[x0]
S = QQ[y0]
```

Only the `R` line is suppressed. The next line means the next physical line,
not the next nonblank statement: a blank line or comment consumes that slot.
For multi-line diagnostics, the diagnostic's starting line determines whether
it is hidden. A trailing directive comment still targets the following line,
not the line containing the comment.

### Omitting selectors and handling mistakes

```m2
-- m2-ls: disable-next-line
R = QQ[x0]
```

With no selectors, every diagnostic is hidden in the chosen scope. Similarly,
`-- m2-ls: disable` hides all diagnostics in the file. An all-rules suppression
cannot be narrowed by adding a later directive.

Unknown selectors are silently ignored. A nonempty selector list containing
only unknown selectors suppresses nothing; it does not mean all rules.
Unknown commands are also ignored. Unlike configuration JSON, directives have
no invalid-setting warning. There is no trailing prose or comment delimiter
inside the directive: any extra words are parsed as more selectors, and any
that name valid rules take effect.

### Interaction with settings and quick fixes

A source comment can hide a diagnostic of any severity, including errors.
It cannot re-enable a rule hidden by `diagnostics.enabled: false` or
`diagnostics.disabled`, and it cannot change severity. Suppression filters the
published findings; analysis, highlighting, and inferred types still run.
In `m2-lint`, suppressed findings are neither printed nor counted toward
failure status.

Every diagnostic offers [quick fixes](../writing/code-actions.md#suppression)
for **Disable … on this line** and **Disable … in this file**. They insert a
`disable-next-line CODE` comment above the finding or a `disable CODE` comment
at the start of the file, respectively.

## Primary implementation code { #primary-code }

```m2
-- m2-ls: primaryCode
ZZ = 7
```

This file property is intended for source implementing the library itself,
such as Core. It applies to the entire file wherever the comment appears.
Files in the discovered Core source and in the directories listed by
[`sources.primaryCodePaths`](configuration.md#primary-code-paths) are in this
mode without the comment.
The command takes no arguments: its trimmed body must be exactly
`primaryCode`. For example, `primaryCode true` or `primaryCode -- Core` is not
recognized. Repeating it has no additional effect.

In this mode:

- Analysis starts without the normal preloaded packages, including packages
  added to the normal session environment by `init.m2`.
- Explicit package imports still become visible in source order.
- Assignments to protected indexed symbols do not produce `E13`.
- Protected names and export strings do not receive the `readonly` modifier.
- Source definitions are analyzed using the resulting environment; other
  applicable diagnostics remain enabled.

This changes analysis rather than merely hiding one error. By comparison,
`disable E13` retains normal prelude lookup and protection highlighting.
`primaryCode` does not run the source or permit an otherwise invalid
assignment in a real Macaulay2 session.

Adding or removing the property reanalyzes the whole file, including earlier
lines. Remove the comment to restore normal session analysis, unless the
file's location implies the mode; there is no `primaryCode false` or region
form. TEST blocks and documentation examples retain their separately analyzed
session environments.

An `E13` error offers the comment as a
[quick fix](../writing/code-actions.md#primary-code), and its message names
it.

## Init file { #init-file }

Every document starts as if the configured
[initialization files](configuration.md#init-file) ran first. Some files should not: a
package, whose code Macaulay2 runs in its own dictionary, or a script you
run with `M2 -q`. Others belong to a project with an init file of its own:

```m2
-- m2-ls: initFile none
newPackage "Thesis"
```

```m2
-- m2-ls: initFile ../session-init.m2
R = QQ[x, y]
helperFromSessionInit R
```

`none`
: Start without an init file: neither its packages nor its globals are
  known.

a path
: Start with that file instead: its packages are loaded and its globals
  known, those of the configured initialization files are not. A relative path is
  relative to the directory of the file, a leading `~` is the home directory,
  and the rest of the line, spaces included, is the path. A file named
  `none` is written `./none`.

The first `initFile` comment counts; later ones are ignored, and one without
an argument is no directive. The file it selects is read the first time a
document needs it and again when it changes, like the configured one, and
its problems are [published the same way](configuration.md#init-file). An
init file never starts with itself, and in
[`primaryCode`](#primary-code) files the directive has no effect: they start
without the session's prelude anyway.

When the selected file cannot be read, the directive is flagged with
`E18` [`unavailable-init-file`](../writing/diagnostics.md#catalog), saying
why, and the document starts without an init file.

TEST blocks and documentation examples never see an init file:
`check` and `installPackage` run them with `-q`.

## Documentation comments are separate { #documentation }

Comments directly above definitions, block documentation comments, Markdown
fences, and `[[name]]` links are [documentation syntax](../beyond/documentation-comments.md),
not configuration commands. They attach text to hover and navigation without
setting server options. A directive must appear as an actual source comment;
text resembling a directive in an ordinary string is not a file property.
