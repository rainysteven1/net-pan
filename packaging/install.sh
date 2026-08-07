#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_DIR="${NET_PAN_INSTALL_DIR:-$HOME/.local/bin}"
SOURCE_BIN="$SCRIPT_DIR/net-pan"

if [[ ! -x "$SOURCE_BIN" ]]; then
  echo "archive does not contain an executable net-pan binary" >&2
  exit 1
fi

install -d -m 0755 "$INSTALL_DIR"
install -m 0755 "$SOURCE_BIN" "$INSTALL_DIR/net-pan"
printf 'installed net-pan to %s\n' "$INSTALL_DIR/net-pan"

case ":${PATH}:" in
  *":$INSTALL_DIR:"*) ;;
  *) printf 'add %s to PATH to run net-pan directly\n' "$INSTALL_DIR" ;;
esac
