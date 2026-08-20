#!/bin/bash
# Guard-behavior tests for release.sh (#60): runs ONLY the guard section
# against synthetic repos — no validation, no publishing. Exits nonzero on
# any contract violation.
set -euo pipefail
cd "$(dirname "$0")/.."
SRC=$PWD
T=$(mktemp -d /tmp/dmux-relguard.XXXX)
trap 'rm -rf "$T"' EXIT

# The guard section = everything between the Guards comment and the version
# computation; run it via a here-extracted snippet so the test can't drift
# from the real script.
guards() ( # $1 = repo dir
  cd "$1"
  sed -n '/^# Guards/,/^git fetch -q --tags/p' "$SRC/scripts/release.sh" | sed '$d' | bash
)

git init -q -b main "$T/origin" && (cd "$T/origin" && git commit -q --allow-empty -m one)
git clone -q "$T/origin" "$T/wt"

fail=0
# 1. Clean clone on main, synced → pass.
guards "$T/wt" >/dev/null 2>&1 || { echo "FAIL: synced main should release"; fail=1; }
# 2. Issue branch at the same commit → pass (the #60 change).
(cd "$T/wt" && git checkout -q -b issue-123)
guards "$T/wt" >/dev/null 2>&1 || { echo "FAIL: synced issue branch should release"; fail=1; }
# 3. Detached HEAD at the same commit → pass.
(cd "$T/wt" && git checkout -q --detach)
guards "$T/wt" >/dev/null 2>&1 || { echo "FAIL: synced detached HEAD should release"; fail=1; }
# 4. Dirty tree → blocked.
(cd "$T/wt" && echo x > dirty.txt)
guards "$T/wt" >/dev/null 2>&1 && { echo "FAIL: dirty tree must block"; fail=1; }
(cd "$T/wt" && rm dirty.txt)
# 5. Ahead of origin → blocked.
(cd "$T/wt" && git checkout -q main && git commit -q --allow-empty -m ahead)
guards "$T/wt" >/dev/null 2>&1 && { echo "FAIL: ahead of origin must block"; fail=1; }
# 6. Behind origin → blocked.
(cd "$T/wt" && git reset -q --hard HEAD~1 && cd "$T/origin" && git commit -q --allow-empty -m newer)
guards "$T/wt" >/dev/null 2>&1 && { echo "FAIL: behind origin must block"; fail=1; }

[ "$fail" = 0 ] && echo "release-guards: ALL PASS"
exit "$fail"
