#!/bin/bash
# Move an issue's card on the configured GitHub Project board.
#
#   scripts/board.sh <issue-number> <status>
#
# <status> matches (case-insensitively) one of the Status options the
# configured Project defines — discovered dynamically, no fixed vocabulary.
# An unknown status prints the Project's available options.
#
# Primary path: the @standardagents/issues GitHub App credentials
# (~/.standardagents/issues/) via board.py — needs no gh scopes.
# Never blocks the loop: missing credentials or permissions exit 0 with a
# note on stderr.
set -euo pipefail
exec python3 "$(dirname "$0")/board.py" "$@"
