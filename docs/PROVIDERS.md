# Provider support

Discodex intentionally separates **rich session tracking** from **focused-app detection**. This keeps provider-specific parsing small and reviewable while still covering popular AI applications whose local storage formats are unstable or private.

## Rich session tracking

Rich adapters read local session data and convert only selected metadata into safe activity fields.

| Provider | Local source | Store | Detail |
| --- | --- | --- | --- |
| OpenAI Codex | `~\.codex\sessions\...\rollout-*.jsonl` | JSONL | Project, stack/language, file basename, activity, phase, timer |
| Claude Code | `~\.claude\projects\...\*.jsonl` | JSONL | Project, stack/language, file basename, activity, phase, timer |
| Google Antigravity | `~\.gemini\antigravity\brain\...\transcript.jsonl` | JSONL | Project, stack/language, file basename, activity, phase, timer |
| DeepSeek Harness | `~\.dsh\sessions\...\session.v4.jsonl[.zstd]` | JSONL | Project, stack/language, model, reasoning effort, file basename, activity, phase, timer |
| Gemini CLI | `~\.gemini\tmp\<project>\chats\session-*.jsonl` | JSONL snapshot | Project, stack/language, model, file basename, activity, phase, timer |
| Qwen Code | `~\.qwen\projects\<project>\chats\<session>.jsonl` | JSONL | Project, stack/language, model, file basename, activity, phase, timer |
| GitHub Copilot CLI | `~\.copilot\session-state\<id>\events.jsonl` | JSONL | Project, model, file basename, activity, phase, timer |
| Factory Droid | `~\.factory\sessions\<project>\<id>.jsonl` | JSONL | Project, model, file basename, activity, phase, timer |
| Kiro | `~\.kiro\sessions\<workspace>\sess_<id>\messages.jsonl` | JSONL | Project, model, file basename, activity, phase, timer |
| Cursor | `~\.cursor\projects\<project>\agent-transcripts\<id>\<id>.jsonl` | JSONL | Project, file basename, activity, phase, timer |
| Windsurf / Cascade | `~\.windsurf\transcripts\<id>.jsonl` | JSONL | Project, activity, phase, timer |
| Junie (JetBrains) | `~\.junie\sessions\<id>\events.jsonl` | JSONL | Project, activity, phase, timer |
| Aider | `<repository>\.aider.chat.history.md` | Markdown | Project, model, file basename, activity, phase, timer |
| Cline | `~\.cline\data\sessions\<id>\<id>.messages.json` and `<id>.json` | JSON | Project, model, file basename, activity, phase, timer |
| Roo Code | `%APPDATA%\<editor>\User\globalStorage\rooveterinaryinc.roo-cline\tasks\<id>\ui_messages.json` | JSON | Project, mode, file basename, activity, phase, timer |
| Continue | `~\.continue\sessions\<id>.json` | JSON | Project, model, file basename, activity, phase, timer |
| Block Goose | `%APPDATA%\Block\goose\data\sessions\sessions.db` | SQLite | Project, model, file basename, activity, phase, timer |
| Charm Crush | `%LOCALAPPDATA%\crush\<project>\crush.db` | SQLite | Project, model, file basename, activity, phase, timer |
| OpenCode | `~\.local\share\opencode\opencode.db` | SQLite | Project, model, file basename, activity, phase, timer |
| Zed | `%LOCALAPPDATA%\Zed\threads\threads.db` | SQLite | Project, model, file basename, activity, phase, timer |
| Kilo Code | `~\.local\share\kilo\kilo.db` | SQLite | Project, model, file basename, activity, phase, timer |
| Sourcegraph Amp | `%LOCALAPPDATA%\amp\logs\threads\<id>.log` | JSONL | Activity, phase, timer |

Session adapters do not forward prompt text, assistant responses, raw tool arguments, raw tool output, or full paths to Discord.

Each provider has one normalised mark in `assets/logos/`, used as the Rich Presence large image. See [Provider artwork](ARTWORK.md) for the set and how it is rebuilt.

When multiple tracked sessions are active, sessions running tools take priority, followed by the most recently active session. Window focus does not affect session selection. Rich tracking continues when an app is minimized or unfocused, and when focused-app detection is disabled.

### Store kinds

Harnesses expose their activity in three different shapes, and each shape needs its own ingestion path:

- **JSONL** – append-only, newline-delimited files. The worker tails them and parses one record per line.
- **JSONL snapshot** – a file that appends a full snapshot of the conversation for every change (older Gemini CLI builds). The file is re-read and only records that were not reported before are applied.
- **JSON** – a single document that is rewritten in place (editor extensions, Continue, Cline). The file is re-read and only records that were not reported before are applied.
- **SQLite** – a database that the worker polls for newly written rows. The session key of a row is the conversation it belongs to, so parallel conversations never share one presence.

### Provider notes

#### DeepSeek Harness

The adapter follows the upstream [v4 session event schema](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/core/session/src/types.ts) and [JSONL persistence format](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/session/session-persistence-jsonl/src/zstd.ts). It reads plain JSONL or concatenated Zstandard frames, waits for incomplete frames to finish, and validates compressed-frame checksums. Frames larger than 16 MiB after decompression are rejected.

`request/header` supplies the actual model and optional reasoning effort, including models from other providers. `turn/start`, `step/start`, `tool/call`, `tool/result`, and `turn/end` supply activity and completion. Parallel tools are matched by call ID, so one result does not mark the session as thinking while another tool is running. Completed, cancelled, failed, and blocked turns clear activity. Restored/forked history does not count as new activity.

Updates follow durable log writes, which may lag the live UI during a streamed model response. Prompts, assistant text, thought text, session titles, tool descriptions, and tool output are ignored. Only normalized tool labels and file basenames are published. Session tracking works in the background with focused-app detection disabled; an idle Harness window alone does not publish activity. Currently only format v4 is supported.

#### Gemini CLI

The session log lives in `~\.gemini\tmp\<project>\chats\`. Two record shapes are supported: one message record per line, and the older `{$set: {messages: [...]}}` snapshot that is appended for every change. Every message is keyed by its id, so a repeated snapshot never replays the conversation.

The main session record only holds a one-way hash of the workspace, so the project is resolved from the ownership marker `<project>\.project_root` next to the chats folder. Tool calls are written when they complete, so the presence follows the tool that just finished.

#### Qwen Code

Every record carries the workspace path and an RFC-3339 timestamp, so the project and the elapsed timer are exact. Tool calls arrive as `functionCall` parts and are completed by separate `tool_result` records; both carry a call id, so parallel tools are tracked separately. Legacy tool aliases are normalized before the activity label is chosen.

#### GitHub Copilot CLI

The session events live in `~\.copilot\session-state\<id>\events.jsonl`. The workspace, the selected model and the tool arguments come from the session start and tool execution events. Timestamps are epoch milliseconds.

#### Factory Droid

The session log lives in `~\.factory\sessions\<workspace>\<id>.jsonl`. The header record carries the workspace, every record carries an RFC-3339 timestamp, and the tool blocks use the Anthropic style `tool_use` and `tool_result` parts, so a tool call can be paired with its result by id.

#### Kiro

The IDE transcript `messages.jsonl` is append-only JSONL with a top-level timestamp and payload objects that name the tool, its arguments and its status. The older CLI transcript nests the tool data in the content parts and keeps the timestamp in `data.meta.timestamp` as float seconds. The workspace of a CLI session only exists in the sidecar file next to the transcript, so it is not reported when it is unknown.

#### Cursor

The agent transcript is plain JSONL. It has no per-record timestamp and no tool result records, so a tool start is reported without a matching finish, and the turn boundary is used to close it. The project is recovered from an absolute path inside a tool input.

#### Cline

Cline 4 stores one directory per session with a transcript (`<id>.messages.json`) and a manifest (`<id>.json`). Both files describe the same session, so they share one presence. The manifest carries the provider, the model, the workspace and the start time; the transcript carries the tool calls with their ids. Older versions wrote `ui_messages.json` into the editor global storage, which is still supported.

#### Roo Code

Roo Code is a Cline fork and keeps the same per-task layout. It does not persist the model id, so the mode of the task (`architect`, `code`, `ask`, `debug`, `orchestrator`) is published instead. The raw conversation file of a task is never read.

#### Block Goose, Charm Crush, OpenCode, Zed, Kilo Code

These harnesses keep their sessions in a SQLite database that is written while Discodex reads it. The worker opens the database read-only, waits for the writer with a busy timeout, and polls for new rows. The session id of a row is the key of the presence, so several conversations stay separate.

#### Aider

Aider writes a Markdown transcript into the working tree of the repository it edits, so the common workspace folders are scanned for `.aider.chat.history.md`. The transcript has no per-turn timestamp, so the elapsed timer follows the observation time.

#### Sourcegraph Amp

Amp redacts its local log: it keeps the tool name but drops the arguments, so no workspace, model or file is available. The presence shows the tool activity only.

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

Providers with a complete session adapter are excluded from focused-app detection. Their presence comes exclusively from session activity. Providers that gained a session adapter but keep a useful focused fallback (Cursor, Kiro, Cline, Roo Code, OpenCode, Zed, Continue, Aider, Windsurf) report the session while it runs and fall back to the focused window otherwise.

## Detection priority

1. Active rich session data is preferred when available.
2. Focused-app detection is used as the compatibility fallback.
3. When no supported activity is active, Discodex clears Rich Presence.

## Adding a provider

Provider recognition is centralized in `src/provider.rs`. A new provider normally needs:

1. A `Provider` enum entry, display name, and logo URL mapping in `Provider::logo_url`.
2. Executable/window recognition for standard detection.
3. Optionally, a `SessionSource` plus parser support in `src/sources/` for rich tracking.
4. Tests proving that sensitive message content is discarded and logo asset is present.

### Rich Presence logos

Product icons use PNGs from [Lobe Icons](https://github.com/lobehub/lobe-icons), pinned to `@lobehub/icons-static-png@1.97.0` on jsDelivr. Dark variants keep monochrome marks visible on Discord's dark background. Codex uses this repository's existing artwork; Zed, Aider, and Continue use their verified product organization avatars.

When changing a mapping, fetch the URL and verify that it returns a decodable image, then inspect the artwork itself. A working URL or passing serialization test alone cannot detect a wrong brand. Use the specific product logo, especially for Antigravity, Claude Code, GitHub Copilot, and Microsoft Copilot; do not substitute the parent company's avatar. External images require network access and may remain cached by Discord after a change.

Rich adapters should only be added when the local event format is sufficiently stable to parse without collecting conversation content.
