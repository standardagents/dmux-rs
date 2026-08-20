#!/bin/bash
# Fast quality gate: formatting, Clippy, the full workspace test suite,
# and the script suites. The COMPLETE pre-push gate is scripts/validate.sh,
# which runs this and then the rendering-fidelity harness.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash scripts/test-release.sh
bash scripts/test-work-issue.sh
bash scripts/test-start-task.sh
bash scripts/test-resolve-build.sh
bash scripts/release-guards-test.sh
