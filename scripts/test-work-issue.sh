#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."

script="$PWD/scripts/work-issue.sh"
test_dir=$(mktemp -d "${TMPDIR:-/tmp}/dmux-work-issue-test.XXXXXX")
cleanup() {
  rm -rf "$test_dir"
}
trap cleanup EXIT

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

if (cd "$repo" && "$script" invalid >/dev/null 2>&1); then
  echo "work-issue test failed: invalid issue number succeeded"
  exit 1
fi

path=$(cd "$repo" && DMUX_ISSUE_WORKTREE_ROOT="$worktrees" "$script" 123)
[ "$path" = "$worktrees/issue-123" ] || {
  echo "work-issue test failed: unexpected path '$path'"
  exit 1
}
[ "$(git -C "$path" branch --show-current)" = "issue-123" ] || {
  echo "work-issue test failed: issue branch was not checked out"
  exit 1
}
[ "$(git -C "$path" rev-parse HEAD)" = "$(git -C "$repo" rev-parse origin/main)" ] || {
  echo "work-issue test failed: worktree did not start at origin/main"
  exit 1
}
[ "$(git -C "$repo" branch --show-current)" = main ] || {
  echo "work-issue test failed: source checkout branch changed"
  exit 1
}

if (cd "$repo" && DMUX_ISSUE_WORKTREE_ROOT="$worktrees" "$script" 123 >/dev/null 2>&1); then
  echo "work-issue test failed: duplicate issue worktree succeeded"
  exit 1
fi

echo "work-issue tests: PASS"
