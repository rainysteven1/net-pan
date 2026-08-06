# net-pan

`net-pan` is a Rust terminal downloader for cloud-drive folders. The goal is a small TUI that lets
you open saved netdisk sources, browse one level at a time, and download the highlighted item to a
local directory.

Provider scope:

- Google Drive: implemented
- Aliyun Drive: OAuth refresh, user/drive information, sources, file listing/metadata, and downloads implemented

The app is no longer tied to a JuiceFS mount. The default destination is `~/Downloads`; pass
`--destination <PATH>` to download somewhere else.

## Features

- Saved source registry; no link is required on the command line
- `Sources` virtual entry level with a visible source count
- Level-by-level Ratatui browser with middle-collapsed breadcrumbs
- Download of the highlighted folder or regular file, preserving the saved-root hierarchy
- Streaming writes through same-directory `.part` files
- HTTP Range resume, size validation, `sync_all`, and atomic final rename
- Multi-row transfer progress with per-file and aggregate speeds
- Overall item-count percentage independent of file sizes
- One validated global download root plus a read-only target preview and 1-5 concurrent file workers
- Loguru-style session logs with date directories and size rotation
- Left-side `Accounts / Files / Transfers / Logs / Settings` navigation
- Top-shell destination and a cc-switch-style contextual shortcut bar
- Header Provider Type switching, custom Account names, and independent browser sessions per Source
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

Every Provider Account has a generated stable ID such as `p_zr6bktv5`. The user-facing Account
name is editable and does not affect paths or existing Jobs. Account files are isolated under:

```text
~/.net-pan/providers/google/<PROFILE_ID>/
```

Start the TUI, open `Accounts`, and press `n` to create another Google Account. The initial run also
creates one disabled draft Account for each built-in Provider.

### Installed App OAuth

1. Enable the Google Drive API in a Google Cloud project.
2. Create an OAuth client with application type `Desktop app`, download its JSON, and install it
   privately:

   ```bash
   install -d -m 700 ~/.net-pan/providers/google/PROFILE_ID
   install -m 600 /path/to/client_secret.json \
     ~/.net-pan/providers/google/PROFILE_ID/oauth-client.json
   ```

3. Configure `~/.net-pan/providers/google/PROFILE_ID/config.toml`:

   ```toml
   name = "Personal Google"
   enabled = true
   auth_mode = "installed_app"
   credentials_file = "oauth-client.json"
   token_file = "token.json"
   sources_file = "sources.json"
   ```

4. Create the sources file as shown below, then complete the first login:

   ```bash
   net-pan auth login google PROFILE_ID
   net-pan auth status google --profile PROFILE_ID
   ```

The login command prints Google's authorization URL and listens on a temporary
`127.0.0.1` callback port for up to five minutes. It uses Authorization Code + PKCE and persists
the access/refresh token state atomically with mode `0600`. Refresh-token rotation is saved before
the refreshed access token is returned to callers. Use
`net-pan auth revoke google PROFILE_ID` to remove that Account's local token state.

### Service Account

1. Enable the Google Drive API in the service account's Google Cloud project.
2. Create and download a JSON key for the service account.
3. In Google Drive, share the source folder with the JSON file's `client_email` as a viewer.
4. Store the key outside this repository and restrict its permissions:

   ```bash
   install -d -m 700 ~/.net-pan/providers/google/PROFILE_ID
   install -m 600 /path/to/downloaded-key.json \
     ~/.net-pan/providers/google/PROFILE_ID/service-account.json
   ```

5. Save the shared folder links in `~/.net-pan/providers/google/PROFILE_ID/sources.json`:

   ```json
   {
     "sources": [
       "https://drive.google.com/drive/folders/FOLDER_ID",
       "ANOTHER_FOLDER_ID"
     ]
   }
   ```

   ```bash
   chmod 600 ~/.net-pan/providers/google/PROFILE_ID/sources.json
   ```

6. Create `~/.net-pan/providers/google/PROFILE_ID/config.toml` with mode `0600`:

   ```toml
   name = "Work Google"
   enabled = true
   auth_mode = "service_account"
   credentials_file = "service-account.json"
   token_file = "token.json"
   sources_file = "sources.json"
   ```

Google configuration is loaded only from the selected Account directory. Relative credential,
token, and sources paths are resolved inside that directory.

Duplicate folder IDs are collapsed. Each saved source is validated at startup and must currently be
a Google Drive folder that the service account can read.

Do not commit the JSON key. Access tokens and private-key material are never written to logs or to
the download destination.

## Aliyun Drive Setup

Create an Aliyun Account in the TUI, then edit
`~/.net-pan/providers/aliyun/PROFILE_ID/config.toml` with mode `0600`:

```toml
name = "Personal Aliyun"
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
refresh tokens are atomically persisted to the same Account directory as `token.json` with
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

Choose an initial Provider and destination:

```bash
cargo run --release -- \
  --provider google \
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
net-pan daemon
```

Query, pause, resume, or stop Jobs through the private local Unix socket:

```bash
net-pan status
net-pan pause 42
net-pan resume 42
net-pan cancel 42
```

Provider browsing and script submission use the same daemon-owned Provider instances as the TUI:

```bash
net-pan providers
net-pan sources google --profile PROFILE_ID
net-pan ls google SOURCE_ID FOLDER_OBJECT_ID --profile PROFILE_ID
net-pan stat google SOURCE_ID OBJECT_ID --profile PROFILE_ID
net-pan download google SOURCE_ID OBJECT_ID /absolute/local/destination \
  --profile PROFILE_ID --workers 2
net-pan job 42
net-pan watch
```

These commands emit JSON (`watch` emits JSON Lines). Provider, Source, and object IDs are opaque;
obtain them from the preceding command instead of parsing provider-specific URLs in scripts.

The daemon owns the SQLite state, resumes interrupted Jobs when their Provider is available, and
removes its `0600` socket on graceful shutdown. When the socket exists, the TUI validates the
daemon, subscribes to complete Job snapshots, and submits downloads through IPC. Closing the TUI
does not stop those Jobs. When no socket exists, the TUI uses an embedded foreground runtime. An
existing but unusable socket is reported as an error instead of opening the same SQLite database
from a second runtime.

IPC protocol v5 carries provider-neutral Account catalog, Source, `RemoteRef`, masked settings, Job
DTOs, and pause/resume controls.
It persists idempotency mappings for `client_id + request_id` in the same transaction
as the Job. A repeated matching request returns the original Job ID; reusing the key with different
request JSON is rejected. Subscription startup always sends a complete snapshot, including after a
daemon restart where the in-memory revision counter starts over. `net-pan submit <REQUEST.json>` is
the low-level IPC/debug client and accepts an optional `--request-id`.

The binary is only the CLI adapter for this path. `DaemonService` owns Provider instances, recovery,
the download runtime, notification worker, and socket lifecycle. TUI and CLI use `DaemonPanClient`;
without a socket the TUI swaps in `EmbeddedPanClient`. Both implement the same `PanClient`
contract, and only the IPC module constructs versioned wire requests.

An example user service is available at `contrib/net-pan.service`. Install the binary at
`~/.local/bin/net-pan`, place the unit under `~/.config/systemd/user/`, then enable it with
`systemctl --user enable --now net-pan.service`.

## Notifications

The daemon loads `~/.net-pan/notifiers.toml`. Restrict it to mode `0600`:

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
| `[` / `]` | Switch Provider Type while restoring its last active Account |
| `Left` / `Right` | Move between Navigation and the current page |
| `Up` / `Down`, then `Enter` | Select and open a Navigation route |
| `Tab` / `Shift-Tab` | Switch subpanes only on pages that contain multiple panes |
| `g` | Open the separate Global Settings layer |
| `Backspace` | Return from Global Settings or the current breadcrumb level |
| `p` | Open the current Account's Source selector from Files |
| `q` / `Ctrl-C` | Open the shared exit confirmation |

The left Navigation contains `Accounts`, `Files`, `Transfers`, `Logs`, and `Settings`. On Accounts,
use `Up` / `Down` to select an Account, `Enter` to open `Accounts / <account>` in place, `Space` to
activate it, and `a` to create one with a custom name. `Backspace` returns from Account
details to the list. The Settings route always edits the current Provider Account. Inside the Source
selector, use `Up` / `Down`, `Enter`, and `Esc`. Each Source restores its own directory stack,
selected item, list offset, and last status/error. Switching does not replace the Provider instance
held by an active download job.

### `Sources` level

| Key | Action |
|---|---|
| `Up` / `k` | Select the previous item |
| `Down` / `j` | Select the next item |
| `PageUp` / `PageDown` | Move by one page |
| `Home` / `End` | Select the first or last item |
| `Enter` | Open the selected saved link |
| `d` | Download the highlighted source folder recursively |
| `r` | Reload source metadata |

### Real folder level

| Key | Action |
|---|---|
| `Up` / `k`, `Down` / `j` | Move the selection |
| `PageUp` / `PageDown`, `Home` / `End` | Move by a page or jump to an edge |
| `Enter` / `Right` | Open the highlighted child folder; hidden for other item types |
| `Backspace` | Return to the parent level |
| `r` | Reload the current folder's direct children |
| `d` | Download the highlighted folder or downloadable regular file |

### Download Form

| Key | Action |
|---|---|
| `Left` / `Right`, `-` / `+`, `1`-`5` | Adjust concurrent file workers |
| `Enter` | Validate the global root and start the download |
| `Esc` | Cancel the form |
| `q` / `Ctrl-C` | Open the shared exit confirmation |

The destination is the single global download root configured in the upper Settings panel. It must
be an existing absolute directory and cannot be a symbolic link. The form shows it read-only and
combines it with the complete path from the saved source root to the highlighted item. A selected
folder is downloaded as a complete tree below that root. File workers default to `1` and are
clamped to `1`-`5`.

### Transfers

Each row renders a width-aware combined progress value such as
`[############--------] 60% 140.1MB/233.5MB`. Narrow layouts shorten the bar and hide the separate
speed column before truncating the percentage or byte counts. Use `Space` to pause or resume the
current Job and `x` to stop it. Pausing retains `.part` files and the original Job ID; stopping is a
manual failure shown as `STOP` and written at `ERROR` level. The selected Transfer's complete path
and error chain are shown in the Detail area instead of replacing its progress cell.

### Logs Tab

| Key | Action |
|---|---|
| `Up` / `k`, `Down` / `j` | Scroll one log entry |
| `PageUp` / `PageDown` | Scroll one page |
| `Home` / `End` | Jump to the first or latest filtered entry |
| `a` | Show all levels |
| `i` | Show `INFO` entries |
| `e` | Show `ERROR` entries |
| `Tab` / `Shift-Tab` | Switch between the log list and detail pane |

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

Persistent runtime files share one `net-pan` state root:

```text
~/.net-pan/settings.toml
~/.net-pan/providers/<provider>/<profile>/config.toml
~/.net-pan/state/state.db
~/.net-pan/state/state.db-wal
~/.net-pan/state/state.db-shm
~/.net-pan/state/client-id
~/.net-pan/state/net-pan.sock
~/.net-pan/logs/
```

Directories are restricted to mode `0700`; configuration, credentials, tokens, the database,
stable IPC client ID, socket, and log files use private permissions. Secrets are not stored in
SQLite.

Each process writes immediately flushed, Loguru-style lines under:

```text
~/.net-pan/logs/YYYY-MM-DD/net-pan_HH-mm-ss_000.log
```

Files rotate at 10 MiB by incrementing the final number. Planning, file starts, completed files,
skipped files, and summaries use `INFO`; failed files and fatal errors use `ERROR`.

## Global Settings

Press `g` from any non-editing context to open the separate Global Settings layer. The editable
table contains only `Download path` and `Proxy URL`; connection mode, proxy precedence, and
environment policy are shown separately as read-only information. Use `Up` / `Down` to select a
field, `Enter` to start editing, and `Ctrl-S` to save. The download path must be an
existing absolute real directory; relative, missing, regular-file, and symbolic-link paths are
rejected. Saving it changes the root used by every subsequent Job. Enter an absolute HTTP or HTTPS
proxy URL, or clear it for a direct connection; changing the proxy reconnects Providers. `Esc`
stops editing, and `Backspace` returns from the layer when no field is being edited.
Successful saves keep the Settings page open, leave editing mode, and show a centered confirmation
Toast for two seconds. Validation and write failures keep the field in editing mode.

The setting is stored with restricted permissions in `~/.net-pan/settings.toml`:

```toml
[download]
destination = "/home/user/Downloads"

[network]
proxy = "http://127.0.0.1:7890"
```

OAuth and provider API/download clients use this setting. The application explicitly ignores
`HTTP_PROXY`, `HTTPS_PROXY`, and `NO_PROXY`; when `proxy` is absent or blank, requests connect
directly.

The Settings navigation route displays fields supplied by the current
`ProviderFactory::settings_schema()`. Use `Up` / `Down` to choose a field, `Enter` to start editing,
`Ctrl-S` to save and rebuild, and `Space` to toggle booleans. Secret values are always masked; leaving an existing secret blank
keeps it unchanged. Saving rebuilds only that Provider. If reconnect fails, the previous instance
and edited input remain available. Editable Provider fields and read-only Action/derived fields are
rendered in separate `EDITABLE SETTINGS` and `READ ONLY` sections; selection never enters read-only
rows. Provider saves use the same centered two-second Toast behavior.

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

The TUI shell and interaction hierarchy adapt MIT-licensed layout patterns from
[`cc-switch-cli`](https://github.com/saladday/cc-switch-cli). See
[`THIRD_PARTY_NOTICES`](THIRD_PARTY_NOTICES) for attribution and license terms.
