#!/bin/bash

release_lock_path() {
  local repo_slug=${1//\//-}
  printf '%s/dmux-rs-release-%s.lock\n' "${TMPDIR:-/tmp}" "$repo_slug"
}

with_release_lock() {
  local lock_file=$1
  shift
  exec lockf -k "$lock_file" "$@"
}

next_release_version() {
  local latest=${1:-v0.1.0}
  local bump=$2
  local major minor patch
  IFS=. read -r major minor patch <<< "${latest#v}"
  case "$bump" in
    major) major=$((major + 1)); minor=0; patch=0 ;;
    minor) minor=$((minor + 1)); patch=0 ;;
    patch) patch=$((patch + 1)) ;;
    *) return 2 ;;
  esac
  printf 'v%s.%s.%s\n' "$major" "$minor" "$patch"
}

# Fail fast when origin/main has advanced past HEAD (#84). $1 names the
# validation phase that just finished, for the error message.
assert_main_unmoved() {
  git fetch -q origin main
  if [ "$(git rev-parse HEAD)" != "$(git rev-parse origin/main)" ]; then
    echo "origin/main advanced during $1 — stopping before further validation; re-sync and rerun"
    exit 1
  fi
}
