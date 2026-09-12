# Configuration

Discodex stores per-user settings at:

```text
%APPDATA%\Discodex\config.toml
```

The file is created automatically on first launch and reloaded when it changes.

## Default configuration

```toml
discord_application_id = ""
enabled = true
start_with_windows = false
foreground_detection = true
```

## Settings

### `discord_application_id`

Optional runtime override for the Discord Developer application ID used by Rich Presence.

- Empty: use the ID embedded in the executable at build time.
- Numeric value: override the embedded ID for this machine.

Discord application IDs are public identifiers. Do not put bot tokens or OAuth client secrets in this field.

### `enabled`

Controls whether Discodex publishes activity to Discord.

- `true`: presence is enabled.
- `false`: presence is cleared and no activity is published.

The tray menu can toggle this setting.

### `start_with_windows`

Controls per-user startup registration.

- `true`: Discodex starts when the current Windows user signs in.
- `false`: the startup entry is removed.

The tray menu can toggle this setting.

### `foreground_detection`

Controls the compatibility detector for AI apps, browser tabs, and IDE panels.

- `true`: inspect the focused executable and locally match known product names in its window title.
- `false`: disable focused-app detection while keeping rich Codex and Claude Code session tracking active.

No window title is stored or sent to Discord.

## Build-time application ID

Release builders can provide an application ID through:

```powershell
$env:DISCODEX_DISCORD_APPLICATION_ID = "YOUR_APPLICATION_ID"
cargo build --release
```

If the environment variable is absent, the build script can use a valid `discord_application_id` from the local runtime config. Otherwise it falls back to the repository's default shared application ID.

At runtime, a non-empty config value always takes precedence over the embedded value.

