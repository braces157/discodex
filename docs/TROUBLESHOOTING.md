# Troubleshooting

## Discord shows no activity

Check these in order:

1. Discord Desktop is running. Browser-only Discord does not expose the local Rich Presence IPC endpoint used by Discodex.
2. Right-click Discodex and confirm **Presence Enabled** is checked.
3. Choose **Test Presence**. If the test appears, Discord connectivity is working and the issue is provider detection.
4. If you rely on browser or IDE detection, confirm **Detect focused AI apps** is enabled.
5. Exit and relaunch Discodex after a Discord update if the IPC connection appears stuck.

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

