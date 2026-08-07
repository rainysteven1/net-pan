#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="${NET_PAN_CHECK_PROFILE:-full}"
RUN_PRIVACY=1
RUN_FMT=1
RUN_CHECK=1
RUN_CLIPPY=1
RUN_TESTS=1
RUN_BUILD=1

usage() {
  cat <<'EOF'
usage: ./scripts/check.sh [--pre-commit|--ci|--full]
                          [--skip-privacy] [--skip-fmt] [--skip-check]
                          [--skip-clippy] [--skip-tests] [--skip-build]
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --pre-commit) PROFILE="pre-commit" ;;
    --ci) PROFILE="ci" ;;
    --full) PROFILE="full" ;;
    --skip-privacy) RUN_PRIVACY=0 ;;
    --skip-fmt) RUN_FMT=0 ;;
    --skip-check) RUN_CHECK=0 ;;
    --skip-clippy) RUN_CLIPPY=0 ;;
    --skip-tests) RUN_TESTS=0 ;;
    --skip-build) RUN_BUILD=0 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
  shift
done

case "$PROFILE" in
  pre-commit)
    RUN_TESTS=0
    RUN_BUILD=0
    ;;
  ci)
    RUN_BUILD=0
    ;;
  full) ;;
  *) echo "unknown check profile: $PROFILE" >&2; exit 2 ;;
esac

cd "$ROOT_DIR"

step() {
  printf '\n[check] %s\n' "$1"
}

if [[ "$RUN_PRIVACY" -eq 1 ]]; then
  step "Checking repository privacy"
  ./scripts/privacy-check.sh
fi
if [[ "$RUN_FMT" -eq 1 ]]; then
  step "Checking Rust formatting"
  ./scripts/format.sh --check
fi
if [[ "$RUN_CHECK" -eq 1 ]]; then
  step "Checking all Rust targets"
  cargo check --locked --all-targets
fi
if [[ "$RUN_CLIPPY" -eq 1 ]]; then
  step "Running Clippy"
  cargo clippy --locked --all-targets -- -D warnings
fi
if [[ "$RUN_TESTS" -eq 1 ]]; then
  step "Running tests with an isolated HOME"
  original_home="${HOME:?HOME is required}"
  sandbox_home="$(mktemp -d)"
  trap 'rm -rf "$sandbox_home"' EXIT
  env \
    HOME="$sandbox_home" \
    USERPROFILE="$sandbox_home" \
    XDG_CONFIG_HOME="$sandbox_home/.config" \
    XDG_RUNTIME_DIR="$sandbox_home/.runtime" \
    XDG_STATE_HOME="$sandbox_home/.state" \
    CARGO_HOME="${CARGO_HOME:-$original_home/.cargo}" \
    RUSTUP_HOME="${RUSTUP_HOME:-$original_home/.rustup}" \
    RUST_TEST_THREADS=1 \
    cargo test --locked
fi
if [[ "$RUN_BUILD" -eq 1 ]]; then
  step "Building the release binary"
  cargo build --locked --release
fi

step "All requested checks passed"
