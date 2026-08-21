#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."

test_dir=$(mktemp -d "${TMPDIR:-/tmp}/dmux-validate-test.XXXXXX")
trap 'rm -rf "$test_dir"' EXIT
mkdir -p "$test_dir/scripts"
cp scripts/validate.sh "$test_dir/scripts/validate.sh"
log="$test_dir/order.log"

write_step() {
  local name=$1 status=${2:-0}
  cat > "$test_dir/scripts/$name.sh" <<SH
#!/bin/bash
echo ${name} >> "\${VALIDATE_TEST_LOG:?}"
exit $status
SH
  chmod +x "$test_dir/scripts/$name.sh"
}

write_step check
write_step ui-smoke
write_step fidelity
VALIDATE_TEST_LOG="$log" bash "$test_dir/scripts/validate.sh" --between \
  'echo between >> "${VALIDATE_TEST_LOG:?}"'
expected=$'check\nui-smoke\nbetween\nfidelity'
[ "$(cat "$log")" = "$expected" ] || {
  echo "validate test failed: unexpected phase order"
  cat "$log"
  exit 1
}

: > "$log"
write_step ui-smoke 7
status=0
VALIDATE_TEST_LOG="$log" bash "$test_dir/scripts/validate.sh" || status=$?
[ "$status" = 7 ] || {
  echo "validate test failed: UI failure returned $status"
  exit 1
}
[ "$(cat "$log")" = $'check\nui-smoke' ] || {
  echo "validate test failed: fidelity ran after a UI failure"
  cat "$log"
  exit 1
}

echo "validate tests: PASS"
