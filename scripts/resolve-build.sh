#!/bin/bash
# Resolve the installed dmux-rs build to its exact Git source snapshot (#80).
#
#   scripts/resolve-build.sh [binary-path]
#
# Reports the installed binary's path, version, release tag, and commit;
# verifies the local tag object matches the commit the binary was built
# from; relates it to the current checkout's HEAD; and prints copyable
# read-only commands for inspecting the installed source. Entirely
# read-only: never switches branches, touches worktrees, fetches, or
# modifies the binary. DMUX_RESOLVE_REPO overrides the repo (tests).
set -u

REPO_DIR=${DMUX_RESOLVE_REPO:-$(cd "$(dirname "$0")/.." && pwd)}
BIN=${1:-"$HOME/.local/bin/dmux-rs"}
g() { git -C "$REPO_DIR" "$@"; }

if [ ! -x "$BIN" ]; then
  echo "no installed binary at $BIN"
  echo "  install one:  gh api repos/standardagents/dmux-rs/contents/scripts/install.sh -H \"Accept: application/vnd.github.raw\" | bash"
  echo "  or pass a path:  scripts/resolve-build.sh /path/to/dmux-rs"
  exit 1
fi

VERSION_LINE=$("$BIN" --version 2>/dev/null | head -1)
if [ -z "$VERSION_LINE" ]; then
  echo "binary at $BIN did not report a version (corrupt or not dmux-rs?)"
  exit 1
fi
# Formats: "dmux-rs v0.22.6 (79b22ad)" · "dmux-rs dev (fac23f4)"
#          · "dmux-rs v0.20.8" (release builds before #80: no embedded commit)
VERSION=$(printf '%s\n' "$VERSION_LINE" | /usr/bin/awk '{print $2}')
SHA=$(printf '%s\n' "$VERSION_LINE" | /usr/bin/sed -n 's/.*(\([0-9a-f]\{7,40\}\)).*/\1/p')

echo "binary:  $BIN"
echo "version: $VERSION_LINE"

TAG=""
case "$VERSION" in
  dev)
    echo "build:   untagged development build — not from a release"
    ;;
  v*)
    TAG=$VERSION
    ;;
  *)
    echo "unrecognized version format: $VERSION_LINE"
    exit 1
    ;;
esac

FAIL=0

TAG_COMMIT=""
if [ -n "$TAG" ]; then
  TAG_COMMIT=$(g rev-parse -q --verify "refs/tags/$TAG^{commit}" 2>/dev/null || true)
  if [ -z "$TAG_COMMIT" ]; then
    echo "tag:     $TAG — NOT PRESENT locally (run 'git fetch --tags' yourself; this tool never fetches)"
    FAIL=1
  else
    echo "tag:     $TAG → $(g rev-parse --short "$TAG_COMMIT")"
  fi
fi

# The commit the binary was actually built from: embedded sha when present,
# else the tag's commit (pre-#80 release builds).
SRC=""
if [ -n "$SHA" ]; then
  SRC=$(g rev-parse -q --verify "$SHA^{commit}" 2>/dev/null || true)
  if [ -z "$SRC" ]; then
    echo "commit:  $SHA — NO LOCAL OBJECT (run 'git fetch' yourself; this tool never fetches)"
    FAIL=1
  else
    echo "commit:  $(g rev-parse --short "$SRC") (embedded in the binary)"
  fi
  if [ -n "$TAG_COMMIT" ] && [ -n "$SRC" ] && [ "$TAG_COMMIT" != "$SRC" ]; then
    echo "MISMATCH: tag $TAG points at $(g rev-parse --short "$TAG_COMMIT") but the binary was built from $(g rev-parse --short "$SRC")"
    echo "          (moved tag, or a binary built outside the release flow) — trust the embedded commit"
    FAIL=1
  fi
elif [ -n "$TAG_COMMIT" ]; then
  SRC=$TAG_COMMIT
  echo "commit:  $(g rev-parse --short "$SRC") (via tag; build predates embedded commits — cannot prove tag wasn't moved)"
fi

HEAD_SHA=$(g rev-parse -q --verify HEAD 2>/dev/null || true)
if [ -z "$HEAD_SHA" ]; then
  echo "checkout: $REPO_DIR has no HEAD (not a git repo?)"
  FAIL=1
elif [ -n "$SRC" ]; then
  if [ "$HEAD_SHA" = "$SRC" ]; then
    echo "checkout: HEAD $(g rev-parse --short HEAD) == installed build — read files normally"
  else
    AHEAD=$(g rev-list --count "$SRC..$HEAD_SHA" 2>/dev/null || echo "?")
    BEHIND=$(g rev-list --count "$HEAD_SHA..$SRC" 2>/dev/null || echo "?")
    echo "checkout: HEAD $(g rev-parse --short HEAD) — $AHEAD commit(s) ahead, $BEHIND behind the installed build"
    echo "          files in this checkout may differ from the running process"
  fi
else
  echo "checkout: HEAD $(g rev-parse --short HEAD) — installed commit unresolved, relationship unknown"
fi

if [ -n "$SRC" ]; then
  SHORT=$(g rev-parse --short "$SRC")
  echo
  echo "inspect the installed source snapshot (read-only):"
  echo "  git show $SHORT:crates/dmux/src/main.rs        # any file, exact installed version"
  echo "  git ls-tree -r --name-only $SHORT               # list every file in the snapshot"
  echo "  git grep <pattern> $SHORT -- crates/            # search inside the snapshot"
fi

exit $FAIL
