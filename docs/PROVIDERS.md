# Provider support

Discodex intentionally separates **rich session tracking** from **focused-app detection**. This keeps provider-specific parsing small and reviewable while still covering popular AI applications whose local storage formats are unstable or private.

## Rich session tracking

Rich adapters read local session event files and convert only selected metadata into safe activity fields.

| Provider | Local source | Current detail |
| --- | --- | --- |
| OpenAI Codex | `~\.codex\sessions\...\rollout-*.jsonl` | Project, stack/language, file basename, activity, phase, timer |
| Claude Code | `~\.claude\projects\...\*.jsonl` | Project, stack/language, file basename, activity, phase, timer |
| Google Antigravity | `~\.gemini\antigravity\brain\...\transcript.jsonl` | Project, stack/language, file basename, activity, phase, timer |

Session adapters do not forward prompt text, assistant responses, raw tool arguments, raw tool output, or full paths to Discord.

When multiple tracked sessions are active, Discodex selects the most recently active session.

## Focused-app detection

For providers without a stable session adapter, Discodex checks the foreground executable name and locally matches known AI product names in the focused window title. The title is used only for recognition and is discarded immediately.

Current recognition includes:

- ChatGPT
- Claude
- Google Gemini / Google AI Studio
- Cursor
- Windsurf
- Google Antigravity
- Trae
- Kiro
- Zed AI
- OpenCode
- Aider
- GitHub Copilot
- Cline
- Roo Code
- Continue
- Perplexity
- Microsoft Copilot
- Poe

This mode is best-effort. Browser title formats, editor extension titles, and executable names can change without notice.

## Detection priority

1. Active rich session data is preferred when available.
2. Focused-app detection is used as the compatibility fallback.
3. When no supported activity is active, Discodex clears Rich Presence.

## Adding a provider

Provider recognition is centralized in `src/provider.rs`. A new provider normally needs:

1. A `Provider` enum entry, display name, and logo URL mapping in `Provider::logo_url`.
2. Executable/window recognition for standard detection.
3. Optionally, a `SessionSource` plus parser support in `src/events.rs` for rich tracking.
4. Tests proving that sensitive message content is discarded and logo asset is present.

### Rich Presence logos

Product icons use PNGs from [Lobe Icons](https://github.com/lobehub/lobe-icons), pinned to `@lobehub/icons-static-png@1.97.0` on jsDelivr. Dark variants keep monochrome marks visible on Discord's dark background. Codex uses this repository's existing artwork; Zed, Aider, and Continue use their verified product organization avatars.

When changing a mapping, fetch the URL and verify that it returns a decodable image, then inspect the artwork itself. A working URL or passing serialization test alone cannot detect a wrong brand. Use the specific product logo, especially for Antigravity, Claude Code, GitHub Copilot, and Microsoft Copilot; do not substitute the parent company's avatar. External images require network access and may remain cached by Discord after a change.

Rich adapters should only be added when the local event format is sufficiently stable to parse without collecting conversation content.
