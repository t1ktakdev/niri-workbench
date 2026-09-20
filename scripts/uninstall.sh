#!/bin/sh
set -eu

prefix=${PREFIX:-"$HOME/.local"}
bindir=${BINDIR:-"$prefix/bin"}
appdir=${APPDIR:-"$prefix/share/applications"}

removed=0
for target in     "$bindir/niri-workbench"     "$bindir/niri-workbench-ui"     "$appdir/dev.t1ktak.NiriWorkbench.desktop"     "$appdir/niri-workbench.desktop"
do
    if [ -e "$target" ]; then
        rm -- "$target"
        echo "removed $target"
        removed=1
    fi
done

[ "$removed" -eq 1 ] || echo "niri-workbench is not installed in $prefix"

# Recipes and UI preferences are intentionally left untouched.
