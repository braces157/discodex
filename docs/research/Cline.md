# Cline — research report for Discodex

**Harness:** Cline (VS Code extension `saoudrizwan.claude-dev` + standalone CLI `@cline/cli`)
**Upstream snapshot studied:** `cline/cline` @ `main` (tarball `codeload.github.com/cline/cline/tar.gz/refs/heads/main`, downloaded 2026‑10‑21)
**Versions verified:** VS Code extension `package.json` → `name=claude-dev`, `publisher=saoudrizwan`, `version=4.1.25`, `displayName=Cline`; Marketplace page `saoudrizwan.claude-dev` → 4.1.25 (HTTP 200). CLI `apps/cli/package.json` → `@cline/cli` 3.0.73, `bin: {"cline": ...}`.
**Method:** upstream source only. **No local harness data was read** (no `%APPDATA%\Code\User\globalStorage`, no `~/.cline`, no `~/.config`). Every claim below cites an upstream file/function or an HTTP check.

---

## 0. Headline finding (this changes the premise of the brief)

The brief (and the Roo Code / Kilo Code reports) assume Cline stores **one JSON file per task** in the editor's VS Code global storage, like Roo Code does.
**That is no longer true for current upstream Cline (v4.x).** Upstream Cline moved task persistence into a shared SDK session store:

* The classic `Task` class (`apps/vscode/src/core/task/`) **no longer exists** in upstream `main` — it was deleted.
* `apps/vscode/src/sdk/sdk-task-control-coordinator.ts:143-144` states it explicitly:
  > `// SDK session persistence owns conversation history. Do not write classic ui_messages.json here; history viewing reloads from SDK readMessages().`
* Only a dev tool writes `ui_messages.json` any more (`apps/vscode/src/dev/commands/tasks.ts:119`).
* The only writer of `ui_messages.json` in the whole `apps/vscode` tree is that dev tool (`grep "GlobalFileNames.uiMessages"` → 1 hit, the constant definition).

So upstream Cline v4 writes, per session:

| Artifact | Path | Format | Written by |
|---|---|---|---|
| Session transcript | `<data>/sessions/<sessionId>/<sessionId>.messages.json` | JSON document (rewritten whole) | `SessionManifestStore.persistSessionMessages` |
| Session manifest | `<data>/sessions/<sessionId>/<sessionId>.json` | JSON document (rewritten whole) | `SessionManifestStore.writeSessionManifest` |
| Compaction sidecar | `<data>/sessions/<sessionId>/<sessionId>.compaction.json` | JSON | `persistSessionCompactionState` |
| Session index (default) | `<data>/db/sessions.db` | **SQLite** | `SqliteSessionStore` |
| Session index (fallback) | `<data>/sessions/sessions.index.json` | JSON | `FileSessionPersistenceAdapter` |
| Global state / secrets | `<data>/globalState.json`, `<data>/secrets.json` | JSON (0o600 for secrets) | `ClineFileStorage` |

The classic per-task files (`ui_messages.json`, `api_conversation_history.json`, `task_metadata.json`, `settings.json`, `state/taskHistory.json`) still exist **only for legacy (pre‑SDK) tasks** created by Cline ≤ 3.x, and the enterprise "prompt storage" worker still reads them.

**Consequence for Discodex:** the `JsonFiles` ingestion path that already exists in this repo (built for Roo Code) is the right shape, but the filename rule and the roots must change, and the *authoritative* document for current Cline is `<sessionId>.messages.json` + `<sessionId>.json`, **not** `ui_messages.json`.

---

## 1. Exact storage paths (Windows)

### 1.1 VS Code extension (current, v4.x)

The extension does **not** set `CLINE_DATA_DIR` outside tests/debug, so it uses the shared data dir:

```
C:\Users\<user>\.cline\data\
├── globalState.json
├── secrets.json                     (0o600 — API keys, NEVER ingest)
├── settings\providers.json, global-settings.json, cline_mcp_settings.json
├── state\taskHistory.json           (legacy task list, Cline ≤3 tasks)
├── tasks\<taskId>\                  (legacy Cline ≤3 tasks)
│   ├── ui_messages.json
│   ├── api_conversation_history.json
│   ├── task_metadata.json
│   └── settings.json
├── sessions\<sessionId>\
│   ├── <sessionId>.messages.json    ← SDK-native transcript (rewritten whole)
│   ├── <sessionId>.json             ← SDK-native manifest (rewritten whole)
│   └── <sessionId>.compaction.json
├── sessions\sessions.index.json     (file-backend index fallback)
├── db\sessions.db                   (SQLite index, default backend)
└── logs\*.log
```

Evidence:
* `sdk/packages/shared/src/storage/paths.ts:186-200` — `resolveClineDataDir()` = `CLINE_DATA_DIR` → `CLINE_DIR + "/data"` → `~/.cline/data`; `resolveSessionDataDir()` = `<data>/sessions`; `resolveDbDataDir()` = `<data>/db`.
* `sdk/packages/core/src/runtime/host/host.ts:64-97` — `createLocalBackend()` uses `new SqliteSessionStore()` (→ `<data>/db/sessions.db`), falling back to `new FileSessionService()` (→ `<data>/sessions/sessions.index.json`) if SQLite is unavailable.
* `sdk/packages/core/src/session/services/session-service.ts:20-31` — the persistence adapter always uses `resolveSessionDataDir()` → `<data>/sessions` for **artifacts**, regardless of the index backend.
* `sdk/packages/core/src/services/session-artifacts.ts:63-94` — `sessionMessagesPath()` = `<sessions>/<id>/<id>.messages.json`; `sessionManifestPath()` = `<sessions>/<id>/<id>.json`.
* `sdk/packages/core/src/runtime/host/local-runtime-host.ts:539-551` — same paths, built from `ensureSessionsDir()`.

### 1.2 VS Code extension (legacy Cline ≤3 tasks)

`HostProvider.globalStorageFsPath` = `context.globalStorageUri.fsPath`
(`apps/vscode/src/extension.ts:591-601`, `apps/vscode/src/hosts/host-provider.ts:45-46`), i.e.

```
%APPDATA%\<VSCodeApp>\User\globalStorage\saoudrizwan.claude-dev\
├── tasks\<taskId>\ui_messages.json
├── tasks\<taskId>\api_conversation_history.json
├── tasks\<taskId>\task_metadata.json
├── tasks\<taskId>\settings.json
├── state\taskHistory.json
└── checkpoints\<taskId>\…
```

`<VSCodeApp>` ∈ `Code`, `Code - Insiders`, `VSCodium`, `Cursor`, `Windsurf`, `Trae`, `Kiro`, `Void`, `Positron`, `Antigravity`, `PearAI` (the list this repo already uses in `provider.rs::vscode_app_dirs()`).
Evidence: `apps/vscode/src/core/controller/file/openDiskConversationHistory.ts:14-16` (`path.join(globalStoragePath, "tasks", id, "api_conversation_history.json")`); `apps/vscode/src/core/storage/disk.ts:17-20,51-53,155-160`; `apps/vscode/src/hosts/vscode/vscode-to-file-migration.ts:25-32` (“for VSCode, globalStorageFsPath is still the VSCode-managed path (not ~/.cline/data/)”).

### 1.3 Standalone Cline CLI

The CLI shares the same agent core and the same data dir:

```
C:\Users\<user>\.cline\data\      (default; override with --data-dir / CLINE_DATA_DIR)
```

* `apps/cli/README.md:265` — “`--data-dir <path>` … instead of `~/.cline/data`”.
* `apps/cli/README.md:361` — `CLINE_DATA_DIR` = “Base data directory for sessions/settings/teams/hooks”.
* `apps/cli/src/utils/helpers.ts:596` — `CLINE_SESSION_DATA_DIR` = `<data>/sessions`.
* CLI sessions carry `source: "cli"` in the manifest / SQLite row (`apps/cli/src/commands/history.test.ts:32`, `apps/cli/src/runtime/interactive/exit-summary.test.ts:13`); VS Code sessions carry `source: "vscode"` (`apps/vscode/src/sdk/sdk-task-history.ts:121`).
* The standalone host also points `globalStorageUri` at the data dir, so the CLI writes classic task files to `~/.cline/data/tasks/<taskId>/` (`apps/vscode/src/standalone/vscode-context.ts:18-19,52-53`; documented in `docs/enterprise-solutions/monitoring/prompt-storage.mdx:10`).

---

## 2. Storage format per file

### 2.1 `<sessionId>.messages.json` (SDK-native transcript) — PRIMARY

Built by `buildMessagesFilePayload()` (`sdk/packages/core/src/services/session-data.ts:334-359`):

```jsonc
{
  "version": 1,
  "updated_at": "2026-10-21T01:26:32.412Z",   // ISO-8601, written on every persist
  "agent": "lead",                              // "lead" | "subagent" | "teammate"
  "sessionId": "1758…",
  "taskType": "subagent_task",                  // optional
  "origin": { "source": "vscode", "mode": "user", "sessionId": "1758…",
              "parentThreadId": "…", "subagent": "…" },
  "messages": [ /* MessageWithMetadata[] */ ],
  "system_prompt": "…"                          // optional — NEVER publish
}
```

Each message (`sdk/packages/shared/src/llms/messages.ts:129-165`):

```jsonc
{
  "role": "user" | "assistant",
  "content": "…" | [ /* ContentBlock[] */ ],
  "id": "…",                    // optional
  "agent": "…", "sessionId": "…",
  "metadata": { … },
  "modelInfo": { "id": "claude-sonnet-4-5", "provider": "anthropic", "family": "claude" },
  "metrics": { "inputTokens": 0, "outputTokens": 0, "cost": 0 },
  "ts": 1758…                  // optional; NOT reliably populated
}
```

Content blocks (`sdk/packages/shared/src/llms/messages.ts:19-125`):
`text{text}`, `tool_use{id, name, input{}}`, `tool_result{tool_use_id, name, content, is_error?}`, `thinking{thinking}`, `redacted_thinking{data}`, `file{path,content}`, `image{data,mediaType}`, `media{media}`.

### 2.2 `<sessionId>.json` (SDK-native manifest) — metadata gold mine

Schema: `sdk/packages/core/src/session/models/session-manifest.ts:6-28`.

```jsonc
{
  "version": 1,
  "session_id": "1758…",
  "source": "vscode",                 // or "cli"
  "pid": 12345,
  "started_at": "2026-10-21T01:20:00.000Z",   // ISO-8601
  "ended_at": "…", "exit_code": 0,             // optional
  "status": "running",                 // running|idle|completed|failed|cancelled|…
  "interactive": true,
  "provider": "anthropic",             // ← provider id
  "model": "claude-sonnet-4-5",        // ← MODEL ID IS PERSISTED
  "cwd": "C:\\Users\\me\\Documents\\Discodex",
  "workspace_root": "C:\\Users\\me\\Documents\\Discodex",
  "team_name": "…",                    // optional
  "enable_tools": true, "enable_spawn": false, "enable_teams": false,
  "prompt": "<USER PROMPT — never publish>",
  "metadata": { "title": "<derived from prompt — never publish>" },
  "messages_path": "…\\<sessionId>.messages.json",
  "compaction_path": "…"
}
```

### 2.3 `db/sessions.db` (SQLite index) — same fields, relational

`SqliteSessionStore.create()` (`sdk/packages/core/src/services/storage/sqlite-session-store.ts:85-92`) writes columns
`session_id, source, pid, started_at, ended_at, exit_code, status, status_lock, interactive, provider, model, cwd, workspace_root, team_name, enable_tools, enable_spawn, enable_teams, parent_session_id, parent_agent_id, agent_id, conversation_id, is_subagent, prompt, metadata_json, transcript_path, hook_path, messages_path, updated_at`.
The file-backed fallback `sessions.index.json` is `{ "version": 1, "sessions": { "<id>": <SessionRow> } }` (`sdk/packages/core/src/session/services/file-session-service.ts:19-22, 62-88`).

### 2.4 Legacy (Cline ≤3) files

* `ui_messages.json` — array of `ClineMessage` (`apps/vscode/src/shared/ExtensionMessage.ts:191-221`):
  `{ ts: number(ms), type: "ask"|"say", ask?: ClineAsk, say?: ClineSay, text?, reasoning?, images?, media?, files?, partial?, seq?, epoch?, modelInfo? }`.
  `ClineAsk` ∈ followup|plan_mode_respond|act_mode_respond|command|command_output|completion_result|tool|api_req_failed|resume_task|resume_completed_task|mistake_limit_reached|browser_action_launch|use_mcp_server|new_task|condense|summarize_task|report_bug|use_subagents (`:223-241`).
  `ClineSay` ∈ task|error|api_req_started|api_req_finished|text|reasoning|completion_result|plan_completion_result|user_feedback|user_feedback_diff|command|command_output|tool|shell_integration_warning*|browser_action*|mcp_*|use_mcp_server|diff_error|deleted_api_reqs|clineignore_error|command_permission_denied|checkpoint_created|load_mcp_documentation|info|hook_status|hook_output_stream|subagent|use_subagents|subagent_usage|conditional_rules_applied|compaction (`:243-279`).
* `api_conversation_history.json` — `Anthropic.MessageParam[]` (`apps/vscode/src/core/storage/disk.ts:155-162`): `{role, content: string | [{type:"text",text}|{type:"tool_use",id,name,input}|{type:"tool_result",tool_use_id,content}]}`.
* `task_metadata.json` — `TaskMetadata` = `{ files_in_context, model_usage, environment_history }` (`apps/vscode/src/core/storage/disk.ts:164-184`).
* `state/taskHistory.json` — `HistoryItem[]` (`apps/vscode/src/shared/HistoryItem.ts:3-36`): `{ id, ts, task(=PROMPT), tokensIn, tokensOut, totalCost, size, cwdOnTaskInitialization, isFavorited, modelId, apiProvider, … }` — **legacy tasks DO carry a model id here** (`modelId` + `apiProvider` + `cwdOnTaskInitialization`).

---

## 3. WRITE SEMANTICS — the decisive question

**Whole-file rewrite. Not append. Not write-once.**

`SessionManifestStore.persistSessionMessages()` (`sdk/packages/core/src/session/stores/session-manifest-store.ts:220-255`):

```ts
const payload = buildMessagesFilePayload({ updatedAt: nowIso(), context: …, messages, systemPrompt })
const contents = `${JSON.stringify(payload, null, 2)}\n`;
mkdirSync(dirname(path), { recursive: true });
writeFileSync(path, contents, "utf8");          // ← FULL REWRITE of the whole array
```

The manifest is likewise rewritten with `writeFileSync` (`session-manifest-store.ts:112-119`); compaction state uses an atomic temp+rename (`session-manifest-store.ts:305-313`, `session/stores/atomic-file.ts:23-52`); the file index uses temp+rename (`file-session-service.ts:44-48`); SQLite rows are `INSERT OR REPLACE`.

Legacy `ui_messages.json` was also rewritten wholesale (one `writeFile` of the whole array) — the classic Cline `Task` saved the entire message array on a debounce.

**Implication:** the existing `LogTailer` in `src/watcher.rs` (append-only, newline-delimited, byte-offset cursor) **cannot** be used for Cline. A rewrite makes the file *shorter* than the cursor, which the tailer handles only as a full reset (`watcher.rs:32-35`), and the content is one JSON document, not lines.

**This repo already solved exactly this for Roo Code**: `SessionStore::JsonFiles` + `watcher::recent_documents()` + `main.rs::apply_document()` (re-read the whole file, parse, emit only records not in a per-file `seen` set). The Cline adapter should reuse that machinery — **no new watcher architecture is required**, only a different filename rule, different roots, and a per-record dedupe key.

The only genuinely new risk: a notify event can be delivered while `writeFileSync` is in flight, so the file may be observed as truncated/invalid JSON. Mitigation is a periodic rescan (see §7.4).

---

## 4. Field availability (upstream Cline, current)

| Field | Recoverable? | Where | Notes |
|---|---|---|---|
| **workspace / cwd** | **YES** | manifest `.cwd` / `.workspace_root`; SQLite `cwd`/`workspace_root`; legacy `HistoryItem.cwdOnTaskInitialization` | absolute path — publish only `project_name(cwd)` (final folder) |
| **model id** | **YES (upstream persists it — unlike the Roo fork)** | manifest `.model` (+ `.provider`); per-message `modelInfo.{id,provider,family}`; SQLite `model`; legacy `HistoryItem.modelId` | This is the headline difference vs. the Roo report: upstream Cline **does** persist the model id |
| **provider id** | YES | manifest `.provider`; SQLite `provider`; legacy `HistoryItem.apiProvider` | e.g. `anthropic`, `openai`, `bedrock` |
| **current file basename** | YES | `tool_use.input` per tool (see §5) | basename only; never the absolute path |
| **tool name + tool id** | YES | `tool_use.name`, `tool_use.id`; `tool_result.tool_use_id` | enables `TrackedToolStarted/TrackedToolFinished` (Roo cannot do this — it has no call ids) |
| **tool result** | present but MUST be ignored | `tool_result.content` | may contain file bodies / command output — never publish |
| **turn / phase boundaries** | YES (derived) | `submit_and_exit` tool ⇒ turn finished; presence of a `tool_use` without its `tool_result` ⇒ tool running; `status` in the manifest ⇒ session state | |
| **timestamps** | PARTIAL | manifest `started_at`/`ended_at` (ISO‑8601, UTC); messages file `updated_at` (ISO); file mtime; per-message `ts` (ms epoch) is **optional and not reliably populated** | task start = manifest `started_at` (session start) or the first user message |
| **prompt / answer text** | present but MUST be ignored | `messages[].content[].text`, `system_prompt`, manifest `prompt`/`metadata.title` | |
| **reasoning text** | present but MUST be ignored | `content[].type == "thinking"` | publish `Thinking { reasoning: None }` |

---

## 5. Tool vocabulary and how to map it

Current upstream SDK tools (`sdk/packages/core/src/extensions/tools/definitions.ts`): `read_files` (:275), `search_codebase` (:372), `run_commands` (:530), `fetch_web_content` (:576), `apply_patch` (:670), `editor` (:719), `skills` (:789), `ask_question` (:835), `submit_and_exit` (:859).

Input schemas (`sdk/packages/core/src/extensions/tools/schemas.ts`):

| Tool | input | file path key | notes |
|---|---|---|---|
| `read_files` | `{files:[{path,start_line?,end_line?}]}` (tolerant: `file_paths`, `paths`, plain string) | `input.files[].path` | `:57-63`, `:88-106` |
| `search_codebase` | `{queries:[string]}` | – | `:111-116` |
| `run_commands` | `{commands:[string]}` (tolerant: `command`, `cmd`, string) | – | `:149-172` |
| `fetch_web_content` | `{requests:[{url,prompt}]}` | – | `:185-190` |
| `apply_patch` | `{input:"*** Begin Patch\n*** Update File: …"}` | parse `*** Update File: / *** Add File: / *** Delete File:` | `:234-245` |
| `editor` | `{path, old_text?, new_text, insert_line?}` | `input.path` | `:194-229` |
| `skills` | `{skill, args?}` | – | `:255-259` |
| `ask_question` | question payload | – | `:835` |
| `submit_and_exit` | result payload | – | `:859` — **this is Cline’s “attempt_completion”** |

Legacy Cline ≤3 tool names (seen in `api_conversation_history.json` / `ui_messages.json` `say:"tool"`): `read_file`, `write_to_file`, `replace_in_file`, `execute_command`, `list_files`, `list_code_definition_names`, `search_files`, `browser_action`, `attempt_completion`, `ask_followup_question`, `new_task`, `use_mcp_server`.

**Label classification (normalized, privacy-safe) mapping to the existing `classify_tool()` vocabulary:**

| Cline tool | Discodex label |
|---|---|
| `read_files`, `read_file` | `Reading project files` |
| `editor`, `apply_patch`, `write_to_file`, `replace_in_file` | `Editing files` |
| `search_codebase`, `search_files` | `Searching project` |
| `run_commands`, `execute_command` | `Running command` (or `Running Rust tests` / `Building Rust project` … via the existing `classify_tool` command sniffing — the raw command is NEVER published) |
| `fetch_web_content`, `browser_action` | `Browsing documentation` |
| `skills` | `Using development tools` |
| `ask_question`, `ask_followup_question` | `Waiting for input` |
| `submit_and_exit`, `attempt_completion` | *(completion → `Completed`)* |

Note: the existing `events.rs::classify_tool` matches on substrings and would mis-handle Cline names (`editor` → “Using development tools”, `run_commands` hits the `contains("command")` branch by luck, `read_files` hits `contains("read")` correctly, `search_codebase` hits `contains("search")` correctly, `apply_patch` hits `contains("apply_patch")` correctly). **Only `editor` and `skills` need explicit handling**, so a small Cline-specific classifier in `src/sources/cline.rs` is the cleanest option (no change to shared `events.rs`).

---

## 6. Redacted sample records (faithfully reconstructed from upstream schemas)

### 6.1 `<sessionId>.messages.json`

```json
{
  "version": 1,
  "updated_at": "2026-10-21T01:26:32.412Z",
  "agent": "lead",
  "sessionId": "1758430000000_abcde",
  "origin": { "source": "vscode", "mode": "user", "sessionId": "1758430000000_abcde" },
  "messages": [
    { "role": "user", "content": [ { "type": "text", "text": "<REDACTED USER PROMPT>" } ] },
    { "role": "assistant",
      "content": [ { "type": "thinking", "thinking": "<REDACTED REASONING>" } ],
      "modelInfo": { "id": "claude-sonnet-4-5", "provider": "anthropic", "family": "claude" } },
    { "role": "assistant",
      "content": [ { "type": "tool_use", "id": "toolu_01ABC", "name": "read_files",
                     "input": { "files": [ { "path": "C:\\Users\\me\\Documents\\Discodex\\src\\main.rs" } ] } } ] },
    { "role": "user",
      "content": [ { "type": "tool_result", "tool_use_id": "toolu_01ABC", "name": "read_files",
                     "content": "<REDACTED FILE BODY>" } ] },
    { "role": "assistant",
      "content": [ { "type": "tool_use", "id": "toolu_01DEF", "name": "editor",
                     "input": { "path": "C:\\Users\\me\\Documents\\Discodex\\src\\events.rs",
                                "old_text": "<REDACTED>", "new_text": "<REDACTED>" } } ] },
    { "role": "assistant",
      "content": [ { "type": "tool_use", "id": "toolu_01GHI", "name": "submit_and_exit",
                     "input": { "result": "<REDACTED ANSWER>" } } ] }
  ]
}
```

### 6.2 `<sessionId>.json` (manifest)

```json
{
  "version": 1,
  "session_id": "1758430000000_abcde",
  "source": "vscode",
  "pid": 4242,
  "started_at": "2026-10-21T01:20:00.000Z",
  "status": "running",
  "interactive": true,
  "provider": "anthropic",
  "model": "claude-sonnet-4-5",
  "cwd": "C:\\Users\\me\\Documents\\Discodex",
  "workspace_root": "C:\\Users\\me\\Documents\\Discodex",
  "enable_tools": true, "enable_spawn": false, "enable_teams": false,
  "prompt": "<REDACTED USER PROMPT>",
  "metadata": { "title": "<REDACTED TITLE>" },
  "messages_path": "C:\\Users\\me\\.cline\\data\\sessions\\1758430000000_abcde\\1758430000000_abcde.messages.json"
}
```

### 6.3 Legacy `ui_messages.json` (Cline ≤3, same shape Roo forked)

```json
[
  { "ts": 1758430000000, "type": "say", "say": "task", "text": "<REDACTED USER PROMPT>" },
  { "ts": 1758430001000, "type": "say", "say": "reasoning", "reasoning": "<REDACTED>" },
  { "ts": 1758430002000, "type": "say", "say": "tool",
    "text": "{\"tool\":\"readFile\",\"path\":\"src/main.rs\",\"content\":\"<REDACTED ABSOLUTE PATH>\"}" },
  { "ts": 1758430003000, "type": "ask", "ask": "command", "text": "<REDACTED SHELL COMMAND>" },
  { "ts": 1758430004000, "type": "say", "say": "completion_result", "text": "<REDACTED ANSWER>" }
]
```

---

## 7. Concrete Discodex integration plan

> The repo has moved on since the “adapter contract” in the brief: `src/provider.rs` now has `enum SessionStore { LogFiles, JsonFiles, Sqlite }`, `src/sources/` is a module tree, `src/sources/cline.rs` **exists but is a stub** (`parse_document` returns `Vec::new()`), and `src/provider.rs::is_session_document` already maps `RooCode | Cline => "ui_messages.json"`. The plan below is written against **that** current state.

### 7.1 Provider variant, display name, logo

* Variant: `Provider::Cline` — **already exists** (`src/provider.rs:20`), `display_name() == "Cline"` (`:140`), `default_activity() == "Working with AI editor"` (`:172`).
* `from_executable("cline")` → `Provider::Cline` already exists (`:196`) — this now also matches the standalone CLI binary `cline` (`apps/cli/package.json` `bin.cline`).
* Logo URL: `https://cdn.jsdelivr.net/npm/@lobehub/icons-static-png@1.97.0/dark/cline.png` — **already in `provider.rs:99-101`** and **verified**: `HEAD` → `200 OK`, `Content-Type: image/png`, `Content-Length: 5360`.
  * Verified negative: `.../dark/cline-color.png` → **404**. Do not “fix” the URL to a `-color` variant.
  * Also verified: `.../dark/roocode.png` → 200 (Roo’s icon is a separate asset, so Cline and Roo never share an image).
* `has_session_tracking()`: Cline should be added **only if** the session path is the sole presence source. Recommendation: **add `Provider::Cline` to `has_session_tracking()`** once the Cline adapter is live, so a focused VS Code window cannot produce a bare “Working with AI editor” presence while a Cline session is actually running. (Do this together with the adapter, never before — otherwise Cline windows go silent.)

### 7.2 SessionSource roots

Add a **new** source for the SDK-native store (the current, authoritative one):

```rust
// src/provider.rs :: session_sources()
SessionSource {
    provider: Provider::Cline,
    root: home.join(".cline").join("data").join("sessions"),
    store: SessionStore::JsonFiles,
}
```

* Use `<data>/sessions` — **not** `<data>` — because `<data>` also contains `secrets.json` (API keys), `globalState.json`, `settings/providers.json`. A narrow root is defence in depth: even a buggy filename rule can never reach the secrets file.
* Keep the already-registered legacy VS Code source (`%APPDATA%\<app>\User\globalStorage\saoudrizwan.claude-dev`, `JsonFiles`) — it still carries pre-SDK (Cline ≤3) tasks, and the same root also covers the extension’s `state/taskHistory.json`.
* Optionally add `home.join(".cline").join("data").join("tasks")` (`JsonFiles`) for the CLI/standalone classic task layout (`~/.cline/data/tasks/<taskId>/ui_messages.json`).
* Env overrides (`CLINE_DATA_DIR`) are **not** honoured by Discodex; document that as a known limitation (sandbox/`--data-dir` users are not tracked).

### 7.3 Filename rule (`is_session_document`) — must not collide with Roo/Kilo

Current rule (`src/provider.rs:665-669`):

```rust
Provider::RooCode | Provider::Cline => filename == "ui_messages.json",
```

**Split the arm** and give Cline its own rule:

```rust
Provider::RooCode => filename == "ui_messages.json",   // Roo: classic per-task file only

Provider::Cline => {
    // Legacy per-task UI message log (Cline <= 3).
    if filename == "ui_messages.json" { return true; }
    // SDK-native transcript: <data>/sessions/<sessionId>/<sessionId>.messages.json
    // The stem must equal the containing directory name. That excludes
    // sessions.index.json, subagent-spawn-queue.json and the per-subagent
    // files (<agentId>.messages.json) that live in the ROOT session folder.
    if let Some(dir) = path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) {
        if let Some(stem) = filename.strip_suffix(".messages.json")
            && !stem.is_empty()
            && stem.eq_ignore_ascii_case(dir) {
            return true;
        }
    }
    false
}
```

No collision with Roo (`rooveterinaryinc.roo-cline` root, `ui_messages.json` only) or Kilo (SQLite store, separate provider) — different roots **and** different filename grammars. Kilo Code has no JSON documents at all, so `is_session_document` stays untouched for it.

The manifest `<sessionId>.json` is **not** matched by the rule; it is read by the adapter as a sibling file (§7.5) — this keeps exactly one document type per watched path and avoids the “two documents, two state keys” problem.

### 7.4 Parse mapping (SDK-native transcript + manifest)

`src/sources/cline.rs::parse_document(doc, seen)` — implement as:

**Document discrimination** (the same file could be a transcript or, in future, something else):
* `doc["messages"]` is an array ⇒ transcript.
* otherwise ⇒ unknown ⇒ return `Vec::new()` (fail safe).

**Transcript → events**, iterating `doc["messages"]` in order:

| Raw record | ParsedEvent |
|---|---|
| `role == "user"`, first content block `type == "text"` | `ParsedEvent::TaskStarted { started_at_ms }` — only for a **new** user message (dedupe key), timestamp = manifest `started_at` if available, else file mtime. **The text is never read.** |
| `role == "user"`, block `type == "tool_result"` | `ParsedEvent::TrackedToolFinished { call_id: block["tool_use_id"] }` (the `content` is never read) |
| `role == "assistant"`, block `type == "tool_use"`, `name == "submit_and_exit"` | `ParsedEvent::Completed` (and **no** ToolStarted) |
| `role == "assistant"`, block `type == "tool_use"`, other name | `ParsedEvent::TrackedToolStarted { call_id: block["id"], activity }` with `activity.label` from the Cline classifier and `activity.file` = basename of the tool input path (see §5) |
| `role == "assistant"`, block `type == "thinking"` | `ParsedEvent::Thinking { reasoning: None }` — reasoning text is **never** published |
| `role == "assistant"`, block `type == "text"` only | `ParsedEvent::Thinking { reasoning: None }` (keeps the presence alive). *(Legacy Cline uses `say:"completion_result"` → `TerminalCandidate` instead.)* |
| anything else | no event |

**Manifest → events** (read by the adapter from the sibling `<sessionId>.json`):
* `cwd` → `ParsedEvent::SessionMetadata { project: project_name(cwd), cwd }` — publish the folder name only.
* `model` (+ `provider`) → `ParsedEvent::AgentDetected { agent }` — reuse the `deepseek_model_label` whitelist (ASCII alphanumeric + `-_.:/@`, ≤128 chars) so a malformed manifest can never inject free text into the Discord presence.
* `started_at` (ISO) → `ParsedEvent::TaskStarted { started_at_ms }`.
* `status` ∈ {completed, failed, cancelled} → `ParsedEvent::Completed` (only when the status **changes**, see dedupe).

**Dedupe keys** (`seen: &mut HashSet<String>`), per file:
* tool call: `t:<tool_use.id>` (stable across rewrites — Cline assigns a real call id).
* tool result: `r:<tool_use_id>`.
* user turn: `u:<index>:<sessionId>` — index is safe here because a *user* turn is append-only; compaction truncates from the **front** of the history, and the transcript is rewritten in full, so prefer `u:<message.id>` when `message.id` exists, else fall back to `u:<index>`.
* assistant text/thinking: `m:<message.id>` when present, else `m:<index>:<role>`.
* manifest: `manifest:model:<value>`, `manifest:status:<value>`, `manifest:started:<value>`.
* Subagent files are excluded by the filename rule (§7.3), so no sub-agent noise.

Because the transcript is rewritten **whole**, the adapter must be **idempotent**: a second parse of the same document with the same `seen` set must return no events. The `t:`/`r:` id-based keys make that trivially true.

### 7.5 One state key per Cline session (the correlation problem)

The transcript and the manifest are two different files. If they map to two different `ActivityState` keys, the presence would show tool activity (from the transcript, project = default “AI session”) **without** project/model, because `ActivityState` keys sessions by path (`src/state.rs:104`, `src/state.rs:113`).

Two viable fixes — pick one:

**(A) Canonical session key (recommended).** Add to `src/provider.rs`:

```rust
/// The state key a provider's document contributes to.
pub fn session_key(provider: Provider, path: &Path) -> PathBuf {
    match provider {
        // <data>/sessions/<sessionId>/<sessionId>.messages.json  and  <sessionId>.json
        Provider::Cline => path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(|id| PathBuf::from(format!("cline-session:{}", id.to_ascii_lowercase())))
            .unwrap_or_else(|| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}
```

and in `src/main.rs::apply_document`, use `provider::session_key(provider, path)` as the key passed to `activity.apply(...)` (the `document_state` “seen” map stays keyed by the real file path, so each document is still diffed independently). Result: the manifest’s `SessionMetadata`/`AgentDetected`/`TaskStarted` and the transcript’s tool events all land on **one** session ⇒ project + model + file + phase + timer.

**(B) Sibling-manifest read inside the adapter.** Change the signature to `parse_json_document(provider, path, doc, seen)` (the call site in `main.rs:638` already has the path) and let `cline::parse_document` read `<sessionId>.json` next to the transcript and merge its metadata into the transcript’s events. Works without touching `main.rs`’s keying, but it changes a shared signature (Roo/Continue adapters must be updated) and it re-reads a file on every event.

(A) is smaller and also gives correct sub-agent behaviour later (Cline sub-session ids embed the root id).

### 7.6 Legacy (Cline ≤3) mapping — optional second phase

* `ui_messages.json` (array of `ClineMessage`): `say:"tool"` → `ToolStarted` (the `text` field is a JSON blob whose `path` is already relative to cwd — take the **basename only**, never the `content` field, which for `write_to_file` is the entire new file body); `ask:"command"` → `ToolStarted` classified from the raw command (never published); `say:"command_output"` → `ToolFinished`; `say:"reasoning"` → `Thinking { reasoning: None }`; `say:"text"` → `Thinking { reasoning: None }`; `say:"completion_result"` → `TerminalCandidate`; `ask:"completion_result"` → `Completed`; `ask:"followup"` → `Thinking { reasoning: None }` (waiting for the user); `ts` = epoch milliseconds.
* `state/taskHistory.json` (array of `HistoryItem`) gives `cwdOnTaskInitialization` (⇒ project) and `modelId`/`apiProvider` (⇒ agent) keyed by task id — but it is a **single array for all tasks**, so the adapter must map `id → (cwd, modelId)` and apply it to the matching `tasks/<taskId>/ui_messages.json`. The `task` field there is the user prompt — never read it.

Because upstream v4 no longer writes these files, the legacy path is a “best effort for old installs” bonus. If the parent prefers a single clean implementation, ship the SDK-native path first and treat legacy as a follow-up.

### 7.7 Changes to `watcher.rs` / `main.rs`

| File | Change | Required? |
|---|---|---|
| `src/provider.rs` | new `SessionSource` for `~/.cline/data/sessions` | **yes** |
| `src/provider.rs` | split the `is_session_document` arm; add the Cline rule (§7.3) | **yes** |
| `src/provider.rs` | new `session_key()` (§7.5-A) | **yes** |
| `src/provider.rs` | add `Provider::Cline` to `has_session_tracking()` (with the adapter) | recommended |
| `src/main.rs` | `apply_document` uses `provider::session_key(provider, path)` for `activity.apply` | **yes** (§7.5-A) |
| `src/main.rs` | periodic **rescan** of `JsonFiles` sources | **recommended** (see below) |
| `src/watcher.rs` | `recent_documents` already keeps `tasks` folders and skips dot-dirs — no change needed; but raise the per-source limit from 8 to ~12 so a Cline session’s transcript+manifest are not crowded out by other files | recommended |
| `src/sources/cline.rs` | implement `parse_document` | **yes** |
| `src/sources/mod.rs` | already routes `Provider::Cline ⇒ cline::parse_document` | no change |

**Rescan rationale (the one genuinely new watcher requirement).** `persistSessionMessages` uses `writeFileSync`, so a `notify` event can be delivered while the file is half-written → `serde_json::from_str` fails → `apply_document` returns early → that turn is **lost** until the next write. Two mitigations, both cheap:
1. In `apply_document`, on a JSON parse error, retry once after ~150 ms (or re-read on the *next* notify event for the same path).
2. Add a periodic rescan (mirroring the existing `STORE_POLL` for SQLite): every 2 s, re-read the N most recently modified documents of every `JsonFiles` source and re-diff them through the same `seen` sets. Because the diff is id-based, a rescan emits **no** duplicate events — it only recovers missed ones.

### 7.8 Unit tests to add (6)

In `src/sources/cline.rs` (module `#[cfg(test)] mod tests`) and `src/provider.rs`:

1. `cline_document_rule_matches_only_session_artifacts`
   * `…/.cline/data/sessions/abc/abc.messages.json` → **true**
   * `…/.cline/data/sessions/abc/other.messages.json` (sub-agent file) → **false**
   * `…/.cline/data/sessions/sessions.index.json` → **false**
   * `…/.cline/data/sessions/subagent-spawn-queue.json` → **false**
   * `…/.cline/data/secrets.json` / `globalState.json` → **false**
   * `…/globalStorage/saoudrizwan.claude-dev/tasks/1/ui_messages.json` → **true**
   * `…/globalStorage/rooveterinaryinc.roo-cline/tasks/1/ui_messages.json` under `Provider::Cline` → **false** (no cross-provider collision), and **true** under `Provider::RooCode`.
2. `cline_parses_transcript_into_safe_tracked_tool_events`
   * redacted transcript from §6.1 ⇒ expect `TrackedToolStarted{call_id:"toolu_01ABC", label:"Reading project files", file:Some("main.rs"), language:Some("Rust")}`, then `TrackedToolFinished{call_id:"toolu_01ABC"}`, then `TrackedToolStarted{label:"Editing files", file:Some("events.rs")}`, then `Completed` for `submit_and_exit`.
3. `cline_manifest_metadata_only` (PRIVACY)
   * manifest with `cwd`, `model`, `started_at` ⇒ `SessionMetadata{project:"Discodex"}`, `AgentDetected{"claude-sonnet-4-5"}`, `TaskStarted{…}`; a manifest **without** `model` ⇒ **no** `AgentDetected`; a manifest with `model:"evil prose\ntext"` ⇒ **no** `AgentDetected` (whitelist rejection).
4. `cline_never_forwards_prompt_response_or_tool_output` (MANDATORY PRIVACY)
   * transcript whose user text, assistant text, thinking text, `tool_result.content`, `system_prompt` and manifest `prompt` all contain the marker `SECRET_MARKER_42`; assert that **no** produced `ParsedEvent` renders to a string containing `SECRET_MARKER_42`, that the absolute path `C:\Users\me\Documents\Discodex\src\main.rs` never appears, and that only the basename `main.rs` does.
5. `cline_rewritten_document_is_not_replayed`
   * parse the same document twice with the same `seen` set ⇒ the second call returns `Vec::new()`; then append one new tool_use and parse again ⇒ exactly one new event.
6. `cline_unknown_shapes_degrade_to_no_activity`
   * `{}`, `{"foo":1}`, `{"messages":"nope"}`, a tool_use whose `input` is a string, a tool_result without `tool_use_id` ⇒ **no events, no panic**, and (important) no fabricated file name.
   * Additionally: a document whose `messages` array is **shorter** than before (compaction) must not re-fire old events (id-based keys ⇒ nothing new).

Optional 7th: `cline_legacy_ui_messages_mapping` — legacy `ui_messages.json` ⇒ `ToolStarted`/`ToolFinished`/`Thinking`/`TerminalCandidate`/`Completed` with no text leakage.

### 7.9 Format-churn risk and fail-safe posture

* **Churn is real**: upstream Cline replaced the whole per-task storage model between 3.x and 4.x (`ui_messages.json` → `<sessionId>.messages.json` + `<sessionId>.json`). A 5.x could move again.
* **Fail safe rules for the adapter**:
  1. Unknown document shape ⇒ `Vec::new()` (no activity) — never guess.
  2. Unknown `version` in the manifest (`version != 1`) ⇒ ignore the manifest fields we do not recognise, but still read `cwd`/`model` if present (they are additive).
  3. Unknown tool name ⇒ label `Using development tools`, file `None` — still a valid activity, never a wrong file name.
  4. Missing `cwd` ⇒ fall back to `workspace_root`; both missing ⇒ no `SessionMetadata` (the presence keeps the default project rather than a wrong one).
  5. Model string must pass the ASCII whitelist, else no `AgentDetected`.
  6. A tool input path that is not a string, or that is empty ⇒ `file: None` (never a partial path).
  7. Never publish anything that is not in the allow-list: only the folder name, the basename, the language, the normalized label, the phase, the model id, the start time.
* **Wrong path is worse than “not viable”**: if the `~/.cline/data/sessions` root does not exist, the source is simply absent — Discodex must not fall back to scanning `%APPDATA%` broadly.

### 7.10 Privacy summary for this provider

| Field | Published? |
|---|---|
| provider name (`Cline`) | ✅ |
| workspace folder name (from `cwd`) | ✅ |
| language/framework (from the file extension) | ✅ |
| file **basename** | ✅ |
| normalized activity label | ✅ |
| phase (thinking / running tools) | ✅ |
| task start time | ✅ |
| model id (from the manifest) | ✅ *recommended* — consistent with the Codex/DSH/Antigravity adapters that already publish `agent`; **flag for the parent**: the brief’s strict allow-list does not mention the model, so drop `AgentDetected` if the parent prefers the literal list. |
| user prompt / assistant answer / reasoning text | ❌ never |
| tool arguments / tool output / command text | ❌ never (only the basename + the normalized label) |
| full file paths | ❌ never |
| `secrets.json`, `globalState.json`, `providers.json` | ❌ never ingested (narrow root + strict filename rule) |

---

## 8. Cline vs. its forks (Roo Code, Kilo Code)

| Aspect | **Cline (upstream, v4.1.25)** | **Roo Code (fork)** | **Kilo Code (fork)** |
|---|---|---|---|
| Extension id | `saoudrizwan.claude-dev` | `rooveterinaryinc.roo-cline` | `kilocode.kilo-code` |
| Store root | `~/.cline/data` (+ VS Code globalStorage for legacy) | `%APPDATA%\<app>\User\globalStorage\rooveterinaryinc.roo-cline` | `~\.local\share\kilo` (SQLite) |
| Layout (current) | `<data>/sessions/<id>/<id>.messages.json` + `<id>.json` (manifest) + `<data>/db/sessions.db` | `tasks/<taskId>/ui_messages.json` + `api_conversation_history.json` + `history_item.json` | one SQLite DB |
| Layout (legacy ≤3) | `tasks/<taskId>/ui_messages.json` + `api_conversation_history.json` + `state/taskHistory.json` | same as legacy Cline | – |
| Record format | JSON object `{version, updated_at, agent, sessionId, origin, messages[]}` (rewritten whole) | JSON array of `ClineMessage` (rewritten whole) | SQLite rows |
| **Model id persisted?** | **YES** — manifest `.model`, `modelInfo.id` per message, SQLite `model`, legacy `HistoryItem.modelId` | **NO** (per the peer report) | – |
| Tool call id | **YES** (`tool_use.id` / `tool_result.tool_use_id`) ⇒ `TrackedToolStarted/TrackedToolFinished` | no call id ⇒ plain `ToolStarted/ToolFinished` | – |
| Tool vocabulary | `read_files`, `editor`, `apply_patch`, `run_commands`, `search_codebase`, `fetch_web_content`, `skills`, `ask_question`, `submit_and_exit` | Cline-3 names: `read_file`, `write_to_file`, `replace_in_file`, `execute_command`, … | – |
| Completion signal | `submit_and_exit` tool, manifest `status` | `ask/say:"completion_result"` | – |
| Write semantics | whole-file rewrite | whole-file rewrite | row insert |

**Consequence:** one adapter cannot serve Cline and Roo with a single document grammar — the record shapes differ (`{messages:[…]}` vs `[{ts,type,ask|say,…}]`) and the tool vocabularies differ. They share only the *ingestion strategy* (`SessionStore::JsonFiles` + re-read + id-dedupe), which the repo already has. Keep them as separate providers with separate adapters (as the repo already does), but share:
* the `ToolActivity` construction helpers,
* the privacy whitelist for model ids,
* the basename extraction (Cline needs extra shapes: `input.files[].path`, `input.path`, `apply_patch` body).

---

## 9. Feasibility verdict

### **RICH**

Cline exposes everything the Codex/DSH level of richness needs, **as metadata only**:

* ✅ project (workspace folder name from the manifest `cwd`)
* ✅ current file basename (from the tool input)
* ✅ tool activity (normalized label from the tool name; command classification from the raw command, published only as a fixed label)
* ✅ phase (tool call open ⇒ *Running tools*; no open call ⇒ *Thinking*; `submit_and_exit` ⇒ *Completed*)
* ✅ task start timer (manifest `started_at`)
* ✅ **model id** (manifest `model` + per-message `modelInfo.id`) — a capability the Roo fork does **not** have
* ✅ tracked tool start/finish pairing (Cline has real tool call ids)

**Caveat that shapes the implementation (not the verdict):** Cline’s store is **not** append-only, so the `LogTailer` path is unusable and the adapter must use the repo’s existing `JsonFiles` path (re-read + id-dedupe), plus a periodic rescan to survive a notify event that lands mid-rewrite.

**Confidence:** **High** on paths, formats, write semantics, field availability and the model-id claim — all read directly from the upstream `cline/cline` tree at `main` (v4.1.25 / CLI 3.0.73) and cross-checked against the published Marketplace version. **Medium** on two operational details: (a) how often `persistSessionMessages` is called (it is called per turn, and every call rewrites the file — but the exact cadence across a turn is not observable from source alone), and (b) whether the VS Code extension always uses the SDK store for *new* tasks (the code says yes — “SDK session persistence owns conversation history” — but a very old extension build or a partially rolled-out SDK migration could still produce classic files; that is exactly why the legacy path is kept as a fallback).

---

## 10. Evidence

Upstream source (all paths relative to `https://github.com/cline/cline/blob/main/`):
* `apps/vscode/package.json` — extension id / version
* `apps/vscode/src/extension.ts` (L591‑601, L715‑716) — `globalStorageUri` wiring, `taskHistory.json` migration
* `apps/vscode/src/hosts/host-provider.ts` (L45‑46, L71‑97) — `globalStorageFsPath`
* `apps/vscode/src/hosts/vscode/vscode-to-file-migration.ts` (L1‑33) — legacy VS Code path, `~/.cline/data` migration status
* `apps/vscode/src/core/storage/disk.ts` (L17‑36, L51‑53, L155‑184, L196‑231) — `GlobalFileNames`, task dir, `api_conversation_history.json`, `task_metadata.json`, `settings.json`
* `apps/vscode/src/core/controller/file/openDiskConversationHistory.ts` — `{globalStorage}/tasks/<id>/api_conversation_history.json`
* `apps/vscode/src/sdk/legacy-state-reader.ts` (L22‑75, L149‑160) — `~/.cline/data/{globalState,secrets}.json`, `state/taskHistory.json`, `tasks/<id>/…`
* `apps/vscode/src/sdk/sdk-task-history.ts` (L24‑52, L103‑145, L200‑228, L544‑559) — HistoryItem ⇄ session metadata, `modelId`, `cwdOnTaskInitialization`, session dir
* `apps/vscode/src/sdk/sdk-task-control-coordinator.ts` (L141‑150) — “SDK session persistence owns conversation history. Do not write classic ui_messages.json here”
* `apps/vscode/src/shared/storage/storage-context.ts` (L66‑147) — `~/.cline/data` layout, `resolveDataDirFromEnv`
* `apps/vscode/src/shared/ExtensionMessage.ts` (L191‑221, L223‑279) — `ClineMessage`, `ClineAsk`, `ClineSay`
* `apps/vscode/src/shared/HistoryItem.ts` (L3‑36) — legacy history fields incl. `modelId`, `apiProvider`, `cwdOnTaskInitialization`
* `apps/vscode/src/standalone/vscode-context.ts` (L18‑19, L52‑53) — standalone host data dir
* `apps/vscode/src/dev/commands/tasks.ts` (L70‑71, L96, L119) — dev tool writing `ui_messages.json`
* `apps/cli/package.json` — `@cline/cli` 3.0.73, `bin.cline`
* `apps/cli/README.md` (L265, L361) — `--data-dir`, `CLINE_DATA_DIR`
* `apps/cli/src/utils/helpers.ts` (L596) — `CLINE_SESSION_DATA_DIR`
* `sdk/packages/shared/src/storage/paths.ts` (L158‑200, L247‑253) — `resolveClineDataDir`, `resolveSessionDataDir`, `resolveDbDataDir`
* `sdk/packages/shared/src/llms/messages.ts` (L19‑177) — content block and message schemas
* `sdk/packages/core/src/runtime/host/host.ts` (L17‑97, L116‑120) — backend selection (SQLite → file fallback)
* `sdk/packages/core/src/runtime/host/local-runtime-host.ts` (L539‑580) — manifest construction, message/manifest paths
* `sdk/packages/core/src/session/models/session-manifest.ts` (L6‑28) — manifest schema
* `sdk/packages/core/src/session/services/session-service.ts` (L20‑70) — SQLite index + `resolveSessionDataDir()` artifacts
* `sdk/packages/core/src/session/services/file-session-service.ts` (L19‑22, L44‑48, L62‑88) — `sessions.index.json`
* `sdk/packages/core/src/session/services/persistence-service.ts` (L103‑184) — root session + manifest creation
* `sdk/packages/core/src/session/stores/session-manifest-store.ts` (L104‑119, L220‑255, L305‑341) — **whole-file rewrite** of the transcript, manifest write, compaction
* `sdk/packages/core/src/session/stores/atomic-file.ts` — atomic temp+rename
* `sdk/packages/core/src/services/session-artifacts.ts` (L60‑144) — `sessionMessagesPath`, `sessionManifestPath`, subagent paths
* `sdk/packages/core/src/services/session-data.ts` (L281‑380) — messages-file payload shape
* `sdk/packages/core/src/services/storage/sqlite-session-store.ts` (L25‑120) — `sessions.db` path and columns
* `sdk/packages/core/src/extensions/tools/definitions.ts` (L268‑289, L364‑384, L530, L576, L662‑704, L711‑767, L774‑789, L835, L859) — tool names
* `sdk/packages/core/src/extensions/tools/schemas.ts` (L44‑106, L111‑190, L194‑259) — tool input schemas
* `docs/enterprise-solutions/monitoring/prompt-storage.mdx` (L10, L46‑77) — documented `~/.cline/data/tasks/<taskId>/api_conversation_history.json`

URLs:
* https://github.com/cline/cline
* https://github.com/cline/cline/blob/main/apps/vscode/src/core/storage/disk.ts
* https://github.com/cline/cline/blob/main/apps/vscode/src/sdk/legacy-state-reader.ts
* https://github.com/cline/cline/blob/main/apps/vscode/src/sdk/sdk-task-history.ts
* https://github.com/cline/cline/blob/main/apps/vscode/src/sdk/sdk-task-control-coordinator.ts
* https://github.com/cline/cline/blob/main/apps/vscode/src/shared/HistoryItem.ts
* https://github.com/cline/cline/blob/main/apps/vscode/src/shared/ExtensionMessage.ts
* https://github.com/cline/cline/blob/main/apps/vscode/src/hosts/vscode/vscode-to-file-migration.ts
* https://github.com/cline/cline/blob/main/apps/vscode/src/shared/storage/storage-context.ts
* https://github.com/cline/cline/blob/main/apps/vscode/package.json
* https://github.com/cline/cline/blob/main/apps/cli/package.json
* https://github.com/cline/cline/blob/main/apps/cli/README.md
* https://github.com/cline/cline/blob/main/sdk/packages/shared/src/storage/paths.ts
* https://github.com/cline/cline/blob/main/sdk/packages/shared/src/llms/messages.ts
* https://github.com/cline/cline/blob/main/sdk/packages/core/src/session/models/session-manifest.ts
* https://github.com/cline/cline/blob/main/sdk/packages/core/src/session/stores/session-manifest-store.ts
* https://github.com/cline/cline/blob/main/sdk/packages/core/src/session/services/session-service.ts
* https://github.com/cline/cline/blob/main/sdk/packages/core/src/session/services/file-session-service.ts
* https://github.com/cline/cline/blob/main/sdk/packages/core/src/services/session-artifacts.ts
* https://github.com/cline/cline/blob/main/sdk/packages/core/src/services/session-data.ts
* https://github.com/cline/cline/blob/main/sdk/packages/core/src/services/storage/sqlite-session-store.ts
* https://github.com/cline/cline/blob/main/sdk/packages/core/src/runtime/host/host.ts
* https://github.com/cline/cline/blob/main/sdk/packages/core/src/extensions/tools/definitions.ts
* https://github.com/cline/cline/blob/main/sdk/packages/core/src/extensions/tools/schemas.ts
* https://github.com/cline/cline/blob/main/docs/enterprise-solutions/monitoring/prompt-storage.mdx
* https://marketplace.visualstudio.com/items?itemName=saoudrizwan.claude-dev (version 4.1.25 confirmed)
* https://cdn.jsdelivr.net/npm/@lobehub/icons-static-png@1.97.0/dark/cline.png (HTTP 200, image/png, 5360 B)

Fork evidence (secondary, used only for the comparison table in §8): the peer Roo Code / Kilo Code reports in `research/roo-code.json` and `research/kilo-code.json` (fork behaviour, not upstream Cline).
