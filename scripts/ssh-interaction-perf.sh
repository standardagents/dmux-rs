#!/bin/bash
# Exercise keyboard, hover, and trackpad latency through the caller's real SSH
# terminal. The fixture is isolated from the user's tmux servers and settings.
set -u

cd "$(dirname "$0")/.." || exit 1

BIN=${1:-$PWD/target/release/dmux-rs}
if [ ! -x "$BIN" ]; then
  echo "ssh-interaction: binary is missing or not executable: $BIN" >&2
  echo "ssh-interaction: run cargo build --release --bin dmux-rs first" >&2
  exit 1
fi
if [ ! -t 0 ] || [ ! -t 1 ]; then
  echo "ssh-interaction: an interactive terminal is required" >&2
  exit 1
fi

read -r ROWS COLS < <(stty size)
export TERM=xterm-256color
RUN_ROOT=$(mktemp -d /tmp/dmux-ssh-interaction.XXXXXX)
SOCKET=dmux-ssh-interaction-$$
SESSION=dmux-ssh-interaction-$$
PROJECT=$RUN_ROOT/project
mkdir -p "$PROJECT" "$RUN_ROOT/home"
git -C "$PROJECT" init -q -b main

cleanup() {
  tmux -L "$SOCKET" kill-server 2>/dev/null || true
  if [[ "$RUN_ROOT" == /tmp/dmux-ssh-interaction.* ]]; then
    rm -rf "$RUN_ROOT"
  fi
}
trap cleanup EXIT HUP INT TERM

start_shell() {
  local pane=$1
  local command="exec bash --noprofile --norc -c 'n=1; while [ \"\$n\" -le 600 ]; do printf \"pane-$pane history %04d\\n\" \"\$n\"; n=\$((n + 1)); done; exec bash --noprofile --norc -i'"
  if [ "$pane" -eq 1 ]; then
    tmux -L "$SOCKET" -f /dev/null new-session -d -s "$SESSION" \
      -x "$COLS" -y "$ROWS" -c "$PROJECT" -n "input-$pane" "$command"
  else
    tmux -L "$SOCKET" new-window -d -t "$SESSION" -c "$PROJECT" \
      -n "input-$pane" "$command"
  fi
}

for pane in 1 2 3 4; do
  start_shell "$pane"
done

echo "ssh-interaction: ${COLS}x${ROWS}, 4 interactive panes, binary $BIN" >&2
echo "ssh-interaction: type in a pane, hover across sidebar rows, and scroll with the trackpad" >&2
echo "ssh-interaction: HUD latency begins when input bytes reach dmux on this host" >&2

HOME=$RUN_ROOT/home \
DMUX_NO_UPDATE=1 \
DMUX_NO_REPORT=1 \
DMUX_VERIFY=0 \
DMUX_TRACKING_SECS=86400 \
"$BIN" --project "$PROJECT" --session "$SESSION" --socket "$SOCKET" --hud \
  --log-file "$RUN_ROOT/dmux.log"
