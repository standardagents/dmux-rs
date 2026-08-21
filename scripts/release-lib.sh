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

assert_release_checkout() {
  local git_dir common_dir branch
  git_dir=$(cd "$(git rev-parse --git-dir)" && pwd -P)
  common_dir=$(cd "$(git rev-parse --git-common-dir)" && pwd -P)
  [ "$git_dir" = "$common_dir" ] || {
    echo "release must run from the primary checkout"
    return 1
  }
  branch=$(git symbolic-ref --quiet --short HEAD || true)
  [ "$branch" = "main" ] || {
    echo "release must run from the main branch"
    return 1
  }
  [ -z "$(git status --porcelain)" ] || {
    echo "working tree dirty"
    return 1
  }
  git fetch -q origin main
  [ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || {
    echo "HEAD not in sync with origin/main"
    return 1
  }
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

# Refresh only immutable release tags. The issue tool uses moving
# sa-issues-claim/* tags, which a broad --tags fetch can reject as clobbers.
refresh_release_refs() {
  git fetch -q origin main '+refs/tags/v*:refs/tags/v*'
}

latest_published_release_tag() {
  local repo=$1
  gh api "repos/$repo/releases/latest" --jq .tag_name 2>/dev/null || true
}

release_has_asset() {
  local repo=$1
  local tag=$2
  local asset=$3
  gh release view "$tag" -R "$repo" --json assets --jq '.assets[].name' 2>/dev/null \
    | /usr/bin/grep -Fxq "$asset"
}

select_release_version() {
  local published=$1
  local bump=$2
  local repo=$3
  local asset=$4
  local highest candidate tag_commit head
  highest=$(git tag -l 'v*' --sort=-v:refname | head -1)
  head=$(git rev-parse HEAD)
  if [ -n "$highest" ]; then
    tag_commit=$(git rev-list -n1 "$highest")
    if [ "$tag_commit" = "$head" ] && ! release_has_asset "$repo" "$highest" "$asset"; then
      printf '%s\n' "$highest"
      return
    fi
  fi

  candidate=$(next_release_version "${published:-v0.1.0}" "$bump")
  if git rev-parse -q --verify "refs/tags/$candidate" >/dev/null; then
    tag_commit=$(git rev-list -n1 "$candidate")
    if [ "$tag_commit" != "$head" ]; then
      echo "release tag $candidate points to $tag_commit instead of $head" >&2
      return 1
    fi
  fi
  printf '%s\n' "$candidate"
}

publish_release() {
  local repo=$1
  local version=$2
  local asset_path=$3
  local sha=$4
  local notes="Automated test-ring release ($sha). Running heads self-update within ~1 minute."
  if gh release view "$version" -R "$repo" >/dev/null 2>&1; then
    gh release upload "$version" "$asset_path" -R "$repo" --clobber
  else
    gh release create "$version" "$asset_path" -R "$repo" \
      --target "$sha" \
      --title "$version" \
      --notes "$notes"
  fi
}
