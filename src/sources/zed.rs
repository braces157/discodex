//! Zed adapter: agent threads in the Zed threads database
//!
//! Zed keeps every agent thread of every project in one SQLite database
//! (`%LOCALAPPDATA%\Zed\threads\threads.db`). The `threads` table holds one
//! row per thread and Zed rewrites the whole row on every save, so there is
//! no log to tail: the adapter polls the database and reports the delta of
//! every thread snapshot.
//!
//! A row carries the thread id, the worktree paths of the conversation, the
//! update time and a blob with the whole conversation. The blob holds the
//! model, the messages, the tool calls and their results.
//!
//! Only structural facts leave this adapter: the workspace folder name, a file
//! basename, an inferred language, a normalized activity label, the model id,
//! the conversation id and timing. Prompts, answers, reasoning, tool
//! arguments, tool results, the thread title and path prefixes stay in the
//! database.

use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{Map, Value};

use super::{StoreEvent, column_text_opt, open_database};
use crate::events::{
    ParsedEvent, ToolActivity, classify_tool, extract_file_basename, language_from_file,
    project_name, timestamp_ms,
};

/// Number of thread rows that are read per poll.
const ROW_LIMIT: i64 = 200;

/// Largest thread blob that is decoded, in bytes.
const MAX_BLOB_BYTES: usize = 32 * 1024 * 1024;

/// Places below the Zed root where the thread database may live.
const DATABASE_PATHS: &[&str] = &["threads.db", "threads/threads.db"];

/// One row of the `threads` table.
struct ThreadRow {
    id: String,
    folder_paths: Option<String>,
    updated_at: Option<String>,
    created_at: Option<String>,
    data_type: Option<String>,
    data: Vec<u8>,
}

/// Databases that hold the harness activity below the given root.
pub fn databases(root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    for relative in DATABASE_PATHS {
        let candidate = root.join(relative);
        if candidate.is_file() && !found.contains(&candidate) {
            found.push(candidate);
        }
    }
    found
}

/// Poll the database for rows that were written since the last poll.
///
/// The `seen` set holds the row keys that were already reported for this
/// database. Because a Zed row is a whole snapshot that is rewritten in place,
/// the key of a row carries the revision of the snapshot: an unchanged row is
/// skipped, a changed row reports only the events that were not reported yet.
pub fn poll(db: &Path, seen: &mut HashSet<String>) -> Vec<StoreEvent> {
    let Ok(connection) = open_database(db) else {
        return Vec::new();
    };
    let Some(rows) = read_rows(&connection) else {
        return Vec::new();
    };

    let mut events = Vec::new();
    // The query returns the newest rows first; report them oldest first so the
    // presence follows the order of the conversations.
    for row in rows.into_iter().rev() {
        events.extend(row_events(&row, seen));
    }
    events
}

/// Read the newest thread rows, tolerating a changed schema.
///
/// The columns are resolved by name, so a column that Zed added or renamed
/// only removes the fact it carried and never the whole row.
fn read_rows(connection: &Connection) -> Option<Vec<ThreadRow>> {
    const QUERIES: &[&str] = &[
        "SELECT * FROM threads ORDER BY updated_at DESC LIMIT ?",
        "SELECT * FROM threads ORDER BY rowid DESC LIMIT ?",
        "SELECT * FROM threads LIMIT ?",
    ];

    for query in QUERIES {
        let Ok(mut statement) = connection.prepare(query) else {
            continue;
        };
        let Ok(mut rows) = statement.query(rusqlite::params![ROW_LIMIT]) else {
            continue;
        };
        let mut out = Vec::new();
        while let Ok(Some(row)) = rows.next() {
            if let Some(thread) = thread_row(row) {
                out.push(thread);
            }
        }
        return Some(out);
    }
    None
}

/// Read one thread row by column name.
fn thread_row(row: &rusqlite::Row<'_>) -> Option<ThreadRow> {
    let id = column_text_opt(row, "id")?;
    let data = column_bytes(row, "data")?;
    Some(ThreadRow {
        id,
        folder_paths: column_text_opt(row, "folder_paths"),
        updated_at: column_text_opt(row, "updated_at"),
        created_at: column_text_opt(row, "created_at"),
        data_type: column_text_opt(row, "data_type"),
        data,
    })
}

/// Read a column as raw bytes.
///
/// The thread blob is compressed, so it must not be converted to text.
fn column_bytes(row: &rusqlite::Row<'_>, name: &str) -> Option<Vec<u8>> {
    let index = row.as_ref().column_index(name).ok()?;
    match row.get_ref(index) {
        Ok(rusqlite::types::ValueRef::Blob(bytes)) => Some(bytes.to_vec()),
        Ok(rusqlite::types::ValueRef::Text(bytes)) => Some(bytes.to_vec()),
        _ => None,
    }
}

/// Convert one thread row into the events that are new since the last poll.
fn row_events(row: &ThreadRow, seen: &mut HashSet<String>) -> Vec<StoreEvent> {
    let Some(session) = sanitize_session_id(&row.id) else {
        // A thread id that is not an opaque id cannot key a presence.
        return Vec::new();
    };

    // The revision of the snapshot is part of the row key, so a row that was
    // not rewritten is skipped and a rewritten row reports only the new
    // events of the conversation.
    let revision = format!(
        "{}|{}|{:016x}",
        row.updated_at.as_deref().unwrap_or_default(),
        row.data.len(),
        fingerprint(&row.data)
    );
    let row_key = format!("row:{session}:{revision}");
    if seen.contains(&row_key) {
        return Vec::new();
    }

    let Some(snapshot) = decode_snapshot(row.data_type.as_deref(), &row.data) else {
        // A row that cannot be read is skipped and retried on the next poll.
        // The cursor is never advanced for a broken row.
        return Vec::new();
    };
    if !known_version(&snapshot) {
        return Vec::new();
    }

    let mut events = Vec::new();

    // The workspace of the conversation. Only the folder name is published, the
    // path itself stays local.
    if let Some(cwd) = workspace_path(row.folder_paths.as_deref())
        && let Some(project) = project_name(&cwd)
        && seen.insert(format!("meta:{session}"))
    {
        events.push(StoreEvent {
            session: session.clone(),
            event: ParsedEvent::SessionMetadata { project, cwd },
        });
    }

    // The model of the conversation.
    if let Some(agent) = model_id(&snapshot)
        && seen.insert(format!("model:{session}:{agent}"))
    {
        events.push(StoreEvent {
            session: session.clone(),
            event: ParsedEvent::AgentDetected { agent },
        });
    }

    // The messages of the conversation. A user message starts a turn, an
    // agent message carries the tool calls, their results and the answer.
    if let Some(Value::Array(messages)) = snapshot.get("messages") {
        for (index, message) in messages.iter().enumerate() {
            events.extend(message_events(&session, index, message, row, seen));
        }
    }

    seen.insert(row_key);
    events
}

/// Convert one message of a thread into the events that are new.
fn message_events(
    session: &str,
    index: usize,
    message: &Value,
    row: &ThreadRow,
    seen: &mut HashSet<String>,
) -> Vec<StoreEvent> {
    let mut events = Vec::new();
    let Some(object) = message.as_object() else {
        return events;
    };

    // A message is an externally tagged enum: {"User": {...}} or
    // {"Agent": {...}}. Every other variant is ignored.
    if object.get("User").is_some_and(|user| !user.is_null()) {
        if seen.insert(format!("turn:{session}:{index}")) {
            events.push(StoreEvent {
                session: session.to_string(),
                event: ParsedEvent::TaskStarted {
                    started_at_ms: turn_started_ms(row),
                },
            });
        }
        return events;
    }
    let Some(agent) = object.get("Agent").and_then(Value::as_object) else {
        return events;
    };

    // The content of the answer: tool calls, thinking and the final text.
    if let Some(Value::Array(blocks)) = agent.get("content") {
        for (slot, block) in blocks.iter().enumerate() {
            let Some(block) = block.as_object() else {
                continue;
            };
            if let Some(tool_use) = block.get("ToolUse").and_then(Value::as_object) {
                events.extend(tool_started_events(session, index, slot, tool_use, seen));
            } else if block.contains_key("Thinking") || block.contains_key("RedactedThinking") {
                // The reasoning itself never leaves the adapter.
                if seen.insert(format!("think:{session}:{index}:{slot}")) {
                    events.push(StoreEvent {
                        session: session.to_string(),
                        event: ParsedEvent::Thinking { reasoning: None },
                    });
                }
            } else if block.contains_key("Text") {
                // The text of the answer is private, only its presence is
                // reported: it ends the turn after the debounce.
                if seen.insert(format!("text:{session}:{index}:{slot}")) {
                    events.push(StoreEvent {
                        session: session.to_string(),
                        event: ParsedEvent::TerminalCandidate,
                    });
                }
            }
        }
    }

    // The results of the tool calls. The results are a map of the tool call
    // id, a list of results is read as well.
    match agent.get("tool_results") {
        Some(Value::Object(results)) => {
            for (key, result) in results {
                events.extend(tool_finished_events(
                    session,
                    index,
                    Some(key),
                    result,
                    seen,
                ));
            }
        }
        Some(Value::Array(results)) => {
            for result in results {
                events.extend(tool_finished_events(session, index, None, result, seen));
            }
        }
        _ => {}
    }

    events
}

/// Report the start of a tool call.
fn tool_started_events(
    session: &str,
    index: usize,
    slot: usize,
    tool_use: &Map<String, Value>,
    seen: &mut HashSet<String>,
) -> Vec<StoreEvent> {
    let mut events = Vec::new();
    let name = tool_use.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = tool_arguments(tool_use);
    let activity = tool_activity(name, &arguments);
    let call_id = tool_use
        .get("id")
        .and_then(Value::as_str)
        .and_then(sanitize_call_id);

    match call_id {
        Some(call_id) => {
            if seen.insert(format!("tool:{session}:{call_id}")) {
                events.push(StoreEvent {
                    session: session.to_string(),
                    event: ParsedEvent::TrackedToolStarted { call_id, activity },
                });
            }
        }
        None => {
            if seen.insert(format!("tool:{session}:{index}:{slot}")) {
                events.push(StoreEvent {
                    session: session.to_string(),
                    event: ParsedEvent::ToolStarted { activity },
                });
            }
        }
    }
    events
}

/// Report the end of a tool call.
fn tool_finished_events(
    session: &str,
    index: usize,
    key: Option<&str>,
    result: &Value,
    seen: &mut HashSet<String>,
) -> Vec<StoreEvent> {
    let mut events = Vec::new();
    let call_id = result
        .get("tool_use_id")
        .and_then(Value::as_str)
        .or(key)
        .and_then(sanitize_call_id);

    match call_id {
        Some(call_id) => {
            if seen.insert(format!("toolend:{session}:{call_id}")) {
                events.push(StoreEvent {
                    session: session.to_string(),
                    event: ParsedEvent::TrackedToolFinished { call_id },
                });
            }
        }
        None => {
            if seen.insert(format!("toolend:{session}:{index}")) {
                events.push(StoreEvent {
                    session: session.to_string(),
                    event: ParsedEvent::ToolFinished,
                });
            }
        }
    }
    events
}

/// The arguments of a tool call.
///
/// The stored input is either a typed input object or the raw JSON string of
/// the call. Both are read, but only the file path and the command are used,
/// and they are reduced to a basename and a fixed label.
fn tool_arguments(tool_use: &Map<String, Value>) -> Vec<Value> {
    let mut candidates = Vec::new();
    if let Some(input) = tool_use.get("input") {
        if let Some(value) = input.get("value").filter(|value| value.is_object()) {
            candidates.push(value.clone());
        }
        if input.is_object() {
            candidates.push(input.clone());
        }
    }
    match tool_use.get("raw_input") {
        Some(Value::String(text)) => {
            if let Ok(value @ Value::Object(_)) = serde_json::from_str::<Value>(text) {
                candidates.push(value);
            }
        }
        Some(value @ Value::Object(_)) => candidates.push(value.clone()),
        _ => {}
    }
    candidates
}

/// The activity of a tool call.
///
/// Only a fixed label, the basename of the file and its language leave the
/// adapter. The command, the arguments and the result stay behind.
fn tool_activity(name: &str, arguments: &[Value]) -> ToolActivity {
    let command = arguments
        .iter()
        .filter_map(|args| args.get("command"))
        .filter_map(Value::as_str)
        .next()
        .unwrap_or("");
    let file = arguments
        .iter()
        .find_map(extract_file_basename)
        .filter(|name| name != "." && name != "..");
    let language = file
        .as_deref()
        .and_then(language_from_file)
        .map(str::to_string);
    ToolActivity {
        label: classify_tool(&name.to_ascii_lowercase(), command).to_string(),
        file,
        language,
    }
}

/// The worktree path of a conversation.
///
/// The column holds the newline joined paths of the worktrees of the thread.
fn workspace_path(folder_paths: Option<&str>) -> Option<String> {
    let text = folder_paths?;
    text.split('\n')
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

/// The moment a user turn started.
///
/// The messages of a thread carry no timestamp, so the update time of the
/// row is used. It is the time of the save that first held the message.
fn turn_started_ms(row: &ThreadRow) -> i64 {
    row.updated_at
        .as_deref()
        .and_then(|text| timestamp_ms(&Value::from(text)))
        .or_else(|| {
            row.created_at
                .as_deref()
                .and_then(|text| timestamp_ms(&Value::from(text)))
        })
        .unwrap_or_else(now_ms)
}

/// The model of a conversation.
fn model_id(snapshot: &Value) -> Option<String> {
    let model = snapshot.get("model")?;
    let raw = match model {
        Value::String(text) => text.as_str(),
        Value::Object(_) => model
            .get("model")
            .and_then(Value::as_str)
            .or_else(|| model.get("provider").and_then(Value::as_str))?,
        _ => return None,
    };
    sanitize_model_id(raw)
}

/// Keep only a model id that looks like a model id.
fn sanitize_model_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return None;
    }
    if !trimmed.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'@' | b'-')
    }) {
        return None;
    }
    Some(trimmed.to_string())
}

/// Keep only a conversation id that looks like an opaque id.
fn sanitize_session_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
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
fn sanitize_call_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return None;
    }
    if !trimmed.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'@' | b'-')
    }) {
        return None;
    }
    Some(trimmed.to_string())
}

/// Decode the thread blob of a row.
///
/// Zed compresses the blob and tags the row with the encoding. A tag that is
/// not known yields no events at all.
fn decode_snapshot(data_type: Option<&str>, data: &[u8]) -> Option<Value> {
    let kind = data_type.unwrap_or("").trim().to_ascii_lowercase();
    match kind.as_str() {
        "zstd" => decode_zstd(data),
        "json" | "text" => serde_json::from_slice(data).ok(),
        // A row without a tag is read as JSON first and as a compressed blob
        // afterwards.
        "" => serde_json::from_slice(data)
            .ok()
            .or_else(|| decode_zstd(data)),
        _ => None,
    }
}

/// Decode a compressed thread blob.
fn decode_zstd(data: &[u8]) -> Option<Value> {
    if data.is_empty() {
        return None;
    }
    let mut buffer = CappedBuffer {
        bytes: Vec::new(),
        limit: MAX_BLOB_BYTES,
    };
    let reader = io::Cursor::new(data).take(MAX_BLOB_BYTES as u64);
    zstd::stream::copy_decode(reader, &mut buffer).ok()?;
    serde_json::from_slice(&buffer.bytes).ok()
}

/// Whether the snapshot has a version this adapter understands.
///
/// A version that is not a plain `major.minor.patch` number means the shape of
/// the blob changed, so the row is skipped instead of guessed.
fn known_version(snapshot: &Value) -> bool {
    match snapshot.get("version") {
        None => true,
        Some(Value::String(text)) => is_version_text(text),
        Some(_) => false,
    }
}

/// Whether a text looks like a `major.minor.patch` version.
fn is_version_text(text: &str) -> bool {
    let mut parts = text.split('.');
    let numbered = (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    });
    numbered && parts.next().is_none()
}

/// A cheap fingerprint of a blob, used to notice a rewritten row.
fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The current time in milliseconds since the epoch.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

/// A writer that refuses to buffer more than the given number of bytes.
struct CappedBuffer {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for CappedBuffer {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.bytes.len().saturating_add(data.len()) > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "thread blob is too large",
            ));
        }
        self.bytes.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THREAD_ID: &str = "01JQZ8H4W9B7VCXK3M2N6P5R8T";
    const UPDATED_AT: &str = "2026-10-21T07:12:33.412Z";
    const WORKSPACE: &str = r"C:\Users\PC\Documents\Discodex";

    fn temp_db(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("discodex-zed-{name}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn create_db(path: &Path) -> Connection {
        let connection = Connection::open(path).expect("create database");
        connection
            .execute_batch(
                "CREATE TABLE threads (
                     id TEXT PRIMARY KEY,
                     summary TEXT NOT NULL,
                     updated_at TEXT NOT NULL,
                     data_type TEXT NOT NULL,
                     data BLOB NOT NULL
                 );
                 ALTER TABLE threads ADD COLUMN parent_id TEXT;
                 ALTER TABLE threads ADD COLUMN folder_paths TEXT;
                 ALTER TABLE threads ADD COLUMN folder_paths_order TEXT;
                 ALTER TABLE threads ADD COLUMN created_at TEXT;",
            )
            .expect("create schema");
        connection
    }

    /// Insert one thread row with a compressed blob.
    ///
    /// Zed rewrites the whole row of a thread, so the row is upserted.
    fn insert_thread(
        connection: &Connection,
        id: &str,
        folder_paths: Option<&str>,
        updated_at: &str,
        snapshot: &str,
    ) {
        let blob = zstd::stream::encode_all(snapshot.as_bytes(), 3).expect("compress snapshot");
        connection
            .execute(
                "INSERT INTO threads (id, summary, updated_at, data_type, data, folder_paths, created_at)
                 VALUES (?1, ?2, ?3, 'zstd', ?4, ?5, ?3)
                 ON CONFLICT(id) DO UPDATE SET
                     updated_at = excluded.updated_at,
                     data_type = excluded.data_type,
                     data = excluded.data,
                     folder_paths = excluded.folder_paths,
                     created_at = excluded.created_at",
                rusqlite::params![
                    id,
                    "redacted thread title",
                    updated_at,
                    blob,
                    folder_paths
                ],
            )
            .expect("insert thread");
    }

    /// A thread snapshot in the shape Zed stores.
    fn snapshot() -> String {
        serde_json::json!({
            "version": "0.3.0",
            "title": "redacted thread title",
            "updated_at": UPDATED_AT,
            "model": { "provider": "anthropic", "model": "claude-sonnet-4-5" },
            "messages": [
                { "User": { "id": "u1", "content": [ { "Text": "PRIVATE_PROMPT" } ] } },
                { "Agent": {
                    "content": [ { "ToolUse": {
                        "id": "toolu_01",
                        "name": "edit_file",
                        "raw_input": "{\"path\":\"C:\\\\Users\\\\PC\\\\Documents\\\\Discodex\\\\src\\\\tray.rs\",\"old_string\":\"PRIVATE_OLD\",\"new_string\":\"PRIVATE_NEW\"}",
                        "input": { "type": "json", "value": { "path": "C:\\Users\\PC\\Documents\\Discodex\\src\\tray.rs" } },
                        "is_input_complete": true,
                        "thought_signature": null } } ],
                    "tool_results": { "toolu_01": {
                        "tool_use_id": "toolu_01",
                        "tool_name": "edit_file",
                        "is_error": false,
                        "content": [ { "Text": "PRIVATE_TOOL_OUTPUT" } ],
                        "output": "PRIVATE_TOOL_OUTPUT" } },
                    "reasoning_details": null } },
                { "Agent": {
                    "content": [ { "Text": "PRIVATE_FINAL_ANSWER" } ],
                    "tool_results": {},
                    "reasoning_details": null } }
            ]
        })
        .to_string()
    }

    fn updated_at_ms() -> i64 {
        chrono::DateTime::parse_from_rfc3339(UPDATED_AT)
            .expect("timestamp")
            .timestamp_millis()
    }

    #[test]
    fn maps_a_thread_snapshot_to_normalized_events() {
        let path = temp_db("happy");
        {
            let connection = create_db(&path);
            insert_thread(
                &connection,
                THREAD_ID,
                Some(WORKSPACE),
                UPDATED_AT,
                &snapshot(),
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        let parsed: Vec<ParsedEvent> = events.iter().map(|event| event.event.clone()).collect();

        assert!(events.iter().all(|event| event.session == THREAD_ID));
        assert_eq!(
            parsed,
            vec![
                // The workspace folder name, never the path itself.
                ParsedEvent::SessionMetadata {
                    project: "Discodex".to_string(),
                    cwd: WORKSPACE.to_string(),
                },
                ParsedEvent::AgentDetected {
                    agent: "claude-sonnet-4-5".to_string(),
                },
                // A user message starts the turn.
                ParsedEvent::TaskStarted {
                    started_at_ms: updated_at_ms(),
                },
                // The tool call carries the label, the basename and the language.
                ParsedEvent::TrackedToolStarted {
                    call_id: "toolu_01".to_string(),
                    activity: ToolActivity {
                        label: "Editing files".to_string(),
                        file: Some("tray.rs".to_string()),
                        language: Some("Rust".to_string()),
                    },
                },
                ParsedEvent::TrackedToolFinished {
                    call_id: "toolu_01".to_string(),
                },
                // The answer of the agent ends the turn after the debounce.
                ParsedEvent::TerminalCandidate,
            ]
        );

        // The same row is never reported twice.
        assert!(poll(&path, &mut seen).is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn never_forwards_prompts_answers_tool_results_or_paths() {
        let path = temp_db("privacy");
        let snapshot = serde_json::json!({
            "version": "0.3.0",
            "model": { "provider": "anthropic", "model": "claude-sonnet-4-5" },
            "messages": [
                { "User": { "id": "u1", "content": [ { "Text": "PRIVATE_PROMPT hunter2" } ] } },
                { "Agent": {
                    "content": [
                        { "Thinking": { "text": "PRIVATE_REASONING" } },
                        { "ToolUse": {
                            "id": "toolu_01",
                            "name": "write_file",
                            "raw_input": "{\"path\":\"C:\\\\private\\\\vault\\\\secret_dir\\\\notes.md\",\"content\":\"PRIVATE_FILE_BODY\"}",
                            "input": { "type": "json", "value": { "path": "C:\\private\\vault\\secret_dir\\notes.md" } } } },
                        { "ToolUse": {
                            "id": "toolu_02",
                            "name": "terminal",
                            "raw_input": "{\"command\":\"cat C:\\\\private\\\\vault\\\\creds.env\"}",
                            "input": { "type": "json", "value": { "command": "cat C:\\private\\vault\\creds.env" } } } }
                    ],
                    "tool_results": { "toolu_01": {
                        "tool_use_id": "toolu_01",
                        "tool_name": "write_file",
                        "is_error": false,
                        "content": [ { "Text": "PRIVATE_TOOL_OUTPUT" } ],
                        "output": "PRIVATE_TOOL_OUTPUT" } },
                    "reasoning_details": "PRIVATE_REASONING_DETAILS" } },
                { "Agent": {
                    "content": [ { "Text": "PRIVATE_FINAL_ANSWER hunter2" } ],
                    "tool_results": {},
                    "reasoning_details": null } }
            ]
        })
        .to_string();
        {
            let connection = create_db(&path);
            insert_thread(
                &connection,
                THREAD_ID,
                Some(r"C:\workspace\zed-demo"),
                UPDATED_AT,
                &snapshot,
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        let debug = format!("{events:?}");

        for secret in [
            "PRIVATE_PROMPT",
            "PRIVATE_REASONING",
            "PRIVATE_REASONING_DETAILS",
            "PRIVATE_FILE_BODY",
            "PRIVATE_TOOL_OUTPUT",
            "PRIVATE_FINAL_ANSWER",
            "hunter2",
            "secret_dir",
            "creds.env",
            r"C:\private\vault",
        ] {
            assert!(!debug.contains(secret), "leaked {secret} in {debug}");
        }

        // Only the basename of the file and a fixed label leave the adapter.
        let write = events
            .iter()
            .find_map(|event| match &event.event {
                ParsedEvent::TrackedToolStarted { call_id, activity } if call_id == "toolu_01" => {
                    Some(activity)
                }
                _ => None,
            })
            .expect("write tool");
        assert_eq!(write.label, "Editing files");
        assert_eq!(write.file.as_deref(), Some("notes.md"));
        assert_eq!(write.language.as_deref(), Some("Markdown"));

        // The command is reduced to a label, the command itself stays behind.
        let shell = events
            .iter()
            .find_map(|event| match &event.event {
                ParsedEvent::TrackedToolStarted { call_id, activity } if call_id == "toolu_02" => {
                    Some(activity)
                }
                _ => None,
            })
            .expect("terminal tool");
        assert!(shell.label == "Running command" || shell.label == "Reading project files");
        assert_eq!(shell.file, None);
        assert_eq!(shell.language, None);

        // The reasoning is never forwarded.
        assert!(
            events
                .iter()
                .any(|event| matches!(event.event, ParsedEvent::Thinking { reasoning: None }))
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn malformed_and_unknown_records_produce_no_events() {
        let path = temp_db("malformed");
        {
            let connection = create_db(&path);
            // A blob that is not valid JSON at all.
            connection
                .execute(
                    "INSERT INTO threads (id, summary, updated_at, data_type, data)
                     VALUES ('broken', 'redacted', ?1, 'json', 'not json')",
                    rusqlite::params![UPDATED_AT],
                )
                .expect("insert broken row");
            // A blob that claims a different encoding.
            connection
                .execute(
                    "INSERT INTO threads (id, summary, updated_at, data_type, data)
                     VALUES ('unknown-type', 'redacted', ?1, 'gzip', ?2)",
                    rusqlite::params![UPDATED_AT, b"whatever".to_vec()],
                )
                .expect("insert unknown row");
            // A blob with compressed garbage.
            connection
                .execute(
                    "INSERT INTO threads (id, summary, updated_at, data_type, data)
                     VALUES ('broken-zstd', 'redacted', ?1, 'zstd', ?2)",
                    rusqlite::params![UPDATED_AT, b"\x28\xb5\x2f\xfd\x00".to_vec()],
                )
                .expect("insert broken zstd row");
            // An empty object, an unknown version and an unknown message.
            for (id, snapshot) in [
                ("empty", "{}"),
                ("no-messages", r#"{"version":"0.3.0"}"#),
                (
                    "unknown-version",
                    r#"{"version":"banana","messages":[{"User":{"content":[]}}]}"#,
                ),
                (
                    "unknown-message",
                    r#"{"version":"0.3.0","messages":[{"Surprise":{"content":[]}}]}"#,
                ),
                (
                    "unknown-block",
                    r#"{"version":"0.3.0","messages":[{"Agent":{"content":[{"Mystery":1}]}}]}"#,
                ),
                (
                    "unknown-model",
                    r#"{"version":"0.3.0","model":{"provider":"a provider with spaces"}}"#,
                ),
            ] {
                insert_thread(&connection, id, None, UPDATED_AT, snapshot);
            }
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        assert!(events.is_empty(), "unexpected events {events:?}");

        // A database without the threads table is tolerated as well.
        let empty = temp_db("no-table");
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
    fn surfaces_the_model_id_and_the_turn_timestamp() {
        let path = temp_db("model");
        {
            let connection = create_db(&path);
            insert_thread(
                &connection,
                THREAD_ID,
                Some(r"C:\workspace\zed-demo"),
                UPDATED_AT,
                r#"{"version":"0.3.0","model":{"provider":"anthropic","model":"claude-sonnet-4-5"},
                    "messages":[{"User":{"id":"u1","content":[{"Text":"PRIVATE_PROMPT"}]}}]}"#,
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        let parsed: Vec<ParsedEvent> = events.iter().map(|event| event.event.clone()).collect();
        assert_eq!(
            parsed,
            vec![
                ParsedEvent::SessionMetadata {
                    project: "zed-demo".to_string(),
                    cwd: r"C:\workspace\zed-demo".to_string(),
                },
                ParsedEvent::AgentDetected {
                    agent: "claude-sonnet-4-5".to_string(),
                },
                ParsedEvent::TaskStarted {
                    started_at_ms: updated_at_ms(),
                },
            ]
        );

        // A model id that is not a model id is dropped.
        assert_eq!(sanitize_model_id("gpt 5 codex"), None);
        assert_eq!(sanitize_model_id(""), None);
        assert_eq!(sanitize_model_id(&"a".repeat(129)), None);
        assert_eq!(
            sanitize_model_id("openai/gpt-5-codex:v2"),
            Some("openai/gpt-5-codex:v2".to_string())
        );
        assert_eq!(
            sanitize_session_id("01JQZ8H4W9B7VCXK3M2N6P5R8T"),
            Some(THREAD_ID.to_string())
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reports_only_the_delta_of_a_rewritten_thread() {
        let path = temp_db("delta");
        {
            let connection = create_db(&path);
            insert_thread(
                &connection,
                THREAD_ID,
                Some(WORKSPACE),
                UPDATED_AT,
                r#"{"version":"0.3.0","model":{"provider":"anthropic","model":"claude-sonnet-4-5"},
                    "messages":[{"User":{"id":"u1","content":[{"Text":"PRIVATE_PROMPT"}]}}]}"#,
            );
        }

        let mut seen = HashSet::new();
        let first = poll(&path, &mut seen);
        assert_eq!(first.len(), 3, "unexpected events {first:?}");
        // The same snapshot is not reported again.
        assert!(poll(&path, &mut seen).is_empty());

        // Zed rewrites the whole row when the thread grows.
        {
            let connection = Connection::open(&path).expect("open database");
            insert_thread(
                &connection,
                THREAD_ID,
                Some(WORKSPACE),
                "2026-10-21T07:13:00.000Z",
                r#"{"version":"0.3.0","model":{"provider":"anthropic","model":"claude-sonnet-4-5"},
                    "messages":[
                        {"User":{"id":"u1","content":[{"Text":"PRIVATE_PROMPT"}]}},
                        {"Agent":{"content":[{"ToolUse":{"id":"toolu_01","name":"read_file",
                            "raw_input":"{\"path\":\"C:\\\\Users\\\\PC\\\\Documents\\\\Discodex\\\\src\\\\state.rs\"}",
                            "input":{"type":"json","value":{"path":"C:\\Users\\PC\\Documents\\Discodex\\src\\state.rs"}}}}],
                            "tool_results":{"toolu_01":{"tool_use_id":"toolu_01","tool_name":"read_file",
                            "is_error":false,"content":[]}},
                            "reasoning_details":null}}]}"#,
            );
        }

        let second = poll(&path, &mut seen);
        let parsed: Vec<ParsedEvent> = second.iter().map(|event| event.event.clone()).collect();
        assert_eq!(
            parsed,
            vec![
                ParsedEvent::TrackedToolStarted {
                    call_id: "toolu_01".to_string(),
                    activity: ToolActivity {
                        label: "Reading project files".to_string(),
                        file: Some("state.rs".to_string()),
                        language: Some("Rust".to_string()),
                    },
                },
                ParsedEvent::TrackedToolFinished {
                    call_id: "toolu_01".to_string(),
                },
            ],
            "the old turn must not be reported again"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reads_tool_results_that_are_stored_as_a_list() {
        let path = temp_db("list-results");
        {
            let connection = create_db(&path);
            insert_thread(
                &connection,
                THREAD_ID,
                Some(WORKSPACE),
                UPDATED_AT,
                r#"{"version":"0.3.0","messages":[{"Agent":{
                    "content":[{"ToolUse":{"id":"toolu_01","name":"grep",
                        "raw_input":"{\"pattern\":\"PRIVATE_PATTERN\"}"}}],
                    "tool_results":[
                        {"tool_use_id":"toolu_01","tool_name":"grep","is_error":false,"output":"PRIVATE_OUTPUT"},
                        {"tool_name":"unknown_tool","is_error":false,"output":"PRIVATE_OUTPUT"}]}}]}"#,
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
                    cwd: WORKSPACE.to_string(),
                },
                ParsedEvent::TrackedToolStarted {
                    call_id: "toolu_01".to_string(),
                    activity: ToolActivity {
                        label: "Searching project".to_string(),
                        file: None,
                        language: None,
                    },
                },
                ParsedEvent::TrackedToolFinished {
                    call_id: "toolu_01".to_string(),
                },
                // A result without a call id cannot be tracked.
                ParsedEvent::ToolFinished,
            ]
        );

        let debug = format!("{events:?}");
        assert!(!debug.contains("PRIVATE_PATTERN"));
        assert!(!debug.contains("PRIVATE_OUTPUT"));

        let _ = std::fs::remove_file(&path);
    }
}
