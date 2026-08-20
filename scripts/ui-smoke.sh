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
WORK=$(cd "$WORK" && pwd -P)
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
pane_snapshot() {
  tmux -L "$TGT" list-panes -a \
    -F '#{pane_id} path=#{pane_current_path} command=#{pane_current_command} window=#{window_name}'
}

fail() { # $1 = label
  echo "FAIL $1"
  mkdir -p "$DIAG"
  cap > "$DIAG/$1-plain.txt"
  cap_ansi > "$DIAG/$1-ansi.txt"
  pane_snapshot > "$DIAG/$1-panes.txt" 2>&1
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

sidebar_rows() { # $1 = pane title; prints every matching 1-based row
  cap | python3 -c '
import sys
pane = sys.argv[1]
for row, line in enumerate(sys.stdin, 1):
    label = line[:40][4:].strip()
    for suffix in (" (hidden)", " (closing…)"):
        if label.endswith(suffix):
            label = label[:-len(suffix)]
    if label == pane:
        print(row)
' "$1"
}

sidebar_snapshot() {
  cap | python3 -c '
import sys
for row, line in enumerate(sys.stdin, 1):
    print(f"{row:3d} | {line[:40].rstrip()}")
'
}

stable_sidebar_row() { # $1 = pane title, $2 = failure label
  local pane=$1 label=$2 rows count previous=""
  SIDEBAR_ROW=""
  for _ in $(seq 1 16); do
    rows=$(sidebar_rows "$pane")
    count=$(printf '%s\n' "$rows" | /usr/bin/sed '/^$/d' | wc -l | tr -d ' ')
    if [ "$count" = 1 ] && [ "$rows" = "$previous" ]; then
      SIDEBAR_ROW=$rows
      return 0
    fi
    if [ "$count" = 1 ]; then previous=$rows; else previous=""; fi
    sleep 0.5
  done
  echo "  expected one stable sidebar row for pane '$pane'; observed rows: ${rows:-<none>}"
  sidebar_snapshot
  fail "$label"
  return 1
}

wait_sidebar_gone() { # $1 = pane title, $2 = failure label
  local pane=$1 label=$2 rows previous_empty=0
  for _ in $(seq 1 16); do
    rows=$(sidebar_rows "$pane")
    if [ -z "$rows" ]; then
      if [ "$previous_empty" = 1 ]; then return 0; fi
      previous_empty=1
    else
      previous_empty=0
    fi
    sleep 0.5
  done
  echo "  expected sidebar pane '$pane' to be gone; observed rows: ${rows:-<none>}"
  sidebar_snapshot
  fail "$label"
  return 1
}

menu_title_at() { # $1 = pane title, $2 = column, $3 = row
  cap | python3 -c '
import sys
pane, col, wanted_row = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
found = False
for row, line in enumerate(sys.stdin, 1):
    if row == wanted_row:
        menu = line[col - 1:]
        found = menu.startswith("╭") and f" {pane} " in menu
        break
raise SystemExit(0 if found else 1)
' "$1" "$2" "$3"
}

open_sidebar_menu() { # $1 = pane title, $2 = pointer column, $3 = failure label
  local pane=$1 col=$2 label=$3
  stable_sidebar_row "$pane" "$label-target" || return 1
  right_click "$col" "$SIDEBAR_ROW"
  for _ in $(seq 1 8); do
    if menu_title_at "$pane" "$col" "$SIDEBAR_ROW" && cap | /usr/bin/grep -q "Open in editor"; then
      return 0
    fi
    sleep 0.5
  done
  echo "  expected context menu for pane '$pane' at column $col, row $SIDEBAR_ROW"
  echo "  observed row: $(cap | /usr/bin/sed -n "${SIDEBAR_ROW}p")"
  fail "$label"
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
if stable_sidebar_row "other-term" editor-sidebar-target; then
  ROW=$SIDEBAR_ROW
  left_click 5 "$ROW"
  open_sidebar_menu "other-term" 5 menu-open && {
    drv_keys o; sleep 3
    if [ -f "$WORK/editor-cwd.txt" ] && [ "$(cat "$WORK/editor-cwd.txt")" = "$WORK/other-project" ]; then
      echo "PASS editor-cwd (pane-owned directory)"
    else
      echo "  expected $WORK/other-project, got: $(cat "$WORK/editor-cwd.txt" 2>/dev/null || echo '<missing>')"
      fail editor-cwd
    fi
  }
fi

# ---- case 2: context menu dims the scene but not its source ----------------
# Focus terminal-1, paint a colored marker in it, right-click it, and assert
# the marker keeps its color while another region carries the scrim ramp.
if stable_sidebar_row "terminal-1" marker-sidebar-target; then
  ROW1=$SIDEBAR_ROW
  left_click 5 "$ROW1"; sleep 1
  P1=$(tmux -L "$TGT" list-panes -a -F '#{pane_id} #{pane_current_path} #{window_name}' | \
    /usr/bin/awk -v root="$WORK" '$2 == root && $3 !~ /keepalive|edit/ { print $1; exit }')
  if [ -z "$P1" ]; then
    echo "  expected a tmux pane owned by terminal-1"
    pane_snapshot
    fail marker-pane-identity
  else
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
      if wait_sidebar_gone "terminal-1" context-close; then
        echo "PASS context-close (pane closed from right-click menu)"
      fi
    else
      fail marker-paint
    fi
  fi
fi

# ---- case 3: keyboard close retains confirmation --------------------------
if stable_sidebar_row "other-term" keyboard-sidebar-target; then
  ROW2=$SIDEBAR_ROW
  left_click 5 "$ROW2"
  leader x
  if wait_for "process will be killed" keyboard-close-confirm 4; then
    echo "PASS keyboard-close (confirmation retained)"
    drv_keys Escape; sleep 0.5
  fi
fi

# ---- case 4: overlay placement contract (#91) -------------------------------
# A sidebar right-click menu opens AT the pointer; a global overlay
# (Settings) opens immediately right of the sidebar at the top of screen.
char_col() { # $1 = 1-based row, $2 = glyph → 1-based char column (0 = absent)
  cap | /usr/bin/sed -n "$1p" | python3 -c "import sys; print(sys.stdin.read().find('$2') + 1)"
}
open_sidebar_menu "other-term" 25 menu-pointer-open && {
  ROWP=$SIDEBAR_ROW
  COL=$(char_col "$ROWP" "╭")
  if [ "$COL" = 25 ]; then
    echo "PASS pointer-anchor (sidebar right-click opens at pointer)"
  else
    echo "  expected menu ╭ at col 25 row $ROWP, got col $COL"
    fail pointer-anchor
  fi
  drv_keys Escape; sleep 0.8
}
leader s
wait_for "Settings" settings-open 4 && {
  COL=$(char_col 1 "╭")
  if [ "$COL" -gt 40 ]; then
    echo "PASS global-anchor (Settings right of sidebar, top row)"
  else
    echo "  expected Settings ╭ right of sidebar on row 1, got col $COL"
    fail global-anchor
  fi
  drv_keys Escape; sleep 0.8
}

# ---- case 5: sidebar context close keeps pane identity ---------------------
# Earlier cases created an editor pane and closed terminal-1. Resolve the
# moved other-term row again, verify the menu title, then close that pane.
EDITOR_PANE=$(tmux -L "$TGT" list-panes -a -F '#{pane_id} #{pane_current_path} #{pane_current_command}' | \
  /usr/bin/awk -v root="$WORK/other-project" '$2 == root && $3 == "sleep" { print $1; exit }')
if [ -z "$EDITOR_PANE" ]; then
  echo "  expected the earlier editor probe pane to remain present"
  pane_snapshot
  fail sidebar-context-fixture
elif open_sidebar_menu "other-term" 25 sidebar-context-menu; then
  drv_keys Up Enter; sleep 0.5
  if wait_sidebar_gone "other-term" sidebar-context-close; then
    if tmux -L "$TGT" display-message -p -t "$EDITOR_PANE" '#{pane_id}' >/dev/null 2>&1; then
      echo "PASS sidebar-context-close (target closed, editor pane retained)"
    else
      echo "  expected editor pane $EDITOR_PANE to survive closing other-term"
      fail sidebar-context-identity
    fi
  fi
fi

# ---- result ----------------------------------------------------------------
if [ "$FAILS" -eq 0 ]; then
  echo "ui-smoke: ALL PASS"
  rm -rf "$WORK"
else
  echo "ui-smoke: $FAILS FAILURES (diagnostics in $DIAG, fixture in $WORK)"
fi
exit "$FAILS"
