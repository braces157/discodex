# Installation

Discodex is a portable Windows application. It does not require an installer, a local web server, or a Discodex account.

## Requirements

- Windows 10 or Windows 11
- Discord Desktop
- At least one supported AI app, editor, or coding agent

Discord Rich Presence uses Discord's local IPC connection, so Discord Desktop must be installed and running for activity to appear.

## Install from a GitHub release

1. Open the [latest GitHub release](https://github.com/braces157/discodex/releases/latest).
2. Download `discodex.exe`.
3. Move it to a location you want to keep, for example `%LOCALAPPDATA%\Programs\Discodex\discodex.exe`.
4. Run the executable.
5. Confirm the Discodex icon appears in the Windows notification area.

The first run creates `%APPDATA%\Discodex\config.toml`.

Windows may show a reputation/SmartScreen warning for unsigned community builds. Verify that the executable came from this repository's release page before running it.

## Start with Windows

Right-click the tray icon and enable **Start with Windows**. Discodex writes a per-user startup entry under the standard Windows `Run` registry key.

Disable the same menu item to remove that entry.

## Build from source

Install Rust 1.85 or newer, then run:

```powershell
git clone https://github.com/braces157/discodex.git
cd discodex
cargo test
cargo build --release
```

The resulting executable is:

```text
target\release\discodex.exe
```

To embed a different Discord application ID in the binary:

```powershell
$env:DISCODEX_DISCORD_APPLICATION_ID = "YOUR_APPLICATION_ID"
cargo build --release
```

The application ID is a public Discord application identifier, not an authentication secret.

## Updating

For portable builds, exit Discodex from the tray menu, replace `discodex.exe` with the newer version, and launch it again. Runtime settings are stored separately in `%APPDATA%\Discodex`, so replacing the executable does not remove your configuration.

