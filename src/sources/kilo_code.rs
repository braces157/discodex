//! Kilo Code adapter: durable session events in the kilo database
//!
//! Kilo Code keeps one SQLite database in WAL mode at
//! `%USERPROFILE%\.local\share\kilo\kilo.db`. The database is shared by the
//! editor extension, the JetBrains plugin and the `kilo` CLI, so the adapter
//! polls it and reports the rows that were written since the last poll.
//!
//! Three sources are read, in this order:
//!
//! 1. `event` — the durable, append only event log. Every row carries the
//!    conversation id (`aggregate_id`), a per conversation `seq`, a `type`
//!    and a JSON `data` blob.
//! 2. `session_message` — the canonical message stream. The assistant row is
//!    rewritten in place while it streams, so the reader diffs the tool
//!    states of a row instead of trusting the row count.
//! 3. `part` / `tool` — the tool projection, used only when the two streams
//!    above are unavailable.
//!
//! The `session` table holds the workspace directory and the model of a
//! conversation.
//!
//! Only structural facts leave this adapter: the workspace folder name, a
//! file basename, an inferred language, a fixed activity label, the model id
//! and the timing of a turn. Prompts, answers, reasoning, tool arguments,
//! tool results, shell commands, window titles and full paths never leave
//! the database.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;

use super::{StoreEvent, column_json, column_text_opt, open_database};
use crate::events::{
    ParsedEvent, ToolActivity, classify_tool, extract_file_basename, language_from_file,
    project_name, timestamp_ms,
};

/// Number of rows that are read per poll.
const ROW_LIMIT: i64 = 200;

/// Number of session rows that are read per poll.
const SESSION_LIMIT: i64 = 200;

/// Longest published basename.
const MAX_BASENAME: usize = 128;

/// Longest published model or agent id.
const MAX_AGENT: usize = 128;

/// Longest published tool call id.
const MAX_CALL_ID: usize = 64;

/// The queries that read the durable event log.
///
/// The first one that the database accepts is used, so a renamed column or a
/// different row order does not stop the adapter.
const EVENT_QUERIES: &[&str] = &[
    "SELECT * FROM event ORDER BY rowid DESC LIMIT ?",
    "SELECT * FROM event ORDER BY seq DESC LIMIT ?",
    "SELECT * FROM event ORDER BY id DESC LIMIT ?",
];

/// The queries that read the canonical message stream.
const MESSAGE_QUERIES: &[&str] = &[
    "SELECT * FROM session_message ORDER BY rowid DESC LIMIT ?",
    "SELECT * FROM session_message ORDER BY seq DESC LIMIT ?",
    "SELECT * FROM session_message ORDER BY id DESC LIMIT ?",
];

/// The queries that read the tool projection.
const PART_QUERIES: &[&str] = &[
    "SELECT * FROM part ORDER BY rowid DESC LIMIT ?",
    "SELECT * FROM part ORDER BY seq DESC LIMIT ?",
    "SELECT * FROM tool ORDER BY rowid DESC LIMIT ?",
];

/// The queries that read the session table.
const SESSION_QUERIES: &[&str] = &[
    "SELECT * FROM session ORDER BY rowid DESC LIMIT ?",
    "SELECT * FROM session ORDER BY time_updated DESC LIMIT ?",
    "SELECT * FROM session LIMIT ?",
];

/// Where a record came from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The durable event log.
    Event,
    /// The canonical message stream.
    Message,
    /// The tool projection.
    Part,
}

/// One row of the database.
struct Record {
    source: Source,
    /// The conversation the row belongs to.
    session_id: String,
    /// The stable key of the row.
    key: String,
    /// The type of the event or of the message.
    kind: String,
    /// The JSON payload of the row.
    data: Value,
    /// The per conversation order of the row.
    seq: i64,
    /// The time the row was written, when the row carries one.
    timestamp_ms: Option<i64>,
}

/// The facts of a conversation that belong to the session and not to a row.
#[derive(Default)]
struct SessionInfo {
    /// The absolute workspace directory.
    directory: Option<String>,
    /// The model of the conversation.
    model: Option<String>,
}

/// The kinds of durable events that the adapter understands.
enum EventKind {
    Prompted,
    ToolCalled,
    ToolFinished,
    ShellStarted,
    ShellFinished,
    ReasoningStarted,
    TextEnded,
    StepEnded,
    AgentSwitched,
    ModelSwitched,
}

/// Databases that hold the harness activity below the given root.
pub fn databases(root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();

    // The root may already point at the database.
    if root.is_file() {
        return vec![root.to_path_buf()];
    }

    found.extend(databases_in(root));

    // Upstream resolves the data directory with `xdg-basedir`, which is
    // `%USERPROFILE%\.local\share\kilo` on Windows, but the changelog of the
    // extension mentions `%APPDATA%`, so the other places are probed too.
    for alternative in alternative_roots() {
        found.extend(databases_in(&alternative));
    }

    // An explicit database of the environment wins.
    if let Some(path) = std::env::var_os("KILO_DB") {
        let path = PathBuf::from(path);
        if path.is_file() {
            found.push(path);
        }
    }

    found.sort();
    found.dedup();
    found
}

/// The databases of one directory.
fn databases_in(directory: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();

    // The database of the current channel.
    let primary = directory.join("kilo.db");
    if primary.is_file() {
        found.push(primary);
    }

    // A development channel keeps its own database next to it. The WAL and
    // SHM sidecars do not end in `.db` and are therefore skipped.
    if let Ok(entries) = std::fs::read_dir(directory) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.starts_with("kilo-") || !name.ends_with(".db") {
                continue;
            }
            if path.is_file() {
                found.push(path);
            }
        }
    }

    found
}

/// The other places where the data directory of the harness may live.
fn alternative_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(appdata) = dirs::data_dir() {
        roots.push(appdata.join("kilo"));
    }
    if let Some(local) = dirs::data_local_dir() {
        roots.push(local.join("kilo"));
    }
    roots
}

/// Poll the database for rows that were written since the last poll.
///
/// The `seen` set holds the row keys that were already reported. Every key
/// is namespaced (`event:`, `msg:`, `part:`, `meta:`, `model:`, `call:`) so
/// the bookkeeping of a conversation never collides with the row cursor.
pub fn poll(db: &Path, seen: &mut HashSet<String>) -> Vec<StoreEvent> {
    let Ok(connection) = open_database(db) else {
        return Vec::new();
    };

    let sessions = read_sessions(&connection);

    // The durable event log is the canonical source. The message stream is
    // only read when the log is missing or empty, so that a single action is
    // never reported twice.
    let records = read_event_records(&connection)
        .filter(|records| !records.is_empty())
        .or_else(|| read_message_records(&connection))
        .or_else(|| read_part_records(&connection));
    let Some(records) = records else {
        return Vec::new();
    };

    // Group the rows by conversation and restore the order of the session.
    let mut grouped: BTreeMap<String, Vec<Record>> = BTreeMap::new();
    for record in records {
        grouped
            .entry(record.session_id.clone())
            .or_default()
            .push(record);
    }

    let mut events = Vec::new();
    for (session_id, mut rows) in grouped {
        rows.sort_by_key(|row| row.seq);

        // The facts of the conversation come first, so the presence knows the
        // workspace before the first action is reported.
        if let Some(info) = sessions.get(&session_id) {
            events.extend(session_events(&session_id, info, seen));
        }

        for row in &rows {
            if !seen.insert(row.key.clone()) {
                continue;
            }
            for event in record_events(row, &session_id, seen) {
                events.push(StoreEvent {
                    session: session_id.clone(),
                    event,
                });
            }
        }
    }
    events
}

/// The facts of a conversation, reported once.
fn session_events(
    session_id: &str,
    info: &SessionInfo,
    seen: &mut HashSet<String>,
) -> Vec<StoreEvent> {
    let mut events = Vec::new();

    // The workspace of the session. Only the folder name is published, the
    // full path stays inside the presence state.
    if let Some(cwd) = info.directory.as_deref()
        && let Some(project) = project_name(cwd)
    {
        let key = format!("meta:{session_id}");
        if !seen.contains(&key) {
            seen.insert(key);
            events.push(StoreEvent {
                session: session_id.to_string(),
                event: ParsedEvent::SessionMetadata {
                    project,
                    cwd: cwd.to_string(),
                },
            });
        }
    }

    // The model of the session.
    if let Some(model) = info.model.as_deref() {
        let key = format!("model:{session_id}:{model}");
        if !seen.contains(&key) {
            seen.insert(key);
            events.push(StoreEvent {
                session: session_id.to_string(),
                event: ParsedEvent::AgentDetected {
                    agent: model.to_string(),
                },
            });
        }
    }

    events
}

/// Read the session table.
///
/// The `title` and the `slug` of a session hold the prompt of the
/// conversation and are deliberately ignored.
fn read_sessions(connection: &Connection) -> HashMap<String, SessionInfo> {
    let mut sessions = HashMap::new();
    let Some(mut statement) = prepare_first(connection, SESSION_QUERIES) else {
        return sessions;
    };
    let Ok(mut rows) = statement.query(rusqlite::params![SESSION_LIMIT]) else {
        return sessions;
    };
    while let Some(row) = rows.next().ok().flatten() {
        let Some(id) = column_text_opt(row, "id") else {
            continue;
        };
        let directory = column_text_opt(row, "directory")
            .or_else(|| column_text_opt(row, "cwd"))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let model = session_model(row);
        sessions.insert(id, SessionInfo { directory, model });
    }
    sessions
}

/// The model of a session row.
///
/// The column holds the JSON object `{"id": ..., "providerID": ...}`; a
/// plain column holds the id itself.
fn session_model(row: &rusqlite::Row<'_>) -> Option<String> {
    if let Some(value) = json_column(row, "model")
        && let Some(model) = model_id(&value)
    {
        return Some(model);
    }
    column_text_opt(row, "model").and_then(|text| {
        let trimmed = text.trim();
        // A broken JSON blob is not a model id.
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            return None;
        }
        sanitize_agent(trimmed)
    })
}

/// Read the durable event log.
fn read_event_records(connection: &Connection) -> Option<Vec<Record>> {
    let mut statement = prepare_first(connection, EVENT_QUERIES)?;
    let mut rows = statement.query(rusqlite::params![ROW_LIMIT]).ok()?;
    let mut records = Vec::new();
    while let Some(row) = rows.next().ok().flatten() {
        let data = json_column(row, "data").unwrap_or(Value::Null);
        let Some(session_id) = row_session_id(row, &data) else {
            continue;
        };
        let kind = column_text_opt(row, "type").unwrap_or_default();
        let seq = row_seq(row);
        let id = column_text_opt(row, "id").unwrap_or_default();
        let key = if id.trim().is_empty() {
            format!("event:{session_id}:{seq}")
        } else {
            format!("event:{}", id.trim())
        };
        let timestamp_ms = column_text_opt(row, "timestamp")
            .and_then(|text| text.trim().parse::<i64>().ok())
            .and_then(|value| timestamp_ms(&Value::from(value)));
        records.push(Record {
            source: Source::Event,
            session_id,
            key,
            kind,
            data,
            seq,
            timestamp_ms,
        });
    }
    Some(records)
}

/// Read the canonical message stream.
fn read_message_records(connection: &Connection) -> Option<Vec<Record>> {
    let mut statement = prepare_first(connection, MESSAGE_QUERIES)?;
    let mut rows = statement.query(rusqlite::params![ROW_LIMIT]).ok()?;
    let mut records = Vec::new();
    while let Some(row) = rows.next().ok().flatten() {
        let data = json_column(row, "data").unwrap_or(Value::Null);
        let Some(session_id) = row_session_id(row, &data) else {
            continue;
        };
        let kind = column_text_opt(row, "type")
            .or_else(|| data.get("type").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        let seq = row_seq(row);
        let id = column_text_opt(row, "id").unwrap_or_default();
        // The assistant row is rewritten in place while it streams, so the
        // key carries the tool states of the row. A changed state is a new
        // key and the row is read again.
        let key = format!("msg:{}:{}", id.trim(), message_signature(&data));
        let timestamp_ms = column_text_opt(row, "time_created")
            .and_then(|text| text.trim().parse::<i64>().ok())
            .and_then(|value| timestamp_ms(&Value::from(value)));
        records.push(Record {
            source: Source::Message,
            session_id,
            key,
            kind,
            data,
            seq,
            timestamp_ms,
        });
    }
    Some(records)
}

/// Read the tool projection.
fn read_part_records(connection: &Connection) -> Option<Vec<Record>> {
    let mut statement = prepare_first(connection, PART_QUERIES)?;
    let mut rows = statement.query(rusqlite::params![ROW_LIMIT]).ok()?;
    let mut records = Vec::new();
    while let Some(row) = rows.next().ok().flatten() {
        let data = json_column(row, "data").unwrap_or(Value::Null);
        let Some(session_id) = row_session_id(row, &data) else {
            continue;
        };
        let id = column_text_opt(row, "id").unwrap_or_default();
        // The part row is upserted while the tool runs, so the key carries
        // the state of the tool.
        let status = tool_status(&data);
        let key = format!("part:{}:{status}", id.trim());
        records.push(Record {
            source: Source::Part,
            session_id,
            key,
            kind: "tool".to_string(),
            data,
            seq: row_seq(row),
            timestamp_ms: None,
        });
    }
    Some(records)
}

/// Map one row to normalized events.
fn record_events(
    record: &Record,
    session_id: &str,
    seen: &mut HashSet<String>,
) -> Vec<ParsedEvent> {
    match record.source {
        Source::Event => event_events(record),
        Source::Message => message_events(record, session_id, seen),
        Source::Part => part_events(record, session_id, seen),
    }
}

/// Map one durable event to normalized events.
fn event_events(record: &Record) -> Vec<ParsedEvent> {
    // An unknown event type is not a guess: it yields no activity.
    let Some(kind) = classify_event(&record.kind) else {
        return Vec::new();
    };
    let data = &record.data;

    match kind {
        EventKind::Prompted => {
            // The prompt itself is never read, only the moment of the turn.
            let started_at_ms = data
                .get("timestamp")
                .and_then(timestamp_ms)
                .or(record.timestamp_ms)
                .unwrap_or_else(current_time_ms);
            vec![ParsedEvent::TaskStarted { started_at_ms }]
        }
        EventKind::ToolCalled => {
            let name = tool_name(data);
            // A tool call without a name is a renamed field, not a tool.
            // It degrades to "no activity" instead of a wrong label.
            if name.trim().is_empty() {
                return Vec::new();
            }
            let activity = tool_activity(name, tool_input(data));
            match call_id(data) {
                Some(call_id) => vec![ParsedEvent::TrackedToolStarted { call_id, activity }],
                None => vec![ParsedEvent::ToolStarted { activity }],
            }
        }
        EventKind::ToolFinished => match call_id(data) {
            Some(call_id) => vec![ParsedEvent::TrackedToolFinished { call_id }],
            None => vec![ParsedEvent::ToolFinished],
        },
        EventKind::ShellStarted => {
            // The command is only used to pick a fixed label; it never
            // leaves the adapter.
            let input = tool_input(data);
            let command = data
                .get("command")
                .and_then(Value::as_str)
                .or_else(|| {
                    input
                        .and_then(|value| value.get("command"))
                        .and_then(Value::as_str)
                })
                .unwrap_or("");
            let activity = activity_from_parts("bash", command, input);
            match call_id(data) {
                Some(call_id) => vec![ParsedEvent::TrackedToolStarted { call_id, activity }],
                None => vec![ParsedEvent::ToolStarted { activity }],
            }
        }
        EventKind::ShellFinished => match call_id(data) {
            Some(call_id) => vec![ParsedEvent::TrackedToolFinished { call_id }],
            None => vec![ParsedEvent::ToolFinished],
        },
        // The reasoning text is never read.
        EventKind::ReasoningStarted => vec![ParsedEvent::Thinking { reasoning: None }],
        EventKind::TextEnded => vec![ParsedEvent::TerminalCandidate],
        EventKind::StepEnded => vec![ParsedEvent::Completed],
        EventKind::AgentSwitched => {
            match data
                .get("agent")
                .and_then(Value::as_str)
                .and_then(sanitize_agent)
            {
                Some(agent) => vec![ParsedEvent::AgentDetected { agent }],
                None => Vec::new(),
            }
        }
        EventKind::ModelSwitched => match switched_model(data) {
            Some(agent) => vec![ParsedEvent::AgentDetected { agent }],
            None => Vec::new(),
        },
    }
}

/// Map one message of the message stream to normalized events.
fn message_events(
    record: &Record,
    session_id: &str,
    seen: &mut HashSet<String>,
) -> Vec<ParsedEvent> {
    let data = &record.data;
    let kind = if record.kind.trim().is_empty() {
        data.get("type").and_then(Value::as_str).unwrap_or("")
    } else {
        record.kind.as_str()
    };

    match kind {
        "user" => {
            // The text of the prompt is never read.
            let started_at_ms = data
                .get("time")
                .and_then(|time| time.get("created"))
                .and_then(timestamp_ms)
                .or_else(|| data.get("timestamp").and_then(timestamp_ms))
                .or(record.timestamp_ms)
                .unwrap_or_else(current_time_ms);
            vec![ParsedEvent::TaskStarted { started_at_ms }]
        }
        "assistant" => {
            let mut events = Vec::new();
            let Some(items) = data.get("content").and_then(Value::as_array) else {
                return events;
            };
            for item in items {
                match item.get("type").and_then(Value::as_str).unwrap_or("") {
                    "tool" => events.extend(tool_item_events(item, session_id, seen)),
                    // The reasoning text is never read.
                    "reasoning" => events.push(ParsedEvent::Thinking { reasoning: None }),
                    // The answer itself is never read.
                    "text" => events.push(ParsedEvent::TerminalCandidate),
                    _ => {}
                }
            }
            events
        }
        "agent-switched" => {
            match data
                .get("agent")
                .and_then(Value::as_str)
                .and_then(sanitize_agent)
            {
                Some(agent) => vec![ParsedEvent::AgentDetected { agent }],
                None => Vec::new(),
            }
        }
        "model-switched" => match switched_model(data) {
            Some(agent) => vec![ParsedEvent::AgentDetected { agent }],
            None => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Map one tool item of an assistant message to normalized events.
///
/// The item is rewritten in place while the tool runs, so the call id is
/// remembered and the same call is never reported twice.
fn tool_item_events(
    item: &Value,
    session_id: &str,
    seen: &mut HashSet<String>,
) -> Vec<ParsedEvent> {
    let status = item
        .get("state")
        .and_then(|state| state.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| item.get("tool").and_then(Value::as_str))
        .unwrap_or("");
    // The state input is a string while the tool is pending and an object
    // while it runs, so only an object is read.
    let input = item
        .get("state")
        .and_then(|state| state.get("input"))
        .filter(|value| value.is_object());
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| item.get("callID").and_then(Value::as_str))
        .and_then(sanitize_call_id);

    match status {
        "pending" | "running" => {
            // A tool without a name is a renamed field, not a tool.
            if name.trim().is_empty() {
                return Vec::new();
            }
            let activity = tool_activity(name, input);
            match id {
                Some(call_id) => {
                    if seen.insert(format!("call:{session_id}:{call_id}:started")) {
                        vec![ParsedEvent::TrackedToolStarted { call_id, activity }]
                    } else {
                        Vec::new()
                    }
                }
                None => vec![ParsedEvent::ToolStarted { activity }],
            }
        }
        "completed" | "error" => match id {
            Some(call_id) => {
                if seen.insert(format!("call:{session_id}:{call_id}:finished")) {
                    vec![ParsedEvent::TrackedToolFinished { call_id }]
                } else {
                    Vec::new()
                }
            }
            None => vec![ParsedEvent::ToolFinished],
        },
        _ => Vec::new(),
    }
}

/// Map one row of the tool projection to normalized events.
fn part_events(record: &Record, session_id: &str, seen: &mut HashSet<String>) -> Vec<ParsedEvent> {
    let data = &record.data;
    // Only a tool part is an activity.
    let kind = data.get("type").and_then(Value::as_str).unwrap_or("tool");
    if kind != "tool" {
        return Vec::new();
    }
    tool_item_events(data, session_id, seen)
}

/// The kind of a durable event.
///
/// The name is matched on its last two segments, so a renamed prefix does not
/// change the meaning, and an unknown name yields no activity.
fn classify_event(name: &str) -> Option<EventKind> {
    let mut segments = name.rsplit('.');
    let last = segments.next().unwrap_or_default();
    let second = segments.next().unwrap_or_default();
    let tail = if second.is_empty() {
        last.to_string()
    } else {
        format!("{second}.{last}")
    };

    match tail.as_str() {
        "tool.called" => Some(EventKind::ToolCalled),
        "tool.success" | "tool.failed" | "tool.completed" | "tool.error" => {
            Some(EventKind::ToolFinished)
        }
        "shell.started" => Some(EventKind::ShellStarted),
        "shell.finished" | "shell.exited" | "shell.ended" => Some(EventKind::ShellFinished),
        "reasoning.started" => Some(EventKind::ReasoningStarted),
        "text.ended" => Some(EventKind::TextEnded),
        "step.ended" => Some(EventKind::StepEnded),
        "agent.switched" => Some(EventKind::AgentSwitched),
        "model.switched" => Some(EventKind::ModelSwitched),
        _ => match last {
            "prompted" => Some(EventKind::Prompted),
            _ => None,
        },
    }
}

/// The name of the tool of a record.
fn tool_name(data: &Value) -> &str {
    data.get("tool")
        .and_then(Value::as_str)
        .or_else(|| data.get("name").and_then(Value::as_str))
        .unwrap_or("")
}

/// The input of a tool of a record.
fn tool_input(data: &Value) -> Option<&Value> {
    data.get("input").filter(|value| value.is_object())
}

/// The call id of a record.
fn call_id(data: &Value) -> Option<String> {
    data.get("callID")
        .and_then(Value::as_str)
        .or_else(|| data.get("callId").and_then(Value::as_str))
        .or_else(|| data.get("id").and_then(Value::as_str))
        .and_then(sanitize_call_id)
}

/// The state of a tool part.
fn tool_status(data: &Value) -> String {
    data.get("state")
        .and_then(|state| state.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// The model of a model switch.
fn switched_model(data: &Value) -> Option<String> {
    let value = data.get("model")?;
    model_id(value)
}

/// The model id of a JSON value.
fn model_id(value: &Value) -> Option<String> {
    let raw = match value {
        Value::Object(map) => map.get("id").and_then(Value::as_str)?,
        Value::String(text) => text.as_str(),
        _ => return None,
    };
    sanitize_agent(raw)
}

/// Build the activity of a tool call.
///
/// The name is only used for a fixed label and the input only for a file
/// path. The command, the arguments and the result never leave here.
fn tool_activity(name: &str, input: Option<&Value>) -> ToolActivity {
    let base = name.trim().to_ascii_lowercase();
    // Only a shell tool reads its command, and only to pick a label.
    let command = if is_shell_tool(&base) {
        input
            .and_then(|value| value.get("command"))
            .and_then(Value::as_str)
            .unwrap_or("")
    } else {
        ""
    };
    activity_from_parts(&base, command, input)
}

/// Build the activity of a tool call from its parts.
fn activity_from_parts(name: &str, command: &str, input: Option<&Value>) -> ToolActivity {
    let label = classify_tool(name, command);
    let file = file_basename(name, input);
    let language = file
        .as_deref()
        .and_then(language_from_file)
        .map(str::to_string);
    ToolActivity {
        label: label.to_string(),
        file,
        language,
    }
}

/// True for the tools that run a shell command.
fn is_shell_tool(name: &str) -> bool {
    matches!(
        name,
        "bash"
            | "sh"
            | "zsh"
            | "shell"
            | "shell_command"
            | "run_command"
            | "runcommand"
            | "cmd"
            | "powershell"
            | "pwsh"
            | "terminal"
            | "exec"
            | "execute"
    )
}

/// True for the tools that work on a file.
///
/// A search tool carries a directory in its path, so its path is not a file
/// name and is not published.
fn is_file_tool(name: &str) -> bool {
    matches!(
        name,
        "read"
            | "write"
            | "edit"
            | "apply_patch"
            | "apply_diff"
            | "multiedit"
            | "patch"
            | "view"
            | "notebook_edit"
            | "str_replace"
            | "str_replace_editor"
            | "read_file"
            | "write_file"
            | "edit_file"
            | "create_file"
    )
}

/// The file a tool touched, as a basename only.
fn file_basename(name: &str, input: Option<&Value>) -> Option<String> {
    if !is_file_tool(name) {
        return None;
    }
    let input = input?;
    for key in ["path", "file", "file_path", "filepath"] {
        let Some(path) = input.get(key).and_then(Value::as_str) else {
            continue;
        };
        // Only the path is handed to the helper, so the body of the file, the
        // diff and the reason of the tool can never be read.
        let holder = serde_json::json!({ "path": path });
        let Some(base) = extract_file_basename(&holder) else {
            continue;
        };
        if base == "." || base == ".." || base.is_empty() || base.len() > MAX_BASENAME {
            continue;
        }
        return Some(base);
    }
    None
}

/// A fingerprint of the tool states of a message row.
///
/// Only the structure of the row is hashed, never its text, so the key of a
/// rewritten row changes when a tool advances but never carries content.
fn message_signature(data: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(items) = data.get("content").and_then(Value::as_array) {
        for item in items {
            let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
            let id = item.get("id").and_then(Value::as_str).unwrap_or("");
            let status = item
                .get("state")
                .and_then(|state| state.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("");
            parts.push(format!("{kind}:{id}:{status}"));
        }
    }
    let mut hasher = DefaultHasher::new();
    parts.join(";").hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// The conversation a row belongs to.
fn row_session_id(row: &rusqlite::Row<'_>, data: &Value) -> Option<String> {
    column_text_opt(row, "aggregate_id")
        .or_else(|| column_text_opt(row, "session_id"))
        .or_else(|| column_text_opt(row, "sessionID"))
        .or_else(|| {
            data.get("sessionID")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .and_then(|value| sanitize_session_id(&value))
}

/// The order of a row inside its conversation.
fn row_seq(row: &rusqlite::Row<'_>) -> i64 {
    column_text_opt(row, "seq")
        .and_then(|text| text.trim().parse::<i64>().ok())
        .unwrap_or(0)
}

/// Read a JSON column by name.
fn json_column(row: &rusqlite::Row<'_>, name: &str) -> Option<Value> {
    let index = row.as_ref().column_index(name).ok()?;
    column_json(row, index)
}

/// Prepare the first statement that the database accepts.
fn prepare_first<'a>(
    connection: &'a Connection,
    queries: &[&str],
) -> Option<rusqlite::Statement<'a>> {
    for query in queries {
        if let Ok(statement) = connection.prepare(query) {
            return Some(statement);
        }
    }
    None
}

/// Keep only a conversation id that looks like one.
fn sanitize_session_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return None;
    }
    if !trimmed
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// Keep only a model or agent id that looks like one.
fn sanitize_agent(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_AGENT {
        return None;
    }
    if !trimmed.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'@' | b'-')
    }) {
        return None;
    }
    Some(trimmed.to_string())
}

/// Keep only a tool call id that looks like an opaque id.
fn sanitize_call_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_CALL_ID {
        return None;
    }
    if !trimmed.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-' | b'/')
    }) {
        return None;
    }
    Some(trimmed.to_string())
}

fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("discodex-kilo-{name}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
        path
    }

    /// The schema of the upstream database.
    fn create_db(path: &Path) -> Connection {
        let connection = Connection::open(path).expect("create database");
        connection
            .execute_batch(
                "CREATE TABLE session (
                     id TEXT PRIMARY KEY,
                     project_id TEXT,
                     parent_id TEXT,
                     slug TEXT,
                     directory TEXT,
                     path TEXT,
                     title TEXT,
                     agent TEXT,
                     model TEXT,
                     time_created INTEGER,
                     time_updated INTEGER
                 );
                 CREATE TABLE event (
                     id TEXT PRIMARY KEY,
                     aggregate_id TEXT,
                     seq INTEGER,
                     type TEXT,
                     data TEXT
                 );
                 CREATE TABLE session_message (
                     id TEXT PRIMARY KEY,
                     session_id TEXT,
                     type TEXT,
                     seq INTEGER,
                     time_created INTEGER,
                     data TEXT
                 );",
            )
            .expect("create schema");
        connection
    }

    fn insert_session(connection: &Connection, id: &str, directory: &str, model: &str) {
        connection
            .execute(
                "INSERT INTO session (id, project_id, parent_id, slug, directory, path, title, agent, model, time_created, time_updated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    id,
                    "prj_01J8Q",
                    rusqlite::types::Null,
                    "redacted-slug",
                    directory,
                    directory,
                    "PRIVATE_SESSION_TITLE",
                    "build",
                    model,
                    1_758_400_000_000i64,
                    1_758_400_060_000i64
                ],
            )
            .expect("insert session");
    }

    fn insert_event(
        connection: &Connection,
        id: &str,
        session: &str,
        seq: i64,
        kind: &str,
        data: Value,
    ) {
        connection
            .execute(
                "INSERT INTO event (id, aggregate_id, seq, type, data) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, session, seq, kind, data.to_string()],
            )
            .expect("insert event");
    }

    fn insert_message(
        connection: &Connection,
        id: &str,
        session: &str,
        seq: i64,
        kind: &str,
        time_created: i64,
        data: Value,
    ) {
        connection
            .execute(
                "INSERT INTO session_message (id, session_id, type, seq, time_created, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![id, session, kind, seq, time_created, data.to_string()],
            )
            .expect("insert message");
    }

    /// A realistic slice of one turn of a Kilo session.
    fn write_turn(connection: &Connection, session: &str) {
        insert_event(
            connection,
            "evt_01",
            session,
            40,
            "session.next.prompted",
            serde_json::json!({
                "timestamp": 1_758_400_051_200i64,
                "sessionID": session,
                "prompt": { "text": "PRIVATE_USER_PROMPT" }
            }),
        );
        insert_event(
            connection,
            "evt_02",
            session,
            41,
            "session.next.tool.called",
            serde_json::json!({
                "timestamp": 1_758_400_052_000i64,
                "sessionID": session,
                "assistantMessageID": "msg_01",
                "callID": "call_9f2",
                "tool": "edit",
                "input": {
                    "path": "src/provider.rs",
                    "oldString": "PRIVATE_OLD_STRING",
                    "newString": "PRIVATE_NEW_STRING"
                },
                "provider": { "executed": true }
            }),
        );
        insert_event(
            connection,
            "evt_03",
            session,
            42,
            "session.next.tool.success",
            serde_json::json!({
                "timestamp": 1_758_400_054_800i64,
                "sessionID": session,
                "assistantMessageID": "msg_01",
                "callID": "call_9f2",
                "structured": {},
                "content": [],
                "result": "PRIVATE_TOOL_RESULT"
            }),
        );
        insert_event(
            connection,
            "evt_04",
            session,
            43,
            "session.next.reasoning.started",
            serde_json::json!({
                "timestamp": 1_758_400_055_000i64,
                "sessionID": session
            }),
        );
        insert_event(
            connection,
            "evt_05",
            session,
            44,
            "session.next.text.ended",
            serde_json::json!({
                "timestamp": 1_758_400_056_000i64,
                "sessionID": session,
                "text": "PRIVATE_ASSISTANT_ANSWER"
            }),
        );
        insert_event(
            connection,
            "evt_06",
            session,
            45,
            "session.next.step.ended",
            serde_json::json!({
                "timestamp": 1_758_400_057_000i64,
                "sessionID": session
            }),
        );
    }

    #[test]
    fn maps_a_kilo_turn_to_normalized_events() {
        let path = temp_db("happy");
        {
            let connection = create_db(&path);
            insert_session(
                &connection,
                "ses_01J8Q",
                r"C:\Users\PC\Documents\Discodex",
                r#"{"id":"claude-sonnet-4-5","providerID":"anthropic"}"#,
            );
            write_turn(&connection, "ses_01J8Q");
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        let parsed: Vec<ParsedEvent> = events.iter().map(|event| event.event.clone()).collect();

        // Every event belongs to the conversation of the row.
        assert!(events.iter().all(|event| event.session == "ses_01J8Q"));

        assert_eq!(
            parsed,
            vec![
                ParsedEvent::SessionMetadata {
                    project: "Discodex".to_string(),
                    cwd: r"C:\Users\PC\Documents\Discodex".to_string(),
                },
                ParsedEvent::AgentDetected {
                    agent: "claude-sonnet-4-5".to_string(),
                },
                ParsedEvent::TaskStarted {
                    started_at_ms: 1_758_400_051_200,
                },
                ParsedEvent::TrackedToolStarted {
                    call_id: "call_9f2".to_string(),
                    activity: ToolActivity {
                        label: "Editing files".to_string(),
                        file: Some("provider.rs".to_string()),
                        language: Some("Rust".to_string()),
                    },
                },
                ParsedEvent::TrackedToolFinished {
                    call_id: "call_9f2".to_string(),
                },
                ParsedEvent::Thinking { reasoning: None },
                ParsedEvent::TerminalCandidate,
                ParsedEvent::Completed,
            ]
        );

        // The same rows are never reported twice.
        assert!(poll(&path, &mut seen).is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn never_forwards_prompts_answers_results_or_full_paths() {
        let path = temp_db("privacy");
        {
            let connection = create_db(&path);
            insert_session(
                &connection,
                "ses_01J8Q",
                r"C:\Users\PC\Documents\Discodex",
                r#"{"id":"claude-sonnet-4-5"}"#,
            );
            insert_event(
                &connection,
                "evt_01",
                "ses_01J8Q",
                1,
                "session.next.prompted",
                serde_json::json!({
                    "timestamp": 1_758_400_051_200i64,
                    "prompt": { "text": "PRIVATE_USER_PROMPT" }
                }),
            );
            insert_event(
                &connection,
                "evt_02",
                "ses_01J8Q",
                2,
                "session.next.tool.called",
                serde_json::json!({
                    "timestamp": 1_758_400_052_000i64,
                    "callID": "call_9f2",
                    "tool": "write",
                    "input": {
                        "path": r"C:\Users\PC\Documents\Discodex\PRIVATE_REPO_MARKER\src\secret.rs",
                        "content": "PRIVATE_FILE_BODY"
                    }
                }),
            );
            insert_event(
                &connection,
                "evt_03",
                "ses_01J8Q",
                3,
                "session.next.tool.success",
                serde_json::json!({
                    "timestamp": 1_758_400_054_800i64,
                    "callID": "call_9f2",
                    "content": ["PRIVATE_TOOL_RESULT"],
                    "result": "PRIVATE_TOOL_RESULT"
                }),
            );
            insert_event(
                &connection,
                "evt_04",
                "ses_01J8Q",
                4,
                "session.next.shell.started",
                serde_json::json!({
                    "timestamp": 1_758_400_055_000i64,
                    "callID": "call_shell",
                    "command": "cat C:\\Users\\PC\\secrets\\creds.env"
                }),
            );
            insert_event(
                &connection,
                "evt_05",
                "ses_01J8Q",
                5,
                "session.next.text.ended",
                serde_json::json!({
                    "timestamp": 1_758_400_056_000i64,
                    "text": "PRIVATE_ASSISTANT_ANSWER"
                }),
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        let rendered = format!("{events:?}");

        for secret in [
            "PRIVATE_USER_PROMPT",
            "PRIVATE_FILE_BODY",
            "PRIVATE_TOOL_RESULT",
            "PRIVATE_ASSISTANT_ANSWER",
            "PRIVATE_REPO_MARKER",
            "creds.env",
            "secrets",
            r"C:\Users\PC\Documents\PRIVATE_REPO_MARKER\src\secret.rs",
        ] {
            assert!(
                !rendered.contains(secret),
                "the event leaked {secret}: {rendered}"
            );
        }

        // The basename of the file is the only part of the path that is
        // published.
        assert!(rendered.contains("secret.rs"));
        // The shell command is only used to pick a label.
        assert!(rendered.contains("Running command"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unknown_and_broken_records_produce_no_events() {
        let path = temp_db("broken");
        {
            let connection = create_db(&path);
            insert_event(
                &connection,
                "evt_01",
                "ses_01J8Q",
                1,
                "session.next.brand.new.event",
                serde_json::json!({ "timestamp": 1_758_400_051_200i64 }),
            );
            insert_event(
                &connection,
                "evt_02",
                "ses_01J8Q",
                2,
                "session.next.tool.called",
                serde_json::json!({}),
            );
            insert_event(
                &connection,
                "evt_03",
                "ses_01J8Q",
                3,
                "session.next.tool.called",
                serde_json::json!({ "tool": "edit", "input": { "path": "src/main.rs" } }),
            );
            // A row whose payload is not a JSON object at all.
            connection
                .execute(
                    "INSERT INTO event (id, aggregate_id, seq, type, data) VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        "evt_04",
                        "ses_01J8Q",
                        4i64,
                        "session.next.tool.called",
                        "not json at all"
                    ],
                )
                .expect("insert event");
            // A row without a conversation.
            connection
                .execute(
                    "INSERT INTO event (id, aggregate_id, seq, type, data) VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        "evt_05",
                        rusqlite::types::Null,
                        5i64,
                        "session.next.tool.called",
                        r#"{"tool":"edit"}"#
                    ],
                )
                .expect("insert event");
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        // Only the record with a known shape and a known file tool is read.
        assert_eq!(
            events
                .iter()
                .map(|event| event.event.clone())
                .collect::<Vec<ParsedEvent>>(),
            vec![ParsedEvent::ToolStarted {
                activity: ToolActivity {
                    label: "Editing files".to_string(),
                    file: Some("main.rs".to_string()),
                    language: Some("Rust".to_string()),
                }
            }]
        );

        // A database without the tables of the harness yields no events and
        // does not panic.
        let empty = temp_db("empty");
        {
            let connection = Connection::open(&empty).expect("create database");
            connection
                .execute_batch("CREATE TABLE unrelated (id TEXT);")
                .expect("create schema");
        }
        let mut seen = HashSet::new();
        assert!(poll(&empty, &mut seen).is_empty());

        // A database that does not exist at all.
        let mut seen = HashSet::new();
        assert!(poll(Path::new("does-not-exist.db"), &mut seen).is_empty());

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&empty);
    }

    #[test]
    fn reports_the_model_and_the_timestamp_of_the_record() {
        let path = temp_db("model");
        {
            let connection = create_db(&path);
            insert_session(
                &connection,
                "ses_01J8Q",
                r"C:\work\Repo",
                r#"{"id":"claude-sonnet-4-5","providerID":"anthropic"}"#,
            );
            insert_event(
                &connection,
                "evt_01",
                "ses_01J8Q",
                1,
                "session.next.prompted",
                serde_json::json!({ "timestamp": 1_700_000_123_456i64 }),
            );
            insert_event(
                &connection,
                "evt_02",
                "ses_01J8Q",
                2,
                "session.next.model.switched",
                serde_json::json!({ "model": { "id": "gpt-5-codex" } }),
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        let parsed: Vec<ParsedEvent> = events.iter().map(|event| event.event.clone()).collect();

        assert_eq!(
            parsed,
            vec![
                ParsedEvent::SessionMetadata {
                    project: "Repo".to_string(),
                    cwd: r"C:\work\Repo".to_string(),
                },
                ParsedEvent::AgentDetected {
                    agent: "claude-sonnet-4-5".to_string(),
                },
                ParsedEvent::TaskStarted {
                    started_at_ms: 1_700_000_123_456,
                },
                ParsedEvent::AgentDetected {
                    agent: "gpt-5-codex".to_string(),
                },
            ]
        );

        // A model id that is not a model id is dropped instead of published.
        let other = temp_db("model-bad");
        {
            let connection = create_db(&other);
            insert_session(
                &connection,
                "ses_02",
                r"C:\work\Repo",
                r#"{"id":"PRIVATE MODEL NAME WITH SPACES"}"#,
            );
        }
        let mut seen = HashSet::new();
        let events = poll(&other, &mut seen);
        let rendered = format!("{events:?}");
        assert!(!rendered.contains("PRIVATE MODEL NAME WITH SPACES"));
        assert!(!rendered.contains("AgentDetected"));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&other);
    }

    #[test]
    fn reads_the_message_stream_when_the_event_log_is_missing() {
        let path = temp_db("messages");
        {
            let connection = create_db(&path);
            insert_session(
                &connection,
                "ses_01J8Q",
                r"C:\Users\PC\Documents\Discodex",
                r#"{"id":"claude-sonnet-4-5"}"#,
            );
            insert_message(
                &connection,
                "msg_01",
                "ses_01J8Q",
                1,
                "user",
                1_758_400_051_200,
                serde_json::json!({
                    "type": "user",
                    "time": { "created": 1_758_400_051_200i64 },
                    "parts": [{ "type": "text", "text": "PRIVATE_USER_PROMPT" }]
                }),
            );
            insert_message(
                &connection,
                "msg_02",
                "ses_01J8Q",
                2,
                "assistant",
                1_758_400_052_000,
                serde_json::json!({
                    "type": "assistant",
                    "model": { "id": "claude-sonnet-4-5" },
                    "content": [{
                        "type": "tool",
                        "id": "call_9f2",
                        "name": "read",
                        "state": {
                            "status": "running",
                            "input": { "path": r"C:\Users\PC\Documents\Discodex\src\provider.rs" },
                            "content": []
                        },
                        "time": { "created": 1_758_400_052_000i64 }
                    }]
                }),
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        let parsed: Vec<ParsedEvent> = events.iter().map(|event| event.event.clone()).collect();

        assert_eq!(
            parsed,
            vec![
                ParsedEvent::SessionMetadata {
                    project: "Discodex".to_string(),
                    cwd: r"C:\Users\PC\Documents\Discodex".to_string(),
                },
                ParsedEvent::AgentDetected {
                    agent: "claude-sonnet-4-5".to_string(),
                },
                ParsedEvent::TaskStarted {
                    started_at_ms: 1_758_400_051_200,
                },
                ParsedEvent::TrackedToolStarted {
                    call_id: "call_9f2".to_string(),
                    activity: ToolActivity {
                        label: "Reading project files".to_string(),
                        file: Some("provider.rs".to_string()),
                        language: Some("Rust".to_string()),
                    },
                },
            ]
        );

        // The prompt of the message is never published.
        let rendered = format!("{events:?}");
        assert!(!rendered.contains("PRIVATE_USER_PROMPT"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn finds_the_database_below_the_root() {
        let root = temp_db("root");
        let _ = std::fs::remove_file(&root);
        std::fs::create_dir_all(&root).expect("create root");
        let primary = root.join("kilo.db");
        std::fs::write(&primary, b"sqlite").expect("write database");
        let channel = root.join("kilo-beta.db");
        std::fs::write(&channel, b"sqlite").expect("write database");
        let sidecar = root.join("kilo.db-wal");
        std::fs::write(&sidecar, b"wal").expect("write sidecar");

        let found = databases(&root);
        assert!(found.contains(&primary));
        assert!(found.contains(&channel));
        // The WAL sidecar is not a database.
        assert!(!found.contains(&sidecar));

        let _ = std::fs::remove_dir_all(&root);
    }
}
