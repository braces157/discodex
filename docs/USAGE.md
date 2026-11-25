# Track model usage

Click the Discodex tray icon to open the GPUI usage cockpit in Compact or Wide view. Right-click still opens the tray menu. You can also run the cockpit independently:

```powershell
cargo run --release -- --usage
cargo run --release -- --usage --wide
```

The popup closes with Escape, its close button, or a click outside it. Pin (`p`) keeps it open while you use other windows, `w` toggles Compact and Wide layout, `c` switches between Stacked Bar and Area charts, `1`–`4` switch periods, and `r` refreshes. Closing it leaves Discord presence running.

## Read usage for a period

Select Last 24h, 7 days, 30 days, or All. Last 24h spans the rolling last 24 local clock hours ending in the current hour, while 7 days and 30 days start at local midnight. The scanner reconciles cumulative counters before selecting timestamped usage, so a recent model does not bring its older token totals into the selected period.

Hero cards show recorded tokens and base-rate API cost estimates with period delta badges and sparklines, followed by a telemetry strip for logged calls, cache hit rate, cache savings, and task time. The interactive chart switches between tokens, cost, and calls, stacked bars or area curves, token mix or provider stacks, and a rolling 24-hour local timeline on Last 24h. Below it sit a 12-Week streak calendar and 7×24 Peak Hours activity heatmap, a Token Mix composition bar, provider filter chips, and sortable model rows with expandable detail drawers.

Calls count distinct logged usage observations. They are not unique sessions, network requests, or failed attempts that left no usage record. Missing fields stay unknown. Charts use the dated records available locally.

Cached input is already part of input tokens. Reasoning output is already part of output tokens. Providers that store uncached input or non-reasoning output separately (such as Claude Code, Antigravity, DeepSeek Harness, Factory Droid, Cline, Roo Code, Zed, OpenCode, and Kilo Code) are normalized to inclusive totals once so categories are never double-counted.

Task wall time uses matched task start and completion timestamps (for example in Codex, DeepSeek Harness, and GitHub Copilot CLI) and includes tools and waits. Concurrent task durations can overlap. Message-only stores do not establish complete task intervals, so their task time remains unknown.

## Automatic prices

Discodex bundles an official rate catalog checked on November 19, 2026. Known exact OpenAI, Claude, Google Gemini, DeepSeek, and Qwen model identifiers receive estimates automatically across harnesses when the required token categories (input, cached input, and output) are recorded. Unknown identifiers or records missing required token categories remain unpriced.

Sources are [OpenAI API pricing](https://developers.openai.com/api/docs/pricing), [Claude API pricing](https://platform.claude.com/docs/en/about-claude/pricing), [Gemini Developer API pricing](https://ai.google.dev/gemini-api/docs/pricing), and [DeepSeek API pricing](https://api-docs.deepseek.com/quick_start/pricing). The catalog retains its source URLs and verification date. It is a bundled snapshot, not a live network request on every refresh.

Displayed costs are approximate Standard API base-rate equivalents. They do not represent subscription charges, Codex credit billing, historical invoices, or taxes. Current catalog rates apply to selected historical usage.

Newer API models can charge separate cache-write premiums, and some providers vary rates by time of day. Available write counts use the catalog's default write price. Missing write counts, longer write lifetimes, long-context, speed-tier, time-of-day, regional, and tool charges can make the estimate incomplete. The popup marks estimates as approximate and reports these assumptions. Reasoning tokens do not incur a second output charge.

## Override rates

The pricing action opens `%APPDATA%\Discodex\usage-rates.toml`. Save custom rates and refresh the popup to override the bundled catalog for that exact provider and model.

Each `[[rates]]` entry uses `provider`, `model`, `input_per_million`, `cached_input_per_million`, `output_per_million`, and optional `cache_write_per_million`. Prices are USD per million tokens and must be finite and nonnegative. Supported `provider` values are `codex`, `claude-code`, `antigravity`, `deepseek-harness`, `gemini-cli`, `qwen-code`, `opencode`, `kilo-code`, `factory-droid`, `copilot-cli`, `junie`, `cline`, `roo-code`, `zed`, `crush`, and `aider`.

A configured cache-write rate is an effective price. Choose a value appropriate to your agreement and cache lifetime. Malformed or duplicate overrides produce a visible warning.

## Coverage and privacy

Supported local stores include:

- **OpenAI Codex** (`codex`) — plain `.jsonl` and compressed `.jsonl.zst` / `.jsonl.zstd` sessions and archives (`CODEX_HOME` supported)
- **Claude Code** (`claude-code`) — project `.jsonl` logs (`CLAUDE_CONFIG_DIR` supported)
- **Google Antigravity** (`antigravity`) — `transcript.jsonl` conversation logs (including T3 Code ACP transcripts under `~\.t3\userdata\providers\antigravity\`)
- **DeepSeek Harness** (`deepseek-harness`) — `session.v4.jsonl` and compressed `session.v4.jsonl.zstd` logs
- **Gemini CLI** (`gemini-cli`) — `chats/*.json` and `chats/*.jsonl` transcripts
- **Qwen Code** (`qwen-code`) — `chats/*.jsonl` telemetry and assistant logs
- **OpenCode** (`opencode`) — SQLite `opencode*.db` message tables (deduplicated across v1/v2 tables)
- **Kilo Code** (`kilo-code`) — SQLite `kilo*.db` message tables
- **Factory Droid** (`factory-droid`) — session-level `*.settings.json` totals
- **GitHub Copilot CLI** (`copilot-cli`) — `events.jsonl` / session `.jsonl` usage events
- **Junie** (`junie`) — `events.jsonl` LLM response metadata
- **Cline** (`cline`) and **Roo Code** (`roo-code`) — task `ui_messages.json` API request entries
- **Zed** (`zed`) — SQLite `threads.db` thread token usage
- **Crush** (`crush`) — SQLite `crush.db` session token columns when present
- **Aider** (`aider`) — `.aider.chat.history.md` token reports (bounded shallow scan)

Missing local directories for harnesses you have not installed are skipped silently. Focused-window-only apps (such as web ChatGPT, Claude web, Gemini web, Perplexity, or Poe) and harnesses that do not record local token counts cannot supply token usage.

The scanner caches unchanged files and databases (including SQLite `-wal` sidecars) in a background tray preloader across all four periods and persists the latest aggregated snapshot to `%APPDATA%\Discodex\usage-cache.json` so the popup opens and switches periods without waiting for a cold log scan. Refreshes and restarts reconstruct totals from source logs rather than adding to a saved counter, and deleting source logs removes their contribution. Limits on scanned files, bytes, lines, and metadata records produce coverage warnings.

Snapshots and `usage-cache.json` contain model identifiers, effort, numeric usage, timestamps, and pricing metadata. They exclude prompts, responses, reasoning prose, tool arguments, project names, and raw absolute paths. The popup opens no network listener and adds no usage totals to Discord.

## Build the GPUI window

Windows release builds require the Windows SDK shader compiler `fxc.exe`. Make it available on PATH or set `GPUI_FXC_PATH` to its full path before building. GPUI uses Direct3D for the native popup.

If `where.exe fxc.exe` returns more than one path, set `GPUI_FXC_PATH` to one full executable path. GPUI 0.2.2 does not choose a single compiler from multiple PATH matches.
