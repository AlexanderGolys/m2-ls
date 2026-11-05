# Known limitations

The server is useful precisely because it does not need to execute a file, but
that also sets its boundary.

- **Dynamic results may stay unknown.** Code that constructs names, methods, or
  values at runtime can escape what an editor can prove from the source.
- **Completion is intentionally sparse.** A broad expression position produces
  no giant global list. Type a narrowing prefix or use a context such as
  `new`, a package import, or an option argument.
- **The library catalog is a snapshot.** Custom packages and objects from a
  different Macaulay2 installation may have less complete documentation,
  signatures, types, or completion.
- **Library source is optional.** Jumps into Core and package files need their
  source, found next to `M2` or configured in `sources.packagePaths`; without it, the
  rest of the package knowledge remains available. Functions compiled into
  Macaulay2 have no source definition, so jumps to them return nothing.
- **The workspace is what your files load.** Cross-file navigation, search and
  rename cover the open files, what they `load`/`needs`/`input`, the user
  packages they import, and `init.m2` — not every `.m2` file below the project
  root. A file nothing opens or loads stays invisible, except that moving a
  file also edits the loads in the workspace folders that mention it.
- **Incomplete syntax reduces context.** While a construct is half typed, some
  semantic features may temporarily disappear and return when its shape is
  recognizable again.
- **Uncommon method forms are still uneven.** Very elaborate installation and
  assignment patterns may receive only partial analysis.
- **Formatting is document-wide.** Review the result in files that intentionally
  use highly unusual visual layout. A multi-line `else if` chain on the right
  of `:=` inside a block is currently indented unevenly.

These are conservative failures: the usual outcome is missing information, not
a fabricated destination or type.
