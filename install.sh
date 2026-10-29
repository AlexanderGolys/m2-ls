#!/bin/sh
# Install this checkout and prepare its external builtin catalog.
set -eu

source_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
installation_root=${CARGO_INSTALL_ROOT:-${CARGO_HOME:-}}
if [ -z "$installation_root" ]; then
    : "${HOME:?Set HOME, CARGO_HOME, or CARGO_INSTALL_ROOT}"
    installation_root=$HOME/.cargo
fi

explicit_root=false
previous_option=
for option in "$@"; do
    if [ "$previous_option" = --root ]; then
        installation_root=$option
        previous_option=
        continue
    fi
    case "$option" in
        --root)
            explicit_root=true
            previous_option=--root
            ;;
        --root=*)
            explicit_root=true
            installation_root=${option#--root=}
            ;;
    esac
done

if [ "$explicit_root" = true ]; then
    cargo install --path "$source_directory" --locked "$@"
else
    cargo install --path "$source_directory" --locked --root "$installation_root" "$@"
fi

"$installation_root/bin/m2-ls" --download-catalog
