# Changelog

All notable user-facing changes to Discodex are documented here.

## Unreleased

### Added

- Rich session tracking and Discord artwork for DeepSeek Harness, including model, reasoning effort, project, file, tool activity, phase, and elapsed time from v4 JSONL/Zstandard logs.
- Rich session tracking for Gemini CLI, Qwen Code, GitHub Copilot CLI, Factory Droid, Kiro, Cursor, Windsurf/Cascade, Junie, Aider, Cline, Roo Code, Continue, Block Goose, Charm Crush, OpenCode, Zed, and Kilo Code.
- Session adapters for harnesses that keep their activity in a rewritten document or in a database, with a polling path for the database-backed providers.
- A live test that checks the adapters against the session files that are installed on the machine (`cargo test -- --ignored live_`).

### Changed

- Session-backed providers use log activity alone; window focus no longer affects their selection. Focused-app detection is reserved for providers without session tracking.
- Providers that gained a session adapter but keep a useful focused fallback now report the session while it runs and fall back to the focused window otherwise.

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
