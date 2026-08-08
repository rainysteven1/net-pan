# net-pan

[![CI](https://github.com/rainysteven1/net-pan/actions/workflows/ci.yml/badge.svg)](https://github.com/rainysteven1/net-pan/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/rainysteven1/net-pan)](https://github.com/rainysteven1/net-pan/releases)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

English | [简体中文](docs/README_zh.md)

`net-pan` is a provider-neutral terminal application for browsing cloud drives and downloading
files or folders to local storage. It provides an interactive TUI, scriptable CLI, persistent jobs,
an optional daemon, resumable downloads, and completion notifications.

## Providers

| Provider | Status |
| --- | --- |
| Google Drive | Browsing, metadata, download, resume, Installed App OAuth, service accounts |
| Aliyun Drive | OAuth refresh, drive information, browsing, metadata, download, resume |

Aliyun is mock-tested; real-account acceptance is still in progress. Google-native Docs, Sheets,
and Slides are currently skipped because export formats are not implemented.

## Install

Install the latest Linux or macOS release with checksum verification:

```sh
curl -fsSL https://github.com/rainysteven1/net-pan/releases/latest/download/install.sh | sh
```

The binary is installed to `~/.local/bin/net-pan`. Override the destination when needed:

```sh
curl -fsSL https://github.com/rainysteven1/net-pan/releases/latest/download/install.sh \
  | NET_PAN_INSTALL_DIR="$HOME/bin" sh
```

Supported release targets:

- Linux x86_64
- macOS x86_64
- macOS arm64

Release archives, SHA-256 checksums, and a Homebrew formula are available on the
[Releases](https://github.com/rainysteven1/net-pan/releases) page.

## Quick Start

Launch the TUI:

```sh
net-pan
```

The first run creates `~/.net-pan` and draft accounts for the built-in providers. Use `Accounts`
to create or activate an account, `Settings` to configure it, and `Files` to browse its sources.
Press `g` for application-wide download and proxy settings.

The global download directory defaults to `~/Downloads`. It must be an existing absolute directory
and cannot be a symbolic link. Existing completed files are skipped by default; `.part` files are
resumed with HTTP Range when the provider supports it.

## Google Setup

Each account has a stable profile directory:

```text
~/.net-pan/providers/google/<PROFILE_ID>/
```

For Installed App OAuth:

1. Enable Google Drive API and create a Desktop OAuth client.
2. Place the downloaded client JSON at `oauth-client.json` in the profile directory.
3. Add one or more Drive folder IDs or links to `sources.json`.
4. Configure the account in the TUI, then run:

```sh
net-pan auth login google <PROFILE_ID>
net-pan auth status google --profile <PROFILE_ID>
```

Service-account authentication is also supported. Store its JSON key with mode `0600`, share each
source folder with the service-account email, and select the service-account mode in Provider
Settings. Credentials, refresh tokens, and signed download URLs are never written to logs.

Example `sources.json`:

```json
{
  "sources": [
    "https://drive.google.com/drive/folders/FOLDER_ID",
    "ANOTHER_FOLDER_ID"
  ]
}
```

## TUI Keys

| Key | Action |
| --- | --- |
| `Up` / `Down` | Move in the current list |
| `Enter` / `Right` | Open the selected route or folder |
| `Backspace` | Return to the parent breadcrumb |
| `Left` | Focus navigation |
| `[` / `]` | Switch provider type |
| `d` | Download the selected file or folder |
| `o` | Open the source selector |
| `r` | Reload the current source or folder |
| `l` | Open runtime logs |
| `g` | Open global settings |
| `Space` | Pause or resume the active job |
| `x` | Stop the active job |
| `q` / `Ctrl-C` | Confirm exit |

Logs support `a/i/e` level filters, `f` target filters, and `Tab` between list and detail. Provider,
source, daemon, settings, and download events share one chronological session log.

## Daemon and CLI

Start the long-lived runtime:

```sh
net-pan daemon
```

The TUI attaches to an available daemon; otherwise it uses an embedded runtime. Jobs owned by the
daemon continue after the TUI exits.

Common script commands:

```sh
net-pan status
net-pan providers
net-pan sources google --profile <PROFILE_ID>
net-pan ls google <SOURCE_ID> <OBJECT_ID> --profile <PROFILE_ID>
net-pan stat google <SOURCE_ID> <OBJECT_ID> --profile <PROFILE_ID>
net-pan download google <SOURCE_ID> <OBJECT_ID> /absolute/destination \
  --profile <PROFILE_ID> --workers 2
net-pan job <JOB_ID>
net-pan watch
net-pan pause <JOB_ID>
net-pan resume <JOB_ID>
net-pan cancel <JOB_ID>
```

Provider, source, and object IDs are opaque. Obtain them from preceding JSON responses rather than
parsing provider-specific URLs. An example user service is available at `contrib/net-pan.service`.

## Notifications

The daemon reads `~/.net-pan/notifiers.toml` and supports DingTalk and Feishu webhook notifiers.
Terminal job events are persisted in a SQLite outbox before delivery, retried with bounded backoff,
and delivered idempotently. Restrict this file to mode `0600`.

## Local Data

All runtime data is stored below `~/.net-pan`:

```text
settings.toml
providers/<provider>/<profile>/config.toml
state/state.db
state/net-pan.sock
logs/YYYY-MM-DD/net-pan_HH-mm-ss_NNN.log
notifiers.toml
```

Provider directories use mode `0700`; configuration, token, database, socket, and log files use
private permissions. System `HTTP_PROXY`, `HTTPS_PROXY`, and `NO_PROXY` variables are ignored.
Configure the global or provider proxy in the TUI.

## Build

```sh
git clone https://github.com/rainysteven1/net-pan.git
cd net-pan
cargo build --release
./target/release/net-pan
```

Run project checks with:

```sh
./scripts/check.sh --full
```

Release and packaging details are documented in [docs/releasing.md](docs/releasing.md).

## License

Licensed under MIT. See [LICENSE](LICENSE) and [THIRD_PARTY_NOTICES](THIRD_PARTY_NOTICES).
