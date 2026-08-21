#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."

test_dir=$(mktemp -d "${TMPDIR:-/tmp}/dmux-install-test.XXXXXX")
trap 'rm -rf "$test_dir"' EXIT
test_home="$test_dir/home"
fake_bin="$test_dir/bin"
destination="$test_home/.local/bin/dmux-rs"
mkdir -p "$test_home/.local/bin" "$fake_bin"

cat > "$fake_bin/tmux" <<'SH'
#!/bin/bash
exit 0
SH
chmod +x "$fake_bin/tmux"

cat > "$fake_bin/mv" <<'SH'
#!/bin/bash
[ "${FAKE_MOVE_FAIL:-0}" != 1 ] || exit 83
exec /bin/mv "$@"
SH
chmod +x "$fake_bin/mv"

cat > "$fake_bin/gh" <<'SH'
#!/bin/bash
set -euo pipefail
case "${1:-} ${2:-}" in
  "auth status") exit 0 ;;
  "api repos/test/repo") exit 0 ;;
  "api repos/test/repo/releases/latest")
    echo "${FAKE_TAG:?}"
    exit 0
    ;;
  "release download")
    [ "$("$FAKE_OLD_DEST" --version)" = "old" ] || {
      echo "installer replaced the existing executable before download completed" >&2
      exit 80
    }
    [ "${FAKE_DOWNLOAD_FAIL:-0}" != 1 ] || exit 81
    output=
    while [ "$#" -gt 0 ]; do
      if [ "$1" = "-O" ]; then
        output=$2
        break
      fi
      shift
    done
    [ -n "$output" ]
    cat > "$output" <<'CANDIDATE'
#!/bin/bash
case "${1:-}" in
  --version) echo "dmux-rs ${FAKE_REPORTED_TAG:?} (fixture)" ;;
  --help) exit "${FAKE_HELP_STATUS:-0}" ;;
  *) exit 0 ;;
esac
CANDIDATE
    exit 0
    ;;
esac
echo "unexpected gh command: $*" >&2
exit 82
SH
chmod +x "$fake_bin/gh"

write_previous() {
  cat > "$destination" <<'SH'
#!/bin/bash
[ "${1:-}" = "--version" ] && echo old
SH
  chmod +x "$destination"
}

assert_previous_runs() {
  [ "$("$destination" --version)" = old ] || {
    echo "install test failed: previous executable is unavailable"
    exit 1
  }
}

assert_no_staging_file() {
  if find "$test_home/.local/bin" -name '.dmux-rs.staged.*' -print -quit | grep -q .; then
    echo "install test failed: staged executable was not cleaned up"
    exit 1
  fi
}

run_install() {
  env \
    HOME="$test_home" \
    PATH="$fake_bin:$PATH" \
    DMUX_RS_REPO=test/repo \
    FAKE_TAG=v9.8.7 \
    FAKE_REPORTED_TAG="${FAKE_REPORTED_TAG:-v9.8.7}" \
    FAKE_HELP_STATUS="${FAKE_HELP_STATUS:-0}" \
    FAKE_DOWNLOAD_FAIL="${FAKE_DOWNLOAD_FAIL:-0}" \
    FAKE_MOVE_FAIL="${FAKE_MOVE_FAIL:-0}" \
    FAKE_OLD_DEST="$destination" \
    bash scripts/install.sh > "$test_dir/output" 2>&1
}

write_previous
run_install
[ "$(FAKE_REPORTED_TAG=v9.8.7 "$destination" --version)" = "dmux-rs v9.8.7 (fixture)" ] || {
  echo "install test failed: verified candidate was not installed"
  exit 1
}
assert_no_staging_file

write_previous
FAKE_DOWNLOAD_FAIL=1 run_install && {
  echo "install test failed: failed download succeeded"
  exit 1
}
assert_previous_runs
assert_no_staging_file

write_previous
FAKE_MOVE_FAIL=1 run_install && {
  echo "install test failed: failed activation move succeeded"
  exit 1
}
assert_previous_runs
assert_no_staging_file

write_previous
FAKE_REPORTED_TAG=v9.8.6 run_install && {
  echo "install test failed: wrong release tag succeeded"
  exit 1
}
assert_previous_runs
assert_no_staging_file

write_previous
FAKE_HELP_STATUS=1 run_install && {
  echo "install test failed: failed startup check succeeded"
  exit 1
}
assert_previous_runs
assert_no_staging_file

echo "install tests: PASS"
