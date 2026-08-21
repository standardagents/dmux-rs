#!/bin/bash
# Checkout and publication guard tests against synthetic repositories.
set -euo pipefail
cd "$(dirname "$0")/.."
SRC=$PWD
T=$(mktemp -d /tmp/dmux-relguard.XXXX)
trap 'rm -rf "$T"' EXIT

guards() ( # $1 = repo dir
  cd "$1"
  source "$SRC/scripts/release-lib.sh"
  assert_release_checkout
)

git init -q -b main "$T/origin" && (cd "$T/origin" && git commit -q --allow-empty -m one)
git clone -q "$T/origin" "$T/wt"

fail=0
# 1. Clean clone on main, synced → pass.
guards "$T/wt" >/dev/null 2>&1 || { echo "FAIL: synced main should release"; fail=1; }
# 2. A linked worktree at the same commit → blocked.
git -C "$T/wt" worktree add -q --detach "$T/linked" origin/main
out=$(guards "$T/linked" 2>&1) && { echo "FAIL: linked worktree must block"; fail=1; }
echo "$out" | /usr/bin/grep -q "primary checkout" \
  || { echo "FAIL: linked worktree should report the primary-checkout requirement"; fail=1; }
# 3. Issue branch at the same commit → blocked.
(cd "$T/wt" && git checkout -q -b issue-123)
out=$(guards "$T/wt" 2>&1) && { echo "FAIL: issue branch must block"; fail=1; }
echo "$out" | /usr/bin/grep -q "main branch" \
  || { echo "FAIL: issue branch should report the main-branch requirement"; fail=1; }
# 4. Detached HEAD at the same commit → blocked.
(cd "$T/wt" && git checkout -q --detach)
out=$(guards "$T/wt" 2>&1) && { echo "FAIL: detached HEAD must block"; fail=1; }
echo "$out" | /usr/bin/grep -q "main branch" \
  || { echo "FAIL: detached HEAD should report the main-branch requirement"; fail=1; }
# 5. Dirty tree → blocked.
(cd "$T/wt" && git checkout -q main && echo x > dirty.txt)
guards "$T/wt" >/dev/null 2>&1 && { echo "FAIL: dirty tree must block"; fail=1; }
(cd "$T/wt" && rm dirty.txt)
# 6. Ahead of origin → blocked.
(cd "$T/wt" && git commit -q --allow-empty -m ahead)
guards "$T/wt" >/dev/null 2>&1 && { echo "FAIL: ahead of origin must block"; fail=1; }
# 7. Behind origin → blocked.
(cd "$T/wt" && git reset -q --hard HEAD~1 && cd "$T/origin" && git commit -q --allow-empty -m newer)
guards "$T/wt" >/dev/null 2>&1 && { echo "FAIL: behind origin must block"; fail=1; }

# 8. Between-phase advancement guard (#84): synced → continues.
(cd "$T/wt" && git fetch -q origin && git reset -q --hard origin/main)
(cd "$T/wt" && source "$SRC/scripts/release-lib.sh" && assert_main_unmoved test) \
  || { echo "FAIL: synced repo must pass assert_main_unmoved"; fail=1; }
# 9. Remote advanced after a phase → stops with the phase in the message.
(cd "$T/origin" && git commit -q --allow-empty -m advance)
rc=0
out=$(cd "$T/wt" && source "$SRC/scripts/release-lib.sh" && assert_main_unmoved "workspace checks" 2>&1) || rc=$?
{ [ "$rc" != 0 ] && echo "$out" | /usr/bin/grep -q "advanced during workspace checks"; } \
  || { echo "FAIL: advanced remote must stop the release after the phase"; fail=1; }
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
