# Troubleshooting

## Discodex could not start: Access is denied (os error 5)

Older builds treated a failure to update the Windows launch-at-login registry entry as a fatal startup error, even when **Start with Windows** was disabled. Rebuild or update Discodex to get the fix. Discodex now continues running if Windows blocks that optional setting, and only requests registry write access when needed.

If another startup step fails, the error dialog identifies the step. Details are also written to `%APPDATA%\Discodex\discodex.log` when that folder is writable. For a configuration error, check access to `%APPDATA%\Discodex\config.toml`.

## Discord shows no activity

Check these in order:

1. Discord Desktop is running. Browser-only Discord does not expose the local Rich Presence IPC endpoint used by Discodex.
2. Right-click Discodex and confirm **Presence Enabled** is checked.
3. Choose **Test Presence**. If the test appears, Discord connectivity is working and the issue is provider detection.
4. If you rely on browser or IDE detection, confirm **Detect focused AI apps** is enabled.
5. Exit and relaunch Discodex after a Discord update if the IPC connection appears stuck.

## Inspect detection diagnostics

Choose **Diagnostics** from the tray menu. Notepad opens `%APPDATA%\Discodex\diagnostics.txt`. Reopen the menu item to refresh the snapshot.

Check the provider's session access, recognized events, activity observation time, and monitoring state. A missing directory means the configured source was not found. A readable store with no recognized events can be idle or use an unsupported format. Some providers require session activity and deliberately have no focused-app fallback. The report names that policy and shows the current Discord connection or last IPC error.

Activity observation times can reflect history read during startup. They do not always mean the provider performed work at that time.

For a standalone snapshot, run `cargo run -- --diagnostics`. It does not connect to Discord or inspect another running process's IPC connection. Use the tray report for that connection state.

## Codex or Claude Code is not detected

Rich detection currently watches the standard per-user session locations:

- Codex: `%USERPROFILE%\.codex\sessions`
- Claude Code: `%USERPROFILE%\.claude\projects`

If your installation stores sessions elsewhere, open a feature request with the provider version and the non-sensitive directory layout. Do not attach private session contents.

## The wrong provider is detected in a browser or editor

Focused-app recognition is best-effort and depends on executable/window naming. File a bug with:

- The application name and version
- The provider you expected
- The provider Discodex showed

Do not include conversation text or sensitive window titles. A redacted title containing only the product-name portion is enough when needed.

## Presence remains after a task ends

Rich session tracking uses terminal-event debouncing so short gaps between events do not make Discord flicker. If a provider exits without writing a normal completion event, startup freshness checks prevent old sessions from being restored indefinitely.

If stale presence persists during the same run, toggling **Presence Enabled** off and on will clear and republish state.

## Configuration changes do not apply

The config file is:

```text
%APPDATA%\Discodex\config.toml
```

Discodex watches the file for changes. Make sure the TOML syntax is valid and values use the documented types. See [Configuration](CONFIGURATION.md).

## Build errors

Run:

```powershell
rustc --version
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Discodex uses Rust edition 2024 and requires Rust 1.85 or newer.

