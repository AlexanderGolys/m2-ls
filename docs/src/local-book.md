# Read this guide locally

The book is configured for local use only. It does not include a deployment or
hosting workflow.

Install the two documentation tools once:

```bash
cargo install mdbook mdbook-tsitter
```

From the repository root, prepare the Tree-sitter highlighters and start the
live-reloading server:

```bash
./docs/setup.sh
mdbook serve docs --hostname 127.0.0.1 --port 3000
```

Open `http://127.0.0.1:3000`. Saving a Markdown chapter rebuilds the book and
refreshes the page.

`docs/setup.sh` uses the installed Neovim Tree-sitter parsers for Bash, JSON,
Lua, Rust, and TOML, and the sibling `tree-sitter-macaulay2` checkout for
Macaulay2. If those live elsewhere, provide their locations:

```bash
NVIM_TS_DIR=/path/to/nvim-treesitter \
M2_GRAMMAR=/path/to/tree-sitter-macaulay2 \
./docs/setup.sh
```

Every fenced language used by the guide is passed through `mdbook-tsitter`, so
the Macaulay2 examples and the setup/configuration examples use the same
Tree-sitter-based highlighting path.
