#!/bin/sh
set -eu

prefix=${PREFIX:-"$HOME/.local"}
bindir=${BINDIR:-"$prefix/bin"}
appdir=${APPDIR:-"$prefix/share/applications"}
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

find_binary() {
    name=$1
    if [ -x "$script_dir/$name" ]; then
        printf '%s\n' "$script_dir/$name"
    elif [ -x "$script_dir/../target/release/$name" ]; then
        printf '%s\n' "$script_dir/../target/release/$name"
    else
        return 1
    fi
}

cli_bin=$(find_binary niri-workbench) || {
    echo "niri-workbench binary not found." >&2
    echo "Build with 'cargo build --release' or run this script from a release archive." >&2
    exit 1
}
ui_bin=$(find_binary niri-workbench-ui) || {
    echo "niri-workbench-ui binary not found." >&2
    echo "Build the full workspace with 'cargo build --release'." >&2
    exit 1
}

mkdir -p "$bindir"
install -m 0755 "$cli_bin" "$bindir/niri-workbench"
install -m 0755 "$ui_bin" "$bindir/niri-workbench-ui"

desktop_source=""
for candidate in     "$script_dir/niri-workbench.desktop"     "$script_dir/../packaging/niri-workbench.desktop"
do
    if [ -f "$candidate" ]; then
        desktop_source=$candidate
        break
    fi
done

if [ -n "$desktop_source" ]; then
    mkdir -p "$appdir"
    install -m 0644 "$desktop_source" "$appdir/niri-workbench.desktop"
fi

echo "installed $bindir/niri-workbench"
echo "installed $bindir/niri-workbench-ui"
[ -n "$desktop_source" ] && echo "installed $appdir/niri-workbench.desktop"

case ":$PATH:" in
    *":$bindir:"*) ;;
    *) echo "note: $bindir is not currently in PATH" ;;
esac

echo "run 'niri-workbench ui' for the full manager or 'niri-workbench' for the quick launcher"
