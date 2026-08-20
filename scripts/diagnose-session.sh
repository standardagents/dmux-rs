#!/bin/bash
# Read-only live-session diagnostic (#78): builds the current checkout and
# prints the installed-build/pane/record picture for the project in $PWD
# (or the directory passed as $1). Safe to run against a live session —
# it performs no tmux, config, or filesystem mutations.
#
# Usage: scripts/diagnose-session.sh [project-dir] [extra dmux-rs flags…]
set -eu
cd "$(dirname "$0")/.."
cargo build --quiet --bin dmux-rs
DIR=${1:-$PWD}
[ $# -gt 0 ] && shift
exec target/debug/dmux-rs --project "$DIR" --diagnose-session "$@"
