#!/bin/sh
set -eu

REPO="${NET_PAN_REPO:-rainysteven1/net-pan}"
VERSION="${NET_PAN_VERSION:-latest}"
TMP_DIR="$(mktemp -d)"

cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup 0
trap 'exit 1' HUP INT TERM

fetch() {
  curl -fsSL \
    --retry 5 \
    --retry-delay 2 \
    --retry-all-errors \
    --connect-timeout 30 \
    --max-time 300 \
    "$@"
}

case "$(uname -s)" in
  Linux) os="linux" ;;
  Darwin) os="macos" ;;
  *) echo "net-pan release assets support Linux and macOS" >&2; exit 1 ;;
esac

case "$(uname -m)" in
  x86_64|amd64) arch="x86_64" ;;
  arm64|aarch64) arch="aarch64" ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

if [ "$os" = "linux" ] && [ "$arch" != "x86_64" ]; then
  echo "Linux arm64 is not published yet" >&2
  exit 1
fi

base="https://github.com/$REPO/releases"
cd "$TMP_DIR"
if [ "$VERSION" = "latest" ]; then
  fetch \
    -o release.json \
    "https://api.github.com/repos/$REPO/releases/latest"
  tag="$(awk -F '"' '/"tag_name"/ { print $4; exit }' release.json)"
  asset_base="$base/download/$tag"
  fetch -o checksums.txt "$asset_base/checksums.txt"
else
  tag="v${VERSION#v}"
  asset_base="$base/download/$tag"
  fetch -o checksums.txt "$asset_base/checksums.txt"
fi

case "$tag" in
  v[0-9]*.[0-9]*.[0-9]*) ;;
  *) echo "could not resolve a release tag" >&2; exit 1 ;;
esac

archive="net-pan-$tag-$os-$arch.tar.gz"
fetch -o "$archive" "$asset_base/$archive"

awk -v archive="$archive" '
  {
    path = $2
    sub(/^.*\//, "", path)
    if (path == archive) {
      print $1 "  " archive
      found = 1
    }
  }
  END { exit found ? 0 : 1 }
' checksums.txt > checksums.one || {
  echo "checksum for $archive is missing" >&2
  exit 1
}

if command -v sha256sum >/dev/null 2>&1; then
  sha256sum -c checksums.one
elif command -v shasum >/dev/null 2>&1; then
  shasum -a 256 -c checksums.one
else
  echo "sha256sum or shasum is required" >&2
  exit 1
fi

tar -xzf "$archive"
package_dir="$TMP_DIR/${archive%.tar.gz}"
"$package_dir/install.sh"
