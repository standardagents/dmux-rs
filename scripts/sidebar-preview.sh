#!/bin/sh
# Render deterministic sidebar states without connecting to a tmux server.
set -eu

DMUX_PREVIEW_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
DMUX_PREVIEW_FILE=$(mktemp "${TMPDIR:-/tmp}/dmux-sidebar-preview.XXXXXX")
trap 'rm -f "$DMUX_PREVIEW_FILE"' EXIT HUP INT TERM

case "${1:-}" in
  "") ;;
  --plain) export NO_COLOR=1 ;;
  *)
    echo "usage: scripts/sidebar-preview.sh [--plain]" >&2
    exit 2
    ;;
esac

cd "$DMUX_PREVIEW_ROOT"
DMUX_SIDEBAR_PREVIEW_OUT="$DMUX_PREVIEW_FILE" \
  cargo test --quiet -p dmux --bin dmux-rs \
    render::sidebar_preview::write_sidebar_preview_artifact -- \
    --ignored --exact >/dev/null
cat "$DMUX_PREVIEW_FILE"
