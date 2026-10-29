# Track model usage

Click the Discodex tray icon to open the compact GPUI usage popup. Right-click still opens the tray menu. You can also run the popup independently:

```powershell
cargo run --release -- --usage
```

The popup closes with Escape, its close button, or a click outside it. Pin keeps it open while you use other windows. Closing it leaves Discord presence running.

## Read usage for a period

Select Today, 7 days, 30 days, or All. Periods start at local midnight. The scanner reconciles cumulative counters before selecting timestamped usage, so a recent model does not bring its older token totals into the selected period.

Summary cards show recorded tokens, logged usage calls, a base-rate API cost estimate, and cache hit rate. The chart switches between tokens, cost, and calls. Model rows show model and reasoning effort with their available usage and task time.

Calls count distinct logged usage observations. They are not unique sessions, network requests, or failed attempts that left no usage record. Missing fields stay unknown. Charts use the dated records available locally.

Cached input is already part of input tokens. Reasoning output is already part of output tokens. Claude cache creation is included once in normalized input. Do not add these subsets to the totals again.

Task wall time uses matched task start and completion timestamps and includes tools and waits. Concurrent task durations can overlap. Claude messages do not establish complete task intervals, so their task time remains unknown.

## Automatic prices

Discodex bundles an official rate catalog checked on October 29, 2026. Known exact OpenAI and Claude model identifiers receive estimates automatically. Unknown identifiers remain unpriced.

Sources are [OpenAI API pricing](https://developers.openai.com/api/docs/pricing) and [Claude API pricing](https://platform.claude.com/docs/en/about-claude/pricing). The catalog retains its source URLs and verification date. It is a bundled snapshot, not a live network request on every refresh.

Displayed costs are approximate Standard API base-rate equivalents. They do not represent subscription charges, Codex credit billing, historical invoices, or taxes. Current catalog rates apply to selected historical usage.

Newer API models can charge separate cache-write premiums. Available write counts use the catalog's default write price. Missing write counts, longer write lifetimes, long-context, speed-tier, regional, and tool charges can make the estimate incomplete. The popup marks estimates as approximate and reports these assumptions. Reasoning tokens do not incur a second output charge.

## Override rates

The pricing action opens `%APPDATA%\Discodex\usage-rates.toml`. Save custom rates and refresh the popup to override the bundled catalog for that exact provider and model.

Each `[[rates]]` entry uses `provider`, `model`, `input_per_million`, `cached_input_per_million`, `output_per_million`, and optional `cache_write_per_million`. Prices are USD per million tokens and must be finite and nonnegative. Provider values are `codex` and `claude-code`.

A configured cache-write rate is an effective price. Choose a value appropriate to your agreement and cache lifetime. Malformed or duplicate overrides produce a visible warning.

## Coverage and privacy

Only local Codex sessions and archives and Claude Code project logs are supported initially. `CODEX_HOME` and `CLAUDE_CONFIG_DIR` override their standard roots. Focused-window detection cannot supply token usage.

The scanner caches unchanged files and reconstructs totals from source records. Refreshes and restarts do not add to a saved counter. Deleting source logs removes their contribution. Limits on scanned files, bytes, lines, and metadata records produce coverage warnings.

Snapshots contain model identifiers, effort, numeric usage, timestamps, and pricing metadata. They exclude prompts, responses, reasoning prose, tool arguments, project names, and absolute paths. The popup opens no network listener and adds no usage totals to Discord.

## Build the GPUI window

Windows release builds require the Windows SDK shader compiler `fxc.exe`. Make it available on PATH or set `GPUI_FXC_PATH` to its full path before building. GPUI uses Direct3D for the native popup.
