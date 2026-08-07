#!/usr/bin/env bash
set -euo pipefail

REPO="${NET_PAN_REPO:-rainysteven1/net-pan}"
VERSION="${NET_PAN_VERSION:-latest}"
TMP_DIR="$(mktemp -d)"

cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

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

if [[ "$os" == "linux" && "$arch" != "x86_64" ]]; then
  echo "Linux arm64 is not published yet" >&2
  exit 1
fi

base="https://github.com/$REPO/releases"
if [[ "$VERSION" == "latest" ]]; then
  tag_url="$(curl -fsSIL --retry 3 -o /dev/null -w '%{url_effective}' "$base/latest")"
  tag="${tag_url##*/}"
  asset_base="$base/latest/download"
else
  tag="v${VERSION#v}"
  asset_base="$base/download/$tag"
fi

archive="net-pan-$tag-$os-$arch.tar.gz"
cd "$TMP_DIR"
curl -fL --retry 3 --retry-delay 1 -o "$archive" "$asset_base/$archive"
curl -fL --retry 3 --retry-delay 1 -o checksums.txt "$asset_base/checksums.txt"

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
