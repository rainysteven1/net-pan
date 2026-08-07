# Project Map

This repository currently builds `net-pan`, a Rust TUI for browsing saved cloud-drive folder sources
and downloading the highlighted item to a local directory.

| Area | Purpose | Owning files | Evidence | Confidence | Open questions |
|---|---|---|---|---|---|
| CLI startup | Loads `~/.net-pan/settings.toml`, builds the Provider registry, connects to the daemon or embedded runtime, and starts TUI/CLI commands. | `src/main.rs` | `run_tui`, `run_daemon`, `provider_registry` | confirmed | None known. |
| Network settings | Stores an app-specific proxy and explicitly ignores system proxy env vars. | `src/settings.rs` | `NetworkSettings::configure_client`, `default_settings_path` | confirmed | Provider-specific proxy requirements should reuse this path. |
| Provider runtime | Registers Google/Aliyun factories and keys Account instances by `ProviderIdentity`. | `src/provider/` | `ProviderFactory`, `ProviderManager`, `ProviderRuntime` | confirmed | Real Aliyun account acceptance remains pending. |
| Google auth | Supports service-account and Installed App OAuth with Profile-local credentials and token state. | `src/auth.rs`, `src/provider/google.rs` | `ServiceAccountTokenProvider`, `InstalledAppTokenProvider`, `GoogleProviderFactory` | confirmed | Real Installed App first-login acceptance remains pending. |
| Source registry | Loads Provider-owned Sources and binds each remote reference to Account identity. | `src/sources.rs`, `src/provider/` | `RemoteSource`, `RemoteRef`, Provider `sources()` | confirmed | None known. |
| Download planning | Builds recursive or single-file download plans with safe local names and duplicate handling. | `src/plan.rs` | Plan tests and `safe_name` usage from TUI | confirmed | Provider-neutral item IDs and native-file semantics need normalization. |
| Download execution | Streams files through `.part`, supports resume, validates sizes, and emits progress events. | `src/download.rs` | `download_item`, `DownloadEvent` | confirmed | Aliyun Drive may require signed or temporary download URLs. |
| TUI | Provides Provider Type header switching, Account navigation, files, transfers, logs, settings, and contextual shortcuts. | `src/tui/` | `BrowserApp`, `render_ui`, `run_browser` | confirmed | Usage/drive summary panels are not yet exposed through `PanClient`. |
| Logging | Writes session logs under `~/.net-pan/logs` with size rotation. | `src/logger.rs` | `SessionLogger::from_home` | confirmed | None known. |
| Engineering delivery | Runs shared lint/test/privacy gates and builds verified Linux/macOS release archives, checksums, installer, release notes, and Homebrew formula. | `scripts/`, `packaging/`, `.github/workflows/`, `justfile` | `scripts/check.sh`, `packaging/verify-package.sh`, release workflow | confirmed | Git history privacy gate must pass before the first public release. |
