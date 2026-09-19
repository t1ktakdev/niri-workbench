#!/bin/sh
set -eu

prefix=${PREFIX:-"$HOME/.local"}
bindir=${BINDIR:-"$prefix/bin"}
target="$bindir/niri-workbench"

if [ -e "$target" ]; then
    rm -- "$target"
    echo "removed $target"
else
    echo "$target is not installed"
fi

# User recipes are deliberately left untouched.
