# pan-tui

`pan-tui` is a Rust terminal downloader for cloud-drive folders. The goal is a small TUI that lets
you open saved netdisk sources, browse one level at a time, and download the highlighted item to a
local directory.

Provider scope:

- Google Drive: implemented
- Aliyun Drive: OAuth refresh, user/drive information, sources, file listing/metadata, and downloads implemented

The app is no longer tied to a JuiceFS mount. The default destination is `~/Downloads`; pass
`--destination <PATH>` to download somewhere else.

## Features

- Saved source registry; no link is required on the command line
- `Shared links` virtual level with a visible saved-link count
- Level-by-level Ratatui browser with breadcrumbs and file details
- Download of the highlighted folder or regular file, preserving the saved-root hierarchy
- Streaming writes through same-directory `.part` files
- HTTP Range resume, size validation, `sync_all`, and atomic final rename
- Multi-row transfer progress with per-file and aggregate speeds
- Overall item-count percentage independent of file sizes
- Download form with an editable destination root and 1-5 concurrent file workers
- Loguru-style session logs with date directories and size rotation
- `Files / Logs / Settings` tabs with independent upper/lower panel focus
- Runtime Provider/Profile/Source selector with independent browser sessions per Source
- Schema-driven Provider settings with masked secrets and isolated instance rebuilds
- Safe local names and deterministic handling of duplicate remote names
- Per-file error handling: one failed file does not stop the remaining plan
- In-app proxy settings with live reconnect; system proxy variables are ignored
- Versioned SQLite Job/Transfer history with WAL, batched progress, and interrupted-job recovery
- Optional Unix-socket daemon with TUI attachment, snapshot subscription, and idempotent submission
- DingTalk and Feishu notifier registry with signed webhooks and a durable SQLite outbox

## Requirements

- A recent stable Rust toolchain
- For Google Drive: a Google Cloud project with the Google Drive API enabled
- For Google Drive: an Installed App OAuth client JSON or a service-account JSON key
- Access to the configured source folders for the selected Google account
- A sources JSON file containing one or more saved Google Drive folder links or IDs
- For Aliyun Drive: an OpenAPI application client ID and refresh token

## Google Drive Setup

### Installed App OAuth

1. Enable the Google Drive API in a Google Cloud project.
2. Create an OAuth client with application type `Desktop app`, download its JSON, and install it
   privately:

   ```bash
   install -d -m 700 ~/.config/pan-tui/providers/google
   install -m 600 /path/to/client_secret.json \
     ~/.config/pan-tui/providers/google/oauth-client.json
   ```

3. Configure `~/.config/pan-tui/providers/google.toml`:

   ```toml
   enabled = true
   auth_mode = "installed_app"
   credentials_file = "google/oauth-client.json"
   token_file = "google/oauth-token.json"
   sources_file = "google/sources.json"
   ```

4. Create the sources file as shown below, then complete the first login:

   ```bash
   pan-tui auth login google
   pan-tui auth status google
   ```

The login command prints Google's authorization URL and listens on a temporary
`127.0.0.1` callback port for up to five minutes. It uses Authorization Code + PKCE and persists
the access/refresh token state atomically with mode `0600`. Refresh-token rotation is saved before
the refreshed access token is returned to callers. Use `pan-tui auth revoke google` to remove the
local token state.

### Service Account

1. Enable the Google Drive API in the service account's Google Cloud project.
2. Create and download a JSON key for the service account.
3. In Google Drive, share the source folder with the JSON file's `client_email` as a viewer.
4. Store the key outside this repository and restrict its permissions:

   ```bash
   install -d -m 700 ~/.config/pan-tui/providers/google
   install -m 600 /path/to/downloaded-key.json \
     ~/.config/pan-tui/providers/google/service-account.json
   ```

5. Save the shared folder links in `~/.config/pan-tui/providers/google/sources.json`:

   ```json
   {
     "sources": [
       "https://drive.google.com/drive/folders/FOLDER_ID",
       "ANOTHER_FOLDER_ID"
     ]
   }
   ```

   ```bash
   chmod 600 ~/.config/pan-tui/providers/google/sources.json
   ```

6. Create `~/.config/pan-tui/providers/google.toml` with mode `0600`:

   ```toml
   enabled = true
   auth_mode = "service_account"
   credentials_file = "google/service-account.json"
   sources_file = "google/sources.json"
   ```

CLI/environment overrides select Service Account mode. The application resolves those Google
credentials in this order:

1. `--credentials <PATH>`
2. `GOOGLE_APPLICATION_CREDENTIALS`
3. `~/.config/pan-tui/providers/google/service-account.json`

It resolves the sources file in this order:

1. `--sources <PATH>`
2. `PAN_TUI_SOURCES`
3. `~/.config/pan-tui/providers/google/sources.json`

The legacy top-level files remain readable and are not deleted automatically.

Duplicate folder IDs are collapsed. Each saved source is validated at startup and must currently be
a Google Drive folder that the service account can read.

Do not commit the JSON key. Access tokens and private-key material are never written to logs or to
the download destination.

## Aliyun Drive Setup

Create `~/.config/pan-tui/providers/aliyun.toml` with mode `0600`:

```toml
enabled = true
client_id = "YOUR_OPENAPI_CLIENT_ID"
refresh_token = "YOUR_INITIAL_REFRESH_TOKEN"
drive = "default"
root_file_id = "root"

[[sources]]
id = "main"
name = "Aliyun Drive"
drive = "default"
root_file_id = "root"
```

The provider uses the personal-drive OpenAPI at `https://openapi.alipan.com`. Rotated access and
refresh tokens are atomically persisted to `~/.config/pan-tui/providers/aliyun/token.json` with
mode `0600`. Signed download URLs and token response bodies are not written to logs.

Folder creation, upload, delete, and asynchronous task operations are part of the common Provider
capability surface but are not implemented by the current Aliyun adapter.

## Build and Run

Build a release binary:

```bash
cargo build --release
```

Start the interactive browser with the default destination:

```bash
cargo run --release
```

Choose explicit configuration files and a destination:

```bash
cargo run --release -- \
  --provider google \
  --credentials ~/.config/pan-tui/service-account.json \
  --sources ~/.config/pan-tui/sources.json \
  --destination ~/Downloads
```

List the saved folder links and their count without opening the TUI or downloading anything:

```bash
cargo run --release -- --list
```

Use `--provider aliyun` to select Aliyun Drive for `--list` or as the initial TUI provider. When the
flag is omitted, Google is preferred when ready; otherwise the first ready provider is used.
The TUI still starts when no Provider is ready and opens `Settings` with `NeedsConfig`,
`ConfigError`, or `ConnectionError` status. One failed Provider does not block another.

Use `--overwrite` to replace completed destination files. Existing files are skipped by default.
Use `--no-resume` to restart `.part` files instead of sending a Range request. Run
`cargo run --release -- --help` for the complete command reference.

## Optional Daemon

Start the long-lived runtime:

```bash
pan-tui daemon
```

Query or cancel Jobs through the private local Unix socket:

```bash
pan-tui status
pan-tui cancel 42
```

Provider browsing and script submission use the same daemon-owned Provider instances as the TUI:

```bash
pan-tui providers
pan-tui sources google
pan-tui ls google SOURCE_ID FOLDER_OBJECT_ID
pan-tui stat google SOURCE_ID OBJECT_ID
pan-tui download google SOURCE_ID OBJECT_ID /absolute/local/destination --workers 2
pan-tui job 42
pan-tui watch
```

These commands emit JSON (`watch` emits JSON Lines). Provider, Source, and object IDs are opaque;
obtain them from the preceding command instead of parsing provider-specific URLs in scripts.

The daemon owns the SQLite state, resumes interrupted Jobs when their Provider is available, and
removes its `0600` socket on graceful shutdown. When the socket exists, the TUI validates the
daemon, subscribes to complete Job snapshots, and submits downloads through IPC. Closing the TUI
does not stop those Jobs. When no socket exists, the TUI uses an embedded foreground runtime. An
existing but unusable socket is reported as an error instead of opening the same SQLite database
from a second runtime.

IPC protocol v3 carries provider-neutral catalog, Source, `RemoteRef`, masked settings, and Job DTOs.
It persists idempotency mappings for `client_id + request_id` in the same transaction
as the Job. A repeated matching request returns the original Job ID; reusing the key with different
request JSON is rejected. Subscription startup always sends a complete snapshot, including after a
daemon restart where the in-memory revision counter starts over. `pan-tui submit <REQUEST.json>` is
the low-level IPC/debug client and accepts an optional `--request-id`.

The binary is only the CLI adapter for this path. `DaemonService` owns Provider instances, recovery,
the download runtime, notification worker, and socket lifecycle. TUI and CLI use `DaemonPanClient`;
without a socket the TUI swaps in `EmbeddedPanClient`. Both implement the same `PanClient`
contract, and only the IPC module constructs versioned wire requests.

An example user service is available at `contrib/pan-tui.service`. Install the binary at
`~/.local/bin/pan-tui`, place the unit under `~/.config/systemd/user/`, then enable it with
`systemctl --user enable --now pan-tui.service`.

## Notifications

The daemon loads `~/.config/pan-tui/notifiers.toml`. Restrict it to mode `0600`:

```toml
[notifiers.dingtalk]
enabled = true
webhook = "https://oapi.dingtalk.com/robot/send?access_token=REDACTED"
secret = "SEC_REDACTED"

[notifiers.feishu]
enabled = false
webhook = "https://open.feishu.cn/open-apis/bot/v2/hook/REDACTED"
secret = "REDACTED"
```

Job completion, partial failure, and fatal failure are inserted into the SQLite outbox in the same
transaction as the terminal Job state. The daemon honors `Retry-After`, retries transient failures
with bounded exponential backoff, and moves permanent or exhausted failures to DeadLetter. Webhook
URLs, secrets, signatures, and signed download URLs are not written to Job history or logs.

## Browser Keys

### Global Navigation

| Key | Action |
|---|---|
| `[` / `]` | Switch among `Files`, `Logs`, and `Settings` |
| `Tab` / `Shift-Tab` | Switch the upper/lower panel inside the current tab |
| `p` | Open the Provider/Profile/Source selector |
| `q` / `Ctrl-C` | Open the shared exit confirmation |

Inside the selector, use `Left` / `Right` to choose a Provider/Profile, `Up` / `Down` to choose a
Source, `Enter` to activate it, and `Esc` to cancel. Each Source restores its own directory stack,
selected item, list offset, and last status/error. Switching does not replace the Provider instance
held by an active download job.

### `Shared links` level

| Key | Action |
|---|---|
| `Up` / `k` | Select the previous item |
| `Down` / `j` | Select the next item |
| `PageUp` / `PageDown` | Move by one page |
| `Home` / `End` | Select the first or last item |
| `Enter` | Open the selected saved link |
| `d` | Download the highlighted shared folder recursively |
| `r` | Reload the sources file and saved-link metadata |

### Real folder level

| Key | Action |
|---|---|
| `Up` / `k`, `Down` / `j` | Move the selection |
| `PageUp` / `PageDown`, `Home` / `End` | Move by a page or jump to an edge |
| `Enter` / `Right` | Open the highlighted child folder; hidden for other item types |
| `Backspace` / `Left` | Return to the parent level |
| `r` | Reload the current folder's direct children |
| `d` | Download the highlighted folder or downloadable regular file |

### Download Form

| Key | Action |
|---|---|
| `Up` / `Down` | Switch between destination and worker fields |
| Text, `Backspace`, `Delete`, `Left`, `Right`, `Home`, `End` | Edit the destination root |
| `Left` / `Right`, `-` / `+`, `1`-`5` | Adjust concurrent file workers |
| `Enter` | Validate the form and start the download |
| `Esc` | Cancel the form |
| `q` / `Ctrl-C` | Open the shared exit confirmation |

The destination root defaults to the CLI destination (`~/Downloads` unless overridden) and must be
an absolute path. The target preview combines that root with the complete path from the saved source
root to the highlighted item. File workers default to `1` and are clamped to `1`-`5`.

### Logs Tab

| Key | Action |
|---|---|
| `Up` / `k`, `Down` / `j` | Scroll one log entry |
| `PageUp` / `PageDown` | Scroll one page |
| `Home` / `End` | Jump to the first or latest filtered entry |
| `a` | Show all levels |
| `i` | Show `INFO` entries |
| `e` | Show `ERROR` entries |

## Download Behavior

After confirmation, the application recursively scans a highlighted folder or directly builds a
single-file plan for a highlighted regular file. It does not write files if plan creation fails.
Downloads run sequentially with the default single worker. With more than one worker, files are
consumed concurrently from one bounded internal queue.

Google Docs, Sheets, Slides, and other Google-native files require an export format and are skipped
in this version. Items whose owner disabled downloading are also skipped.

When interrupted, a partially downloaded file remains as `<name>.part`. The next default run
requests the remaining bytes and validates the returned `Content-Range`. A completed stream is
size-checked when the provider reports a size, synchronized to storage, and renamed to the final
path.

## Local State And Logs

Persistent runtime files share one `pan-tui` state root:

```text
~/.local/state/pan-tui/state.db
~/.local/state/pan-tui/state.db-wal
~/.local/state/pan-tui/state.db-shm
~/.local/state/pan-tui/client-id
~/.local/state/pan-tui/logs/
```

`XDG_STATE_HOME` replaces `~/.local/state` when set. The directory is restricted to mode `0700`;
the database, stable IPC client ID, socket, and log files use private permissions. Provider and
Notifier configuration remains under `~/.config/pan-tui` and is not stored in SQLite.

Each process writes immediately flushed, Loguru-style lines under:

```text
~/.local/state/pan-tui/logs/YYYY-MM-DD/pan-tui_HH-mm-ss_000.log
```

Files rotate at 10 MiB by incrementing the final number. Planning, file starts, completed files,
skipped files, and summaries use `INFO`; failed files and fatal errors use `ERROR`.

## Network Settings

Open the `Settings` tab with `[` / `]`, focus the upper panel, and press `Enter` to edit the
application proxy. Enter an absolute HTTP or HTTPS proxy URL, then press `Enter` to save and
reconnect. Clear the field to connect directly. `Esc` stops editing.

The setting is stored with restricted permissions in `~/.config/pan-tui/config.toml`:

```toml
[network]
proxy = "http://127.0.0.1:7890"
```

OAuth and provider API/download clients use this setting. The application explicitly ignores
`HTTP_PROXY`, `HTTPS_PROXY`, and `NO_PROXY`; when `proxy` is absent or blank, requests connect
directly.

Focus the lower Settings panel with `Tab` to edit fields supplied by the current
`ProviderFactory::settings_schema()`. Use `Up` / `Down` to choose a field, `Enter` to edit and save,
and `Space` to toggle booleans. Secret values are always masked; leaving an existing secret blank
keeps it unchanged. Saving rebuilds only that Provider. If reconnect fails, the previous instance
and edited input remain available.

## Known Limits

- Aliyun behavior is covered by a controllable mock server; real-account browse/download/resume acceptance remains required.
- Google service-account listing, small-file download, and `.part` Range resume were rerun successfully.
- Google Installed App OAuth is implemented and mock-tested; real first-login acceptance requires a Desktop OAuth client JSON.
- Native Aliyun first login, notifier Settings UI, and full daemon/PTY acceptance remain Phase 21-26 work.

## Development Checks

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

The architecture and gated implementation record are maintained in [DESIGN.md](DESIGN.md) and
[TODO.md](TODO.md). Those two planning documents are intentionally written in Chinese.
