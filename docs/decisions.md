# Architecture Decisions

## 2026-08-05: Compile-time provider registry

- Decision: register provider factories in the Rust composition root instead of loading dynamic libraries.
- Basis: v3 design and codebase review.
- Reason: Aliyun and Google are the initial built-in providers; a trait-object registry provides isolation and testability without introducing an unstable plugin ABI.

## 2026-08-05: Provider-neutral download stream

- Decision: providers normalize remote responses into `Full`, `Partial`, or `AlreadyComplete` streams.
- Basis: existing Google Range handling and the planned Aliyun adapter.
- Reason: HTTP status codes and provider-specific download URLs stay inside adapters, while `.part` files, resume validation, progress, and atomic completion remain shared.

## 2026-08-05: Incremental Google migration

- Decision: add the provider contract beside the existing `DriveClient` before routing production behavior through it.
- Basis: Phase 12 has a passing 33-test regression baseline.
- Reason: the abstraction can be tested with a fake provider before changing authenticated browsing and download behavior.

## 2026-08-05: Opaque provider configuration storage

- Decision: the core stores provider TOML and private state as opaque bytes; each provider factory owns parsing and validation.
- Basis: v3 configuration boundary and Phase 15 implementation.
- Reason: global configuration stays provider-neutral, while file permissions, atomic creation, and path isolation remain consistently enforced by shared code.

## 2026-08-05: Explicit legacy Google migration

- Decision: legacy `service-account.json` and `sources.json` remain readable until the user or settings flow explicitly creates `providers/google.toml`.
- Basis: Phase 15 migration requirements.
- Reason: automatic file moves would be destructive and make rollback unclear; an idempotent generated config can reference the existing files without modifying them.
- Compatibility: discovery also checks the pre-rename `~/.config/gdrive-juicefs` directory before reporting that Google needs configuration.

## 2026-08-05: Download opening receives remote metadata

- Decision: `CloudProvider::open_download` receives `RemoteItem` instead of only `RemoteRef`.
- Basis: Google 416 handling and provider-neutral resume semantics.
- Reason: validating an already-complete partial file requires the known remote size and download capability; refetching metadata for every transfer would add latency and new failure points.

## 2026-08-05: Aliyun Drive uses the personal-drive OpenAPI

- Decision: implement the Aliyun provider against `https://openapi.alipan.com`, using `/oauth/access_token`, `/adrive/v1.0/user/getDriveInfo`, `/adrive/v1.0/openFile/list`, `/adrive/v1.0/openFile/get`, and `/adrive/v1.0/openFile/getDownloadUrl`.
- Basis: Alibaba PDS documentation, the Aliyun Drive developer portal references found through AnySearch, and current AList/tickstep OpenAPI implementations.
- Reason: these are the OAuth endpoints for personal Aliyun Drive applications. The Alibaba Cloud PDS `/v2/file/*` APIs describe the underlying service but are not a drop-in authentication model for a personal-drive client.
- Boundary: only an app's own `client_id`, `client_secret`, and OpenAPI refresh token are accepted. Hosted third-party token brokers and browser/mobile private APIs are out of scope.
- Dependency: do not depend on the pre-1.0 `alipan` crate. Implement only OAuth refresh, user/drive information, file list/metadata, and download URL handling locally; use `alipan-rs`, AList, and tickstep only as compatibility references after official documentation and controlled responses.

## 2026-08-05: Aliyun token rotation and download URLs

- Decision: persist each successful rotated refresh token in the provider-private state file before exposing the refreshed access token to later requests. Refresh once and retry when the API reports `AccessTokenInvalid`, `AccessTokenExpired`, or `I400JD`.
- Decision: request a fresh signed URL for every `open_download` call and apply the local Range offset to that URL. A URL-expiry retry obtains another URL without changing `JobId`, `TransferId`, or the `.part` path.
- Basis: official OAuth refresh semantics, official PDS download URL documentation, and current OpenAPI implementations.
- Reason: refresh tokens rotate and signed download URLs are short-lived. Both values are provider state, while transfer identity and local recovery remain core state.
- Persistence: when a refresh response omits a new refresh token, retain the current token; when it returns a non-empty replacement, atomically persist the replacement with mode `0600` before exposing the new access token.

## 2026-08-05: Conservative Aliyun rate handling

- Decision: serialize download-URL creation and keep metadata requests bounded; classify HTTP 429 and `TooManyRequests` as retryable rate-limit errors and honor `Retry-After` when present.
- Basis: the OpenAPI publishes dynamic limits and current clients use conservative local limiters, but no stable official numeric quota was found.
- Reason: third-party request-per-second constants must not be represented as an official guarantee.

## 2026-08-05: Capability-oriented provider operations

- Decision: expose user info, drive info, single-item metadata, folder creation, upload, delete, and async task status through one provider-neutral operation surface, while each method defaults to `Unsupported`.
- Decision: keep login on `ProviderFactory`; authentication creates or refreshes a provider instance and is not a remote-file operation on an already connected provider.
- Reason: Google and Aliyun do not have identical operation sets. A shared capability contract lets the TUI render real support and reasons without provider-ID dispatch or fake implementations.

## 2026-08-05: Factory-owned Provider settings

- Decision: `ProviderFactory` owns its settings schema, masked snapshot, validation, and persistence. The TUI only edits schema fields and never parses Google or Aliyun TOML.
- Decision: snapshots never return secret values. They return a configured-secret marker, and a blank secret edit means retain the existing value.
- Decision: saving Provider settings rebuilds only the selected Provider. A reconnect failure records `ConfigError` or `ConnectionError` while retaining the previous instance and current form input.
- Reason: Provider-private configuration remains isolated, secrets do not cross into generic UI state, and an active download keeps the `Arc<dyn CloudProvider>` captured when its job started.

## 2026-08-05: Optional daemon, transient TUI

- Decision: the TUI and CLI remain transient clients; a separate optional daemon owns long-running jobs, persistence, recovery, and notification delivery.
- Decision: the same binary exposes daemon and client subcommands instead of shipping an always-running TUI process.
- Basis: downloads may outlive a terminal or SSH session, while one-off users should not be forced to install a background service.
- Reason: this separates user-interface lifetime from Job lifetime and gives DingTalk/Feishu delivery a reliable process and SQLite outbox without changing the foreground workflow.

## 2026-08-05: Aliyun enterprise native mode and personal bridge

- Decision: direct Aliyun OpenAPI login requires an already approved enterprise application and remains a native Provider mode.
- Decision: personal users use a separately identified AList/OpenList Bridge Provider; `pan-tui` will not call AList's hosted OAuth token endpoint or present AList-issued credentials as its own.
- Basis: Aliyun Drive stopped accepting new personal developer applications on 2025-07-01, and AList source shows that users without a client ID send refresh tokens to AList's hosted broker.
- Reason: direct reuse would add third-party credential custody, shared quota, availability, and application-policy dependencies. A bridge keeps that trust decision explicit and leaves Aliyun tokens outside `pan-tui`.

## 2026-08-05: SQLite before daemon and notifications

- Decision: detach DownloadJob ownership from Ratatui first, then add SQLite persistence, then local IPC, and only then reliable notifier delivery.
- Decision: use a versioned SQLite schema in WAL mode with a transactional notification outbox; credentials and signed download URLs never enter the database.
- Reason: daemon and notifier correctness depend on stable Job identity, restart recovery, and atomic terminal-state recording. Implementing IPC or webhooks first would duplicate state and failure semantics.

## 2026-08-05: Snapshot-based Job observation

- Decision: `DownloadManager` maintains serializable Job and Transfer snapshots in addition to a bounded event stream.
- Decision: worker events use a bounded channel, while TUI/IPC observers use non-blocking broadcast and snapshot re-query after disconnect or lag.
- Reason: event history is not a durable state model. Snapshots let the TUI, CLI, and daemon reconnect without stopping workers or requiring every progress event to be retained.

## 2026-08-05: Versioned local IPC

- Decision: Linux daemon clients use a `0600` Unix domain socket and newline-delimited, versioned JSON messages containing only public Job requests, snapshots, IDs, and classified errors.
- Decision: the daemon is the SQLite writer while active; foreground TUI remains an embedded fallback until daemon attachment is completed.
- Reason: a local protocol keeps trait objects, Provider credentials, and signed URLs out of the wire contract and permits CLI/TUI evolution behind an explicit protocol version.

## 2026-08-05: Transactional notification outbox

- Decision: terminal Job state and one idempotent outbox row per enabled notifier are committed in the same SQLite transaction.
- Decision: DingTalk and Feishu are compile-time registered notifiers with schema-owned webhook/secret configuration, signed requests, `Retry-After`, bounded exponential retry, and DeadLetter state.
- Reason: direct webhook calls from download workers lose notifications on process exit and couple Provider behavior to messaging services. The outbox makes notification delivery recoverable and independently retryable.

## 2026-08-05: PanClient is the TUI and CLI application boundary

- Decision: TUI and script-oriented CLI commands depend only on `PanClient`; they never hold `CloudProvider`, `ProviderFactory`, `DownloadManager`, or `JobBackend`.
- Decision: `DaemonPanClient` forwards provider browsing, settings, jobs, cancellation, and subscriptions over IPC v3. `EmbeddedPanClient` implements the same contract with an in-process `ProviderRuntime` and `DownloadManager`.
- Decision: in resident mode the daemon is the only owner of Provider instances and Provider runtime state. Saving settings rebuilds only the selected daemon-owned Provider, and a failed rebuild retains the previous instance.
- Reason: a TUI-owned Provider alongside a daemon-owned download runtime creates two divergent connection/configuration states. One client contract keeps TUI, CLI, embedded use, and resident use behaviorally aligned while leaving purely visual state local to Ratatui.
- Wire boundary: IPC uses serializable provider-neutral catalog, settings, `RemoteRef`, `RemoteItem`, `RemoteSource`, and Job DTOs. Credentials, trait objects, signed URLs, webhook secrets, and raw Provider responses never cross the socket.
