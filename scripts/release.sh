#!/bin/bash
# User-authorized semver release for the dmux-rs test ring. It runs only from
# the primary main checkout, refuses dirty or unpushed state, and re-validates
# everything before publishing. Running heads self-update within ~1 minute.
#
#   scripts/release.sh [patch|minor|major]   (default: patch)
set -euo pipefail
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
cd "$SCRIPT_DIR/.."
REPO=${DMUX_RS_REPO:-standardagents/dmux-rs}
BUMP=${1:-patch}
source "$SCRIPT_DIR/release-lib.sh"

case "$BUMP" in
  major | minor | patch) ;;
  *) echo "unknown bump: $BUMP"; exit 1 ;;
esac

if [ "${DMUX_RELEASE_LOCK_HELD:-0}" != "1" ]; then
  command -v lockf >/dev/null || { echo "release requires lockf"; exit 1; }
  lock_file=$(release_lock_path "$REPO")
  echo "waiting for release lock: $lock_file"
  export DMUX_RELEASE_LOCK_HELD=1
  with_release_lock "$lock_file" "$SCRIPT_DIR/release.sh" "$@"
fi

# A release requires the primary main checkout at the exact origin/main commit.
assert_release_checkout

SHA=$(git rev-parse --short HEAD)

# Re-validate before publishing through the suite and fidelity harness.
echo "[release] validating…"
# One shared orchestration with CI (#87). --between re-checks the remote tip
# between the expensive phases so a concurrent push stops the release before
# fidelity starts (#84); the final guard below remains authoritative.
bash scripts/validate.sh --between \
  'source scripts/release-lib.sh && assert_main_unmoved "workspace checks"'


# Main and the tag set may have advanced during validation. The release lock
# prevents another local publisher from changing them between this refresh
# and publication.
refresh_release_refs
assert_release_checkout
PUBLISHED=$(latest_published_release_tag "$REPO")

OS=$(uname -s | tr '[:upper:]' '[:lower:]'); [ "$OS" = "darwin" ] && OS=macos
ARCH=$(uname -m); [ "$ARCH" = "arm64" ] && ARCH=aarch64
ASSET="dmux-rs-$OS-$ARCH"
VERSION=$(select_release_version "$PUBLISHED" "$BUMP" "$REPO" "$ASSET")

echo "[$VERSION] building release binary ($SHA)…"
DMUX_BUILD_TAG="$VERSION" cargo build --release --bin dmux-rs
cp target/release/dmux-rs "/tmp/$ASSET"

publish_release "$REPO" "$VERSION" "/tmp/$ASSET" "$(git rev-parse HEAD)"
echo "released $VERSION"
