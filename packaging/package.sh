#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="$ROOT_DIR/dist"
VERSION=""
TARGET=""
SKIP_BUILD=0

usage() {
  cat <<'EOF'
usage: ./packaging/package.sh [--version <value>] [--target <rust-target>] [--skip-build]
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) VERSION="${2:-}"; shift 2 ;;
    --target) TARGET="${2:-}"; shift 2 ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done

if [[ -z "$VERSION" ]]; then
  VERSION="$(git -C "$ROOT_DIR" describe --tags --always --dirty 2>/dev/null || echo dev)"
fi
if [[ ! "$VERSION" =~ ^[A-Za-z0-9._+-]+$ ]]; then
  echo "invalid package version: $VERSION" >&2
  exit 1
fi
if [[ -z "$TARGET" ]]; then
  TARGET="$(rustc -vV | awk '/^host:/ {print $2}')"
fi

case "$TARGET" in
  x86_64-unknown-linux-gnu) suffix="linux-x86_64" ;;
  aarch64-unknown-linux-gnu) suffix="linux-aarch64" ;;
  x86_64-apple-darwin) suffix="macos-x86_64" ;;
  aarch64-apple-darwin) suffix="macos-aarch64" ;;
  *) echo "unsupported release target: $TARGET" >&2; exit 1 ;;
esac

package_name="net-pan-$VERSION-$suffix"
package_dir="$DIST_DIR/$package_name"
archive="$DIST_DIR/$package_name.tar.gz"
binary="$ROOT_DIR/target/$TARGET/release/net-pan"

cd "$ROOT_DIR"
if [[ "$SKIP_BUILD" -eq 0 ]]; then
  cargo build --locked --release --target "$TARGET"
fi
if [[ ! -x "$binary" ]]; then
  echo "release binary is missing: $binary" >&2
  exit 1
fi

rm -rf "$package_dir"
rm -f "$archive"
mkdir -p "$package_dir" "$DIST_DIR"
install -m 0755 "$binary" "$package_dir/net-pan"
install -m 0755 packaging/install.sh "$package_dir/install.sh"
install -m 0644 README.md LICENSE THIRD_PARTY_NOTICES "$package_dir/"
tar -C "$DIST_DIR" -czf "$archive" "$package_name"
./packaging/verify-package.sh "$archive"

(
  cd "$DIST_DIR"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum net-pan-*.tar.gz > checksums.txt
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 net-pan-*.tar.gz > checksums.txt
  else
    echo "sha256sum or shasum is required" >&2
    exit 1
  fi
)

printf 'wrote %s\n' "$archive"
