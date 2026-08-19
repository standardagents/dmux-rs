#!/bin/bash
# Rendering-fidelity harness: proves dmux-rs paints panes cell-for-cell
# identical (chars + fg + bg) to what a real tmux client renders for the
# same pane — through the FULL pipeline (control mode, VT, compositor,
# host emission), on both the live-output path and the seed path (restart).
#
# Ground truth: a real tmux client attached to the same window via a grouped
# session, its screen captured with -e and parsed through the same VT stack
# (griddump). Ours: the driver terminal hosting dmux-rs, captured the same
# way, aligned by a marker cell. Any differing cell is a fidelity bug.
#
# Usage: scripts/fidelity.sh   (from rust/; needs tmux >= 3.3, ~90s)
set -u
cd "$(dirname "$0")/.." || exit 1
BIN=$PWD/target/debug/dmux-rs
GRID=$PWD/target/debug/griddump
[ -x "$BIN" ] && [ -x "$GRID" ] || { echo "build first: cargo build --bin dmux-rs --bin griddump"; exit 1; }

DRV=fdl-drv; TGT=fdl-tgt; OUT=fdl-out
DW=140; DH=40
FAILS=0

cleanup() { tmux -L $DRV kill-server 2>/dev/null; tmux -L $TGT kill-server 2>/dev/null; tmux -L $OUT kill-server 2>/dev/null; }
cleanup; trap cleanup EXIT

WORKDIR=$(mktemp -d /tmp/dmux-fidelity.XXXX)
cd "$WORKDIR" && git init -q -b main && git commit -q --allow-empty -m init
# Full isolation: dmux-rs must not read the developer's real global settings
# (their inference provider would activate LLM terminal naming mid-case).
mkdir -p "$WORKDIR/home"

# ---- corpus ---------------------------------------------------------------
# Every case paints "MK" at its first cell (marker for alignment) and ends
# parked on `sleep`. Bands use EL/2J with bg — the BCE cases that history
# showed are easy to lose.
mkdir -p cases
cat > cases/rich-static.sh <<'EOF'
#!/bin/bash
printf '\033]110\007\033]111\007\033]104\007'
printf '\033[48;5;235m\033[2J\033[H'
printf '\033[1HMRKR page-bg then bands'
printf '\033[3H\033[48;5;236m\033[K'
printf '\033[4H\033[K> say hello to me'
printf '\033[5H\033[K\033[48;5;235m'
printf '\033[7H\033[K* Hello! \xf0\x9f\x91\x8b on page'
printf '\033[9H\033[48;5;236m\033[K\033[38;5;114m  gpt-5.6-sol high \xc2\xb7 ~/Projects\033[39m'
printf '\033[11H\033[0m\033[Kdefault-bg row after reset'
exec sleep 600
EOF
cat > cases/scrolled-bands.sh <<'EOF'
#!/bin/bash
printf '\033]110\007\033]111\007\033]104\007'
printf '\033[2J\033[H'
printf '\033[48;5;235m'
for i in $(seq 1 60); do printf 'scrolled line %02d\r\n' "$i"; done
printf '\033[48;5;236mband after scroll\033[K\r\n\033[0mtail'
printf '\033[1;1H\033[0mMRKR'
exec sleep 600
EOF
cat > cases/wide-and-styles.sh <<'EOF'
#!/bin/bash
printf '\033]110\007\033]111\007\033]104\007'
printf '\033[2J\033[HMRKR wide chars and styles'
printf '\033[3H\xe6\xbc\xa2\xe5\xad\x97 mixed \xe3\x83\x86\xe3\x82\xb9\xe3\x83\x88 ascii'
printf '\033[5H\033[1mbold\033[0m \033[4munder\033[0m \033[7minv\033[0m \033[38;5;196mred\033[0m'
printf '\033[7H\033[48;5;236m\xe2\x8f\xba ambiguous \xe2\x97\x86 glyphs\033[K'
printf '\033[9Htail-row'
exec sleep 600
EOF
cat > cases/osc-colors.sh <<'EOF'
#!/bin/bash
# Pane-local dynamic palette: default fg/bg via OSC 10/11, index 4 via OSC 4.
printf '\033]10;#e0d7cc\007\033]11;#120f1a\007'
printf '\033]4;4;#ff00aa\007'
printf '\033[2J\033[HMRKR themed defaults'
printf '\033[3Hdefault-colored text row'
printf '\033[5H\033[34mindexed-4 remapped\033[39m'
printf '\033[7H\033[48;5;236m\033[Kband over theme\033[49m'
exec sleep 600
EOF
chmod +x cases/*.sh

# ---- helpers --------------------------------------------------------------
compare() { # $1=truth.grid $2=ours.grid $3=label [$4=tolerate-trailing-bg]
  python3 - "$1" "$2" "$3" "${4:-strict}" <<'PY'
import sys
truth = [l.rstrip('\n\t').split('\t') for l in open(sys.argv[1])]
ours  = [l.rstrip('\n\t').split('\t') for l in open(sys.argv[2])]
label = sys.argv[3]
tolerate = sys.argv[4] == 'tolerate-trailing-bg'
def find_marker(g):
    want = ['M·', 'R·', 'K·', 'R·']
    for y, row in enumerate(g):
        for x in range(len(row) - 3):
            if all(row[x+i].startswith(w) for i, w in enumerate(want)):
                return x, y
    return None
tm, om = find_marker(truth), find_marker(ours)
if not tm or not om:
    print(f"FAIL {label}: marker not found (truth={tm} ours={om})")
    for name, g in (("truth", truth), ("ours", ours)):
        for y in range(min(3, len(g))):
            row = g[y]
            lo = 40 if name == "ours" else 0
            cells = ''.join(t.split('\u00b7')[0] if '\u00b7' in t else t[:1] for t in row[lo:lo+40])
            print(f"  {name} r{y}: {cells!r}")
    sys.exit(1)
dx, dy = om[0]-tm[0], om[1]-tm[1]
diffs = []
for y, row in enumerate(truth):
    for x, cell in enumerate(row):
        oy, ox = y+dy, x+dx
        got = ours[oy][ox] if 0 <= oy < len(ours) and 0 <= ox < len(ours[oy]) else '<out>'
        if got != cell:
            # tmux compacts trailing BCE backgrounds on scrolled rows (its own
            # clients render them default); we keep them like real terminals.
            if tolerate and cell == ' ·d·d' and got.startswith(' ·'):
                continue
            diffs.append((x, y, cell, got))
if diffs:
    print(f"FAIL {label}: {len(diffs)} differing cells; first 8:")
    for x, y, want, got in diffs[:8]:
        print(f"  ({x},{y}) truth={want} ours={got}")
    sys.exit(1)
print(f"PASS {label} ({sum(len(r) for r in truth)} cells)")
PY
}

truth_capture() { # tmux's authoritative grid for the pane
  # Direct -epqN capture: byte-faithful including BCE trailing cells (see
  # seed_command). A nested live-client chain was tried and abandoned — tmux
  # clients add their own view state (pan offsets, centering for larger
  # clients) that says nothing about pane-content fidelity.
  tmux -L $TGT capture-pane -epqN -t "$PANE"
}

ours_capture() { tmux -L $DRV capture-pane -p -e -N -t drv; }

run_case() { # $1 = case script, $2 = path label (live|seed), [$3 tolerance]
  local case_sh=$1 path=$2 mode=${3:-strict} name
  name=$(basename "$case_sh" .sh)
  # window geometry of the dmux pane
  local geom w h
  geom=$(tmux -L $TGT display-message -p -t "$PANE" '#{pane_width} #{pane_height}')
  w=${geom% *}; h=${geom#* }
  truth_capture | "$GRID" "$w" "$h" > /tmp/fdl-truth.grid
  ours_capture | "$GRID" "$DW" "$DH" > /tmp/fdl-ours.grid
  if ! compare /tmp/fdl-truth.grid /tmp/fdl-ours.grid "$name/$path" "$mode"; then
    FAILS=$((FAILS+1))
    cp /tmp/fdl-truth.grid "/tmp/fdl-$name-$path-truth.grid"
    cp /tmp/fdl-ours.grid "/tmp/fdl-$name-$path-ours.grid"
  fi
}

# ---- driver ---------------------------------------------------------------
tmux -L $DRV -f /dev/null new-session -d -s drv -x $DW -y $DH "exec bash"
tmux -L $DRV send-keys -t drv "cd $WORKDIR && env -u TMUX HOME=$WORKDIR/home $BIN --socket $TGT" Enter
sleep 3
# Hermetic pane shell: the user's own shell theme would inject prompt/palette
# noise into every case. (Pane-level OSC palettes are covered explicitly by
# the osc-colors case.)
tmux -L $TGT set -g default-command "/bin/bash --norc --noprofile"
tmux -L $DRV send-keys -t drv -H 02; sleep 0.2; tmux -L $DRV send-keys -t drv 't'; sleep 1.5
SESS=$(tmux -L $TGT list-sessions -F '#{session_name}' 2>/dev/null | /usr/bin/grep '^dmux-' | head -1)
PANE=$(tmux -L $TGT list-panes -s -F '#{pane_id} #{pane_current_command}' | /usr/bin/grep ' bash$' | head -1 | cut -d' ' -f1)
# The user's tmux.conf may tint default cells via window-style; dmux-rs
# intentionally does not reproduce tmux-level theming, so neutralize it for
# the truth client — the harness measures pane CONTENT fidelity.
tmux -L $TGT set -g window-style default 2>/dev/null
tmux -L $TGT set -g window-active-style default 2>/dev/null

for case_sh in cases/rich-static.sh cases/scrolled-bands.sh cases/wide-and-styles.sh; do
  tmux -L $TGT send-keys -t "$PANE" C-c 2>/dev/null; sleep 0.3
  tmux -L $TGT send-keys -t "$PANE" "$WORKDIR/$case_sh" Enter; sleep 1.5
  mode=strict
  [ "$(basename "$case_sh")" = "scrolled-bands.sh" ] && mode=tolerate-trailing-bg
  run_case "$case_sh" live "$mode"
  # seed path: restart dmux-rs so the pane content must round-trip capture-pane
  tmux -L $DRV send-keys -t drv -H 02; sleep 0.2; tmux -L $DRV send-keys -t drv 'd'; sleep 1.5
  tmux -L $DRV send-keys -t drv "env -u TMUX HOME=$WORKDIR/home $BIN --socket $TGT" Enter; sleep 4
  run_case "$case_sh" seed "$mode"
done

# osc-colors: pane-local dynamic palettes (OSC 10/11/4). Contract: written
# cells render with the theme, live. (Full-grid compare vs tmux is not
# meaningful — tmux themes only written cells while real terminals theme
# erased cells too; we follow the terminals. Across a restart the palette
# is unrecoverable from tmux and returns with the app's next repaint.)
tmux -L $TGT send-keys -t "$PANE" C-c 2>/dev/null; sleep 0.3
tmux -L $TGT send-keys -t "$PANE" "$WORKDIR/cases/osc-colors.sh" Enter; sleep 1.5
if ours_capture | /usr/bin/grep -q "38;2;224;215;204"; then
  echo "PASS osc-colors/live (themed default fg reaches the host)"
else
  echo "FAIL osc-colors/live: themed default fg missing from host output"
  FAILS=$((FAILS+1))
fi

echo
if [ "$FAILS" -eq 0 ]; then echo "fidelity: ALL PASS"; else echo "fidelity: $FAILS FAILURES"; fi
rm -rf "$WORKDIR"
exit "$FAILS"
