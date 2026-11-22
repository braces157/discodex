# Changelog

All notable user-facing changes to Discodex are documented here.

## 0.3.0 - 2026-11-22

### Added

- Native Windows GPUI Usage Dashboard cockpit (`Compact` tray popup and `Wide` two-column analytics cockpit, toggleable via `w` or `discodex --usage --wide`) tracking local token usage, cache hit rates, cache savings, task time, and API cost estimates across 16 harnesses (`codex`, `claude-code`, `antigravity`, `deepseek-harness`, `gemini-cli`, `qwen-code`, `opencode`, `kilo-code`, `factory-droid`, `copilot-cli`, `junie`, `cline`, `roo-code`, `zed`, `crush`, `aider`).
- Interactive usage charts with `Tokens`/`Cost`/`Calls` metrics, `Bar`/`Area` styles (`c`), `Mix`/`Providers` stacked breakdowns, 24-hour local timeline on `Today`, and hover crosshairs with exact readouts.
- `12-Week` streak calendar and `7×24` `Peak Hours` activity heatmaps, `Token Mix` 5-way composition bar, proportional provider share filter chips, and sortable model rows with click-to-expand breakdown drawers.
- Background tray preloader and metadata-only local cache (`%APPDATA%\Discodex\usage-cache.json`) across all four periods (`Today`, `7d`, `30d`, `All`) for instant popup opens and period switching.
- Bundled official Standard API base-rate pricing catalog (checked `2026-11-19`) with effort-suffixed model lookup and custom `%APPDATA%\Discodex\usage-rates.toml` overrides.
- Local diagnostics (`discodex --diagnostics` and tray action) reporting detected providers, store access, watcher state, and Discord IPC health without exposing paths or session contents.
- Opt-in privacy controls for reasoning headings and visible project/file names.
- Automatic tray icon recovery when Windows Explorer restarts, plus tray lifecycle controls and a console-free launcher.

### Changed

- Updated the Google Antigravity adapter for v2 workspace paths and preserved model variants.
- Updated the Discodex Rich Presence badge and refreshed the website with an interactive Compact/Wide Usage Cockpit preview.

### Fixed

- Attached release diagnostic CLI output to the parent Windows console so `--diagnostics` prints cleanly from release builds.
- Reconciled copied Claude responses to their original session and day before period filtering to prevent double-counting.
- Resolved effort-suffixed model IDs to bundled base rates without overriding exact model entries.
- Deferred Wide/Compact window resize off the click handler stack and clamped popup placement to the active monitor work area.

## 0.2.0 - 2026-10-23

### Fixed

- Discodex starts even when Windows denies access to the optional launch-at-login registry setting. Starting with that option disabled no longer requests registry write access when there is no entry to remove.
- Startup errors identify the failing step, and startup-setting failures are recorded in the local diagnostic log.

### Added

- Rich session tracking and Discord artwork for DeepSeek Harness, including model, reasoning effort, project, file, tool activity, phase, and elapsed time from v4 JSONL/Zstandard logs.
- Rich session tracking for Gemini CLI, Qwen Code, GitHub Copilot CLI, Factory Droid, Kiro, Cursor, Windsurf/Cascade, Junie, Aider, Cline, Roo Code, Continue, Block Goose, Charm Crush, OpenCode, Zed, and Kilo Code.
- Session adapters for harnesses that keep their activity in a rewritten document or in a database, with a polling path for the database-backed providers.
- A live test that checks the adapters against the session files that are installed on the machine (`cargo test -- --ignored live_`).
- One normalised 512px mark per harness in `assets/logos/`, rebuilt by `tools/logos/build.py` from pinned sources with a recorded digest.

### Changed

- Session-backed providers use log activity alone; window focus no longer affects their selection. Focused-app detection is reserved for providers without session tracking.
- Providers that gained a session adapter but keep a useful focused fallback now report the session while it runs and fall back to the focused window otherwise.
- Provider artwork is served from this repository instead of hotlinked vendor sites and third-party icon packages, so a mark cannot disappear or silently change upstream. Every mark now shares one plate geometry, corner radius and optical size.

## 0.1.1 - 2026-09-12

### Fixed

- Restore the Codex Rich Presence artwork so Discord no longer shows a missing-image placeholder.

## 0.1.0 - 2026-09-12

### Added

- Native Windows tray application for Discord Rich Presence.
- Rich local session tracking for OpenAI Codex.
- Rich local session tracking for Claude Code.
- Focused-app detection for popular AI apps, browsers, editors, and IDE extensions.
- Provider-aware Discord presence labels.
- Automatic Discord IPC reconnection and presence health checks.
- Per-user **Start with Windows** support.
- Runtime TOML configuration with live reload.
- Tray controls for presence, focused-app detection, test activity, configuration, and exit.
- Privacy filtering that excludes prompts, responses, full paths, raw commands, tool arguments, tool output, and window titles from Discord.
