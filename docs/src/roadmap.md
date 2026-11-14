# Roadmap

What is planned next. Released changes are listed in the [changelog](https://github.com/AlexanderGolys/m2-ls/blob/main/CHANGELOG.md).

## Ready for a public release

`m2-ls` is already on crates.io, but getting it today still assumes a Rust
toolchain, a local copy of this guide, and trust that every platform builds.
The plan is to make the first contact as short as possible and keep each
release honest:

- [ ] a continuous integration workflow that builds, tests and runs Clippy on
  Linux, macOS and Windows, and checks the Rust 1.88 floor;
- [ ] prebuilt binaries attached to every GitHub release, so installing does
  not require `cargo`;
- [ ] this guide published online and linked from the README and the crate
  page, instead of only being readable with `mdbook serve`;
- [ ] a shorter README that keeps the pitch, install and first editor setup,
  and points to the guide for the configuration and semantic token tables.

## Smaller items

- [ ] Move the features overview out of the misspelled `gettings-started/`
  folder into `getting-started/overview.md`, which is currently an empty
  placeholder.
- [ ] Say in the README what the local usage log records, where it lives, and
  that `M2_LS_USAGE_LOG=off` turns it off.
- [ ] Indent a multi-line `else if` chain on the right of `:=` inside a block
  evenly when formatting.
- [ ] Offer a ready-made `m2_ls` configuration to nvim-lspconfig, so Neovim
  users can enable the server by name.
