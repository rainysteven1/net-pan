#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${1:-}"

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "usage: ./scripts/release-check.sh <major.minor.patch>" >&2
  exit 2
fi

cd "$ROOT_DIR"
if [[ -n "$(git status --porcelain)" ]]; then
  echo "release requires a clean worktree" >&2
  exit 1
fi

manifest_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
if [[ "$manifest_version" != "$VERSION" ]]; then
  echo "Cargo.toml version $manifest_version does not match requested release $VERSION" >&2
  exit 1
fi

./scripts/privacy-check.sh --history
./scripts/check.sh --full
./packaging/package.sh --version "v$VERSION"

echo "release v$VERSION is ready for manual review and tagging"
