#!/bin/sh
# Render deterministic Issues pane states without tmux or GitHub (#82).
# Prints ANSI-colored previews at wide and narrow widths covering
# assignment groups, repository headings, selection states, long titles,
# labels, and update dates. Companion to scripts/sidebar-preview.sh.
set -eu

DMUX_PREVIEW_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
DMUX_PREVIEW_FILE=$(mktemp "${TMPDIR:-/tmp}/dmux-issues-preview.XXXXXX")
trap 'rm -f "$DMUX_PREVIEW_FILE"' EXIT HUP INT TERM

case "${1:-}" in
  "") ;;
  --plain) export NO_COLOR=1 ;;
  *)
    echo "usage: scripts/issues-preview.sh [--plain]" >&2
    exit 2
    ;;
esac

cd "$DMUX_PREVIEW_ROOT"
DMUX_ISSUES_PREVIEW_OUT="$DMUX_PREVIEW_FILE" \
  cargo test --quiet -p dmux --bin dmux-rs \
    views::issues_preview::write_issues_preview_artifact -- \
    --ignored --exact >/dev/null
cat "$DMUX_PREVIEW_FILE"
