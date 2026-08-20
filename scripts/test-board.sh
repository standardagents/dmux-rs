#!/bin/bash
# Tests for the board helper's project-agnostic contract (#95): status
# options come from the configured Project (any vocabulary), matching is
# case-insensitive, unknown statuses report what the Project offers, and
# the repository derives from the origin remote.
set -euo pipefail
cd "$(dirname "$0")/.."

python3 - <<'EOF'
import importlib.util, sys

spec = importlib.util.spec_from_file_location("board", "scripts/board.py")
board = importlib.util.module_from_spec(spec)
spec.loader.exec_module(board)

fails = 0
def check(label, ok):
    global fails
    print(("PASS " if ok else "FAIL ") + label)
    if not ok:
        fails += 1

# A Project with a custom status vocabulary — no Todo/In Progress/Done.
custom = [{"id": "1", "name": "Backlog"}, {"id": "2", "name": "Doing"},
          {"id": "3", "name": "Review"}, {"id": "4", "name": "Shipped"}]

check("custom-vocabulary match", board.pick_option(custom, "Doing")["id"] == "2")
check("case-insensitive match", board.pick_option(custom, "sHiPpEd")["id"] == "4")
check("unknown status yields none", board.pick_option(custom, "Done") is None)
msg = board.unknown_status_message("Done", custom)
check("unknown status reports Project options",
      "Backlog, Doing, Review, Shipped" in msg and "'Done'" in msg)

check("repo from ssh origin",
      board.repo_from_origin("git@github.com:some-org/some-repo.git") == "some-repo")
check("repo from https origin",
      board.repo_from_origin("https://github.com/some-org/some-repo") == "some-repo")
check("repo from trailing slash",
      board.repo_from_origin("https://github.com/o/r.git/") == "r")
check("no origin yields empty", board.repo_from_origin("") == "")

# The helper must not hardcode any repository or team status vocabulary.
src = open("scripts/board.py").read() + open("scripts/board.sh").read()
check("no hardcoded repo name", 'REPO = "' not in src)
check("no fixed status vocabulary", "Todo|In Progress|Done" not in src)

sys.exit(1 if fails else 0)
EOF
echo "board tests: PASS"
