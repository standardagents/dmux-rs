#!/bin/bash
# The complete local validation gate (#87): everything required before
# dmux-rs work is pushed. Runs scripts/check.sh (formatting, Clippy, the
# full workspace test suite, and the script suites), the UI interaction
# suite, and the rendering-fidelity harness. CI and scripts/release.sh route
# through this same orchestration; release.sh passes --between to re-check
# origin/main after workspace and UI validation (#84).
#
#   scripts/validate.sh [--between <command>]
set -euo pipefail
cd "$(dirname "$0")/.."

between=""
if [ "${1:-}" = "--between" ]; then
  between=${2:?}
  shift 2
fi
[ "$#" -eq 0 ] || { echo "usage: scripts/validate.sh [--between <command>]" >&2; exit 2; }

bash scripts/check.sh
bash scripts/ui-smoke.sh
if [ -n "$between" ]; then
  bash -c "$between"
fi
bash scripts/fidelity.sh
