# Privacy model

Discodex is designed around a narrow rule: Discord should receive activity metadata, not AI conversation content.

## Data that may appear in Rich Presence

Depending on the provider and detection mode, Discodex may publish:

- AI provider name
- Final workspace folder name
- Inferred language or framework
- Current file basename
- Normalized activity such as `Editing files` or `Running Rust tests`
- High-level phase such as thinking or running tools
- Task start timestamp for elapsed time

## Data intentionally excluded

Discodex does not publish:

- User prompts
- Assistant responses
- Full file paths
- Raw shell commands
- Tool arguments
- Tool output
- Raw session JSON
- Window titles
- Browser URLs
- Account identifiers

The parser converts recognized events into a small internal `PresenceSnapshot`; raw session lines are not sent to Discord.

## Focused-window detection

When enabled, Discodex reads the foreground process executable name and may inspect the active window title to identify a known AI product. Matching happens locally. The title itself is not retained in the presence state or Discord payload.

Disable this layer at any time from the tray menu with **Detect focused AI apps**, or set:

```toml
foreground_detection = false
```

Rich Codex and Claude Code session tracking remains available when focused-app detection is disabled.

## Network behavior

Discodex publishes Rich Presence through Discord's local IPC connection. It does not run its own telemetry service or require a Discodex account.

Provider activity is discovered from local files and local Windows foreground-process information.

## Reviewing the boundary

The main privacy-sensitive areas are:

- `src/events.rs` — converts provider events into normalized metadata.
- `src/foreground.rs` — obtains focused app information.
- `src/provider.rs` — maps local app/window signals to provider names.
- `src/discord.rs` — defines the final Discord activity payload.

Changes that broaden collected data should be treated as privacy-sensitive and reviewed carefully.

