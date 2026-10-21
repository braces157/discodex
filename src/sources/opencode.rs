//! OpenCode adapter: durable session events in the opencode database
//!
//! OpenCode keeps every session of every project in one machine wide SQLite
//! database (`%USERPROFILE%\.local\share\opencode\opencode.db`, WAL mode,
//! written by the Drizzle ORM) and appends a durable event log to the `event`
//! table while the agent works. That log is the best source: every row is
//! timestamped, ordered and carries the session it belongs to.
//!
//! | event type                      | normalized event                        |
//! |---------------------------------|----------------------------------------|
//! | `session.created`               | `SessionMetadata` (workspace + model)  |
//! | `session.execution.started`     | `TaskStarted` (the turn begins)        |
//! | `session.step.started`          | `Thinking`                             |
//! | `session.model.selected`        | `AgentDetected` (the model id)         |
//! | `session.agent.selected`        | `AgentDetected` (the agent name)       |
//! | `session.tool.called`            | `TrackedToolStarted` / `ToolStarted`   |
//! | `session.tool.success`          | `TrackedToolFinished` / `ToolFinished` |
//! | `session.tool.failed`           | `TrackedToolFinished` / `ToolFinished` |
//! | `session.shell.started`         | `TrackedToolStarted` / `ToolStarted`   |
//! | `session.shell.ended`           | `TrackedToolFinished` / `ToolFinished` |
//! | `session.execution.succeeded`   | `Completed`                            |
//! | `session.execution.failed`      | `Completed`                            |
//! | `session.execution.interrupted` | `Completed`                            |
//!
//! Every other event type (`session.text.*`, `session.reasoning.*`,
//! `session.tool.input.*`) is ignored on purpose: those payloads carry the
//! prompt, the answer, the reasoning or the raw tool arguments.
//!
//! Only structural facts leave this adapter: the workspace folder name, a
//! file basename, an inferred language, a normalized activity label, the model
//! id, the agent name and timing. Prompts, answers, reasoning, tool
//! arguments, tool results and path prefixes never leave the database.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;

use super::{StoreEvent, column_json, column_text_opt, open_database};
use crate::events::{
    ParsedEvent, ToolActivity, basename, classify_tool, detect_project_root, extract_file_basename,
    language_from_file, project_name, timestamp_ms,
};

/// Number of rows that are read per poll.
const ROW_LIMIT: i64 = 200;

/// The name of the machine wide database.
const DATABASE_FILE: &str = "opencode.db";

/// The columns that may hold the creation time of a row.
const TIME_COLUMNS: &[&str] = &[
    "created",
    "created_at",
    "createdat",
    "time_created",
    "timestamp",
    "time",
];

/// The state of one `session_v2` row.
#[derive(Clone, Default)]
struct SessionInfo {
    /// The absolute working directory of the conversation.
    directory: Option<String>,
    /// The model reference of the conversation.
    model: Option<Value>,
}

/// One row of the durable `event` log.
struct EventRecord {
    /// The primary key of the row, used as the dedup key.
    id: Option<String>,
    /// The session the row belongs to.
    session: String,
    /// The upstream event type.
    kind: String,
    /// The JSON payload of the row.
    data: Option<Value>,
    /// The creation time of the row in epoch milliseconds.
    created_ms: Option<i64>,
    /// The sequence number of the row, used as a fallback dedup key.
    seq: Option<String>,
}

/// One row of the `session_message` projection.
struct MessageRecord {
    id: String,
    session: String,
    kind: String,
    data: Option<Value>,
    created_ms: Option<i64>,
    updated_ms: Option<i64>,
}

/// Databases that hold the harness activity below the given root.
pub fn databases(root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();

    // The known file name is reported even when the directory cannot be read.
    let known = root.join(DATABASE_FILE);
    if known.is_file() {
        found.push(known);
    }

    // Channel builds use `opencode-<channel>.db`; the `-wal` and `-shm`
    // sidecars never end in `.db` and are therefore skipped.
    for directory in [root.to_path_buf(), root.join("opencode")] {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if !name.starts_with("opencode") || !name.ends_with(".db") {
                continue;
            }
            if !found.contains(&path) {
                found.push(path);
            }
        }
    }

    found
}

/// Poll the database for rows that were written since the last poll.
///
/// The `seen` set holds the row keys that were already reported. Every key is
/// namespaced (`event:`, `msg:`, `meta:`, `agent:`) so the conversation scoped
/// bookkeeping never collides with the row cursor.
pub fn poll(db: &Path, seen: &mut HashSet<String>) -> Vec<StoreEvent> {
    let Ok(connection) = open_database(db) else {
        return Vec::new();
    };

    let mut events = Vec::new();
    let mut cache: HashMap<String, Option<SessionInfo>> = HashMap::new();

    // The durable event log is the primary source; the message projection is
    // only read when the log does not exist.
    if !poll_event_log(&connection, &mut cache, seen, &mut events) {
        poll_messages(&connection, &mut cache, seen, &mut events);
    }

    events
}

/// Read the durable event log.
///
/// Returns `false` when the log does not exist (or lost its `type` column) so
/// that the caller can fall back to the message projection.
fn poll_event_log(
    connection: &Connection,
    cache: &mut HashMap<String, Option<SessionInfo>>,
    seen: &mut HashSet<String>,
    out: &mut Vec<StoreEvent>,
) -> bool {
    let Some(columns) = table_columns(connection, "event") else {
        return false;
    };
    if !columns.contains("type") && !columns.contains("event_type") {
        return false;
    }

    let Some(mut statement) = ordered_statement(connection, "event", &columns) else {
        return false;
    };
    let records: Vec<EventRecord> = {
        let Ok(mut rows) = statement.query([]) else {
            return false;
        };
        let mut records = Vec::new();
        while let Ok(Some(row)) = rows.next() {
            if let Some(record) = event_record(row) {
                records.push(record);
            }
        }
        records
    };

    // The query returns the newest rows first; report them oldest first so the
    // presence follows the order of the conversation.
    for record in records.into_iter().rev() {
        let key = record_key(&record);
        if !seen.insert(key) {
            continue;
        }
        let events = record_events(connection, cache, &record, seen);
        for event in events {
            out.push(StoreEvent {
                session: record.session.clone(),
                event,
            });
        }
    }

    true
}

/// Read the message projection, used when the event log is missing.
fn poll_messages(
    connection: &Connection,
    cache: &mut HashMap<String, Option<SessionInfo>>,
    seen: &mut HashSet<String>,
    out: &mut Vec<StoreEvent>,
) {
    let Some(columns) = table_columns(connection, "session_message") else {
        return;
    };
    let Some(mut statement) = ordered_statement(connection, "session_message", &columns) else {
        return;
    };
    let records: Vec<MessageRecord> = {
        let Ok(mut rows) = statement.query([]) else {
            return;
        };
        let mut records = Vec::new();
        while let Ok(Some(row)) = rows.next() {
            if let Some(record) = message_record(row) {
                records.push(record);
            }
        }
        records
    };

    for record in records.into_iter().rev() {
        let key = message_key(&record);
        if !seen.insert(key) {
            continue;
        }
        let events = message_events(connection, cache, &record, seen);
        for event in events {
            out.push(StoreEvent {
                session: record.session.clone(),
                event,
            });
        }
    }
}

/// Prepare a `SELECT *` statement that returns the newest rows first.
fn ordered_statement<'a>(
    connection: &'a Connection,
    table: &str,
    columns: &HashSet<String>,
) -> Option<rusqlite::Statement<'a>> {
    let mut candidates: Vec<&str> = Vec::new();
    for candidate in TIME_COLUMNS {
        if columns.contains(*candidate) {
            candidates.push(candidate);
        }
    }
    for candidate in ["seq", "id"] {
        if columns.contains(candidate) {
            candidates.push(candidate);
        }
    }
    candidates.push("rowid");

    for order in candidates {
        let sql = format!("SELECT * FROM {table} ORDER BY {order} DESC LIMIT {ROW_LIMIT}");
        if let Ok(statement) = connection.prepare(&sql) {
            return Some(statement);
        }
    }
    None
}

/// The columns of a table, or `None` when the table does not exist.
fn table_columns(connection: &Connection, table: &str) -> Option<HashSet<String>> {
    let sql = format!("PRAGMA table_info({table})");
    let Ok(mut statement) = connection.prepare(&sql) else {
        return None;
    };
    let Ok(mut rows) = statement.query([]) else {
        return None;
    };
    let mut columns = HashSet::new();
    while let Ok(Some(row)) = rows.next() {
        if let Some(name) = column_text_opt(row, "name") {
            columns.insert(name.to_ascii_lowercase());
        }
    }
    if columns.is_empty() {
        None
    } else {
        Some(columns)
    }
}

/// Read one row of the event log.
fn event_record(row: &rusqlite::Row<'_>) -> Option<EventRecord> {
    let kind = column_text_opt(row, "type")
        .or_else(|| column_text_opt(row, "event_type"))?
        .trim()
        .to_string();
    if kind.is_empty() {
        return None;
    }

    let data = json_column(row, "data");
    let session = column_text_opt(row, "session_id")
        .or_else(|| column_text_opt(row, "aggregate_id"))
        .or_else(|| data.as_ref().and_then(data_session))
        .map(|session| session.trim().to_string())
        .filter(|session| !session.is_empty())?;

    Some(EventRecord {
        id: column_text_opt(row, "id"),
        session,
        kind,
        data,
        created_ms: row_time_ms(row),
        seq: column_text_opt(row, "seq"),
    })
}

/// The session id inside an event payload.
fn data_session(data: &Value) -> Option<String> {
    for key in [
        "sessionID",
        "session_id",
        "sessionId",
        "aggregate_id",
        "aggregateID",
    ] {
        if let Some(session) = data.get(key).and_then(Value::as_str)
            && !session.trim().is_empty()
        {
            return Some(session.trim().to_string());
        }
    }
    None
}

/// The stable key of an event row.
fn record_key(record: &EventRecord) -> String {
    if let Some(id) = record
        .id
        .as_ref()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
    {
        return format!("event:{id}");
    }
    format!(
        "event:{}:{}:{}:{}",
        record.session,
        record.seq.as_deref().unwrap_or_default(),
        record.kind,
        record.created_ms.unwrap_or_default()
    )
}

/// The creation time of a row in epoch milliseconds.
fn row_time_ms(row: &rusqlite::Row<'_>) -> Option<i64> {
    for name in TIME_COLUMNS {
        let Some(text) = column_text_opt(row, name) else {
            continue;
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if let Ok(number) = text.parse::<i64>()
            && let Some(millis) = timestamp_ms(&Value::from(number))
        {
            return Some(millis);
        }
        if let Some(millis) = timestamp_ms(&Value::from(text)) {
            return Some(millis);
        }
    }
    None
}

/// Read a JSON column by name, tolerating a missing column.
fn json_column(row: &rusqlite::Row<'_>, name: &str) -> Option<Value> {
    let index = row.as_ref().column_index(name).ok()?;
    column_json(row, index)
}

/// The state of one `session_v2` row.
fn query_session_info(connection: &Connection, session: &str) -> Option<SessionInfo> {
    query_session_info_by_id(connection, session)
        .or_else(|| query_session_info_by_scan(connection, session))
}

/// The tables that hold the sessions of a database.
///
/// Newer builds renamed the table to `session_v2`; older ones still use
/// `session`. Both hold the workspace in `directory` and the model in `model`.
const SESSION_TABLES: [&str; 2] = ["session_v2", "session"];

/// Look the session up by its primary key.
fn query_session_info_by_id(connection: &Connection, session: &str) -> Option<SessionInfo> {
    for table in SESSION_TABLES {
        let Ok(mut statement) = connection.prepare(&format!("SELECT * FROM {table} WHERE id = ?1"))
        else {
            continue;
        };
        let Ok(mut rows) = statement.query(rusqlite::params![session]) else {
            continue;
        };
        let Ok(Some(row)) = rows.next() else {
            continue;
        };
        return Some(SessionInfo {
            directory: column_text_opt(row, "directory"),
            model: json_column(row, "model"),
        });
    }
    None
}

/// Look the session up by scanning the (small) session table.
fn query_session_info_by_scan(connection: &Connection, session: &str) -> Option<SessionInfo> {
    for table in SESSION_TABLES {
        let Ok(mut statement) = connection.prepare(&format!("SELECT * FROM {table}")) else {
            continue;
        };
        let Ok(mut rows) = statement.query([]) else {
            continue;
        };
        while let Ok(Some(row)) = rows.next() {
            if column_text_opt(row, "id").as_deref() == Some(session) {
                return Some(SessionInfo {
                    directory: column_text_opt(row, "directory"),
                    model: json_column(row, "model"),
                });
            }
        }
    }
    None
}

/// The cached state of a session.
fn session_info(
    connection: &Connection,
    cache: &mut HashMap<String, Option<SessionInfo>>,
    session: &str,
) -> Option<SessionInfo> {
    if let Some(cached) = cache.get(session) {
        return cached.clone();
    }
    let info = query_session_info(connection, session);
    cache.insert(session.to_string(), info.clone());
    info
}

/// Convert one event row into normalized events.
fn record_events(
    connection: &Connection,
    cache: &mut HashMap<String, Option<SessionInfo>>,
    record: &EventRecord,
    seen: &mut HashSet<String>,
) -> Vec<ParsedEvent> {
    let mut events = Vec::new();
    let data = record.data.clone().unwrap_or(Value::Null);
    let info = session_info(connection, cache, &record.session);

    // The workspace and the model belong to the conversation, not to a row.
    let workspace = session_workspace(&data, info.as_ref());
    let labels = model_labels(&data, info.as_ref());
    push_session_facts(&record.session, workspace, &labels, seen, &mut events);

    match record.kind.as_str() {
        "session.execution.started" => events.push(ParsedEvent::TaskStarted {
            started_at_ms: record.created_ms.unwrap_or_else(now_ms),
        }),
        "session.step.started" => events.push(ParsedEvent::Thinking { reasoning: None }),
        "session.execution.succeeded"
        | "session.execution.failed"
        | "session.execution.interrupted" => events.push(ParsedEvent::Completed),
        "session.tool.called" => {
            let Some(name) = data
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
            else {
                return events;
            };
            let input = data.get("input").unwrap_or(&Value::Null);
            let activity = tool_activity(name, input);
            match call_id(&data) {
                Some(call_id) => events.push(ParsedEvent::TrackedToolStarted { call_id, activity }),
                None => events.push(ParsedEvent::ToolStarted { activity }),
            }
        }
        "session.tool.success" | "session.tool.failed" => {
            if is_empty_payload(&data) {
                return events;
            }
            match call_id(&data) {
                Some(call_id) => events.push(ParsedEvent::TrackedToolFinished { call_id }),
                None => events.push(ParsedEvent::ToolFinished),
            }
        }
        "session.shell.started" => {
            if is_empty_payload(&data) {
                return events;
            }
            let activity = ToolActivity {
                label: classify_tool("shell", shell_command(&data)).to_string(),
                file: None,
                language: None,
            };
            match call_id(&data) {
                Some(call_id) => events.push(ParsedEvent::TrackedToolStarted { call_id, activity }),
                None => events.push(ParsedEvent::ToolStarted { activity }),
            }
        }
        "session.shell.ended" => {
            if is_empty_payload(&data) {
                return events;
            }
            match call_id(&data) {
                Some(call_id) => events.push(ParsedEvent::TrackedToolFinished { call_id }),
                None => events.push(ParsedEvent::ToolFinished),
            }
        }
        // "session.created" needs no event of its own: the workspace and the
        // model are reported with the first row of the session.
        _ => {}
    }

    events
}

/// Report the workspace and the model of a conversation once.
fn push_session_facts(
    session: &str,
    workspace: Option<(String, String)>,
    labels: &[String],
    seen: &mut HashSet<String>,
    out: &mut Vec<ParsedEvent>,
) {
    if let Some((project, cwd)) = workspace
        && seen.insert(format!("meta:{session}"))
    {
        out.push(ParsedEvent::SessionMetadata { project, cwd });
    }
    for label in labels {
        if seen.insert(format!("agent:{session}:{label}")) {
            out.push(ParsedEvent::AgentDetected {
                agent: label.clone(),
            });
        }
    }
}

/// The workspace of a conversation.
///
/// The session row is authoritative; the payload of a `session.created` event
/// is the fallback. When neither is known the file a tool touched is used, but
/// only when it is an absolute path.
fn session_workspace(data: &Value, info: Option<&SessionInfo>) -> Option<(String, String)> {
    if let Some(directory) = info
        .and_then(|info| info.directory.clone())
        .or_else(|| payload_directory(data))
    {
        let project = project_name(&directory)?;
        return Some((project, directory));
    }

    let path = data
        .get("input")
        .and_then(|input| input.get("path"))
        .and_then(Value::as_str)?;
    if !Path::new(path).is_absolute() {
        return None;
    }
    detect_project_root(path)
}

/// The working directory inside an event payload.
fn payload_directory(data: &Value) -> Option<String> {
    data.get("location")
        .and_then(|location| location.get("directory"))
        .or_else(|| data.get("directory"))
        .and_then(Value::as_str)
        .map(|directory| directory.trim().to_string())
        .filter(|directory| !directory.is_empty())
}

/// The model labels that are known for a row.
fn model_labels(data: &Value, info: Option<&SessionInfo>) -> Vec<String> {
    let mut labels: Vec<String> = Vec::new();

    // The model of the session row.
    if let Some(label) = info
        .and_then(|info| info.model.as_ref())
        .and_then(model_label)
    {
        labels.push(label);
    }

    // The model of the payload.
    for key in ["model", "modelID", "model_id", "modelRef"] {
        if let Some(label) = data.get(key).and_then(model_label)
            && !labels.contains(&label)
        {
            labels.push(label);
        }
    }

    // The name of the selected agent.
    if let Some(label) = data
        .get("agent")
        .and_then(Value::as_str)
        .and_then(sanitize_model_id)
        && !labels.contains(&label)
    {
        labels.push(label);
    }

    labels
}

/// The label of a model reference.
///
/// A model reference is an object (`{id, providerID}`) or a plain id.
fn model_label(model: &Value) -> Option<String> {
    match model {
        Value::String(text) => sanitize_model_id(text),
        Value::Object(_) => {
            let id = model
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| model.get("modelID").and_then(Value::as_str))?;
            let id = sanitize_model_id(id)?;
            let provider = model
                .get("providerID")
                .or_else(|| model.get("provider_id"))
                .or_else(|| model.get("provider"))
                .and_then(Value::as_str)
                .and_then(sanitize_model_id);
            match provider {
                Some(provider) => {
                    let combined = format!("{provider}/{id}");
                    if combined.len() <= 128 {
                        Some(combined)
                    } else {
                        Some(id)
                    }
                }
                None => Some(id),
            }
        }
        _ => None,
    }
}

/// Keep only a model id that looks like a model id.
fn sanitize_model_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return None;
    }
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'/' | b'@' | b'-'))
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// Keep only a tool call id that looks like an opaque id.
fn sanitize_call_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return None;
    }
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-' | b'/'))
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// The call id of a tool event.
fn call_id(data: &Value) -> Option<String> {
    data.get("id")
        .and_then(Value::as_str)
        .and_then(sanitize_call_id)
}

/// Whether a payload carries nothing that could be reported.
fn is_empty_payload(data: &Value) -> bool {
    data.as_object().is_none_or(|object| object.is_empty())
}

/// The command of a shell event.
fn shell_command(data: &Value) -> &str {
    data.get("command")
        .and_then(Value::as_str)
        .or_else(|| {
            data.get("input")
                .and_then(|input| input.get("command"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default()
}

/// Build the activity of a tool call.
///
/// The arguments are only inspected for a file path, the tool name only for a
/// fixed label. The command, the arguments and the result never leave here.
fn tool_activity(name: &str, input: &Value) -> ToolActivity {
    let base = name.trim().to_ascii_lowercase();
    let command = input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let label = classify_tool(&base, command);
    let file = extract_file_basename(input)
        .or_else(|| legacy_file_basename(input))
        .filter(|name| name != "." && name != "..");
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

/// The basename of a file argument of an older build.
///
/// Current builds use `path`; older ones used `filePath`. The patch tool
/// carries the file name inside the patch text.
fn legacy_file_basename(input: &Value) -> Option<String> {
    for key in ["filePath", "file_path", "file", "path"] {
        if let Some(path) = input.get(key).and_then(Value::as_str)
            && let Some(name) = basename(path)
            && name != "."
            && name != ".."
        {
            return Some(name);
        }
    }

    if let Some(patch) = input.get("patchText").and_then(Value::as_str) {
        for line in patch.lines() {
            let path = line
                .strip_prefix("*** Update File: ")
                .or_else(|| line.strip_prefix("*** Add File: "))
                .or_else(|| line.strip_prefix("*** Delete File: "));
            if let Some(path) = path
                && let Some(name) = basename(path.trim())
                && name != "."
                && name != ".."
            {
                return Some(name);
            }
        }
    }

    None
}

/// Read one row of the message projection.
fn message_record(row: &rusqlite::Row<'_>) -> Option<MessageRecord> {
    let id = column_text_opt(row, "id")?.trim().to_string();
    if id.is_empty() {
        return None;
    }
    let session = column_text_opt(row, "session_id")
        .or_else(|| column_text_opt(row, "sessionID"))?
        .trim()
        .to_string();
    if session.is_empty() {
        return None;
    }
    Some(MessageRecord {
        id,
        session,
        kind: column_text_opt(row, "type").unwrap_or_default(),
        data: json_column(row, "data"),
        created_ms: row_time_ms(row),
        updated_ms: column_text_opt(row, "time_updated")
            .and_then(|text| text.trim().parse::<i64>().ok()),
    })
}

/// The stable key of a message row.
fn message_key(record: &MessageRecord) -> String {
    format!(
        "msg:{}:{}",
        record.id,
        record.updated_ms.or(record.created_ms).unwrap_or_default()
    )
}

/// Convert one message row into normalized events.
fn message_events(
    connection: &Connection,
    cache: &mut HashMap<String, Option<SessionInfo>>,
    record: &MessageRecord,
    seen: &mut HashSet<String>,
) -> Vec<ParsedEvent> {
    let mut events = Vec::new();
    let data = record.data.clone().unwrap_or(Value::Null);
    let info = session_info(connection, cache, &record.session);

    let workspace = info
        .as_ref()
        .and_then(|info| info.directory.clone())
        .and_then(|directory| project_name(&directory).map(|project| (project, directory)));
    let labels = model_labels(&data, info.as_ref());
    push_session_facts(&record.session, workspace, &labels, seen, &mut events);

    match record.kind.to_ascii_lowercase().as_str() {
        "user" => events.push(ParsedEvent::TaskStarted {
            started_at_ms: record.created_ms.unwrap_or_else(now_ms),
        }),
        "assistant" => {
            let Some(Value::Array(content)) = data.get("content") else {
                return events;
            };
            for entry in content {
                if entry.get("type").and_then(Value::as_str) != Some("tool") {
                    continue;
                }
                let state = entry.get("state");
                let status = state
                    .and_then(|state| state.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let call_id = entry
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(sanitize_call_id);
                let input = state.and_then(|state| state.get("input"));
                match status {
                    "completed" | "error" | "cancelled" => match call_id {
                        Some(call_id) => events.push(ParsedEvent::TrackedToolFinished { call_id }),
                        None => events.push(ParsedEvent::ToolFinished),
                    },
                    _ => {
                        let name = entry
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let activity = tool_activity(name, input.unwrap_or(&Value::Null));
                        match call_id {
                            Some(call_id) => {
                                events.push(ParsedEvent::TrackedToolStarted { call_id, activity })
                            }
                            None => events.push(ParsedEvent::ToolStarted { activity }),
                        }
                    }
                }
            }
        }
        "idle" => events.push(ParsedEvent::Completed),
        _ => {}
    }

    events
}

/// The current time in epoch milliseconds.
fn now_ms() -> i64 {
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
        path.push(format!(
            "discodex-opencode-{name}-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
        path
    }

    fn create_db(path: &Path) -> Connection {
        let connection = Connection::open(path).expect("create database");
        connection
            .execute_batch(
                "CREATE TABLE session_v2 (
                     id TEXT PRIMARY KEY,
                     directory TEXT,
                     model TEXT,
                     agent TEXT,
                     time_created INTEGER
                 );
                 CREATE TABLE event (
                     id TEXT PRIMARY KEY,
                     aggregate_id TEXT,
                     seq INTEGER,
                     type TEXT,
                     data TEXT,
                     created INTEGER
                 );",
            )
            .expect("create schema");
        connection
    }

    fn insert_session(connection: &Connection, id: &str, directory: &str, model: &str) {
        connection
            .execute(
                "INSERT INTO session_v2 (id, directory, model, agent, time_created)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, directory, model, "build", 1_792_540_700_000i64],
            )
            .expect("insert session");
    }

    fn insert_event(
        connection: &Connection,
        id: &str,
        session: &str,
        seq: i64,
        kind: &str,
        data: &str,
        created: i64,
    ) {
        connection
            .execute(
                "INSERT INTO event (id, aggregate_id, seq, type, data, created)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![id, session, seq, kind, data, created],
            )
            .expect("insert event");
    }

    #[test]
    fn finds_the_databases_but_never_the_sidecars() {
        let mut root = std::env::temp_dir();
        root.push(format!("discodex-opencode-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create root");

        for name in [
            "opencode.db",
            "opencode-nightly.db",
            "opencode.db-wal",
            "opencode.db-shm",
            "opencode.log",
            "unrelated.db",
        ] {
            std::fs::write(root.join(name), b"").expect("write file");
        }

        let found = databases(&root);
        assert!(found.contains(&root.join("opencode.db")));
        assert!(found.contains(&root.join("opencode-nightly.db")));
        assert!(!found.contains(&root.join("opencode.db-wal")));
        assert!(!found.contains(&root.join("opencode.db-shm")));
        assert!(!found.contains(&root.join("unrelated.db")));

        // A root that does not exist yields nothing.
        assert!(databases(&root.join("missing")).is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn maps_the_event_log_to_normalized_events() {
        let path = temp_db("happy");
        {
            let connection = create_db(&path);
            insert_session(
                &connection,
                "ses_01",
                "C:/work/opencode-demo",
                r#"{"id":"claude-sonnet-4-5","providerID":"anthropic"}"#,
            );
            insert_event(
                &connection,
                "evt_01",
                "ses_01",
                1,
                "session.execution.started",
                "{}",
                1_792_540_721_885,
            );
            insert_event(
                &connection,
                "evt_02",
                "ses_01",
                2,
                "session.tool.called",
                r#"{"sessionID":"ses_01","id":"call_9","name":"edit",
                    "input":{"path":"C:/work/opencode-demo/src/main.rs",
                             "oldString":"SECRET_OLD","newString":"SECRET_NEW"}}"#,
                1_792_540_722_000,
            );
            insert_event(
                &connection,
                "evt_03",
                "ses_01",
                3,
                "session.tool.success",
                r#"{"sessionID":"ses_01","id":"call_9"}"#,
                1_792_540_722_500,
            );
            insert_event(
                &connection,
                "evt_04",
                "ses_01",
                4,
                "session.execution.succeeded",
                "{}",
                1_792_540_723_000,
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        let parsed: Vec<ParsedEvent> = events.iter().map(|event| event.event.clone()).collect();

        assert!(events.iter().all(|event| event.session == "ses_01"));
        assert_eq!(
            parsed,
            vec![
                ParsedEvent::SessionMetadata {
                    project: "opencode-demo".to_string(),
                    cwd: "C:/work/opencode-demo".to_string(),
                },
                ParsedEvent::AgentDetected {
                    agent: "anthropic/claude-sonnet-4-5".to_string(),
                },
                ParsedEvent::TaskStarted {
                    started_at_ms: 1_792_540_721_885,
                },
                ParsedEvent::TrackedToolStarted {
                    call_id: "call_9".to_string(),
                    activity: ToolActivity {
                        label: "Editing files".to_string(),
                        file: Some("main.rs".to_string()),
                        language: Some("Rust".to_string()),
                    },
                },
                ParsedEvent::TrackedToolFinished {
                    call_id: "call_9".to_string(),
                },
                ParsedEvent::Completed,
            ]
        );

        // The same rows are never reported twice.
        assert!(poll(&path, &mut seen).is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn never_forwards_prompts_answers_results_or_paths() {
        let path = temp_db("privacy");
        {
            let connection = create_db(&path);
            insert_session(
                &connection,
                "ses_02",
                "C:/workspace/oc-privacy",
                r#"{"id":"claude-sonnet-4-5","providerID":"anthropic"}"#,
            );
            insert_event(
                &connection,
                "evt_10",
                "ses_02",
                1,
                "session.execution.started",
                "{}",
                1_792_540_721_885,
            );
            insert_event(
                &connection,
                "evt_11",
                "ses_02",
                2,
                "session.tool.called",
                r#"{"id":"call_1","name":"edit",
                    "input":{"path":"C:/workspace/oc-privacy/src/private_module.rs",
                             "oldString":"SECRET_OLD","newString":"SECRET_NEW"}}"#,
                1_792_540_722_000,
            );
            insert_event(
                &connection,
                "evt_12",
                "ses_02",
                3,
                "session.tool.input.ended",
                r#"{"text":"SECRET_PROMPT"}"#,
                1_792_540_722_100,
            );
            insert_event(
                &connection,
                "evt_13",
                "ses_02",
                4,
                "session.text.ended",
                r#"{"text":"SECRET_ANSWER"}"#,
                1_792_540_722_200,
            );
            insert_event(
                &connection,
                "evt_14",
                "ses_02",
                5,
                "session.tool.success",
                r#"{"id":"call_1","content":"SECRET_RESULT"}"#,
                1_792_540_722_500,
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        let debug = format!("{events:?}");

        for secret in [
            "SECRET",
            "SECRET_PROMPT",
            "SECRET_ANSWER",
            "SECRET_RESULT",
            "SECRET_OLD",
            "SECRET_NEW",
            "src/private_module.rs",
            "C:/workspace/oc-privacy/src",
        ] {
            assert!(!debug.contains(secret), "leaked {secret} in {debug}");
        }

        // The basename is the only part of the path that leaves the adapter.
        let activity = events
            .iter()
            .find_map(|event| match &event.event {
                ParsedEvent::TrackedToolStarted { activity, .. } => Some(activity.clone()),
                _ => None,
            })
            .expect("tool activity");
        assert_eq!(activity.label, "Editing files");
        assert_eq!(activity.file.as_deref(), Some("private_module.rs"));
        assert_eq!(activity.language.as_deref(), Some("Rust"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn malformed_and_unknown_records_produce_no_events() {
        let path = temp_db("malformed");
        {
            let connection = create_db(&path);
            insert_event(
                &connection,
                "evt_20",
                "ses_03",
                1,
                "session.something.new",
                r#"{"payload":"redacted"}"#,
                1_792_540_721_885,
            );
            insert_event(
                &connection,
                "evt_21",
                "ses_03",
                2,
                "session.text.ended",
                r#"{"text":"redacted"}"#,
                1_792_540_721_900,
            );
            insert_event(
                &connection,
                "evt_22",
                "ses_03",
                3,
                "session.tool.called",
                "not json at all",
                1_792_540_721_950,
            );
            insert_event(
                &connection,
                "evt_23",
                "ses_03",
                4,
                "session.tool.called",
                "{}",
                1_792_540_722_000,
            );
            insert_event(
                &connection,
                "evt_24",
                "ses_03",
                5,
                "session.tool.success",
                "{}",
                1_792_540_722_100,
            );
            insert_event(
                &connection,
                "evt_25",
                "",
                6,
                "session.execution.started",
                "{}",
                1_792_540_722_200,
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        assert!(events.is_empty(), "unexpected events {events:?}");

        // A database without the opencode tables is tolerated as well.
        let empty = temp_db("empty");
        {
            let connection = Connection::open(&empty).expect("create database");
            connection
                .execute_batch("CREATE TABLE unrelated (id INTEGER PRIMARY KEY);")
                .expect("create schema");
        }
        let mut seen = HashSet::new();
        assert!(poll(&empty, &mut seen).is_empty());
        assert!(poll(&PathBuf::from("does-not-exist.db"), &mut seen).is_empty());

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&empty);
    }

    #[test]
    fn reports_the_model_and_the_turn_timestamp() {
        let path = temp_db("model");
        {
            let connection = create_db(&path);
            insert_event(
                &connection,
                "evt_30",
                "ses_04",
                1,
                "session.execution.started",
                "{}",
                1_792_540_721_885,
            );
            insert_event(
                &connection,
                "evt_31",
                "ses_04",
                2,
                "session.model.selected",
                r#"{"model":{"id":"claude-sonnet-4-5","providerID":"anthropic"}}"#,
                1_792_540_721_900,
            );
            insert_event(
                &connection,
                "evt_32",
                "ses_04",
                3,
                "session.model.selected",
                r#"{"model":{"id":"PRIVATE prose text","providerID":"openai"}}"#,
                1_792_540_721_950,
            );
            insert_event(
                &connection,
                "evt_33",
                "ses_04",
                4,
                "session.agent.selected",
                r#"{"agent":"build"}"#,
                1_792_540_722_000,
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        let parsed: Vec<ParsedEvent> = events.iter().map(|event| event.event.clone()).collect();

        assert_eq!(
            parsed,
            vec![
                ParsedEvent::TaskStarted {
                    started_at_ms: 1_792_540_721_885,
                },
                ParsedEvent::AgentDetected {
                    agent: "anthropic/claude-sonnet-4-5".to_string(),
                },
                ParsedEvent::AgentDetected {
                    agent: "build".to_string(),
                },
            ]
        );

        // A model id that is not a model id is dropped instead of forwarded.
        let debug = format!("{events:?}");
        assert!(!debug.contains("PRIVATE"));
        assert_eq!(sanitize_model_id("PRIVATE prose text"), None);
        assert_eq!(sanitize_model_id(""), None);
        assert_eq!(sanitize_model_id(&"a".repeat(129)), None);
        assert_eq!(
            sanitize_model_id("openai/gpt-5-codex:v2"),
            Some("openai/gpt-5-codex:v2".to_string())
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn falls_back_to_the_message_projection() {
        let path = temp_db("messages");
        {
            let connection = Connection::open(&path).expect("create database");
            connection
                .execute_batch(
                    "CREATE TABLE session_v2 (
                         id TEXT PRIMARY KEY,
                         directory TEXT,
                         model TEXT,
                         agent TEXT,
                         time_created INTEGER
                     );
                     CREATE TABLE session_message (
                         id TEXT PRIMARY KEY,
                         session_id TEXT,
                         type TEXT,
                         model TEXT,
                         seq INTEGER,
                         time_created INTEGER,
                         time_updated INTEGER,
                         data TEXT
                     );",
                )
                .expect("create schema");
            insert_session(
                &connection,
                "ses_05",
                "C:/work/opencode-demo",
                r#"{"id":"claude-sonnet-4-5","providerID":"anthropic"}"#,
            );
            connection
                .execute(
                    "INSERT INTO session_message
                     (id, session_id, type, model, seq, time_created, time_updated, data)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        "msg_01",
                        "ses_05",
                        "user",
                        r#"{"id":"claude-sonnet-4-5","providerID":"anthropic"}"#,
                        1,
                        1_792_540_721_885i64,
                        1_792_540_721_885i64,
                        "{}"
                    ],
                )
                .expect("insert message");
            connection
                .execute(
                    "INSERT INTO session_message
                     (id, session_id, type, model, seq, time_created, time_updated, data)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        "msg_02",
                        "ses_05",
                        "assistant",
                        r#"{"id":"claude-sonnet-4-5","providerID":"anthropic"}"#,
                        2,
                        1_792_540_722_000i64,
                        1_792_540_722_000i64,
                        r#"{"content":[{"type":"text","text":"SECRET_ANSWER"},
                            {"type":"tool","id":"call_3","name":"read",
                             "state":{"status":"running",
                                      "input":{"path":"C:/work/opencode-demo/src/lib.rs"}}}]}"#
                    ],
                )
                .expect("insert message");
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        let parsed: Vec<ParsedEvent> = events.iter().map(|event| event.event.clone()).collect();

        assert_eq!(
            parsed,
            vec![
                ParsedEvent::SessionMetadata {
                    project: "opencode-demo".to_string(),
                    cwd: "C:/work/opencode-demo".to_string(),
                },
                ParsedEvent::AgentDetected {
                    agent: "anthropic/claude-sonnet-4-5".to_string(),
                },
                ParsedEvent::TaskStarted {
                    started_at_ms: 1_792_540_721_885,
                },
                ParsedEvent::TrackedToolStarted {
                    call_id: "call_3".to_string(),
                    activity: ToolActivity {
                        label: "Reading project files".to_string(),
                        file: Some("lib.rs".to_string()),
                        language: Some("Rust".to_string()),
                    },
                },
            ]
        );

        let debug = format!("{events:?}");
        assert!(!debug.contains("SECRET_ANSWER"));

        let _ = std::fs::remove_file(&path);
    }
}
