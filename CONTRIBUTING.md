# Contributing

`net-pan` uses one implementation for TUI, CLI, daemon, and IPC behavior. Keep changes inside the
existing provider and service boundaries, and do not add credentials or machine-specific paths to
the repository.

The supported compiler baseline is Rust 1.95, pinned by `rust-toolchain.toml`.

## Local checks

```bash
just check --ci
```

The equivalent command is:

```bash
./scripts/check.sh --ci
```

Before opening a pull request:

- Run the privacy check, formatter, Clippy, and tests.
- Add focused tests for behavioral changes.
- Keep Provider secrets in `~/.net-pan`, never in fixtures or examples.
- Use Conventional Commits such as `fix(download): resume stalled streams`.

See `docs/releasing.md` for release requirements.
