#!/bin/bash
# Issue-scoped wrapper over the task-start preflight (#86): validates the
# issue number and keeps the issue-<n> branch naming; everything else
# (fetch, divergence report, worktree creation) lives in start-task.sh.
set -euo pipefail

usage() {
  echo "usage: scripts/work-issue.sh <issue-number>" >&2
  exit 2
}

[ "$#" -eq 1 ] || usage
issue_number=$1
case "$issue_number" in
  '' | *[!0-9]* | 0) usage ;;
esac

exec "$(dirname "$0")/start-task.sh" --branch "issue-$issue_number" "issue-$issue_number"
