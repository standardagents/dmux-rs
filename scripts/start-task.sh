#!/bin/bash
# Task-start preflight (#86): refresh origin/main, report how the current
# checkout relates to it (ahead / behind / diverged / dirty), and prepare a
# clean task worktree from the refreshed tip before edits begin. Existing
# checkout state is never changed or discarded — divergence is reported,
# not "fixed". scripts/work-issue.sh wraps this for numbered issues.
#
#   scripts/start-task.sh <task-slug>     → prints the new worktree path
set -euo pipefail

usage() {
  echo "usage: scripts/start-task.sh <task-slug>" >&2
  exit 2
}

branch=""
if [ "${1:-}" = "--branch" ]; then # internal: work-issue.sh names its branch
  branch=${2:?}
  shift 2
fi
[ "$#" -eq 1 ] || usage
slug=$1
case "$slug" in
  '' | *[!a-zA-Z0-9._-]*) usage ;;
esac
[ -n "$branch" ] || branch="task-$slug"

repo=$(git rev-parse --show-toplevel 2>/dev/null) || {
  echo "start-task: run this command from a Git checkout" >&2
  exit 1
}
git -C "$repo" remote get-url origin >/dev/null 2>&1 || {
  echo "start-task: the checkout has no origin remote" >&2
  exit 1
}

git -C "$repo" fetch origin main

# Divergence report (stderr): where this checkout stands after the refresh.
ahead=$(git -C "$repo" rev-list --count origin/main..HEAD)
behind=$(git -C "$repo" rev-list --count HEAD..origin/main)
if [ "$ahead" -gt 0 ] && [ "$behind" -gt 0 ]; then
  state="DIVERGED ($ahead ahead, $behind behind)"
elif [ "$ahead" -gt 0 ]; then
  state="ahead $ahead"
elif [ "$behind" -gt 0 ]; then
  state="behind $behind"
else
  state="synchronized"
fi
dirty=""
[ -n "$(git -C "$repo" status --porcelain)" ] && dirty=" · working tree dirty"
echo "start-task: checkout is $state vs origin/main$dirty" >&2
case "$state" in
  DIVERGED*)
    echo "start-task: local commits may already have upstream equivalents under different SHAs — reconcile the root checkout separately; the task worktree below starts clean from origin/main" >&2
    ;;
esac

default_root="$(dirname "$repo")/$(basename "$repo")-worktrees"
worktree_root=${DMUX_TASK_WORKTREE_ROOT:-${DMUX_ISSUE_WORKTREE_ROOT:-$default_root}}
case "$worktree_root" in
  '' | /) echo "start-task: unsafe worktree root '$worktree_root'" >&2; exit 1 ;;
esac
target="$worktree_root/$branch"

if [ "$target" = "$repo" ] || [ -e "$target" ]; then
  echo "start-task: target already exists: $target" >&2
  exit 1
fi
if git -C "$repo" show-ref --verify --quiet "refs/heads/$branch"; then
  echo "start-task: branch already exists: $branch" >&2
  exit 1
fi

mkdir -p "$worktree_root"
git -C "$repo" worktree add -b "$branch" "$target" origin/main >&2
printf '%s\n' "$target"
