#!/bin/bash
# Hermetic tests for scripts/resolve-build.sh (#80): a throwaway git repo
# plus fake binaries (shell scripts that print version lines) cover the
# matching-checkout, divergent-checkout, missing-tag, missing-object,
# mismatched-tag, development-build, and missing-binary cases.
set -u
cd "$(dirname "$0")/.."
RESOLVE=$PWD/scripts/resolve-build.sh

T=$(mktemp -d /tmp/resolve-build-test.XXXX)
trap 'rm -rf "$T"' EXIT
FAILS=0

check() { # $1 label, $2 expected-exit, $3 grep-pattern (against $OUT/$RC)
  local out rc
  out=$OUT
  rc=$RC
  if [ "$rc" != "$2" ]; then
    echo "FAIL $1: exit $rc, wanted $2"
    echo "$out" | /usr/bin/sed 's/^/    /'
    FAILS=$((FAILS + 1))
  elif ! echo "$out" | /usr/bin/grep -q "$3"; then
    echo "FAIL $1: output missing '$3'"
    echo "$out" | /usr/bin/sed 's/^/    /'
    FAILS=$((FAILS + 1))
  else
    echo "PASS $1"
  fi
}
run() { # binary path → sets $OUT and $RC
  set +e
  OUT=$(DMUX_RESOLVE_REPO="$T/repo" "$RESOLVE" "$1" 2>&1)
  RC=$?
  set -e
}

mkbin() { # $1 path, $2 version line
  printf '#!/bin/sh\necho "%s"\n' "$2" > "$1"
  chmod +x "$1"
}

# Fixture repo: commit C1 (tagged v1.0.0), then commit C2 on top.
git init -q -b main "$T/repo"
git -C "$T/repo" -c user.email=t@t -c user.name=t commit -q --allow-empty -m one
git -C "$T/repo" tag v1.0.0
C1=$(git -C "$T/repo" rev-parse --short HEAD)
git -C "$T/repo" -c user.email=t@t -c user.name=t commit -q --allow-empty -m two
C2=$(git -C "$T/repo" rev-parse --short HEAD)

# 1. Matching checkout: HEAD sits exactly on the installed commit.
git -C "$T/repo" checkout -q --detach v1.0.0
mkbin "$T/match" "dmux-rs v1.0.0 ($C1)"
run "$T/match"; check matching-checkout 0 "== installed build"

# 2. Divergent checkout: HEAD back on main (1 ahead of the tag).
git -C "$T/repo" checkout -q main
run "$T/match"; check divergent-checkout 0 "1 commit(s) ahead, 0 behind"
run "$T/match"; check divergent-warns 0 "may differ from the running process"

# 3. Copyable snapshot-inspection commands, pinned to the installed commit.
run "$T/match"; check inspect-commands 0 "git show $C1:"

# 4. Missing tag.
mkbin "$T/notag" "dmux-rs v9.9.9 ($C1)"
run "$T/notag"; check missing-tag 1 "NOT PRESENT locally"

# 5. Missing object: plausible sha with no local object.
mkbin "$T/noobj" "dmux-rs v1.0.0 (deadbeef00d)"
run "$T/noobj"; check missing-object 1 "NO LOCAL OBJECT"

# 6. Mismatched tag: tag v1.0.0 is C1, binary claims C2.
mkbin "$T/mismatch" "dmux-rs v1.0.0 ($C2)"
run "$T/mismatch"; check mismatched-tag 1 "MISMATCH"

# 7. Development build: no tag to verify; still resolves the commit.
mkbin "$T/dev" "dmux-rs dev ($C1)"
run "$T/dev"; check dev-build 0 "untagged development build"

# 8. Pre-#80 release build: tag only, no embedded commit.
mkbin "$T/old" "dmux-rs v1.0.0"
run "$T/old"; check legacy-build 0 "via tag; build predates embedded commits"

# 9. Missing binary.
run "$T/absent"; check missing-binary 1 "no installed binary"

# Read-only guarantee: resolution left HEAD, branches, and tags untouched.
[ "$(git -C "$T/repo" rev-parse --short HEAD)" = "$C2" ] \
  && [ "$(git -C "$T/repo" rev-parse --short v1.0.0)" = "$C1" ] \
  && echo "PASS read-only" || { echo "FAIL read-only"; FAILS=$((FAILS + 1)); }

if [ "$FAILS" -eq 0 ]; then echo "resolve-build tests: PASS"; else
  echo "resolve-build tests: $FAILS FAILURES"; fi
exit "$FAILS"
