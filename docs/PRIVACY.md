# Privacy model

Discodex is designed around a narrow rule: Discord should receive activity metadata, not AI conversation content.

## Data that may appear in Rich Presence

Depending on the provider and detection mode, Discodex may publish:

- AI provider name
- Final workspace folder name, unless **Show project names** is disabled
- Inferred language or framework
- Current file basename, unless **Show file names** is disabled
- Normalized activity such as `Editing files` or `Running Rust tests`
- High-level phase such as thinking/reasoning or running tools
- Active agent or subagent role (e.g. `DeepCoder`, `DeepInvestigator`, `Planner`)
- Selected model identifier and reasoning effort when available
- Extracted reasoning headings, only after **Share reasoning** is enabled and both kinds of names are visible
- Task start timestamp for elapsed time

Reasoning headings are provider-authored text and can contain confidential client names, project names, filenames, or other private information. Shortening a heading does not make it private. Sharing is off by default, including for existing configuration files.

Hiding either project or file names suppresses reasoning as well. The final Discord activity builder applies these controls for every provider and test presence. Activity state keeps reasoning separate from normalized activity labels.

## Data intentionally excluded

Discodex does not publish:

- User prompts
- Assistant responses or full chain-of-thought text
- Full file paths
- Raw shell commands
- Tool arguments
- Tool output
- Raw session JSON
- Window titles
- Browser URLs
- Account identifiers

The parser converts recognized events into internal activity fields. The publisher applies privacy controls before serializing Discord activity. Opting in to reasoning sends extracted heading text verbatim, so the exclusions above do not guarantee that an opted-in heading contains no private information.

## Focused-window detection

When enabled, Discodex reads the foreground process executable name and may inspect the active window title to identify a known AI product. Matching happens locally. The title itself is not retained in the presence state or Discord payload.

Disable this layer at any time from the tray menu with **Detect focused AI apps**, or set:

```toml
foreground_detection = false
```

Rich Codex, Claude Code, Google Antigravity, and DeepSeek Harness session tracking remains available when focused-app detection is disabled.

## Local diagnostics and logs

**Diagnostics** opens a local snapshot in Notepad. It reports provider detection, source access, recognized events, observation times, watch state, and Discord connection state. It omits source paths and conversation content and is not sent to Discord.

Presence logs and tray status use the same privacy settings as outgoing activity. Changing settings does not remove older log entries or retract previously published information.

## Network behavior

The native Usage Dashboard reads Codex and Claude Code numeric usage metadata separately from presence detection. Its in-memory rows contain model identifiers, reasoning effort labels, tokens, task durations, and timestamps. It excludes conversation text, project names, and full paths. Usage totals are not added to Discord presence. Local pricing rates are stored in `%APPDATA%\Discodex\usage-rates.toml`. The dashboard opens no network listener. See [Track model usage](USAGE.md) for coverage and cost limitations.

Discodex publishes Rich Presence through Discord's local IPC connection. It does not run its own telemetry service or require a Discodex account.

Provider activity is discovered from local files and local Windows foreground-process information.

## Reviewing the boundary

The main privacy-sensitive areas are:

- `src/events.rs` — converts provider events into normalized metadata.
- `src/foreground.rs` — obtains focused app information.
- `src/provider.rs` — maps local app/window signals to provider names.
- `src/discord.rs` — defines the final Discord activity payload.

Changes that broaden collected data should be treated as privacy-sensitive and reviewed carefully.

