#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION=""
REPO="rainysteven1/net-pan"
ASSETS_DIR="$ROOT_DIR/dist"
OUTPUT_PATH=""
TAP_DIR=""

usage() {
  cat <<'EOF'
usage: ./packaging/generate-homebrew-formula.sh --version <vX.Y.Z>
       [--repo <owner/repo>] [--assets-dir <path>]
       [--output <path> | --tap-dir <path>]
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) VERSION="${2:-}"; shift 2 ;;
    --repo) REPO="${2:-}"; shift 2 ;;
    --assets-dir) ASSETS_DIR="${2:-}"; shift 2 ;;
    --output) OUTPUT_PATH="${2:-}"; shift 2 ;;
    --tap-dir) TAP_DIR="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done

if [[ ! "$VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "--version must match v<major>.<minor>.<patch>" >&2
  exit 2
fi
if [[ ! "$REPO" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
  echo "--repo must be an owner/repository slug" >&2
  exit 2
fi
if [[ -n "$OUTPUT_PATH" && -n "$TAP_DIR" ]]; then
  echo "--output and --tap-dir are mutually exclusive" >&2
  exit 2
fi

if [[ -n "$TAP_DIR" ]]; then
  OUTPUT_PATH="$TAP_DIR/Formula/net-pan.rb"
elif [[ -z "$OUTPUT_PATH" ]]; then
  OUTPUT_PATH="$ROOT_DIR/packaging/homebrew/net-pan.rb"
fi

linux_archive="$ASSETS_DIR/net-pan-$VERSION-linux-x86_64.tar.gz"
macos_intel_archive="$ASSETS_DIR/net-pan-$VERSION-macos-x86_64.tar.gz"
macos_arm_archive="$ASSETS_DIR/net-pan-$VERSION-macos-aarch64.tar.gz"
for archive in "$linux_archive" "$macos_intel_archive" "$macos_arm_archive"; do
  if [[ ! -f "$archive" ]]; then
    echo "required release archive is missing: $archive" >&2
    exit 1
  fi
done

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    echo "sha256sum or shasum is required" >&2
    exit 1
  fi
}

linux_sha="$(sha256 "$linux_archive")"
macos_intel_sha="$(sha256 "$macos_intel_archive")"
macos_arm_sha="$(sha256 "$macos_arm_archive")"
normalized_version="${VERSION#v}"
mkdir -p "$(dirname "$OUTPUT_PATH")"

cat > "$OUTPUT_PATH" <<EOF
class NetPan < Formula
  desc "Provider-neutral TUI and daemon for downloading cloud-drive files"
  homepage "https://github.com/$REPO"
  version "$normalized_version"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/$REPO/releases/download/v#{version}/net-pan-v#{version}-macos-aarch64.tar.gz"
      sha256 "$macos_arm_sha"
    end

    on_intel do
      url "https://github.com/$REPO/releases/download/v#{version}/net-pan-v#{version}-macos-x86_64.tar.gz"
      sha256 "$macos_intel_sha"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/$REPO/releases/download/v#{version}/net-pan-v#{version}-linux-x86_64.tar.gz"
      sha256 "$linux_sha"
    end
  end

  def install
    bin.install "net-pan"
    prefix.install "README.md", "LICENSE", "THIRD_PARTY_NOTICES"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/net-pan --version")
  end
end
EOF

printf 'wrote %s\n' "$OUTPUT_PATH"
