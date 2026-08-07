set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

version := `git describe --tags --always --dirty 2>/dev/null || echo dev`

_default:
    @just --list

fmt:
    ./scripts/format.sh

privacy *args:
    ./scripts/privacy-check.sh {{ args }}

check *args:
    ./scripts/check.sh {{ args }}

test:
    ./scripts/check.sh --ci --skip-fmt --skip-check --skip-clippy

build:
    cargo build --locked --release

run *args:
    cargo run -- {{ args }}

tui:
    cargo run --release

package:
    ./packaging/package.sh --version {{ version }}

verify-package archive:
    ./packaging/verify-package.sh --run-binary {{ archive }}

homebrew-formula version repo="rainysteven1/net-pan" assets_dir="dist":
    ./packaging/generate-homebrew-formula.sh --version {{ version }} --repo {{ repo }} --assets-dir {{ assets_dir }}

homebrew-tap version tap_dir repo="rainysteven1/net-pan" assets_dir="dist":
    ./packaging/generate-homebrew-formula.sh --version {{ version }} --repo {{ repo }} --assets-dir {{ assets_dir }} --tap-dir {{ tap_dir }}

changelog:
    git-cliff -o CHANGELOG.md

release-check version:
    ./scripts/release-check.sh {{ version }}
