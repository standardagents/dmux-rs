#!/bin/bash
set -euo pipefail

usage() {
  echo "usage: scripts/work-issue.sh <issue-number>" >&2
  exit 2
}

[ "$#" -eq 1 ] || usage
issue_number=$1
case "$issue_number" in
  ''|*[!0-9]*|0) usage ;;
esac

repo=$(git rev-parse --show-toplevel 2>/dev/null) || {
  echo "work-issue: run this command from a Git checkout" >&2
  exit 1
}
git -C "$repo" remote get-url origin >/dev/null 2>&1 || {
  echo "work-issue: the checkout has no origin remote" >&2
  exit 1
}

branch="issue-$issue_number"
default_root="$(dirname "$repo")/$(basename "$repo")-worktrees"
worktree_root=${DMUX_ISSUE_WORKTREE_ROOT:-$default_root}
case "$worktree_root" in
  ''|/) echo "work-issue: unsafe worktree root '$worktree_root'" >&2; exit 1 ;;
esac
target="$worktree_root/$branch"

if [ "$target" = "$repo" ] || [ -e "$target" ]; then
  echo "work-issue: target already exists: $target" >&2
  exit 1
fi
if git -C "$repo" show-ref --verify --quiet "refs/heads/$branch"; then
  echo "work-issue: branch already exists: $branch" >&2
  exit 1
fi

mkdir -p "$worktree_root"
git -C "$repo" fetch origin main
git -C "$repo" worktree add -b "$branch" "$target" origin/main >&2
printf '%s\n' "$target"
