#!/bin/sh
set -eu

prefix=${PREFIX:-"$HOME/.local"}
bindir=${BINDIR:-"$prefix/bin"}
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

if [ -x "$script_dir/niri-workbench" ]; then
    source_bin="$script_dir/niri-workbench"
elif [ -x "$script_dir/../target/release/niri-workbench" ]; then
    source_bin="$script_dir/../target/release/niri-workbench"
else
    echo "niri-workbench binary not found." >&2
    echo "Build with 'cargo build --release' or run this script from a release archive." >&2
    exit 1
fi

mkdir -p "$bindir"
install -m 0755 "$source_bin" "$bindir/niri-workbench"

echo "installed $bindir/niri-workbench"
case ":$PATH:" in
    *":$bindir:"*) ;;
    *) echo "note: $bindir is not currently in PATH" ;;
esac
