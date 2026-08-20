#!/bin/bash
# Guard-behavior tests for release.sh (#60): runs ONLY the guard section
# against synthetic repos — no validation, no publishing. Exits nonzero on
# any contract violation.
set -euo pipefail
cd "$(dirname "$0")/.."
SRC=$PWD
T=$(mktemp -d /tmp/dmux-relguard.XXXX)
trap 'rm -rf "$T"' EXIT

# The guard section = everything between the "# Guards" and "# End guards"
# markers; extracted from the real script so the test can't drift from it.
guards() ( # $1 = repo dir
  cd "$1"
  local snippet
  snippet=$(sed -n '/^# Guards/,/^# End guards/p' "$SRC/scripts/release.sh")
  # The extraction must have found the closed marker range, not run to EOF.
  echo "$snippet" | /usr/bin/grep -q "^# End guards" || {
    echo "guards extraction broken: end marker missing"; exit 3; }
  echo "$snippet" | bash
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

# 7. Between-phase advancement guard (#84): synced → continues.
(cd "$T/wt" && git fetch -q origin && git reset -q --hard origin/main)
(cd "$T/wt" && source "$SRC/scripts/release-lib.sh" && assert_main_unmoved test) \
  || { echo "FAIL: synced repo must pass assert_main_unmoved"; fail=1; }
# 8. Remote advanced after a phase → stops with the phase in the message.
(cd "$T/origin" && git commit -q --allow-empty -m advance)
rc=0
out=$(cd "$T/wt" && source "$SRC/scripts/release-lib.sh" && assert_main_unmoved "workspace checks" 2>&1) || rc=$?
{ [ "$rc" != 0 ] && echo "$out" | /usr/bin/grep -q "advanced during workspace checks"; } \
  || { echo "FAIL: advanced remote must stop the release after the phase"; fail=1; }
# 9. Orchestration (#87): release.sh validates through validate.sh, passing
# the between-phase guard, and keeps the authoritative sync guard after it;
# validate.sh itself orders check → between-hook → fidelity.
VAL_LINE=$(/usr/bin/grep -n "bash scripts/validate.sh" "$SRC/scripts/release.sh" | cut -d: -f1)
FINAL_LINE=$(/usr/bin/grep -n "main advanced during validation" "$SRC/scripts/release.sh" | cut -d: -f1)
{ [ -n "$VAL_LINE" ] && [ "$VAL_LINE" -lt "$FINAL_LINE" ]; } \
  || { echo "FAIL: release must validate via validate.sh before the final guard"; fail=1; }
/usr/bin/grep -A1 "bash scripts/validate.sh" "$SRC/scripts/release.sh" | /usr/bin/grep -q 'assert_main_unmoved "workspace checks"' \
  || { echo "FAIL: release must pass the between-phase guard to validate.sh"; fail=1; }
VCHECK=$(/usr/bin/grep -n "bash scripts/check.sh" "$SRC/scripts/validate.sh" | cut -d: -f1)
VBETWEEN=$(/usr/bin/grep -n 'bash -c "$between"' "$SRC/scripts/validate.sh" | cut -d: -f1)
VFID=$(/usr/bin/grep -n "bash scripts/fidelity.sh" "$SRC/scripts/validate.sh" | cut -d: -f1)
{ [ -n "$VCHECK" ] && [ -n "$VBETWEEN" ] && [ -n "$VFID" ] && [ "$VCHECK" -lt "$VBETWEEN" ] && [ "$VBETWEEN" -lt "$VFID" ]; } \
  || { echo "FAIL: validate.sh must order check → between-hook → fidelity"; fail=1; }

# 10. Release-tag refresh ignores the issue tool's moving claim tags.
old_claim=$(git -C "$T/origin" rev-parse HEAD~1)
git -C "$T/origin" tag sa-issues-claim/94 "$old_claim"
git -C "$T/wt" fetch -q origin \
  'refs/tags/sa-issues-claim/94:refs/tags/sa-issues-claim/94'
git -C "$T/origin" tag -f sa-issues-claim/94 HEAD >/dev/null
git -C "$T/origin" tag v0.0.1 HEAD
(cd "$T/wt" && source "$SRC/scripts/release-lib.sh" && refresh_release_refs) \
  || { echo "FAIL: release-tag refresh must ignore moving claim tags"; fail=1; }
[ "$(git -C "$T/wt" rev-parse refs/tags/sa-issues-claim/94)" = "$old_claim" ] \
  || { echo "FAIL: release-tag refresh must leave local claim tags alone"; fail=1; }
[ "$(git -C "$T/wt" rev-parse refs/tags/v0.0.1)" = "$(git -C "$T/origin" rev-parse HEAD)" ] \
  || { echo "FAIL: release-tag refresh must fetch version tags"; fail=1; }

[ "$fail" = 0 ] && echo "release-guards: ALL PASS"
exit "$fail"
