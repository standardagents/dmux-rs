#!/bin/bash
# Automated semver release for the dmux-rs test ring. Safe for unattended
# use by the fixer loop: refuses dirty/unpushed state and re-validates
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

# Guards (#60): clean tree and HEAD identical to origin/main. Commit
# identity — not the local branch name — is what makes a release safe, so
# per-issue worktrees and detached HEADs qualify when fully synchronized.
[ -z "$(git status --porcelain)" ] || { echo "working tree dirty"; exit 1; }
git fetch -q origin main
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || { echo "HEAD not in sync with origin/main"; exit 1; }
# End guards.

SHA=$(git rev-parse --short HEAD)

# Re-validate: never publish anything the suite or the fidelity harness
# hasn't blessed (unattended releases have no human eyeball).
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
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || { echo "main advanced during validation"; exit 1; }
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
