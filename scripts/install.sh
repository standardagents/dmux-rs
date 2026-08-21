#!/bin/bash
# dmux-rs first-party ring installer.
#
#   gh api repos/standardagents/dmux-rs/contents/scripts/install.sh \
#     -H "Accept: application/vnd.github.raw" | bash
#
# Downloads the latest release for this machine into ~/.local/bin/dmux-rs.
# From then on the binary keeps itself fresh: it polls releases every minute
# and hot-swaps in place (your tmux sessions survive the swap).
set -euo pipefail
REPO=${DMUX_RS_REPO:-standardagents/dmux-rs}

if ! command -v gh >/dev/null; then
  echo "error: the GitHub CLI (gh) is required — https://cli.github.com" >&2
  exit 1
fi
if ! gh auth status >/dev/null 2>&1; then
  echo "error: run 'gh auth login' first" >&2
  exit 1
fi
if ! gh api "repos/$REPO" >/dev/null 2>&1; then
  echo "error: your GitHub account does not have access to $REPO — ask to be added to the test ring" >&2
  exit 1
fi
if ! command -v tmux >/dev/null; then
  echo "error: tmux >= 3.3 is required" >&2
  exit 1
fi

OS=$(uname -s | tr '[:upper:]' '[:lower:]'); [ "$OS" = "darwin" ] && OS=macos
ARCH=$(uname -m); [ "$ARCH" = "arm64" ] && ARCH=aarch64
ASSET="dmux-rs-$OS-$ARCH"
TAG=$(gh api "repos/$REPO/releases/latest" -q .tag_name)
DEST="$HOME/.local/bin"
mkdir -p "$DEST"

echo "installing $TAG ($ASSET) → $DEST/dmux-rs"
STAGED=$(mktemp "$DEST/.dmux-rs.staged.XXXXXX")
cleanup() {
  [ -z "${STAGED:-}" ] || rm -f "$STAGED"
}
trap cleanup EXIT
gh release download "$TAG" -R "$REPO" -p "$ASSET" -O "$STAGED" --clobber
chmod +x "$STAGED"
REPORTED_VERSION=$("$STAGED" --version) || {
  echo "error: staged executable could not report its version" >&2
  exit 1
}
case "$REPORTED_VERSION" in
  "dmux-rs $TAG" | "dmux-rs $TAG ("*) ;;
  *)
    echo "error: staged executable reported '$REPORTED_VERSION', expected release $TAG" >&2
    exit 1
    ;;
esac
"$STAGED" --help >/dev/null 2>&1 || {
  echo "error: staged executable failed its startup check" >&2
  exit 1
}
mv -f "$STAGED" "$DEST/dmux-rs"
STAGED=

case ":$PATH:" in
  *":$DEST:"*) ;;
  *) echo "note: add $DEST to your PATH (e.g. 'export PATH=\"$DEST:\$PATH\"' in your shell rc)" ;;
esac

echo
echo "done. cd into any project and run: dmux-rs"
echo "  · auto-updates every minute from $REPO releases (DMUX_NO_UPDATE=1 to opt out)"
echo "  · rendering divergences auto-file issues with reproduction bytes (DMUX_NO_REPORT=1 to opt out)"
echo "installed: $TAG"
