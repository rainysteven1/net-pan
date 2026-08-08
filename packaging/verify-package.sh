#!/usr/bin/env bash
set -euo pipefail

ARCHIVE=""
RUN_BINARY=0

usage() {
  cat <<'EOF'
usage: ./packaging/verify-package.sh [--run-binary] <archive.tar.gz>
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --run-binary) RUN_BINARY=1; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) usage >&2; exit 2 ;;
    *)
      [[ -z "$ARCHIVE" ]] || { usage >&2; exit 2; }
      ARCHIVE="$1"
      shift
      ;;
  esac
done

if [[ -z "$ARCHIVE" || ! -f "$ARCHIVE" ]]; then
  usage >&2
  exit 2
fi

archive_name="$(basename "$ARCHIVE")"
package_name="${archive_name%.tar.gz}"
tmp_dir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

expected="$tmp_dir/expected"
actual="$tmp_dir/actual"
cat > "$expected" <<EOF
$package_name/
$package_name/LICENSE
$package_name/README.md
$package_name/README_zh.md
$package_name/THIRD_PARTY_NOTICES
$package_name/install.sh
$package_name/net-pan
EOF

tar -tzf "$ARCHIVE" | LC_ALL=C sort > "$actual"
LC_ALL=C sort -o "$expected" "$expected"
if ! diff -u "$expected" "$actual"; then
  echo "archive contains unexpected or missing files: $ARCHIVE" >&2
  exit 1
fi

tar -xzf "$ARCHIVE" -C "$tmp_dir"
package_dir="$tmp_dir/$package_name"
if [[ -n "$(find "$package_dir" -type l -print -quit)" ]]; then
  echo "archive must not contain symbolic links" >&2
  exit 1
fi
test -x "$package_dir/net-pan"
test -x "$package_dir/install.sh"
bash -n "$package_dir/install.sh"

if [[ "$RUN_BINARY" -eq 1 ]]; then
  "$package_dir/net-pan" --version
  install_dir="$tmp_dir/installed/bin"
  NET_PAN_INSTALL_DIR="$install_dir" "$package_dir/install.sh"
  "$install_dir/net-pan" --version
fi

printf 'verified %s\n' "$ARCHIVE"
