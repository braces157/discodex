# Discodex

**Discord Rich Presence for the AI tools you actually use.**

[![CI](https://github.com/braces157/discodex/actions/workflows/ci.yml/badge.svg)](https://github.com/braces157/discodex/actions/workflows/ci.yml)
[![Windows](https://img.shields.io/badge/platform-Windows-0078D4?logo=windows11&logoColor=white)](https://github.com/braces157/discodex)
[![Rust](https://img.shields.io/badge/built%20with-Rust-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

Discodex is a lightweight Windows tray application that mirrors AI activity into Discord Rich Presence. It combines privacy-conscious session tracking for supported coding agents with focused-app detection for popular AI apps and editors.

It runs quietly in the notification area, reconnects to Discord automatically, and never sends prompts, responses, raw commands, full paths, tool output, or window titles to Discord.

## Highlights

- **Multi-provider support** — Codex, Claude Code, ChatGPT, Claude, Gemini, Cursor, Windsurf, GitHub Copilot, Cline, Roo Code, Continue, Perplexity, Poe, OpenCode, Aider, and more.
- **Rich coding activity** — supported session adapters can show the project, language/framework, current file basename, normalized tool activity, phase, and elapsed time.
- **Broad fallback detection** — focused applications, browser tabs, and IDE panels can still produce useful provider presence when no stable session format is available.
- **Privacy-first design** — sensitive conversation and tool content is deliberately excluded from Discord payloads.
- **Native Windows tray app** — no browser server, account system, Electron runtime, or background web service.
- **Automatic Discord reconnection** — presence recovers when Discord starts or restarts.
- **Windows startup toggle** — enable or disable launch-at-login directly from the tray menu.

## Provider support

Discodex uses two detection layers. Rich session tracking has the most detail; focused-app detection provides broad compatibility without parsing conversations.

| Provider | Detection | Detail level |
| --- | --- | --- |
| OpenAI Codex | Local session JSONL | Rich |
| Claude Code | Local session JSONL | Rich |
| ChatGPT | Focused app/window | Standard |
| Claude | Focused app/window | Standard |
| Google Gemini | Focused app/window | Standard |
| Cursor | Focused app/window | Standard |
| Windsurf | Focused app/window | Standard |
| Google Antigravity | Focused app/window | Standard |
| Trae | Focused app/window | Standard |
| Kiro | Focused app/window | Standard |
| Zed AI | Focused app/window | Standard |
| OpenCode | Focused app/window | Standard |
| Aider | Focused app/window | Standard |
| GitHub Copilot | Focused app/window | Standard |
| Cline | Focused app/window | Standard |
| Roo Code | Focused app/window | Standard |
| Continue | Focused app/window | Standard |
| Perplexity | Focused app/window | Standard |
| Microsoft Copilot | Focused app/window | Standard |
| Poe | Focused app/window | Standard |

See [Provider support](docs/PROVIDERS.md) for how each detection mode works and what data it exposes.

## Install

### Download a release

1. Open the [latest release](https://github.com/braces157/discodex/releases/latest).
2. Download `discodex.exe`.
3. Run it. Discodex appears in the Windows notification area.
4. Keep Discord Desktop running. Discodex reconnects automatically if Discord starts later.

No installer is required. Only one Discodex process runs per Windows session.

### Build from source

Requirements:

- Windows 10 or Windows 11
- Rust 1.85 or newer
- Discord Desktop for Rich Presence output

```powershell
git clone https://github.com/braces157/discodex.git
cd discodex
cargo test
cargo build --release
```

The executable is written to `target\release\discodex.exe`.

For a custom Discord application ID:

```powershell
$env:DISCODEX_DISCORD_APPLICATION_ID = "YOUR_APPLICATION_ID"
cargo build --release
```

## Tray controls

Right-click the Discodex tray icon to access:

- **Presence Enabled** — globally enable or disable Discord presence.
- **Status** — see the current provider/project state.
- **Test Presence** — publish a short test activity.
- **Detect focused AI apps** — toggle the broad app/browser/IDE fallback detector.
- **Start with Windows** — launch Discodex when you sign in.
- **Open Config** — edit the runtime TOML configuration.
- **Exit** — stop Discodex and clear the current activity.

## Configuration

Discodex creates `%APPDATA%\Discodex\config.toml` automatically:

```toml
discord_application_id = ""
enabled = true
start_with_windows = false
foreground_detection = true
```

The runtime `discord_application_id` overrides the application ID embedded at build time. Leave it empty to use the embedded default.

See [Configuration](docs/CONFIGURATION.md) for every setting and build-time behavior.

## Privacy

Discodex is intentionally conservative about what reaches Discord.

**May be sent:** provider name, final workspace folder name, inferred language/framework, current file basename, normalized activity label, phase, and task start time.

**Never sent:** prompts, assistant responses, full file paths, raw shell commands, tool arguments, tool output, session contents, or window titles.

Focused-app detection checks executable names and known product names in the active window title locally. The title itself is neither stored nor included in Rich Presence.

Read the full [Privacy model](docs/PRIVACY.md).

## Documentation

- [Installation](docs/INSTALLATION.md)
- [Configuration](docs/CONFIGURATION.md)
- [Provider support](docs/PROVIDERS.md)
- [Privacy model](docs/PRIVACY.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Troubleshooting](docs/TROUBLESHOOTING.md)
- [Contributing](CONTRIBUTING.md)
- [Security](SECURITY.md)
- [Changelog](CHANGELOG.md)

## Development

Run the same checks used by CI before opening a pull request:

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

The project is intentionally small and native. Provider parsing lives separately from Discord publishing so new detection adapters can be added without weakening the privacy boundary.

## Project status

Discodex is an early project. Provider storage formats and application window behavior can change upstream, so rich adapters may need maintenance over time. Focused-app detection is designed as the compatibility fallback when a provider does not expose a stable local activity format.

