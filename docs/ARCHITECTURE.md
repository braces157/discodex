# Architecture

Discodex is a small native Rust application with an event-driven worker and a Windows tray UI.

## Data flow

```text
Codex / Claude session logs ─┐
                            ├─> provider + event normalization ─> activity state ─> Discord IPC
Focused Windows app ────────┘
                                      │
                                      └─> tray status
```

The important boundary is normalization: raw provider data is reduced to a small set of activity fields before Discord publishing.

## Modules

### `src/main.rs`

Owns application startup and the background worker loop. It coordinates configuration reloads, filesystem notifications, focused-app polling, session selection, Discord retries, and tray status.

### `src/provider.rs`

Defines supported AI providers, display names, executable/window recognition, rich session roots, and session-log matching.

### `src/events.rs`

Parses supported provider event formats into normalized events such as task start, thinking, tool activity, completion, and terminal candidates. Tool data is classified into safe human-readable activity labels.

### `src/state.rs`

Maintains per-session state and selects the most recently active session. It also debounces terminal events and discards stale sessions restored at startup.

### `src/foreground.rs`

Reads the active Windows process/window and returns only provider recognition needed for the compatibility detector.

### `src/watcher.rs`

Implements append-only JSONL tailing and recursive discovery of recent session files.

### `src/discord.rs`

Owns Discord IPC connection lifecycle and converts the internal presence snapshot into Discord Rich Presence fields.

### `src/tray.rs`

Implements the native Windows notification-area icon and menu.

### `src/config.rs`

Loads and saves `%APPDATA%\Discodex\config.toml` and manages the per-user Windows startup registry entry.

### `src/instance.rs`

Uses a named Windows mutex so only one Discodex process runs per user session.

## Reliability behavior

- Filesystem changes are event-driven through `notify`.
- Discord reconnects are retried after transient failures.
- Active presence is periodically health-checked.
- Session terminal messages are debounced to avoid clearing presence between closely spaced events.
- Old incomplete sessions are discarded during startup restoration.
- Explorer restarts are handled by re-adding the tray icon.

