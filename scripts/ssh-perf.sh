#!/bin/bash
# Run an unreleased dmux binary against synthetic streaming panes. The caller's
# terminal supplies the tested geometry, so invoke this through the same SSH
# path used for normal dmux sessions.
set -u

cd "$(dirname "$0")/.." || exit 1

BIN=${1:-$PWD/target/release/dmux-rs}
if [ ! -x "$BIN" ]; then
  echo "ssh-perf: binary is missing or not executable: $BIN" >&2
  echo "ssh-perf: run cargo build --release --bin dmux-rs first" >&2
  exit 1
fi
if [ ! -t 0 ] || [ ! -t 1 ]; then
  echo "ssh-perf: an interactive terminal is required" >&2
  exit 1
fi

read -r ROWS COLS < <(stty size)
# Ghostty exports xterm-ghostty, which may be absent from the remote host's
# terminfo database. The fixture uses only portable terminal capabilities.
export TERM=xterm-256color
RUN_ROOT=$(mktemp -d /tmp/dmux-ssh-perf.XXXXXX)
SOCKET=dmux-ssh-perf-$$
SESSION=dmux-ssh-perf-$$
PROJECT=$RUN_ROOT/project
mkdir -p "$PROJECT" "$RUN_ROOT/home"
git -C "$PROJECT" init -q -b main

cleanup() {
  tmux -L "$SOCKET" kill-server 2>/dev/null || true
  if [[ "$RUN_ROOT" == /tmp/dmux-ssh-perf.* ]]; then
    rm -rf "$RUN_ROOT"
  fi
}
trap cleanup EXIT HUP INT TERM

start_stream() {
  local pane=$1
  local command="exec bash --noprofile --norc -c 'n=0; while :; do printf \"pane-$pane frame %06d\\n\" \"\$n\"; n=\$((n + 1)); sleep 0.005; done'"
  if [ "$pane" -eq 1 ]; then
    tmux -L "$SOCKET" -f /dev/null new-session -d -s "$SESSION" \
      -x "$COLS" -y "$ROWS" -c "$PROJECT" -n "stream-$pane" "$command"
  else
    tmux -L "$SOCKET" new-window -d -t "$SESSION" -c "$PROJECT" \
      -n "stream-$pane" "$command"
  fi
}

for pane in 1 2 3 4 5 6 7 8; do
  start_stream "$pane"
done

echo "ssh-perf: ${COLS}x${ROWS}, 8 streaming panes, binary $BIN" >&2
echo "ssh-perf: keep the HUD visible for at least 30 seconds" >&2

HOME=$RUN_ROOT/home \
DMUX_NO_UPDATE=1 \
DMUX_NO_REPORT=1 \
DMUX_VERIFY=0 \
DMUX_TRACKING_SECS=86400 \
"$BIN" --project "$PROJECT" --session "$SESSION" --socket "$SOCKET" --hud \
  --log-file "$RUN_ROOT/dmux.log"
