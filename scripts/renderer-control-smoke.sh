#!/bin/bash
# Cross-renderer ownership smoke harness (#112). Two isolated terminal tmux
# servers attach native renderers to one target session. Each failed assertion
# captures its own diagnostics under /tmp.
set -u
cd "$(dirname "$0")/.." || exit 1

cargo build --quiet --bin dmux-rs || { echo "renderer-control: build FAILED"; exit 1; }
BIN=$PWD/target/debug/dmux-rs
RUN=$$
TGT=renderer-target-$RUN
DRVA=renderer-a-$RUN
DRVB=renderer-b-$RUN
DRVC=renderer-c-$RUN
DRVD=renderer-d-$RUN
FAILS=0
DIAG=$(mktemp -d /tmp/renderer-control-fail.XXXXXX)
WORK=$(mktemp -d /tmp/renderer-control.XXXX)
mkdir -p "$WORK/home"
INPUT=$WORK/probe-input.bin
WAIT_ATTEMPTS=200

cleanup_servers() {
  for socket in "$DRVA" "$DRVB" "$DRVC" "$DRVD" "$TGT"; do
    tmux -L "$socket" kill-server 2>/dev/null || true
  done
}

save_diagnostics() {
  label=$(printf '%s' "$1" | tr ' /' '--' | tr -cd '[:alnum:]_.-')
  destination="$DIAG/$(printf '%02d' "$FAILS")-$label"
  mkdir -p "$destination"
  cp -R "$WORK" "$destination/fixture" 2>/dev/null || true
  for spec in "$DRVA:a" "$DRVB:b" "$DRVC:c" "$DRVD:d"; do
    socket=${spec%%:*}; session=${spec#*:}
    tmux -L "$socket" capture-pane -p -e -N -t "$session" \
      > "$destination/$session-screen.txt" 2>&1 || true
  done
  tmux -L "$TGT" list-clients -F '#{client_name}|#{client_pid}|#{client_control_mode}|#{client_width}|#{client_height}' \
    > "$destination/clients.txt" 2>&1 || true
  tmux -L "$TGT" list-panes -a -F '#{session_name}|#{pane_id}|#{window_name}|#{pane_width}x#{pane_height}' \
    > "$destination/panes.txt" 2>&1 || true
  tmux -L "$TGT" list-sessions -F '#{session_name}|#{session_id}|#{session_windows}|#{session_attached}' \
    > "$destination/sessions.txt" 2>&1 || true
  tmux -L "$TGT" list-windows -a -F '#{session_name}|#{window_id}|#{window_name}|#{window_width}x#{window_height}' \
    > "$destination/windows.txt" 2>&1 || true
  for session in shared isolated legacy; do
    tmux -L "$TGT" show-options -t "$session" \
      > "$destination/$session-options.txt" 2>&1 || true
  done
  {
    echo "failure=$1"
    echo "git=$(git rev-parse HEAD 2>/dev/null || echo unknown)"
    echo "tmux=$(tmux -V 2>/dev/null || echo unknown)"
    echo "shared_owner=$(owner shared)"
    echo "shared_token=$(renderer_token shared)"
  } > "$destination/manifest.txt"
  ps -p "${APID:-0},${BPID:-0},${CPID:-0},${DPID:-0}" -o pid=,ppid=,state=,command= \
    > "$destination/processes.txt" 2>&1 || true
}

finish() {
  cleanup_servers
  if [ "$FAILS" -eq 0 ]; then rm -rf "$WORK" "$DIAG"; fi
}
trap finish EXIT
cleanup_servers

pass() { echo "PASS $1"; }
fail() {
  echo "FAIL $1"
  FAILS=$((FAILS + 1))
  printf '%s | shared owner=%s | token=%s\n' "$1" "$(owner shared)" "$(renderer_token shared)" \
    >> "$WORK/failures.txt"
  save_diagnostics "$1"
}

owner() { tmux -L "$TGT" show-options -t "$1" -qv @dmux_renderer_owner 2>/dev/null; }
owner_field() { owner "$1" | /usr/bin/awk -F'|' -v n="$2" '{print $n}'; }
owner_token() { owner_field "$1" 2; }
owner_pid() { owner_field "$1" 3; }
renderer_token() { tmux -L "$TGT" show-options -t "$1" -qv @dmux_renderer_token 2>/dev/null; }

wait_owner_record() {
  session=$1 expected_pid=$2 expected_token=${3:-} expected_connection=${4:-} expected_cols=${5:-} expected_rows=${6:-}
  for _ in $(seq 1 "$WAIT_ATTEMPTS"); do
    record=$(owner "$session")
    token=$(printf '%s' "$record" | /usr/bin/awk -F'|' '{print $2}')
    pid=$(printf '%s' "$record" | /usr/bin/awk -F'|' '{print $3}')
    connection=$(printf '%s' "$record" | /usr/bin/awk -F'|' '{print $5}')
    cols=$(printf '%s' "$record" | /usr/bin/awk -F'|' '{print $6}')
    rows=$(printf '%s' "$record" | /usr/bin/awk -F'|' '{print $7}')
    if [ -n "$token" ] && [ "$pid" = "$expected_pid" ] \
      && [ "$(renderer_token "$session")" = "$token" ] \
      && { [ -z "$expected_token" ] || [ "$token" = "$expected_token" ]; } \
      && { [ -z "$expected_connection" ] || [ "$connection" = "$expected_connection" ]; } \
      && { [ -z "$expected_cols" ] || [ "$cols" = "$expected_cols" ]; } \
      && { [ -z "$expected_rows" ] || [ "$rows" = "$expected_rows" ]; }; then
      return 0
    fi
    sleep 0.05
  done
  return 1
}

wait_owner_pid() { wait_owner_record "$1" "$2"; }

wait_no_owner() {
  session=$1
  for _ in $(seq 1 "$WAIT_ATTEMPTS"); do
    [ -z "$(owner "$session")" ] && [ -z "$(renderer_token "$session")" ] && return 0
    sleep 0.05
  done
  return 1
}

wait_status() {
  socket=$1 session=$2 pattern=$3
  for _ in $(seq 1 "$WAIT_ATTEMPTS"); do
    tmux -L "$socket" capture-pane -p -t "$session" 2>/dev/null | grep -q "$pattern" && return 0
    sleep 0.05
  done
  return 1
}

wait_count() {
  needle=$1 expected=$2
  stable=0
  for _ in $(seq 1 "$WAIT_ATTEMPTS"); do
    count=$(python3 - "$INPUT" "$needle" <<'PY'
import pathlib, sys
data = pathlib.Path(sys.argv[1]).read_bytes() if pathlib.Path(sys.argv[1]).exists() else b""
print(data.count(bytes.fromhex(sys.argv[2])))
PY
)
    if [ "$count" = "$expected" ]; then
      stable=$((stable + 1))
      [ "$stable" -ge 5 ] && return 0
    else
      stable=0
    fi
    sleep 0.05
  done
  return 1
}

wait_owner_and_geometry_stable() {
  session=$1 expected_token=$2 expected_geometry=$3 stable=0
  for _ in $(seq 1 "$WAIT_ATTEMPTS"); do
    geometry=$(tmux -L "$TGT" display-message -p -t "$PANE" '#{pane_width}x#{pane_height}' 2>/dev/null)
    if [ "$(owner_token "$session")" = "$expected_token" ] \
      && [ "$(renderer_token "$session")" = "$expected_token" ] \
      && [ "$geometry" = "$expected_geometry" ]; then
      stable=$((stable + 1))
      [ "$stable" -ge 10 ] && return 0
    else
      stable=0
    fi
    sleep 0.05
  done
  return 1
}

byte_count() {
  python3 - "$INPUT" "$1" <<'PY'
import pathlib, sys
data = pathlib.Path(sys.argv[1]).read_bytes() if pathlib.Path(sys.argv[1]).exists() else b""
print(data.count(bytes.fromhex(sys.argv[2])))
PY
}

send_hex() {
  socket=$1 session=$2 hex=$3
  # shellcheck disable=SC2046
  tmux -L "$socket" send-keys -t "$session" -H $(echo "$hex" | sed 's/../& /g')
}

start_renderer() {
  socket=$1 session=$2 target_session=$3 cols=$4 rows=$5 category=$6 token=${7:-} role=${8:-} expected=${9:-}
  if [ "$category" = ssh ]; then
    connection="SSH_CONNECTION=fixture"
  else
    connection=""
  fi
  preserved=""
  [ -n "$token" ] && preserved="DMUX_RENDERER_TOKEN=$token"
  relaunch=""
  [ -n "$role" ] && relaunch="DMUX_RENDERER_REEXEC_ROLE=$role"
  [ -n "$expected" ] && relaunch="$relaunch DMUX_RENDERER_REEXEC_OWNER=$expected"
  tmux -L "$socket" -f /dev/null new-session -d -s "$session" -x "$cols" -y "$rows" \
    "cd '$WORK'; exec env -u TMUX -u SSH_CONNECTION -u SSH_TTY -u DMUX_RENDERER_TOKEN -u DMUX_RENDERER_REEXEC_ROLE -u DMUX_RENDERER_REEXEC_OWNER $connection $preserved $relaunch HOME='$WORK/home' DMUX_NO_UPDATE=1 DMUX_NO_REPORT=1 '$BIN' --socket '$TGT' --session '$target_session'"
}

cat > "$WORK/probe.py" <<'PY'
import os, pathlib, sys, termios, tty

path = pathlib.Path(sys.argv[1])
fd = sys.stdin.fileno()
saved = termios.tcgetattr(fd)
tty.setraw(fd)
try:
    while True:
        data = os.read(fd, 4096)
        if not data:
            break
        with path.open("ab") as stream:
            stream.write(data)
        if b"Q" in data:
            os.write(sys.stdout.fileno(), b"\x1b[c")
finally:
    termios.tcsetattr(fd, termios.TCSADRAIN, saved)
PY

tmux -L "$TGT" -f /dev/null new-session -d -s shared -x 120 -y 35 \
  "exec python3 '$WORK/probe.py' '$INPUT'"
PANE=$(tmux -L "$TGT" list-panes -t shared -F '#{pane_id}' | head -1)

start_renderer "$DRVA" a shared 140 40 local
APID=$(tmux -L "$DRVA" display-message -p -t a '#{pane_pid}')
if wait_owner_record shared "$APID" "" local 140 40 \
  && wait_status "$DRVA" a 'Controlling · local · 140×40'; then
  pass "startup claim"
else
  fail "startup claim"
fi

# A ready ownership status must never appear before B's startup claim lands.
start_renderer "$DRVB" b shared 90 28 ssh
BPID=$(tmux -L "$DRVB" display-message -p -t b '#{pane_pid}')
FIRST_READY=""
for _ in $(seq 1 100); do
  FIRST_READY=$(tmux -L "$DRVB" capture-pane -p -t b 2>/dev/null | grep -E 'Controlling|Viewing' | tail -1)
  [ -n "$FIRST_READY" ] && break
  sleep 0.03
done
if wait_owner_record shared "$BPID" "" ssh 90 28 \
  && echo "$FIRST_READY" | grep -q 'Controlling · SSH · 90×28'; then
  pass "second startup claims before ready frame"
else
  fail "second startup claims before ready frame"
fi
for _ in $(seq 1 40); do
  tmux -L "$DRVA" capture-pane -p -t a | grep -q 'Viewing · controller is SSH · 90×28' && break
  sleep 0.05
done
if tmux -L "$DRVA" capture-pane -p -t a | grep -q 'Viewing · controller is SSH · 90×28'; then
  pass "former controller status"
else
  fail "former controller status"
fi

B_TOKEN=$(owner_token shared)
GEOMETRY=$(tmux -L "$TGT" display-message -p -t "$PANE" '#{pane_width}x#{pane_height}')
tmux -L "$DRVA" resize-window -t a -x 160 -y 44
send_hex "$DRVA" a 1b5b3c33353b36303b31304d
if wait_owner_and_geometry_stable shared "$B_TOKEN" "$GEOMETRY"; then
  pass "follower resize and hover preserve owner geometry"
else
  fail "follower resize and hover preserve owner geometry"
fi

K_BEFORE=$(byte_count 4b)
send_hex "$DRVA" a 4b
if wait_owner_pid shared "$APID" && wait_count 4b $((K_BEFORE + 1)); then
  pass "key claim reaches pane once"
else
  fail "key claim reaches pane once"
fi
wait_status "$DRVB" b 'Viewing · controller is local' || fail "key follower notification"

# A left press claims. Its release remains passive after the claim fence.
send_hex "$DRVB" b 1b5b3c303b36303b31304d
wait_owner_pid shared "$BPID" || fail "click press claim fence"
send_hex "$DRVB" b 1b5b3c303b36303b31306d
if wait_owner_pid shared "$BPID"; then pass "click claim"; else fail "click claim"; fi
wait_status "$DRVA" a 'Viewing · controller is SSH' || fail "click follower notification"

PASTE_BEFORE=$(byte_count 5041535445313132)
printf 'PASTE112' > "$WORK/paste.txt"
tmux -L "$DRVA" load-buffer -b renderer-paste "$WORK/paste.txt"
tmux -L "$DRVA" paste-buffer -p -b renderer-paste -t a
if wait_owner_pid shared "$APID" && wait_count 5041535445313132 $((PASTE_BEFORE + 1)); then
  pass "paste claim reaches pane once"
else
  fail "paste claim reaches pane once"
fi
wait_status "$DRVB" b 'Viewing · controller is local' || fail "paste follower notification"

send_hex "$DRVB" b 1b5b3c36343b36303b31304d
if wait_owner_pid shared "$BPID"; then pass "wheel claim"; else fail "wheel claim"; fi
wait_status "$DRVA" a 'Viewing · controller is SSH' || fail "wheel follower notification"

# B begins a drag, A claims through a key, and B finishes its passive drag.
send_hex "$DRVB" b 1b5b3c303b36303b31304d
wait_owner_pid shared "$BPID" || fail "initial drag press claim"
D_BEFORE=$(byte_count 44)
send_hex "$DRVA" a 44
wait_owner_pid shared "$APID" || fail "activity claim during remote drag"
wait_count 44 $((D_BEFORE + 1)) || fail "drag handoff key exact once"
A_TOKEN=$(owner_token shared)
wait_status "$DRVB" b 'Viewing · controller is local' || fail "drag handoff follower readiness"
send_hex "$DRVB" b 1b5b3c33323b36313b31304d
send_hex "$DRVB" b 1b5b3c303b36313b31306d
if wait_owner_and_geometry_stable shared "$A_TOKEN" \
  "$(tmux -L "$TGT" display-message -p -t "$PANE" '#{pane_width}x#{pane_height}')"; then
  pass "drag continuation and release preserve owner"
else
  fail "drag continuation and release preserve owner"
fi

# A delayed command carrying B's former token must become inert.
tmux -L "$TGT" if-shell -t shared -F "#{==:#{@dmux_renderer_token},$B_TOKEN}" \
  "set-option -t shared @dmux_former_command ran" ""
if [ -z "$(tmux -L "$TGT" show-options -t shared -qv @dmux_former_command)" ]; then
  pass "former controller command is inert"
else
  fail "former controller command is inert"
fi

Q_BEFORE=$(byte_count 51)
DA_BEFORE=$(byte_count 1b5b3f3663)
send_hex "$DRVA" a 51
if wait_count 51 $((Q_BEFORE + 1)) && wait_count 1b5b3f3663 $((DA_BEFORE + 1)); then
  pass "one controller sends one terminal response"
else
  fail "one controller sends one terminal response"
fi

DIAG_OUT=$(env -u TMUX HOME="$WORK/home" "$BIN" --socket "$TGT" --session shared --diagnose-session)
if echo "$DIAG_OUT" | grep -q 'renderer owner:' \
  && echo "$DIAG_OUT" | grep -q 'control clients (2):' \
  && ! echo "$DIAG_OUT" | grep -q "$(owner_token shared)" \
  && ! echo "$DIAG_OUT" | grep -q 'PASTE112'; then
  pass "diagnostics expose ownership metadata only"
else
  fail "diagnostics expose ownership metadata only"
fi

# B remains and recovers after A's terminal disappears.
tmux -L "$DRVA" kill-server
if wait_owner_pid shared "$BPID"; then pass "controller disconnect recovery"; else fail "controller disconnect recovery"; fi
SHARED_TOKEN=$(owner_token shared)

# A viewer that reloads for an update resumes as a viewer. Its startup must
# preserve B's ownership record.
start_renderer "$DRVA" a shared 140 40 local
APID=$(tmux -L "$DRVA" display-message -p -t a '#{pane_pid}')
wait_owner_pid shared "$APID" || fail "follower reload setup claim"
A_RELOAD_TOKEN=$(owner_token shared)
wait_status "$DRVB" b 'Viewing · controller is local' || fail "follower reload setup viewer"
send_hex "$DRVB" b 52
wait_owner_pid shared "$BPID" || fail "follower reload setup handoff"
wait_status "$DRVA" a 'Viewing · controller is SSH' || fail "follower reload setup notification"
SHARED_TOKEN=$(owner_token shared)
tmux -L "$DRVA" kill-server
start_renderer "$DRVA" a shared 140 40 local "$A_RELOAD_TOKEN" follower "$SHARED_TOKEN"
if wait_status "$DRVA" a 'Viewing · controller is SSH' \
  && [ "$(owner_token shared)" = "$SHARED_TOKEN" ]; then
  pass "follower reload preserves the current owner"
else
  fail "follower reload preserves the current owner"
fi

# A second session on the same target socket owns an independent record.
tmux -L "$TGT" new-session -d -s isolated -x 100 -y 30 "exec sleep 600"
start_renderer "$DRVC" c isolated 110 32 local
CPID=$(tmux -L "$DRVC" display-message -p -t c '#{pane_pid}')
if wait_owner_pid isolated "$CPID" \
  && [ "$(owner_token shared)" = "$SHARED_TOKEN" ] \
  && [ "$(owner_token isolated)" != "$SHARED_TOKEN" ]; then
  pass "sessions keep independent owners"
else
  fail "sessions keep independent owners"
fi

# The updater supplies the current token to its replacement process. An
# abrupt outer-terminal replacement models the detach and new attach window.
C_TOKEN=$(owner_token isolated)
tmux -L "$DRVC" kill-server
start_renderer "$DRVD" d isolated 110 32 local "$C_TOKEN" controller "$C_TOKEN"
DPID=$(tmux -L "$DRVD" display-message -p -t d '#{pane_pid}')
if wait_owner_pid isolated "$DPID" \
  && wait_status "$DRVD" d 'Controlling · local' \
  && [ "$(owner_token isolated)" = "$C_TOKEN" ]; then
  pass "replacement renderer preserves token"
else
  fail "replacement renderer preserves token"
fi
kill -TERM "$DPID"
if wait_no_owner isolated; then
  pass "graceful controller exit compare-clears ownership"
else
  fail "graceful controller exit compare-clears ownership"
fi

# A live legacy controller PID holds authority over a fresh session.
tmux -L "$TGT" new-session -d -s legacy -x 100 -y 30 "exec sleep 600"
tmux -L "$TGT" set-option -t legacy @dmux_controller_pid $$
start_renderer "$DRVC" c legacy 100 30 local
if wait_no_owner legacy \
  && wait_status "$DRVC" c 'Viewing · TypeScript controller'; then
  pass "live TypeScript controller remains authoritative"
else
  fail "live TypeScript controller remains authoritative"
fi

if [ -f "$WORK/.dmux/dmux.config.json" ]; then
  pass "controller persisted shared configuration"
else
  fail "controller persisted shared configuration"
fi

if [ "$FAILS" -eq 0 ]; then
  echo "renderer-control: ALL PASS"
  exit 0
fi
echo "renderer-control: $FAILS FAILURES (diagnostics in $DIAG)"
exit 1
