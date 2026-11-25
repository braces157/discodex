//! T3 Code adapter: durable orchestration events in `statev2.sqlite`
//!
//! T3 Code persists thread, project, run, and turn-item updates in a local
//! SQLite database (`%USERPROFILE%\.t3\userdata\statev2.sqlite`, WAL mode).
//! The adapter reads only structural metadata: workspace folder name, inferred
//! stack/language, file basename, normalized tool activity label, validated
//! model identifier, and timestamps. Prompts, assistant messages, reasoning
//! prose, thread titles, raw commands, tool output, and full paths never leave
//! this module.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;

use super::{StoreEvent, column_text_opt, open_database};
use crate::events::{
    ParsedEvent, ToolActivity, basename, classify_tool, extract_file_basename, language_from_file,
    project_name, timestamp_ms,
};

/// Maximum orchestration events read per batch.
const ROW_LIMIT: i64 = 200;

/// Maximum incremental batches drained in a single poll.
const MAX_BATCHES_PER_POLL: usize = 50;

/// Orchestration event tables in priority order.
const EVENT_TABLES: &[&str] = &["orchestration_events", "orchestration_v2_events"];

/// Ignore historical events older than 15 minutes during cold startup so a
/// crashed or force-closed run never surfaces as live presence.
const STALE_BOOTSTRAP_MS: i64 = 900_000;

/// Candidate SQLite database paths relative to the T3 Code userdata root.
const DATABASE_PATHS: &[&str] = &[
    "statev2.sqlite",
    "userdata/statev2.sqlite",
    "state.sqlite",
    "userdata/state.sqlite",
];

#[derive(Clone, Default)]
struct ThreadContext {
    cwd: Option<String>,
    model: Option<String>,
}

struct EventRow {
    sequence: i64,
    event_id: Option<String>,
    stream_id: Option<String>,
    aggregate_kind: Option<String>,
    event_type: String,
    occurred_at: Option<String>,
    payload: Option<Value>,
}

/// Databases that hold T3 Code activity below the given root.
pub fn databases(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for relative in DATABASE_PATHS {
        let candidate = root.join(relative);
        if candidate.is_file() && !found.contains(&candidate) {
            found.push(candidate);
        }
    }
    found
}

/// Poll the T3 Code database for orchestration events written since the last poll.
pub fn poll(db: &Path, seen: &mut HashSet<String>) -> Vec<StoreEvent> {
    poll_at(db, seen, crate::now_ms())
}

struct PollContext<'a> {
    connection: &'a Connection,
    seen: &'a mut HashSet<String>,
    thread_cache: HashMap<String, ThreadContext>,
    project_cache: HashMap<String, Option<String>>,
    out: Vec<StoreEvent>,
}

impl<'a> PollContext<'a> {
    fn process_rows(
        &mut self,
        rows: Vec<EventRow>,
        initial_poll: bool,
        now_ms: i64,
        cursor_seq: &mut i64,
    ) {
        for row in rows {
            *cursor_seq = (*cursor_seq).max(row.sequence);

            let key = row
                .event_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .map(|id| format!("event:{id}"))
                .unwrap_or_else(|| format!("seq:{}", row.sequence));
            if !self.seen.insert(key) {
                continue;
            }

            if row
                .aggregate_kind
                .as_deref()
                .is_some_and(|kind| !kind.eq_ignore_ascii_case("thread"))
            {
                continue;
            }

            let Some(payload) = row.payload.as_ref() else {
                continue;
            };
            let Some(thread_id) = resolve_thread_id(&row, payload) else {
                continue;
            };

            let occurred_ms = row.occurred_at.as_deref().and_then(parse_timestamp_str);
            if initial_poll
                && occurred_ms.is_some_and(|ts| now_ms.saturating_sub(ts) > STALE_BOOTSTRAP_MS)
            {
                continue;
            }

            let ctx = resolve_thread_context(
                self.connection,
                &thread_id,
                &mut self.thread_cache,
                &mut self.project_cache,
            );
            emit_thread_context(&thread_id, &ctx, payload, self.seen, &mut self.out);

            for event in translate_event(&row.event_type, payload, occurred_ms, now_ms, self.seen) {
                self.out.push(StoreEvent {
                    session: thread_id.clone(),
                    event,
                });
            }
        }
    }
}

fn poll_at(db: &Path, seen: &mut HashSet<String>, now_ms: i64) -> Vec<StoreEvent> {
    let Ok(connection) = open_database(db) else {
        return Vec::new();
    };

    let existing_cursor = parse_cursor(seen);
    let initial_poll = existing_cursor.is_none() && seen.is_empty();
    let mut ctx = PollContext {
        connection: &connection,
        seen,
        thread_cache: HashMap::new(),
        project_cache: HashMap::new(),
        out: Vec::new(),
    };

    match existing_cursor {
        None => {
            let Some((table, mut rows)) = read_bootstrap_rows(ctx.connection) else {
                return Vec::new();
            };
            rows.reverse();
            let mut max_seq = -1_i64;
            ctx.process_rows(rows, initial_poll, now_ms, &mut max_seq);
            store_cursor(ctx.seen, table, max_seq);
        }
        Some((table, mut cursor_seq)) => {
            if let Some(max_db_seq) = max_sequence(ctx.connection, table)
                && max_db_seq < cursor_seq
            {
                cursor_seq = -1;
                store_cursor(ctx.seen, table, cursor_seq);
            }
            for _ in 0..MAX_BATCHES_PER_POLL {
                let Some(rows) = read_event_batch(ctx.connection, table, cursor_seq) else {
                    break;
                };
                if rows.is_empty() {
                    break;
                }
                let batch_len = rows.len() as i64;
                ctx.process_rows(rows, false, now_ms, &mut cursor_seq);
                store_cursor(ctx.seen, table, cursor_seq);
                if batch_len < ROW_LIMIT {
                    break;
                }
            }
        }
    }

    ctx.out
}

fn parse_cursor(seen: &HashSet<String>) -> Option<(&'static str, i64)> {
    seen.iter().find_map(|entry| {
        let rest = entry.strip_prefix("cursor:")?;
        let (table_raw, seq_raw) = rest.rsplit_once(':')?;
        let table = EVENT_TABLES.iter().copied().find(|&t| t == table_raw)?;
        let seq = seq_raw.parse::<i64>().ok()?;
        Some((table, seq))
    })
}

fn store_cursor(seen: &mut HashSet<String>, table: &str, sequence: i64) {
    let exact = format!("cursor:{table}:{sequence}");
    if !seen.contains(&exact) {
        seen.retain(|entry| !entry.starts_with("cursor:"));
        seen.insert(exact);
    }
}

fn max_sequence(connection: &Connection, table: &str) -> Option<i64> {
    let query = format!("SELECT COALESCE(MAX(sequence), -1) FROM {table}");
    let mut stmt = connection.prepare(&query).ok()?;
    stmt.query_row([], |row| row.get::<_, i64>(0)).ok()
}

fn read_bootstrap_rows(connection: &Connection) -> Option<(&'static str, Vec<EventRow>)> {
    for &table in EVENT_TABLES {
        let query = format!("SELECT * FROM {table} ORDER BY sequence DESC LIMIT ?");
        let Ok(mut stmt) = connection.prepare(&query) else {
            continue;
        };
        let Ok(rows) = stmt.query(rusqlite::params![ROW_LIMIT]) else {
            continue;
        };
        let parsed = collect_event_rows(rows);
        if !parsed.is_empty() {
            return Some((table, parsed));
        }
    }
    None
}

fn read_event_batch(connection: &Connection, table: &str, after_seq: i64) -> Option<Vec<EventRow>> {
    let query = format!("SELECT * FROM {table} WHERE sequence > ? ORDER BY sequence ASC LIMIT ?");
    let mut stmt = connection.prepare(&query).ok()?;
    let rows = stmt.query(rusqlite::params![after_seq, ROW_LIMIT]).ok()?;
    Some(collect_event_rows(rows))
}

fn collect_event_rows(mut rows: rusqlite::Rows<'_>) -> Vec<EventRow> {
    let mut out = Vec::new();
    while let Ok(Some(row)) = rows.next() {
        let Some(event_type) = column_text_opt(row, "event_type") else {
            continue;
        };
        let sequence = column_text_opt(row, "sequence")
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let event_id = column_text_opt(row, "event_id");
        let stream_id =
            column_text_opt(row, "thread_id").or_else(|| column_text_opt(row, "stream_id"));
        let aggregate_kind = column_text_opt(row, "aggregate_kind");
        let occurred_at = column_text_opt(row, "occurred_at");
        let payload =
            column_text_opt(row, "payload_json").and_then(|s| serde_json::from_str(&s).ok());
        out.push(EventRow {
            sequence,
            event_id,
            stream_id,
            aggregate_kind,
            event_type,
            occurred_at,
            payload,
        });
    }
    out
}

fn resolve_thread_id(row: &EventRow, payload: &Value) -> Option<String> {
    payload
        .get("threadId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| row.stream_id.clone().filter(|s| !s.is_empty()))
}

fn resolve_thread_context(
    connection: &Connection,
    thread_id: &str,
    thread_cache: &mut HashMap<String, ThreadContext>,
    project_cache: &mut HashMap<String, Option<String>>,
) -> ThreadContext {
    if let Some(cached) = thread_cache.get(thread_id) {
        return cached.clone();
    }

    let mut ctx = ThreadContext::default();
    let mut project_id: Option<String> = None;

    for query in [
        "SELECT project_id, payload_json FROM orchestration_v2_projection_threads WHERE thread_id = ? LIMIT 1",
        "SELECT project_id, worktree_path, model_selection_json FROM projection_threads WHERE thread_id = ? LIMIT 1",
    ] {
        let Ok(mut stmt) = connection.prepare(query) else {
            continue;
        };
        let Ok(mut rows) = stmt.query(rusqlite::params![thread_id]) else {
            continue;
        };
        if let Ok(Some(row)) = rows.next() {
            project_id = column_text_opt(row, "project_id");
            if let Some(worktree) =
                column_text_opt(row, "worktree_path").filter(|s| !s.trim().is_empty())
            {
                ctx.cwd = Some(worktree);
            }
            if let Some(payload) = column_text_opt(row, "payload_json")
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            {
                if ctx.cwd.is_none()
                    && let Some(wt) = payload
                        .get("worktreePath")
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                {
                    ctx.cwd = Some(wt.to_string());
                }
                if let Some(sel) = payload.get("modelSelection") {
                    ctx.model = extract_model_label(sel);
                }
            } else if let Some(sel) = column_text_opt(row, "model_selection_json")
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            {
                ctx.model = extract_model_label(&sel);
            }
            break;
        }
    }

    if ctx.cwd.is_none()
        && let Some(pid) = project_id
    {
        ctx.cwd = resolve_project_workspace(connection, &pid, project_cache);
    }

    thread_cache.insert(thread_id.to_string(), ctx.clone());
    ctx
}

fn resolve_project_workspace(
    connection: &Connection,
    project_id: &str,
    project_cache: &mut HashMap<String, Option<String>>,
) -> Option<String> {
    if let Some(cached) = project_cache.get(project_id) {
        return cached.clone();
    }
    let workspace = connection
        .prepare("SELECT workspace_root FROM projection_projects WHERE project_id = ? LIMIT 1")
        .ok()
        .and_then(|mut stmt| {
            stmt.query_row(rusqlite::params![project_id], |row| {
                Ok(column_text_opt(row, "workspace_root"))
            })
            .ok()
            .flatten()
        })
        .filter(|s| !s.trim().is_empty());
    project_cache.insert(project_id.to_string(), workspace.clone());
    workspace
}

fn update_thread_state(seen: &mut HashSet<String>, prefix: &str, value: &str) -> bool {
    let exact = format!("{prefix}{value}");
    if seen.contains(&exact) {
        return false;
    }
    seen.retain(|entry| !entry.starts_with(prefix));
    seen.insert(exact);
    true
}

fn emit_thread_context(
    thread_id: &str,
    ctx: &ThreadContext,
    payload: &Value,
    seen: &mut HashSet<String>,
    out: &mut Vec<StoreEvent>,
) {
    let cwd = payload
        .get("worktreePath")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .or_else(|| ctx.cwd.clone());

    if let Some(cwd) = cwd
        && let Some(project) = project_name(&cwd)
        && update_thread_state(seen, &format!("meta:{thread_id}:"), &project)
    {
        out.push(StoreEvent {
            session: thread_id.to_string(),
            event: ParsedEvent::SessionMetadata { project, cwd },
        });
    }

    let model = payload
        .get("modelSelection")
        .and_then(extract_model_label)
        .or_else(|| ctx.model.clone());
    if let Some(agent) = model
        && update_thread_state(seen, &format!("agent:{thread_id}:"), &agent)
    {
        out.push(StoreEvent {
            session: thread_id.to_string(),
            event: ParsedEvent::AgentDetected { agent },
        });
    }
}

fn translate_event(
    event_type: &str,
    payload: &Value,
    occurred_ms: Option<i64>,
    now_ms: i64,
    seen: &mut HashSet<String>,
) -> Vec<ParsedEvent> {
    match event_type {
        "run.created" | "run.updated" => {
            let status = payload
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let run_id = payload.get("id").and_then(Value::as_str).unwrap_or("");
            if matches!(status.as_str(), "preparing" | "starting" | "running") {
                if !run_id.is_empty() && !seen.insert(format!("run_started:{run_id}")) {
                    return Vec::new();
                }
                let started_at_ms = payload
                    .get("startedAt")
                    .and_then(timestamp_ms)
                    .or_else(|| payload.get("requestedAt").and_then(timestamp_ms))
                    .or(occurred_ms)
                    .unwrap_or(now_ms);
                vec![ParsedEvent::TaskStarted { started_at_ms }]
            } else if matches!(
                status.as_str(),
                "completed"
                    | "failed"
                    | "cancelled"
                    | "interrupted"
                    | "errored"
                    | "stopped"
                    | "waiting"
            ) {
                vec![ParsedEvent::Completed]
            } else {
                Vec::new()
            }
        }
        "turn-item.updated" => translate_turn_item(payload),
        "provider-turn.updated" => {
            let status = payload
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            match status.as_str() {
                "running" => vec![ParsedEvent::Thinking { reasoning: None }],
                "completed" | "failed" | "cancelled" | "interrupted" | "errored" => {
                    vec![ParsedEvent::TerminalCandidate]
                }
                _ => Vec::new(),
            }
        }
        "node.updated" => {
            let kind = payload
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let status = payload
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            if kind == "reasoning" && status == "running" {
                vec![ParsedEvent::Thinking { reasoning: None }]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

fn translate_turn_item(payload: &Value) -> Vec<ParsedEvent> {
    let item_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let call_id = payload
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());

    let is_active = matches!(status.as_str(), "running" | "pending" | "in_progress");
    let is_done = matches!(
        status.as_str(),
        "completed" | "failed" | "cancelled" | "errored" | "stopped"
    );

    match item_type.as_str() {
        "command_execution" => {
            if is_active {
                let command = payload.get("input").and_then(Value::as_str).unwrap_or("");
                let activity = ToolActivity {
                    label: classify_tool("shell", command).to_string(),
                    file: None,
                    language: None,
                };
                match call_id {
                    Some(id) => vec![ParsedEvent::TrackedToolStarted {
                        call_id: id.to_string(),
                        activity,
                    }],
                    None => vec![ParsedEvent::ToolStarted { activity }],
                }
            } else if is_done {
                match call_id {
                    Some(id) => vec![ParsedEvent::TrackedToolFinished {
                        call_id: id.to_string(),
                    }],
                    None => vec![ParsedEvent::ToolFinished],
                }
            } else {
                Vec::new()
            }
        }
        "dynamic_tool" | "mcp_tool" => {
            if is_active {
                let name = payload
                    .get("toolName")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_ascii_lowercase();
                let input = payload.get("input").unwrap_or(&Value::Null);
                let command = input
                    .get("CommandLine")
                    .or_else(|| input.get("command"))
                    .or_else(|| input.get("cmd"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let file = extract_file_basename(input);
                let language = file
                    .as_deref()
                    .and_then(language_from_file)
                    .map(str::to_string);
                let activity = ToolActivity {
                    label: classify_tool(&name, command).to_string(),
                    file,
                    language,
                };
                match call_id {
                    Some(id) => vec![ParsedEvent::TrackedToolStarted {
                        call_id: id.to_string(),
                        activity,
                    }],
                    None => vec![ParsedEvent::ToolStarted { activity }],
                }
            } else if is_done {
                match call_id {
                    Some(id) => vec![ParsedEvent::TrackedToolFinished {
                        call_id: id.to_string(),
                    }],
                    None => vec![ParsedEvent::ToolFinished],
                }
            } else {
                Vec::new()
            }
        }
        "file_search" | "web_search" => {
            if is_active {
                let label = if item_type == "web_search" {
                    "Browsing documentation"
                } else {
                    "Searching project"
                };
                let activity = ToolActivity {
                    label: label.to_string(),
                    file: None,
                    language: None,
                };
                match call_id {
                    Some(id) => vec![ParsedEvent::TrackedToolStarted {
                        call_id: id.to_string(),
                        activity,
                    }],
                    None => vec![ParsedEvent::ToolStarted { activity }],
                }
            } else if is_done {
                match call_id {
                    Some(id) => vec![ParsedEvent::TrackedToolFinished {
                        call_id: id.to_string(),
                    }],
                    None => vec![ParsedEvent::ToolFinished],
                }
            } else {
                Vec::new()
            }
        }
        "file_change" => {
            if is_active {
                let file = payload
                    .get("fileName")
                    .and_then(Value::as_str)
                    .and_then(basename)
                    .or_else(|| payload.get("input").and_then(extract_file_basename));
                let language = file
                    .as_deref()
                    .and_then(language_from_file)
                    .map(str::to_string);
                let activity = ToolActivity {
                    label: "Editing files".to_string(),
                    file,
                    language,
                };
                match call_id {
                    Some(id) => vec![ParsedEvent::TrackedToolStarted {
                        call_id: id.to_string(),
                        activity,
                    }],
                    None => vec![ParsedEvent::ToolStarted { activity }],
                }
            } else if is_done {
                match call_id {
                    Some(id) => vec![ParsedEvent::TrackedToolFinished {
                        call_id: id.to_string(),
                    }],
                    None => vec![ParsedEvent::ToolFinished],
                }
            } else {
                Vec::new()
            }
        }
        "reasoning" | "assistant_message" if is_active => {
            vec![ParsedEvent::Thinking { reasoning: None }]
        }
        _ => Vec::new(),
    }
}

fn parse_timestamp_str(raw: &str) -> Option<i64> {
    timestamp_ms(&Value::String(raw.to_string()))
}

fn extract_model_label(selection: &Value) -> Option<String> {
    let model = selection.get("model").and_then(Value::as_str)?;
    if model.is_empty()
        || model.len() > 128
        || !model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:/@".contains(&b))
    {
        return None;
    }

    let effort = extract_effort_option(selection.get("options"));
    Some(match effort {
        Some(effort) => format!("{model} ({effort})"),
        None => model.to_string(),
    })
}

fn extract_effort_option(options: Option<&Value>) -> Option<String> {
    let options = options?;
    let raw = if let Some(arr) = options.as_array() {
        arr.iter().find_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str)?;
            matches!(
                id,
                "thought_level" | "effort" | "reasoning_effort" | "reasoningEffort"
            )
            .then(|| entry.get("value").and_then(Value::as_str))
            .flatten()
        })
    } else if let Some(map) = options.as_object() {
        [
            "thought_level",
            "effort",
            "reasoning_effort",
            "reasoningEffort",
        ]
        .into_iter()
        .find_map(|key| map.get(key).and_then(Value::as_str))
    } else {
        None
    }?;

    let valid = !raw.is_empty()
        && raw.len() <= 24
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b));
    valid.then(|| raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TempDb {
        dir: PathBuf,
        path: PathBuf,
    }

    impl TempDb {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "discodex-t3-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("statev2.sqlite");
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE projection_projects (
                    project_id TEXT PRIMARY KEY,
                    workspace_root TEXT NOT NULL
                 );
                 CREATE TABLE orchestration_v2_projection_threads (
                    thread_id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL,
                    payload_json TEXT NOT NULL
                 );
                 CREATE TABLE orchestration_events (
                    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                    event_id TEXT NOT NULL UNIQUE,
                    aggregate_kind TEXT NOT NULL,
                    stream_id TEXT NOT NULL,
                    event_type TEXT NOT NULL,
                    occurred_at TEXT NOT NULL,
                    payload_json TEXT NOT NULL
                 );",
            )
            .unwrap();
            Self { dir, path }
        }

        fn conn(&self) -> Connection {
            Connection::open(&self.path).unwrap()
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn databases_discovers_statev2_and_skips_wal_sidecars() {
        let tmp = TempDb::new("discover");
        std::fs::write(tmp.dir.join("statev2.sqlite-wal"), b"wal").unwrap();
        std::fs::write(tmp.dir.join("server-owner.sqlite"), b"").unwrap();
        assert_eq!(databases(&tmp.dir), vec![tmp.path.clone()]);
    }

    #[test]
    fn polls_runs_tools_files_and_strips_private_text() {
        let tmp = TempDb::new("poll-events");
        let conn = tmp.conn();
        conn.execute(
            "INSERT INTO projection_projects (project_id, workspace_root) VALUES (?1, ?2)",
            rusqlite::params!["proj-1", r"C:\Users\Alice\Documents\Discodex"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO orchestration_v2_projection_threads (thread_id, project_id, payload_json) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "thread-1",
                "proj-1",
                r#"{"id":"thread-1","title":"SECRET_THREAD_TITLE","modelSelection":{"instanceId":"antigravity","model":"gemini-4-neon","options":[{"id":"thought_level","value":"xhigh"}]}}"#
            ],
        )
        .unwrap();

        let events = [
            (
                "evt-1",
                "thread",
                "thread-1",
                "run.created",
                "2026-11-25T10:00:00.000Z",
                r#"{"id":"run-1","threadId":"thread-1","status":"starting","modelSelection":{"model":"gemini-4-neon","options":[{"id":"thought_level","value":"xhigh"}]},"requestedAt":"2026-11-25T10:00:00.000Z"}"#,
            ),
            (
                "evt-2",
                "thread",
                "thread-1",
                "turn-item.updated",
                "2026-11-25T10:00:01.000Z",
                r#"{"id":"call-read","threadId":"thread-1","type":"dynamic_tool","status":"running","toolName":"Read","title":"SECRET_TOOL_TITLE","input":{"Path":"C:\\Users\\Alice\\Documents\\Discodex\\src\\provider.rs"}}"#,
            ),
            (
                "evt-3",
                "thread",
                "thread-1",
                "turn-item.updated",
                "2026-11-25T10:00:02.000Z",
                r#"{"id":"call-cmd","threadId":"thread-1","type":"command_execution","status":"running","input":"cargo test --secret-token=TOPSECRET","output":"PRIVATE_OUTPUT"}"#,
            ),
            (
                "evt-4",
                "thread",
                "thread-1",
                "turn-item.updated",
                "2026-11-25T10:00:03.000Z",
                r#"{"id":"call-read","threadId":"thread-1","type":"dynamic_tool","status":"completed","toolName":"Read"}"#,
            ),
            (
                "evt-5",
                "thread",
                "thread-1",
                "turn-item.updated",
                "2026-11-25T10:00:04.000Z",
                r#"{"id":"call-edit","threadId":"thread-1","type":"file_change","status":"running","fileName":"C:\\Users\\Alice\\Documents\\Discodex\\src\\main.rs"}"#,
            ),
            (
                "evt-5b",
                "thread",
                "thread-1",
                "turn-item.updated",
                "2026-11-25T10:00:04.500Z",
                r#"{"id":"call-search","threadId":"thread-1","type":"file_search","status":"running","title":"SECRET_QUERY"}"#,
            ),
            (
                "evt-6",
                "thread",
                "thread-1",
                "turn-item.updated",
                "2026-11-25T10:00:05.000Z",
                r#"{"id":"msg-1","threadId":"thread-1","type":"reasoning","status":"running","text":"SECRET_REASONING_BODY"}"#,
            ),
            (
                "evt-7",
                "thread",
                "thread-1",
                "run.updated",
                "2026-11-25T10:00:06.000Z",
                r#"{"id":"run-1","threadId":"thread-1","status":"completed"}"#,
            ),
        ];

        for (id, kind, sid, etype, at, pj) in events {
            conn.execute(
                "INSERT INTO orchestration_events (event_id, aggregate_kind, stream_id, event_type, occurred_at, payload_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![id, kind, sid, etype, at, pj],
            )
            .unwrap();
        }

        let now = parse_timestamp_str("2026-11-25T10:00:10.000Z").unwrap();
        let mut seen = HashSet::new();
        let polled = poll_at(&tmp.path, &mut seen, now);
        let serialized = format!("{polled:?}");

        assert!(!serialized.contains("SECRET_THREAD_TITLE"));
        assert!(!serialized.contains("SECRET_TOOL_TITLE"));
        assert!(!serialized.contains("TOPSECRET"));
        assert!(!serialized.contains("PRIVATE_OUTPUT"));
        assert!(!serialized.contains("SECRET_REASONING_BODY"));
        assert!(!serialized.contains("SECRET_QUERY"));

        assert!(polled.iter().any(|e| matches!(
            &e.event,
            ParsedEvent::SessionMetadata { project, .. } if project == "Discodex"
        )));
        assert!(polled.iter().any(|e| matches!(
            &e.event,
            ParsedEvent::AgentDetected { agent } if agent == "gemini-4-neon (xhigh)"
        )));
        assert!(polled.iter().any(|e| matches!(
            &e.event,
            ParsedEvent::TrackedToolStarted { call_id, activity }
                if call_id == "call-read"
                    && activity.label == "Reading project files"
                    && activity.file.as_deref() == Some("provider.rs")
                    && activity.language.as_deref() == Some("Rust")
        )));
        assert!(polled.iter().any(|e| matches!(
            &e.event,
            ParsedEvent::TrackedToolStarted { call_id, activity }
                if call_id == "call-cmd" && activity.label == "Running Rust tests"
        )));
        assert!(polled.iter().any(|e| matches!(
            &e.event,
            ParsedEvent::TrackedToolFinished { call_id } if call_id == "call-read"
        )));
        assert!(polled.iter().any(|e| matches!(
            &e.event,
            ParsedEvent::TrackedToolStarted { call_id, activity }
                if call_id == "call-edit"
                    && activity.label == "Editing files"
                    && activity.file.as_deref() == Some("main.rs")
        )));
        assert!(polled.iter().any(|e| matches!(
            &e.event,
            ParsedEvent::TrackedToolStarted { call_id, activity }
                if call_id == "call-search" && activity.label == "Searching project"
        )));
        assert!(
            polled
                .iter()
                .any(|e| matches!(&e.event, ParsedEvent::Thinking { reasoning: None }))
        );
        assert!(
            polled
                .iter()
                .any(|e| matches!(e.event, ParsedEvent::Completed))
        );

        assert!(poll_at(&tmp.path, &mut seen, now + 1_000).is_empty());
    }

    #[test]
    fn cold_bootstrap_skips_stale_unclosed_runs() {
        let tmp = TempDb::new("stale-bootstrap");
        let conn = tmp.conn();
        conn.execute(
            "INSERT INTO orchestration_events (event_id, aggregate_kind, stream_id, event_type, occurred_at, payload_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "evt-old",
                "thread",
                "thread-old",
                "run.created",
                "2026-11-25T08:00:00.000Z",
                r#"{"id":"run-old","threadId":"thread-old","status":"running"}"#
            ],
        )
        .unwrap();

        let now = parse_timestamp_str("2026-11-25T10:00:00.000Z").unwrap();
        let mut seen = HashSet::new();
        assert!(poll_at(&tmp.path, &mut seen, now).is_empty());
        assert!(seen.contains("event:evt-old"));
    }

    #[test]
    fn model_label_rejects_prose_and_extracts_effort() {
        let valid = serde_json::json!({
            "model": "gpt-5.4",
            "options": {"effort": "high"}
        });
        assert_eq!(
            extract_model_label(&valid).as_deref(),
            Some("gpt-5.4 (high)")
        );

        let prose = serde_json::json!({
            "model": "some invalid model with spaces"
        });
        assert_eq!(extract_model_label(&prose), None);
    }

    #[test]
    fn incremental_cursor_reads_more_than_200_events_in_ascending_batches() {
        let tmp = TempDb::new("cursor-batches");
        let conn = tmp.conn();
        let now = parse_timestamp_str("2026-11-25T10:00:10.000Z").unwrap();

        conn.execute(
            "INSERT INTO orchestration_events (event_id, aggregate_kind, stream_id, event_type, occurred_at, payload_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "evt-seed",
                "thread",
                "thread-1",
                "run.created",
                "2026-11-25T10:00:00.000Z",
                r#"{"id":"run-0","threadId":"thread-1","status":"starting"}"#
            ],
        )
        .unwrap();

        let mut seen = HashSet::new();
        let first = poll_at(&tmp.path, &mut seen, now);
        assert_eq!(first.len(), 1);

        conn.execute(
            "INSERT INTO orchestration_events (event_id, aggregate_kind, stream_id, event_type, occurred_at, payload_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "evt-early-tool",
                "thread",
                "thread-1",
                "turn-item.updated",
                "2026-11-25T10:00:01.000Z",
                r#"{"id":"call-early","threadId":"thread-1","type":"file_search","status":"running"}"#
            ],
        )
        .unwrap();
        for idx in 0..240 {
            conn.execute(
                "INSERT INTO orchestration_events (event_id, aggregate_kind, stream_id, event_type, occurred_at, payload_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    format!("evt-burst-{idx}"),
                    "thread",
                    "thread-1",
                    "turn-item.updated",
                    "2026-11-25T10:00:02.000Z",
                    r#"{"id":"msg-burst","threadId":"thread-1","type":"reasoning","status":"running"}"#
                ],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO orchestration_events (event_id, aggregate_kind, stream_id, event_type, occurred_at, payload_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "evt-complete",
                "thread",
                "thread-1",
                "run.updated",
                "2026-11-25T10:00:03.000Z",
                r#"{"id":"run-0","threadId":"thread-1","status":"completed"}"#
            ],
        )
        .unwrap();

        let polled = poll_at(&tmp.path, &mut seen, now + 2_000);
        assert!(polled.iter().any(|e| matches!(
            &e.event,
            ParsedEvent::TrackedToolStarted { call_id, .. } if call_id == "call-early"
        )));
        assert!(matches!(
            polled.last().map(|e| &e.event),
            Some(ParsedEvent::Completed)
        ));
        assert!(poll_at(&tmp.path, &mut seen, now + 4_000).is_empty());
    }

    #[test]
    fn thread_context_reemits_when_model_or_project_returns_to_earlier_value() {
        let tmp = TempDb::new("context-return");
        let conn = tmp.conn();
        let now = parse_timestamp_str("2026-11-25T10:00:10.000Z").unwrap();

        let rows = [
            (
                "evt-a1",
                r#"{"id":"run-1","threadId":"thread-1","status":"starting","worktreePath":"C:\\repo\\alpha","modelSelection":{"model":"model-a"}}"#,
            ),
            (
                "evt-a2",
                r#"{"id":"run-1","threadId":"thread-1","status":"running","worktreePath":"C:\\repo\\alpha","modelSelection":{"model":"model-a"}}"#,
            ),
            (
                "evt-b",
                r#"{"id":"run-2","threadId":"thread-1","status":"starting","worktreePath":"C:\\repo\\beta","modelSelection":{"model":"model-b"}}"#,
            ),
            (
                "evt-a3",
                r#"{"id":"run-3","threadId":"thread-1","status":"starting","worktreePath":"C:\\repo\\alpha","modelSelection":{"model":"model-a"}}"#,
            ),
        ];
        for (id, payload) in rows {
            conn.execute(
                "INSERT INTO orchestration_events (event_id, aggregate_kind, stream_id, event_type, occurred_at, payload_json) VALUES (?1, 'thread', 'thread-1', 'run.created', '2026-11-25T10:00:01.000Z', ?2)",
                rusqlite::params![id, payload],
            )
            .unwrap();
        }

        let mut seen = HashSet::new();
        let polled = poll_at(&tmp.path, &mut seen, now);
        let agents: Vec<&str> = polled
            .iter()
            .filter_map(|e| match &e.event {
                ParsedEvent::AgentDetected { agent } => Some(agent.as_str()),
                _ => None,
            })
            .collect();
        let projects: Vec<&str> = polled
            .iter()
            .filter_map(|e| match &e.event {
                ParsedEvent::SessionMetadata { project, .. } => Some(project.as_str()),
                _ => None,
            })
            .collect();

        assert_eq!(agents, vec!["model-a", "model-b", "model-a"]);
        assert_eq!(projects, vec!["alpha", "beta", "alpha"]);
    }
}
