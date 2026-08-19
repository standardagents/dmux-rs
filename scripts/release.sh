#!/bin/bash
# Cut a dmux-rs release: build with a build tag, publish to GitHub Releases.
# Running heads poll releases and self-update within minutes.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=${DMUX_RS_REPO:-standardagents/dmux-rs}
SHA=$(git rev-parse --short HEAD)
TAG="build-$(date +%Y%m%d-%H%M)-$SHA"
# Asset names must match updater::asset_name(): dmux-rs-{std OS}-{std ARCH}
OS=$(uname -s | tr '[:upper:]' '[:lower:]'); [ "$OS" = "darwin" ] && OS=macos
ARCH=$(uname -m); [ "$ARCH" = "arm64" ] && ARCH=aarch64
ASSET="dmux-rs-$OS-$ARCH"
echo "building $TAG → $ASSET"
DMUX_BUILD_TAG="$TAG" cargo build --release --bin dmux-rs
cp target/release/dmux-rs "/tmp/$ASSET"
gh release create "$TAG" "/tmp/$ASSET" -R "$REPO" \
  --title "$TAG" \
  --notes "Automated first-party build ($SHA). Running heads self-update to this tag."
echo "released $TAG"
