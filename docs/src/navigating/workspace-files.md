# Workspace files

A Macaulay2 project is a web of files running each other with `load`, `needs`
and `input`. The server follows that web the way Macaulay2 does: file names
are highlighted as namespaces, open on a click, complete as you type them,
warn when they lead nowhere, and stay correct when you move files around.

```m2
load "lib/helper.m2"     -- click to open; go to definition works too
needs "./config.m2"      -- warned about if the file is not there
input "lib/|             -- completes the .m2 files and folders of lib/
```

## How a name is looked up { #lookup }

The server looks a name up exactly as `load` does, starting from the file
making the call:

| Name | Where `load` looks | Example |
| --- | --- | --- |
| absolute | the name itself | `load "/opt/m2/setup.m2"` |
| `~/…` | the home directory | `load "~/m2/init-extra.m2"` |
| `./…`, `../…` | only the directory of the loading file | `load "../shared.m2"` |
| anything else | the directory of the loading file, then each directory of `path` | `load "lib/helper.m2"` |
| `$…`, `!…` | a socket or a pipe, not a file | `load "!generate-code"` |

The directories of `path` are the package directories of your Macaulay2
installation and of [`sources.packagePaths`](../getting-started/configuration.md#sources).

The name is read the way Macaulay2 reads the string, so `"we\"ird.m2"` names
the file `we"ird.m2` and `"\x41.m2"` names `A.m2`, while `///a b.m2///` is
taken verbatim. Paths go through the operating system: after a symbolic link,
`..` leads to the parent of where the link points.

## When a file cannot be loaded { #unloadable }

A name that `load` could not run gets an `unloadable-file` warning (E17) on
its string. The message says why:

```m2
load "nofile.m2"        -- No file `nofile.m2` to load, neither in the directory …
load "./gen.m2"         -- … looked up only in the directory of this file
load "lib"              -- `lib` is a directory, not a file: name a `.m2` file inside it
load "secret.m2"        -- `secret.m2` cannot be opened: permission denied
load ""                 -- An empty file name loads nothing
```

A file you may not read counts as unloadable, and so does one inside a
directory you may not search. The warning goes away as soon as the file is
there and readable: create it, fix its permissions, or correct the name.

Some loads are never warned about, because the server cannot judge them:

- documents that are not saved anywhere yet, such as a new unnamed buffer;
- sockets and pipes, `$host:port` and `!command`;
- names computed when the code runs, such as `load(dir | "/helper.m2")`;
- `~/` names when the server has no home directory.

## Following a name { #following }

Every name that leads to a file is a [document link](document-links.md), and
[go to definition](go-to.md) on it opens the file. Its definitions join
[cross-file navigation](go-to.md#workspace) even while the file is closed.

## Completing a name { #completion }

Inside the string, [completion](../writing/completion.md#files) lists the
`.m2` files and folders of the directory typed so far, found the way `load`
finds it, `~/` and absolute names included. Picking a folder continues into
it; names that need escaping are escaped for you.

## Moving and renaming { #moving }

Rename or move a `.m2` file or a whole folder from your editor's file
explorer, and every load that ran it is rewritten to run it from its new
place:

```m2,pane="Before: main.m2"
load "lib/helper.m2"
load ///lib/helper.m2///
```

```m2,pane="After moving lib/helper.m2 to lib/util/helper.m2"
load "lib/util/helper.m2"
load ///lib/util/helper.m2///
```

- **Moved files fix their own loads.** `load "../shared.m2"` inside a file
  that moves one level deeper becomes `load "../../shared.m2"`. Moving a
  folder leaves the loads between its files alone, since they move together.
- **Names keep their style.** Absolute names stay absolute, `~/` names stay in
  the home directory, a leading `./` stays, and `"…"` or `///…///` stays,
  unless the new name cannot be written that way and is quoted instead.
- **A load keeps pointing at its place.** `load "./gen.m2"` is rewritten when
  its loader moves, even before `gen.m2` exists. A plain name found nowhere
  beside its loader is looked up along `path`, so it stays as written.
- **Every loader is found.** The edit covers the files the server knows and
  every file in the open workspace folders that mentions the moved name.

The edit reaches the server through the standard file-operation requests of
the editor's file explorer. A rename done in a shell is still picked up after
the fact, but its loads are not rewritten.

## Changes on disk { #disk }

Files created, deleted, renamed or given other permissions outside the
editor are picked up through the file watcher: navigation follows the files
on disk, and the warnings of the open documents that load them update.

## Workspace folders { #folders }

Open workspace folders bound where a rename looks for loads to rewrite. They
are never indexed wholesale: navigation, [workspace symbols](workspace-symbols.md)
and [rename](rename.md) cover the open documents, the files they load, the
user packages they import, and `init.m2`. A file you opened stays known after
you close it, until its workspace folder closes.

## Fine print

- Hidden directories and symbolic links to directories are skipped when a
  rename searches a folder; a hidden entry is offered by completion once its
  leading `.` is typed.
- File names that are not valid UTF-8 are neither completed nor rewritten.
