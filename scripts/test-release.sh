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

fake_bin="$test_dir/bin"
fake_state="$test_dir/gh-state"
release_repo="$test_dir/repo"
mkdir -p "$fake_bin" "$fake_state/releases" "$fake_state/assets"
cat > "$fake_bin/gh" <<'SH'
#!/bin/bash
set -euo pipefail
state=${FAKE_RELEASE_STATE:?}
case "${1:-} ${2:-}" in
  "api repos/test/repo/releases/latest")
    cat "$state/published"
    ;;
  "release view")
    tag=$3
    [ -e "$state/releases/$tag" ] || exit 1
    if [[ " $* " = *" --json assets "* ]]; then
      [ ! -e "$state/assets/$tag" ] || cat "$state/assets/$tag"
    fi
    ;;
  "release create")
    tag=$3
    printf 'create %s\n' "$*" >> "$state/calls"
    if [ "${FAKE_CREATE_FAIL_ONCE:-0}" = 1 ] && [ ! -e "$state/create-failed" ]; then
      touch "$state/create-failed"
      exit 1
    fi
    touch "$state/releases/$tag"
    ;;
  "release upload")
    tag=$3
    printf 'upload %s\n' "$*" >> "$state/calls"
    touch "$state/releases/$tag"
    echo dmux-rs-test > "$state/assets/$tag"
    ;;
  *)
    echo "unexpected gh command: $*" >&2
    exit 90
    ;;
esac
SH
chmod +x "$fake_bin/gh"

git init -q -b main "$release_repo"
git -C "$release_repo" config user.email test@example.com
git -C "$release_repo" config user.name test
git -C "$release_repo" commit -q --allow-empty -m published
git -C "$release_repo" tag v1.2.3
git -C "$release_repo" commit -q --allow-empty -m pending
git -C "$release_repo" tag v1.2.4
echo v1.2.3 > "$fake_state/published"
assert_eq v1.2.3 "$(PATH="$fake_bin:$PATH" FAKE_RELEASE_STATE="$fake_state" latest_published_release_tag test/repo)"

retry_version=$(
  cd "$release_repo"
  PATH="$fake_bin:$PATH" FAKE_RELEASE_STATE="$fake_state" \
    select_release_version v1.2.3 patch test/repo dmux-rs-test
)
assert_eq v1.2.4 "$retry_version"

release_asset="$test_dir/dmux-rs-test"
touch "$release_asset"
retry_sha=$(git -C "$release_repo" rev-parse HEAD)
first_status=0
(
  cd "$release_repo"
  PATH="$fake_bin:$PATH" FAKE_RELEASE_STATE="$fake_state" FAKE_CREATE_FAIL_ONCE=1 \
    publish_release test/repo "$retry_version" "$release_asset" "$retry_sha"
) || first_status=$?
[ "$first_status" != 0 ] || {
  echo "release test failed: simulated publication failure succeeded"
  exit 1
}
retry_version_after_failure=$(
  cd "$release_repo"
  PATH="$fake_bin:$PATH" FAKE_RELEASE_STATE="$fake_state" \
    select_release_version v1.2.3 patch test/repo dmux-rs-test
)
assert_eq "$retry_version" "$retry_version_after_failure"
(
  cd "$release_repo"
  PATH="$fake_bin:$PATH" FAKE_RELEASE_STATE="$fake_state" FAKE_CREATE_FAIL_ONCE=1 \
    publish_release test/repo "$retry_version" "$release_asset" "$retry_sha"
)
assert_eq 2 "$(grep -c '^create release create v1.2.4 ' "$fake_state/calls")"

partial_version=$(
  cd "$release_repo"
  PATH="$fake_bin:$PATH" FAKE_RELEASE_STATE="$fake_state" \
    select_release_version v1.2.3 patch test/repo dmux-rs-test
)
assert_eq v1.2.4 "$partial_version"
(
  cd "$release_repo"
  PATH="$fake_bin:$PATH" FAKE_RELEASE_STATE="$fake_state" \
    publish_release test/repo "$partial_version" "$release_asset" "$retry_sha"
)
grep -q '^upload release upload v1.2.4 ' "$fake_state/calls" || {
  echo "release test failed: partial release did not upload the missing asset"
  exit 1
}

git -C "$release_repo" commit -q --allow-empty -m advanced
if (
  cd "$release_repo"
  PATH="$fake_bin:$PATH" FAKE_RELEASE_STATE="$fake_state" \
    select_release_version v1.2.3 patch test/repo dmux-rs-test
) >/dev/null 2>&1; then
  echo "release test failed: tag on another commit was reused"
  exit 1
fi

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
