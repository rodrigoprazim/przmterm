#!/usr/bin/env bash
# Release helper. See docs/VERSIONING.md.
#
#   scripts/release.sh 0.2.0          set version, date the changelog, commit and tag v0.2.0
#   scripts/release.sh --next 0.3.0   start development of 0.3.0 (sets 0.3.0-dev)
set -euo pipefail
cd "$(dirname "$0")/.."

REPO_URL="https://github.com/rodrigoprazim/przmterm"

die() { echo "error: $*" >&2; exit 1; }

set_version() {
  perl -0pi -e 's/^version = "[^"]*"/version = "'"$1"'"/m' Cargo.toml
  cargo update --workspace --quiet
}

current_version() {
  perl -ne 'if (/^version = "([^"]*)"/) { print $1; exit }' Cargo.toml
}

[[ $# -ge 1 ]] || die "usage: $0 [--next] X.Y.Z"

if [[ $1 == "--next" ]]; then
  [[ $# -eq 2 && $2 =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "usage: $0 --next X.Y.Z"
  set_version "$2-dev"
  git commit -am "chore: start $2-dev"
  echo "main is now at $2-dev"
  exit 0
fi

VERSION=$1
[[ $VERSION =~ ^[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$ ]] || die "version must look like X.Y.Z or 1.0.0-rc.N"
[[ -z $(git status --porcelain) ]] || die "working tree is not clean"
git rev-parse "v$VERSION" >/dev/null 2>&1 && die "tag v$VERSION already exists"

# The Unreleased section must say what is in the release.
perl -0ne 'exit(/## \[Unreleased\]\s*\n\s*###/ ? 0 : 1)' CHANGELOG.md \
  || die "CHANGELOG.md has nothing under [Unreleased]"

echo "Releasing $(current_version) as $VERSION"
set_version "$VERSION"

TODAY=$(date +%Y-%m-%d)
PREV=$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null || true)
perl -0pi -e "s/## \[Unreleased\]\n/## [Unreleased]\n\n## [$VERSION] - $TODAY\n/" CHANGELOG.md
perl -0pi -e "s|^\[Unreleased\]: .*\$|[Unreleased]: $REPO_URL/compare/v$VERSION...main|m" CHANGELOG.md
if [[ -n $PREV ]]; then
  echo "[$VERSION]: $REPO_URL/compare/$PREV...v$VERSION" >> CHANGELOG.md
else
  echo "[$VERSION]: $REPO_URL/releases/tag/v$VERSION" >> CHANGELOG.md
fi

cargo test --workspace --quiet
git commit -am "release: v$VERSION"
git tag -a "v$VERSION" -m "PrzmTerm $VERSION"

echo
echo "Tagged v$VERSION. Publish with:"
echo "  git push origin main v$VERSION"
