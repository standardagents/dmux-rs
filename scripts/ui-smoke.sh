#!/bin/bash
# Pane-interaction smoke harness (#66): drives a real dmux-rs through a
# hermetic driver/target tmux pair and asserts application-level behavior —
# menu activation, launch actions, working-directory selection, and overlay
# color treatment — with ANSI-aware capture assertions.
#
# Usage: scripts/ui-smoke.sh          (from a clean checkout; ~30s)
#
# Isolation: per-run sockets, throwaway HOME/projects, `-f /dev/null` driver;
# the developer's tmux servers, config, shell files, and env are untouched.
# Failures keep diagnostics under /tmp/ui-smoke-fail-<pid>/; success cleans up.
set -u
cd "$(dirname "$0")/.." || exit 1

cargo build --quiet --bin dmux-rs --bin griddump || { echo "ui-smoke: build FAILED"; exit 1; }
BIN=$PWD/target/debug/dmux-rs
GRID=$PWD/target/debug/griddump

DRV=uis-drv-$$; TGT=uis-tgt-$$
DW=160; DH=45
FAILS=0
DIAG=/tmp/ui-smoke-fail-$$

cleanup() { tmux -L "$DRV" kill-server 2>/dev/null; tmux -L "$TGT" kill-server 2>/dev/null; }
cleanup; trap cleanup EXIT

WORK=$(mktemp -d /tmp/ui-smoke.XXXX)
mkdir -p "$WORK/home" "$WORK/other-project"
cd "$WORK" && git init -q -b main && git commit -q --allow-empty -m init
(cd "$WORK/other-project" && git init -q -b main && git commit -q --allow-empty -m init)

# Editor probe: records its working directory, then holds like a real editor.
cat > "$WORK/editor-probe.sh" <<'EOF'
#!/bin/bash
pwd > "${UI_SMOKE_RESULT:?}"
exec sleep 600
EOF
chmod +x "$WORK/editor-probe.sh"

# Fixture: one pane owned by a project that differs from the session root.
mkdir -p "$WORK/.dmux"
cat > "$WORK/.dmux/dmux.config.json" <<EOF
{"projectName":"ui-smoke","projectRoot":"$WORK","panes":[
 {"id":"1","slug":"terminal-1","prompt":"","paneId":"%9","type":"shell","shellCwd":"$WORK"},
 {"id":"2","slug":"other-term","prompt":"","paneId":"%10","type":"shell",
  "shellCwd":"$WORK/other-project","projectRoot":"$WORK/other-project"}
]}
EOF

# ---- helpers ---------------------------------------------------------------
drv_keys() { tmux -L "$DRV" send-keys -t drv "$@"; }
drv_hex()  { tmux -L "$DRV" send-keys -t drv -H $(printf '%s' "$1" | xxd -p -c1 | tr '\n' ' '); }
leader()   { drv_hex "$(printf '\002')"; sleep 0.3; drv_keys "$1"; sleep 1; }
sgr()      { drv_hex "$(printf '\033[<%s;%s;%s%s' "$1" "$2" "$3" "$4")"; }
left_click()  { sgr 0 "$1" "$2" M; sgr 0 "$1" "$2" m; sleep 0.8; }
right_click() { sgr 2 "$1" "$2" M; sgr 2 "$1" "$2" m; sleep 0.8; }
cap()      { tmux -L "$DRV" capture-pane -p -t drv; }
cap_ansi() { tmux -L "$DRV" capture-pane -p -e -N -t drv; }

fail() { # $1 = label
  echo "FAIL $1"
  mkdir -p "$DIAG"
  cap > "$DIAG/$1-plain.txt"
  cap_ansi > "$DIAG/$1-ansi.txt"
  FAILS=$((FAILS+1))
}

wait_for() { # $1 = regex, $2 = label, [$3 = seconds]
  local t=${3:-8}
  for _ in $(seq 1 $((t*2))); do
    cap | /usr/bin/grep -q "$1" && return 0
    sleep 0.5
  done
  fail "$2"
  return 1
}

wait_gone() { # $1 = regex, $2 = label, [$3 = seconds]
  local t=${3:-8}
  for _ in $(seq 1 $((t*2))); do
    cap | /usr/bin/grep -q "$1" || return 0
    sleep 0.5
  done
  fail "$2"
  return 1
}

# ---- boot ------------------------------------------------------------------
tmux -L "$DRV" -f /dev/null new-session -d -s drv -x $DW -y $DH "exec bash"
drv_keys "cd $WORK && EDITOR=$WORK/editor-probe.sh UI_SMOKE_RESULT=$WORK/editor-cwd.txt \
env -u TMUX HOME=$WORK/home $BIN --socket $TGT" Enter
sleep 4
# Accept the session-recovery dialog (fixture panes are recreated from it).
drv_keys Enter; sleep 4
wait_for "other-term" boot || { echo "ui-smoke: $FAILS FAILURES"; exit 1; }

# ---- case 1: Open in editor uses the pane-owned directory ------------------
# Focus the other-project pane via its sidebar row (click-to-focus), then
# open its context menu and activate "Open in editor" by first letter.
# Sidebar rows live in the first 40 columns; pane title bars repeat the
# name further right and must not match.
sidebar_row() { cap | /usr/bin/awk -v pat="$1" 'index(substr($0, 1, 40), pat) { print NR; exit }'; }
ROW=$(sidebar_row "other-term")
left_click 5 "$ROW"
right_click 5 "$ROW"
wait_for "Open in editor" menu-open 4 && {
  drv_keys o; sleep 3
  if [ -f "$WORK/editor-cwd.txt" ] && [ "$(cat "$WORK/editor-cwd.txt")" = "$WORK/other-project" ]; then
    echo "PASS editor-cwd (pane-owned directory)"
  else
    echo "  expected $WORK/other-project, got: $(cat "$WORK/editor-cwd.txt" 2>/dev/null || echo '<missing>')"
    fail editor-cwd
  fi
}

# ---- case 2: context menu dims the scene but not its source ----------------
# Focus terminal-1, paint a colored marker in it, right-click it, and assert
# the marker keeps its color while another region carries the scrim ramp.
ROW1=$(sidebar_row "terminal-1")
left_click 5 "$ROW1"; sleep 1
P1=$(tmux -L "$TGT" list-panes -a -F '#{pane_id} #{window_name}' | /usr/bin/grep -v -e keepalive -e edit | head -1 | cut -d' ' -f1)
tmux -L "$TGT" send-keys -t "$P1" "clear; printf '\\033[38;5;196mMARKER\\033[0m\\n'" Enter; sleep 1.5
MX=$(cap | /usr/bin/grep -n "MARKER" | head -1 | cut -d: -f1)
if [ -n "${MX:-}" ]; then
  right_click 60 "$MX"; sleep 1
  ANSI=$(cap_ansi)
  # SGR runs may separate the color set from the text; assert both on the
  # marker's line rather than adjacent.
  echo "$ANSI" | /usr/bin/grep "MARKER" | /usr/bin/grep -q "38;5;196" || fail menu-source-colors
  echo "$ANSI" | /usr/bin/grep -q "38;5;238" || fail menu-scrim-present
  [ "$FAILS" = 0 ] && echo "PASS context-menu colors (source intact, scene dimmed)"
  # The menu starts on its first item. Up wraps to the final Close pane item.
  drv_keys Up Enter; sleep 0.5
  if wait_gone "terminal-1" context-close 8; then
    echo "PASS context-close (pane closed from right-click menu)"
  fi
else
  fail marker-paint
fi

# ---- case 3: keyboard close retains confirmation --------------------------
ROW2=$(sidebar_row "other-term")
left_click 5 "$ROW2"
leader x
if wait_for "process will be killed" keyboard-close-confirm 4; then
  echo "PASS keyboard-close (confirmation retained)"
  drv_keys Escape; sleep 0.5
fi

# ---- result ----------------------------------------------------------------
if [ "$FAILS" -eq 0 ]; then
  echo "ui-smoke: ALL PASS"
  rm -rf "$WORK"
else
  echo "ui-smoke: $FAILS FAILURES (diagnostics in $DIAG, fixture in $WORK)"
fi
exit "$FAILS"
