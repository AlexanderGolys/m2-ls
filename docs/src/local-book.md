# Read this guide locally

The guide is an [mdBook](https://rust-lang.github.io/mdBook/). Install the two
tools once:

```bash
cargo install mdbook mdbook-tsitter
```

From the repository root, start the live-reloading server:

```bash
mdbook serve docs --hostname 127.0.0.1 --port 3000
```

Open `http://127.0.0.1:3000`. Saving a chapter rebuilds the book and refreshes
the page; `mdbook build docs` writes the static site to `docs/book/`.

Every code block is highlighted with Tree-sitter, Macaulay2 included. On the
first build `mdbook-tsitter` downloads and compiles the grammars it needs and
caches them, so later builds are offline.
