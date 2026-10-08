# Contributing

`m2-ls` is a single Rust crate at the repository root. Keep changes focused on
the crate unless a task explicitly touches repository docs or metadata.

## Where things live

```text
src/
  main.rs                 stdio entry point and protocol wiring
  document.rs             versioned snapshots: source, parse tree, analysis
  source.rs               indexed source text and the single position-conversion API
  analysis.rs, analysis/  staged analysis: scopes, bindings, installations, typechecking
  typesystem.rs           static type inference over the object facts
  node_metadata/          per-node facts produced by analysis and consumed by features
  object_registry.rs      canonical object identities and registry lookup
  builtin_index.rs        in-memory form of the builtin corpus
  package_index.rs        imported-package discovery and source-file resolution
  workspace_index.rs      cross-file index of top-level definitions
  diagnostic_registry.rs  every diagnostic's identity, name, and code
  documentation.rs        Markdown documentation attached to source items
  documentation_site.rs   mdBook generation for `m2-ls docs`
  settings.rs             initialization and live workspace configuration
  capabilities/           one module per LSP feature
  data/m2-index.jsonl     generated builtin corpus
docs/                     the user guide (mdBook)
```

Analysis runs first and records what it learns on syntax nodes; the modules in
`capabilities/` read those facts rather than re-deriving them. Treat every
`target/` directory as build output.

## Development

Run Cargo from the repository root. `cargo run` starts the server on stdio;
point an editor at `target/debug/m2-ls` after `cargo build` to try a change.

Before committing, run:

```sh
cargo fmt -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test
cargo build
cargo package --allow-dirty
```

The build is expected to be warning-free. For LSP behavior changes, also test
through an editor client when practical. Non-ASCII text matters: LSP positions
are UTF-16, while Tree-sitter positions are byte-based.

## Builtin metadata

`src/data/m2-index.jsonl` is a generated artifact: one JSON record per line,
produced from an installed Macaulay2's documentation and runtime metadata. The
extractor is not shipped in this repository yet, so do not hand-edit the corpus;
regenerate it wholesale instead. The leading `{"kind":"meta", ...}` record (the
default-loaded package baseline) is mandatory; the server fails fast at startup
without it.

## Documentation

Keep `README.md` short and user-facing. Feature and configuration details belong
in the user guide under `docs/src/`; see `docs/src/local-book.md` to preview it.
