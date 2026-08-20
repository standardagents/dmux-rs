#!/bin/bash
# Complete local quality gate. CI and the release path call this same script.
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
