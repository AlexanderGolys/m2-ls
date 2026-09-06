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
- **Library source is optional.** Jumps into external package files require
  `M2_LSP_SOURCE_PATH`; without it, the rest of the package knowledge remains
  available.
- **The project root defines the workspace.** Only `.m2` files below the root
  chosen by the editor participate in cross-file search and rename.
- **Incomplete syntax reduces context.** While a construct is half typed, some
  semantic features may temporarily disappear and return when its shape is
  recognizable again.
- **Uncommon method forms are still uneven.** Very elaborate installation and
  assignment patterns, and code embedded in `TEST` blocks, may receive only
  partial analysis.
- **Formatting is document-wide.** Review the result in files that intentionally
  use highly unusual visual layout.

These are conservative failures: the usual outcome is missing information, not
a fabricated destination or type.
