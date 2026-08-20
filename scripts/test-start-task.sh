#!/bin/bash
# Tests for the task-start preflight (#86): synchronized, stale, diverged,
# and dirty checkout states, plus the never-touch-existing-state rule.
set -euo pipefail
cd "$(dirname "$0")/.."

script="$PWD/scripts/start-task.sh"
test_dir=$(mktemp -d "${TMPDIR:-/tmp}/dmux-start-task-test.XXXXXX")
trap 'rm -rf "$test_dir"' EXIT

origin="$test_dir/origin.git"
repo="$test_dir/repo"
worktrees="$test_dir/worktrees"
git init --bare --initial-branch=main "$origin" >/dev/null
git init --initial-branch=main "$repo" >/dev/null
git -C "$repo" config user.email test@example.com
git -C "$repo" config user.name "dmux test"
touch "$repo/fixture"
git -C "$repo" add fixture
git -C "$repo" commit -m fixture >/dev/null
git -C "$repo" remote add origin "$origin"
git -C "$repo" push -u origin main >/dev/null

fail=0
run() { # $1 slug → sets $OUT (stderr+stdout), $RC, $WT (worktree path)
  set +e
  OUT=$(cd "$repo" && DMUX_TASK_WORKTREE_ROOT="$worktrees" "$script" "$1" 2>&1)
  RC=$?
  set -e
  WT=$(printf '%s\n' "$OUT" | tail -1)
}
expect() { # $1 label, $2 pattern
  if [ "$RC" != 0 ] || ! printf '%s\n' "$OUT" | /usr/bin/grep -q "$2"; then
    echo "FAIL $1 (rc=$RC): missing '$2'"
    printf '%s\n' "$OUT" | /usr/bin/sed 's/^/    /'
    fail=1
  else
    echo "PASS $1"
  fi
}

# Invalid slug rejected.
if (cd "$repo" && "$script" "bad slug!" >/dev/null 2>&1); then
  echo "FAIL invalid-slug: accepted"; fail=1
else echo "PASS invalid-slug"; fi

# 1. Synchronized checkout.
run sync-case
expect synchronized "checkout is synchronized vs origin/main"
[ "$(git -C "$WT" branch --show-current)" = "task-sync-case" ] || { echo "FAIL branch-name"; fail=1; }
[ "$(git -C "$WT" rev-parse HEAD)" = "$(git -C "$repo" rev-parse origin/main)" ] \
  || { echo "FAIL worktree-tip"; fail=1; }

# 2. Stale checkout (behind): another clone pushes; root repo not pulled.
git clone -q "$origin" "$test_dir/other"
git -C "$test_dir/other" -c user.email=t@t -c user.name=t commit -q --allow-empty -m upstream
git -C "$test_dir/other" push -q origin main
run stale-case
expect stale "checkout is behind 1 vs origin/main"
[ "$(git -C "$WT" rev-parse HEAD)" = "$(git -C "$test_dir/other" rev-parse HEAD)" ] \
  || { echo "FAIL stale-worktree-not-fresh: worktree must start at the REFRESHED tip"; fail=1; }

# 3. Diverged checkout: a local-only commit on top of the stale root.
git -C "$repo" -c user.email=t@t -c user.name=t commit -q --allow-empty -m local-only
run diverged-case
expect diverged "DIVERGED (1 ahead, 1 behind)"
expect diverged-hint "reconcile the root checkout separately"

# 4. Dirty checkout: reported, never touched.
echo scratch > "$repo/uncommitted.txt"
run dirty-case
expect dirty "working tree dirty"
[ -f "$repo/uncommitted.txt" ] && [ "$(cat "$repo/uncommitted.txt")" = scratch ] \
  || { echo "FAIL dirty-preserved: uncommitted file was touched"; fail=1; }
[ "$(git -C "$repo" branch --show-current)" = main ] \
  || { echo "FAIL branch-preserved: root checkout branch changed"; fail=1; }
git -C "$repo" log -1 --format=%s | /usr/bin/grep -q local-only \
  || { echo "FAIL head-preserved: root HEAD moved"; fail=1; }

# 5. Duplicate task refused.
run sync-case
[ "$RC" != 0 ] && echo "PASS duplicate-refused" || { echo "FAIL duplicate-refused"; fail=1; }

[ "$fail" = 0 ] && echo "start-task tests: PASS"
exit "$fail"
