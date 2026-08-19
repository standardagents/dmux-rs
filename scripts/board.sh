#!/bin/bash
# Move a dmux-rs issue's card on the org Project board.
#
#   scripts/board.sh <issue-number> <Todo|In Progress|Done>
#
# Primary path: the @standardagents/issues GitHub App credentials
# (~/.standardagents/issues/) via board.py — needs no gh scopes.
# Never blocks the loop: missing credentials or permissions exit 0 with a
# note on stderr.
set -euo pipefail
exec python3 "$(dirname "$0")/board.py" "$@"
