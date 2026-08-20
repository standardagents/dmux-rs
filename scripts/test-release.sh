#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."

source scripts/release-lib.sh

assert_eq() {
  [ "$1" = "$2" ] || {
    echo "release test failed: expected '$1', got '$2'"
    exit 1
  }
}

assert_eq v1.2.4 "$(next_release_version v1.2.3 patch)"
assert_eq v1.3.0 "$(next_release_version v1.2.3 minor)"
assert_eq v2.0.0 "$(next_release_version v1.2.3 major)"
if next_release_version v1.2.3 invalid >/dev/null 2>&1; then
  echo "release test failed: invalid bump succeeded"
  exit 1
fi

test_dir=$(mktemp -d "${TMPDIR:-/tmp}/dmux-release-test.XXXXXX")
lock_file="$test_dir/release.lock"
ready_file="$test_dir/ready"
cleanup() {
  if [ -n "${holder_pid:-}" ]; then
    kill "$holder_pid" 2>/dev/null || true
    wait "$holder_pid" 2>/dev/null || true
  fi
  rm -rf "$test_dir"
}
trap cleanup EXIT

(
  with_release_lock "$lock_file" bash -c 'touch "$1"; sleep 1' bash "$ready_file"
) &
holder_pid=$!
for _ in 1 2 3 4 5 6 7 8 9 10; do
  [ -e "$ready_file" ] && break
  sleep 0.05
done
[ -e "$ready_file" ] || {
  echo "release test failed: lock holder did not start"
  exit 1
}
if lockf -t 0 -k "$lock_file" true 2>/dev/null; then
  echo "release test failed: concurrent lock acquisition succeeded"
  exit 1
fi
wait "$holder_pid"
holder_pid=

(with_release_lock "$lock_file" bash -c 'exit 9') || status=$?
assert_eq 9 "${status:-0}"
(with_release_lock "$lock_file" true)

echo "release tests: PASS"
