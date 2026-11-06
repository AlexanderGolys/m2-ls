# Command-line tools

Installing `m2-ls` also installs two standalone commands. They use the same
diagnostics and formatter as the server, so a file that is clean in the editor
is clean in CI as well.

| Command | What it does |
| --- | --- |
| `m2-lint` | Reports a file's static diagnostics without running Macaulay2. |
| `m2-fmt` | Formats a file in place, checks it, or prints the result. |

## Linting

```bash
m2-lint file.m2                  # report the file's diagnostics
m2-lint --deny-warnings file.m2  # fail on warnings as well as errors
```

Each finding is printed on one line as `file:line:column: severity[code]:
message`, naming the file as it was given (`-` for standard input). Findings that point at related
locations are followed by one `note:` line per location:

```text
sizes.m2:1:8: hint[S06]: Ring variable `x0` is a multi-character symbol, not an indexed variable; use `x_0` so the generators form one indexed family (`x_0`, `x_1`, ...)
sizes.m2:11:1: warning[E01]: Installing a method on `f` has no effect: `f` is not a method function. Define it with `f = method()` to make method installations take effect.
sizes.m2:10:1: note: `f` is defined here as a plain function
```

Lines and columns are one-based, and columns are counted in UTF-16 code units,
the same way editors count them. Hints and information are printed too, but they never make the
command fail.

## Usage history

`m2-lint` and the language server append actual results to the same JSONL file:
`$XDG_STATE_HOME/m2-ls/usage.jsonl`, or `~/.local/state/m2-ls/usage.jsonl` when
`XDG_STATE_HOME` is unset. Each line is one JSON object with `timestamp_ms`,
`pid`, `tool`, `version`, `event` and `data` fields.

`diagnostics` records contain the document URI, a SHA-256 source hash, the
finding count and the published diagnostics, including their rule codes,
messages, severities and ranges. Source text is not copied into the log.
Ranges use LSP's zero-based lines and UTF-16 columns. Empty diagnostic results
are retained, so you can see when a file became clean. Identical republishes of
the same source and findings within an editor session are skipped; separate
command-line runs are retained.

`catalog` records report available package counts and loaded object counts.
`indexing` records report completed workspace or library indexing, with package
and file counts and elapsed milliseconds. Workspace passes also report processed
files and cache hits. These are results of actual operations, without periodic
resource sampling or protocol traffic.

For example, count findings by rule across the retained history:

```bash
jq -s '[.[] | select(.event == "diagnostics") | .data.diagnostics[]] |
  group_by(.code) | map({code: .[0].code, count: length}) |
  sort_by(.count) | reverse' ~/.local/state/m2-ls/usage.jsonl
```

Set `M2_LS_USAGE_LOG` to another path, or to `off` or an empty value to disable
this history.
The existing operational log at `/tmp/m2-ls.log` remains separate; set
`M2_LS_LOG=off` to disable it. Tests never enable either file writer, and test
subprocesses explicitly disable both logs. Log output does not change lint
results or write into the LSP protocol stream.

## Formatting

```bash
m2-fmt file.m2            # format the file in place
m2-fmt --check file.m2    # exit 1 if it needs formatting; write nothing
m2-fmt --stdout file.m2   # print the formatted file
cat file.m2 | m2-fmt -    # format standard input to standard output
```

`--check` and `--stdout` cannot be combined. `--check` is the form to use in
CI.

## Choosing the input

Both commands take exactly one input: a `.m2` file, or a lone `-` to read
standard input. A missing input, a second one, or a directory is a usage
error. An argument after `--` is always taken as the file, even when it starts
with `-`. To cover a project, run the command once per file, for example from
your build system or CI script.

## Settings

Both tools load `~/.m2-ls/M2LS.toml`, then the nearest ancestor `M2LS.toml`
of the input file (the current directory for stdin). Pass `--config settings.json`
or `--config settings.toml` to overlay explicit settings. These use the same
groups as the server, including `diagnostics` and `formatting`:

```json
{
  "m2-ls": {
    "diagnostics": { "disabled": ["unused-binding"] },
    "formatting": { "indent": 2 }
  }
}
```

`m2-lint` also indexes explicit dependencies. When an ancestor `M2LS.toml`
or `.git` marks a project, it discovers the package entry point and resolves
included fragments in their package context. This does not execute the files.

Like the server, `m2-lint` knows the packages your
[init file](../getting-started/configuration.md#init-file) loads, so names
they export are not reported as unresolved; `initialization.files` and the
`initFile` directive select it the same way.

Unlike the server, the commands do not keep defaults for invalid values: an
invalid setting fails the command with status `2`. A deprecated key is read
as its replacement, with a warning on stderr. See
[Configuration](../getting-started/configuration.md) for every setting.

## Exit status

| Status | Meaning |
| --- | --- |
| `0` | Success. |
| `1` | Lint errors (also warnings with `--deny-warnings`), or a file that needs formatting under `--check`. |
| `2` | A usage, configuration, or I/O error. |

## From a checkout

Without installing, run the commands through Cargo:

```bash
cargo run --bin m2-lint -- file.m2
cargo run --bin m2-fmt -- --check file.m2
```

A plain `cargo run` still starts the language server.
