#!/bin/bash
# Automated semver release for the dmux-rs test ring. Safe for unattended
# use by the fixer loop: refuses dirty/unpushed state and re-validates
# everything before publishing. Running heads self-update within ~1 minute.
#
#   scripts/release.sh [patch|minor|major]   (default: patch)
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=${DMUX_RS_REPO:-standardagents/dmux-rs}
BUMP=${1:-patch}

# Guards: main, clean, and in sync with origin.
[ "$(git branch --show-current)" = "main" ] || { echo "release only from main"; exit 1; }
[ -z "$(git status --porcelain)" ] || { echo "working tree dirty"; exit 1; }
git fetch -q origin main
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || { echo "main not in sync with origin"; exit 1; }

# Next version from the latest v* tag.
git fetch -q --tags origin
LATEST=$(git tag -l 'v*' --sort=-v:refname | head -1)
LATEST=${LATEST:-v0.1.0}
IFS=. read -r MA MI PA <<< "${LATEST#v}"
case "$BUMP" in
  major) MA=$((MA+1)); MI=0; PA=0 ;;
  minor) MI=$((MI+1)); PA=0 ;;
  patch) PA=$((PA+1)) ;;
  *) echo "unknown bump: $BUMP"; exit 1 ;;
esac
VERSION="v$MA.$MI.$PA"
SHA=$(git rev-parse --short HEAD)

# Re-validate: never publish anything the suite or the fidelity harness
# hasn't blessed (unattended releases have no human eyeball).
echo "[$VERSION] validating…"
cargo test --quiet 2>&1 | tail -1
cargo build --quiet --bin dmux-rs --bin griddump
bash scripts/fidelity.sh >/dev/null 2>&1 || { echo "fidelity harness FAILED — no release"; exit 1; }

OS=$(uname -s | tr '[:upper:]' '[:lower:]'); [ "$OS" = "darwin" ] && OS=macos
ARCH=$(uname -m); [ "$ARCH" = "arm64" ] && ARCH=aarch64
ASSET="dmux-rs-$OS-$ARCH"
echo "[$VERSION] building release binary ($SHA)…"
DMUX_BUILD_TAG="$VERSION" cargo build --release --bin dmux-rs
cp target/release/dmux-rs "/tmp/$ASSET"

git tag "$VERSION"
git push -q origin "$VERSION"
gh release create "$VERSION" "/tmp/$ASSET" -R "$REPO" \
  --title "$VERSION" \
  --notes "Automated test-ring release ($SHA). Running heads self-update within ~1 minute."
echo "released $VERSION"
