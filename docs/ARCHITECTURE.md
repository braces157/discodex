# Architecture

Discodex is a small native Rust application with an event-driven worker and a Windows tray UI.

## Data flow

```text
Supported AI session logs ──┐
                             ├─> provider + event normalization ─> activity state ─> Discord IPC
Focused Windows app ────────┘
                                       │
                                       └─> tray status
```

The important boundary is normalization: raw provider data is reduced to a small set of activity fields before Discord publishing.

## Modules

### `src/main.rs`

Owns application startup and the background worker loop. It coordinates configuration reloads, filesystem notifications, focused-app polling, session selection, Discord retries, and tray status. It also drives the two store paths that are not plain file events: the periodic re-read of rewritten session documents and the polling of the database-backed providers.

### `src/provider.rs`

Defines supported AI providers, display names, executable/window recognition, the kind of store every provider uses, rich session roots, and session-log matching.

### `src/sources/`

One module per harness. Every module turns the records of that harness into normalized events and never forwards conversation content.

- `src/sources/mod.rs` dispatches to the module of a provider, opens provider databases for reading, and reads the columns of a row.
- A module of an append-only harness implements `parse_line`.
- A module of a harness that rewrites a document implements `parse_document` and reports only the records that were not seen before.
- A module of a database-backed harness implements `databases` and `poll`.

### `src/events.rs`

Parses supported provider event formats into normalized events such as task start, thinking, tool activity, completion, and terminal candidates. Tool data is classified into safe human-readable activity labels. It also holds the shared helpers that the modules of `src/sources/` use to classify a tool, to reduce a file path to its basename, to infer a language, and to turn a workspace path into its folder name.

### `src/state.rs`

Maintains per-session state, prioritizing running tools and then recent log activity without considering window focus. It also debounces terminal events and discards stale sessions restored at startup.

### `src/foreground.rs`

Reads the active Windows process/window and returns only provider recognition needed for the compatibility detector.

### `src/watcher.rs`

Implements append-only JSONL tailing and recursive discovery of recent session files.

DeepSeek Harness v4 logs can contain concatenated Zstandard frames. The tailer advances by complete compressed frames, validates checksums, and retries partial trailing writes without replaying earlier events.

### `src/discord.rs`

Owns Discord IPC connection lifecycle and converts the internal presence snapshot into Discord Rich Presence fields.

### `src/tray.rs`

Implements the native Windows notification-area icon and menu.

### `src/config.rs`

Loads and saves `%APPDATA%\Discodex\config.toml` and manages the per-user Windows startup registry entry.

### `src/instance.rs`

Uses a named Windows mutex so only one Discodex process runs per user session.

## Store kinds

Harnesses expose their activity in different shapes, and each shape needs its own ingestion path:

- `SessionStore::LogFiles` – append-only, newline-delimited files that are tailed incrementally.
- `SessionStore::JsonLineSnapshot` – a file that appends a snapshot of the whole conversation for every change. The file is re-read and only new records are applied.
- `SessionStore::JsonFiles` – a document that is rewritten in place. The document is re-read and only new records are applied.
- `SessionStore::Sqlite` – a database that is polled for newly written rows. The session key of a row is the conversation it belongs to, so parallel conversations stay separate.

## Reliability behavior

- Filesystem changes are event-driven through `notify`.
- Rewritten session documents are re-read on every change and on a timer, so a write that lands in the middle of a rewrite is not lost.
- Provider databases are polled with a busy timeout and are never locked for writing.
- Discord reconnects are retried after transient failures.
- Active presence is periodically health-checked.
- Session terminal messages are debounced to avoid clearing presence between closely spaced events.
- Old incomplete sessions are discarded during startup restoration.
- Explorer restarts are handled by re-adding the tray icon.
