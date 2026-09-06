#!/usr/bin/env bash
set -euo pipefail

book_dir="$(cd "$(dirname "$0")" && pwd)"
nvim_ts_dir="${NVIM_TS_DIR:-/home/flux/.local/share/nvim/lazy/nvim-treesitter}"
m2_grammar_dir="${M2_GRAMMAR:-/home/flux/m2/tree-sitter-macaulay2}"

mkdir -p "$book_dir/parsers" "$book_dir/queries" "$book_dir/theme"

stage_language() {
    local language="$1"
    local parser="$nvim_ts_dir/parser/$language.so"
    local highlights="$nvim_ts_dir/queries/$language/highlights.scm"

    [[ -f "$parser" ]] || {
        echo "Missing parser: $parser" >&2
        exit 1
    }
    [[ -f "$highlights" ]] || {
        echo "Missing highlights query: $highlights" >&2
        exit 1
    }

    mkdir -p "$book_dir/queries/$language"
    cp "$parser" "$book_dir/parsers/$language.so"
    sed '/^; inherits:/d' "$highlights" > "$book_dir/queries/$language/highlights.scm"
}

for language in bash json lua rust toml; do
    stage_language "$language"
done

[[ -f "$m2_grammar_dir/src/parser.c" ]] || {
    echo "Missing Macaulay2 grammar: $m2_grammar_dir/src/parser.c" >&2
    exit 1
}

m2_scanner=()
if [[ -f "$m2_grammar_dir/src/scanner.c" ]]; then
    m2_scanner=("$m2_grammar_dir/src/scanner.c")
fi

cc -O2 -shared -fPIC -I "$m2_grammar_dir/src" \
    "$m2_grammar_dir/src/parser.c" "${m2_scanner[@]}" \
    -o "$book_dir/parsers/macaulay2.so"

mkdir -p "$book_dir/queries/macaulay2"
cp "$m2_grammar_dir/queries/macaulay2/highlights.scm" \
    "$book_dir/queries/macaulay2/highlights.scm"

mdbook-tsitter css > "$book_dir/theme/treesitter.css"

echo "The documentation assets are ready."
echo "Run: mdbook serve '$book_dir' --hostname 127.0.0.1 --port 3000"
