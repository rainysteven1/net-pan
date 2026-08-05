# Project Map

This repository currently builds `pan-tui`, a Rust TUI for browsing saved cloud-drive folder sources
and downloading the highlighted item to a local directory.

| Area | Purpose | Owning files | Evidence | Confidence | Open questions |
|---|---|---|---|---|---|
| CLI startup | Parses config paths, loads network settings, creates the Google Drive client, and starts the TUI or `--list`. | `src/main.rs` | `Cli`, `resolve_credentials`, `resolve_sources`, `run_browser` call | confirmed | Aliyun Drive will need a provider-aware credential path instead of Google-only credentials. |
| Network settings | Stores an app-specific proxy and explicitly ignores system proxy env vars. | `src/settings.rs` | `NetworkSettings::configure_client`, `default_settings_path` | confirmed | Provider-specific proxy requirements should reuse this path. |
| Google auth | Uses a service-account key to mint Google Drive read-only access tokens. | `src/auth.rs` | `DRIVE_READONLY_SCOPE`, `ServiceAccountTokenProvider` | confirmed | Aliyun Drive auth flow is not implemented yet. |
| Google Drive API | Fetches metadata, lists direct children, downloads media streams, and parses Google Drive links. | `src/drive.rs` | `DriveClient`, `parse_drive_reference` | confirmed | Needs extraction behind a provider trait before adding Aliyun Drive cleanly. |
| Source registry | Loads saved source links from JSON and validates them as Google Drive folders. | `src/sources.rs` | `SourcesFile`, `load_source_folders` | confirmed | Current schema is Google-only; TODO Phase 13 tracks explicit provider support. |
| Download planning | Builds recursive or single-file download plans with safe local names and duplicate handling. | `src/plan.rs` | Plan tests and `safe_name` usage from TUI | confirmed | Provider-neutral item IDs and native-file semantics need normalization. |
| Download execution | Streams files through `.part`, supports resume, validates sizes, and emits progress events. | `src/download.rs` | `download_item`, `DownloadEvent` | confirmed | Aliyun Drive may require signed or temporary download URLs. |
| TUI | Provides shared-link browsing, real folder navigation, download form, logs, settings, and exit confirmation. | `src/tui.rs` | `BrowserApp`, `render_ui`, event loop tests | confirmed | UI should show provider names once mixed Google/Aliyun sources exist. |
| Logging | Writes session logs under `~/.pan-tui/logs` with size rotation. | `src/logger.rs` | `SessionLogger::from_home` | confirmed | None known. |
