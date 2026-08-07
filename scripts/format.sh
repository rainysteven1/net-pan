#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERIFY=0

if [[ "${1:-}" == "--check" ]]; then
  VERIFY=1
  shift
fi
if [[ $# -ne 0 ]]; then
  echo "usage: ./scripts/format.sh [--check]" >&2
  exit 2
fi

cd "$ROOT_DIR"
if [[ "$VERIFY" -eq 1 ]]; then
  cargo fmt --all -- --check
else
  cargo fmt --all
fi
