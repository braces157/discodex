//! Crush adapter: session messages in the per-project Crush database
//!
//! Charm Crush keeps one SQLite database per project. The global app state
//! lives in `%LOCALAPPDATA%\crush\crush.json`, the project registry in
//! `%LOCALAPPDATA%\crush\projects.json` (an array of `{path, data_dir,
//! last_accessed}`) and the session store of a project in
//! `<data_dir>\crush.db`.
//!
//! The `sessions` table holds the conversations (`id`, `created_at`,
//! `updated_at`, `parent_session_id`), the `messages` table holds the turns
//! (`id`, `session_id`, `role`, `model`, `provider`, `created_at`,
//! `updated_at`) and the payload of a turn is the `parts` JSON column. Every
//! part is `{type, data}`; the known types are `text`, `reasoning`,
//! `tool_call`, `tool_result` and `finish`.
//!
//! Only structural facts leave this adapter: the workspace folder name, a file
//! basename, an inferred language, a normalized activity label, the model id
//! and timing. Prompts, answers, reasoning, tool arguments, tool results and
//! path prefixes never leave the database.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde_json::Value;

use super::{StoreEvent, column_json, column_text_opt, open_database};
use crate::events::{
    ParsedEvent, ToolActivity, classify_tool, extract_file_basename, language_from_file,
    project_name, timestamp_ms,
};

/// Number of message rows that are read per poll.
const ROW_LIMIT: i64 = 200;

/// Name of the project registry inside the Crush data directory.
const REGISTRY_FILE: &str = "projects.json";

/// Name of the session database inside a project data directory.
const DATABASE_FILE: &str = "crush.db";

/// Name of the default data directory inside a project.
const DATA_DIR_NAME: &str = ".crush";

/// Magic bytes of a zstd frame. Upstream plans to compress the parts column,
/// so a compressed blob is skipped instead of being guessed at.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Labels of the Crush tools that the shared classifier does not cover.
const CRUSH_TOOL_LABELS: &[(&str, &str)] = &[
    ("view", "Reading project files"),
    ("ls", "Reading project files"),
    ("edit", "Editing files"),
    ("write", "Editing files"),
    ("multi_edit", "Editing files"),
    ("multiedit", "Editing files"),
    ("download", "Editing files"),
    ("fetch", "Browsing documentation"),
    ("web_search", "Browsing documentation"),
    ("todos", "Planning changes"),
    ("todo", "Planning changes"),
    ("question", "Waiting for input"),
];

/// The workspace of every known database.
///
/// The project registry is the only place that knows the workspace of a
/// database that uses a custom data directory, so `databases()` remembers the
/// mapping and `poll()` reads it back.
static WORKSPACES: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();

/// Databases that hold the harness activity below the given root.
pub fn databases(root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();

    // Every project of the registry has its own database.
    if let Some(registry) = registry_path(root) {
        for (data_dir, workspace) in read_registry(&registry, root) {
            let database = data_dir.join(DATABASE_FILE);
            remember_workspace(&database, &workspace);
            if !found.contains(&database) {
                found.push(database);
            }
        }
    }

    // Fallback: without a usable registry the database of the root itself is
    // the only candidate.
    if found.is_empty() {
        let fallback = root.join(DATABASE_FILE);
        if let Some(workspace) = workspace_of(&fallback) {
            remember_workspace(&fallback, &workspace);
        }
        found.push(fallback);

        // A project that is used as the root keeps its database in `.crush`.
        let in_project = root.join(DATA_DIR_NAME).join(DATABASE_FILE);
        if in_project.is_file() {
            if let Some(workspace) = workspace_of(&in_project) {
                remember_workspace(&in_project, &workspace);
            }
            found.push(in_project);
        }
    }

    found
}

/// Poll the database for rows that were written since the last poll.
///
/// The `seen` set holds the row keys that were already reported. Every key is
/// namespaced (`row:`, `meta:`, `model:`, `sub:`, `task:`, `reason:`,
/// `call:`, `result:`, `finish:`) so the conversation scoped bookkeeping never
/// collides with the row cursor.
pub fn poll(db: &Path, seen: &mut HashSet<String>) -> Vec<StoreEvent> {
    let Ok(connection) = open_database(db) else {
        return Vec::new();
    };
    let Some(rows) = read_messages(&connection) else {
        return Vec::new();
    };
    let parents = read_parents(&connection);
    let workspace = workspace_of_database(db);

    let mut events = Vec::new();
    // The query returns the newest rows first; report them oldest first so the
    // presence follows the order of the conversation.
    for row in rows.into_iter().rev() {
        for event in row_events(&row, &parents, workspace.as_deref(), seen) {
            events.push(StoreEvent {
                session: row.session_id.clone(),
                event,
            });
        }
    }
    events
}

/// One row of the `messages` table.
struct MessageRow {
    id: String,
    session_id: String,
    role: String,
    model: Option<String>,
    created_at_ms: Option<i64>,
    updated_at: Option<i64>,
    parts: Option<Value>,
    parts_len: usize,
}

/// The queries that read the message rows, most precise first.
///
/// The parts of a message are flushed while the turn runs, so a row is read
/// again when it changed. The `rowid` is the insertion order of the rows and
/// breaks the ties of the one second resolution of the timestamps.
const MESSAGE_QUERIES: &[&str] = &[
    "SELECT * FROM messages ORDER BY created_at DESC, rowid DESC LIMIT ?",
    "SELECT * FROM messages ORDER BY created_at DESC, id DESC LIMIT ?",
    "SELECT * FROM messages LIMIT ?",
];

/// Read the newest message rows, tolerating a changed schema.
fn read_messages(connection: &Connection) -> Option<Vec<MessageRow>> {
    for query in MESSAGE_QUERIES {
        let Ok(mut statement) = connection.prepare(query) else {
            continue;
        };
        let Ok(mut rows) = statement.query(rusqlite::params![ROW_LIMIT]) else {
            continue;
        };
        let mut out = Vec::new();
        while let Ok(Some(row)) = rows.next() {
            if let Some(message) = message_row(row) {
                out.push(message);
            }
        }
        if !out.is_empty() {
            return Some(out);
        }
    }
    None
}

/// Read one message row.
///
/// Every column is read by name, so a column that was added or renamed by a
/// migration yields `None` instead of an error.
fn message_row(row: &rusqlite::Row<'_>) -> Option<MessageRow> {
    let id = column_text_opt(row, "id")?.trim().to_string();
    if id.is_empty() {
        return None;
    }
    let session_id = column_text_opt(row, "session_id")?.trim().to_string();
    if session_id.is_empty() {
        return None;
    }
    let role = column_text_opt(row, "role")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let model = column_text_opt(row, "model");
    let created_at_ms = epoch_ms(column_text_opt(row, "created_at"));
    let updated_at = epoch_ms(column_text_opt(row, "updated_at"));
    let parts_index = row.as_ref().column_index("parts").ok();
    let parts = parts_index.and_then(|index| parts_column(row, index));
    // The length of the column changes when a part is flushed, so it detects
    // an updated row even when the timestamp was not bumped.
    let parts_len = parts_index
        .and_then(|_| column_text_opt(row, "parts"))
        .map(|text| text.len())
        .unwrap_or_default();
    Some(MessageRow {
        id,
        session_id,
        role,
        model,
        created_at_ms,
        updated_at,
        parts,
        parts_len,
    })
}

/// Read the parts column.
///
/// A compressed blob is detected and skipped, an undecodable blob yields no
/// part at all.
fn parts_column(row: &rusqlite::Row<'_>, index: usize) -> Option<Value> {
    if is_compressed(row, index) {
        return None;
    }
    match row.get_ref(index) {
        Ok(rusqlite::types::ValueRef::Blob(bytes)) => serde_json::from_slice(bytes).ok(),
        _ => column_json(row, index),
    }
}

/// Whether the column holds a compressed frame.
///
/// Upstream plans to store the parts zstd compressed. A compressed blob is
/// skipped instead of being guessed at.
fn is_compressed(row: &rusqlite::Row<'_>, index: usize) -> bool {
    match row.get_ref(index) {
        Ok(rusqlite::types::ValueRef::Blob(bytes)) => bytes.starts_with(&ZSTD_MAGIC),
        Ok(rusqlite::types::ValueRef::Text(bytes)) => bytes.starts_with(&ZSTD_MAGIC),
        _ => false,
    }
}

/// Read the parent session of every conversation.
///
/// A session with a parent is a sub agent of that conversation.
fn read_parents(connection: &Connection) -> HashMap<String, String> {
    let mut parents = HashMap::new();
    let Ok(mut statement) = connection.prepare("SELECT id, parent_session_id FROM sessions") else {
        return parents;
    };
    let Ok(mut rows) = statement.query([]) else {
        return parents;
    };
    while let Ok(Some(row)) = rows.next() {
        let Some(id) = column_text_opt(row, "id") else {
            continue;
        };
        let Some(parent) = column_text_opt(row, "parent_session_id") else {
            continue;
        };
        let parent = parent.trim();
        if parent.is_empty() {
            continue;
        }
        parents.insert(id.trim().to_string(), parent.to_string());
    }
    parents
}

/// Convert one message row into normalized events.
fn row_events(
    row: &MessageRow,
    parents: &HashMap<String, String>,
    workspace: Option<&str>,
    seen: &mut HashSet<String>,
) -> Vec<ParsedEvent> {
    let mut events = Vec::new();

    // The row was already reported in this state.
    let row_key = format!(
        "row:{}:{}:{}:{}",
        row.session_id,
        row.id,
        row.updated_at.unwrap_or_default(),
        row.parts_len
    );
    if !seen.insert(row_key) {
        return events;
    }

    // The workspace of the project, reported once per conversation.
    if seen.insert(format!("meta:{}", row.session_id))
        && let Some(workspace) = workspace
        && let Some(project) = project_name(workspace)
    {
        events.push(ParsedEvent::SessionMetadata {
            project,
            cwd: workspace.to_string(),
        });
    }

    // A conversation with a parent session is a sub agent of it.
    if let Some(parent) = parents.get(&row.session_id)
        && seen.insert(format!("sub:{}", row.session_id))
        && let Some(conversation_id) = sanitize_id(parent)
    {
        let agent = row
            .model
            .as_deref()
            .and_then(sanitize_model_id)
            .unwrap_or_else(|| "Crush Subagent".to_string());
        events.push(ParsedEvent::SubagentSpawned {
            conversation_id,
            agent,
        });
    }

    // The model of the turn.
    if row.role == "assistant"
        && let Some(model) = row.model.as_deref().and_then(sanitize_model_id)
        && seen.insert(format!("model:{}:{}", row.session_id, model))
    {
        events.push(ParsedEvent::AgentDetected { agent: model });
    }

    // A new user turn starts the elapsed timer.
    if row.role == "user" && seen.insert(format!("task:{}:{}", row.session_id, row.id)) {
        events.push(ParsedEvent::TaskStarted {
            started_at_ms: row.created_at_ms.unwrap_or_else(now_ms),
        });
    }

    let Some(Value::Array(parts)) = row.parts.as_ref() else {
        return events;
    };

    for (index, part) in parts.iter().enumerate() {
        let Some(part_type) = part.get("type").and_then(Value::as_str) else {
            continue;
        };
        let Some(data) = part_data(part) else {
            continue;
        };
        match part_type {
            "reasoning" => {
                // The reasoning text itself never leaves the database.
                let key = format!("reason:{}:{}", row.session_id, row.id);
                if seen.insert(key) {
                    events.push(ParsedEvent::Thinking { reasoning: None });
                }
            }
            "tool_call" => {
                let Some(name) = data.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let activity = tool_activity(name, data.get("input"));
                match data.get("id").and_then(Value::as_str).and_then(sanitize_id) {
                    Some(call_id) => {
                        if seen.insert(format!("call:{}:{}", row.session_id, call_id)) {
                            events.push(ParsedEvent::TrackedToolStarted { call_id, activity });
                        }
                    }
                    None => {
                        // The store has no call id, so the tool cannot be paired.
                        if seen.insert(format!("call:{}:{}:{}", row.session_id, row.id, index)) {
                            events.push(ParsedEvent::ToolStarted { activity });
                        }
                    }
                }
            }
            "tool_result" => {
                match data
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .and_then(sanitize_id)
                {
                    Some(call_id) => {
                        if seen.insert(format!("result:{}:{}", row.session_id, call_id)) {
                            events.push(ParsedEvent::TrackedToolFinished { call_id });
                        }
                    }
                    None => {
                        if seen.insert(format!("result:{}:{}:{}", row.session_id, row.id, index)) {
                            events.push(ParsedEvent::ToolFinished);
                        }
                    }
                }
            }
            "finish" => {
                let key = format!("finish:{}:{}", row.session_id, row.id);
                if seen.insert(key) {
                    events.push(ParsedEvent::Completed);
                }
            }
            // "text" carries the prompt or the answer and every other type is
            // unknown: both are ignored.
            _ => {}
        }
    }

    events
}

/// The payload of a part.
///
/// Upstream stores every part as `{type, data}`. A flat part is accepted as
/// well, a part whose payload is not an object is ignored.
fn part_data(part: &Value) -> Option<&Value> {
    match part.get("data") {
        Some(data @ Value::Object(_)) => Some(data),
        Some(_) => None,
        None => Some(part),
    }
}

/// The activity of a tool call.
///
/// The tool name decides the label and the arguments are only inspected for a
/// file path. The command, the prompt and the result never leave here.
fn tool_activity(name: &str, input: Option<&Value>) -> ToolActivity {
    let base = name.trim().to_ascii_lowercase();
    let arguments = input.and_then(parse_arguments);
    let command = arguments
        .as_ref()
        .and_then(|args| args.get("command"))
        .and_then(Value::as_str);
    let file = arguments
        .as_ref()
        .and_then(extract_file_basename)
        .filter(|name| name != "." && name != "..");
    let language = file
        .as_deref()
        .and_then(language_from_file)
        .map(str::to_string);
    ToolActivity {
        label: crush_label(&base, command.unwrap_or("")).to_string(),
        file,
        language,
    }
}

/// The label of a Crush tool.
///
/// The shared classifier covers most tool names, the names Crush adds get
/// their own label.
fn crush_label(name: &str, command: &str) -> &'static str {
    for (tool, label) in CRUSH_TOOL_LABELS {
        if name == *tool {
            return label;
        }
    }
    classify_tool(name, command)
}

/// The arguments of a tool call.
///
/// Crush stores the arguments as a JSON string. A string that is not JSON is
/// ignored, so no argument is ever guessed at.
fn parse_arguments(input: &Value) -> Option<Value> {
    match input {
        Value::String(text) => serde_json::from_str::<Value>(text).ok(),
        Value::Object(map) => Some(Value::Object(map.clone())),
        _ => None,
    }
}

/// Keep only a model id that looks like a model id.
///
/// Model ids are metadata. Prose, control characters and long strings are
/// dropped instead of being forwarded.
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

/// Keep only an identifier that looks like an opaque id.
fn sanitize_id(raw: &str) -> Option<String> {
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

/// Convert a stored timestamp into milliseconds.
///
/// Crush stores Unix seconds. A value that is already in milliseconds is kept
/// as it is.
fn epoch_ms(text: Option<String>) -> Option<i64> {
    let text = text?.trim().to_string();
    if text.is_empty() {
        return None;
    }
    if let Ok(seconds) = text.parse::<i64>() {
        return timestamp_ms(&Value::from(seconds));
    }
    if let Ok(seconds) = text.parse::<f64>() {
        return timestamp_ms(&Value::from(seconds));
    }
    timestamp_ms(&Value::from(text))
}

/// The current time in milliseconds.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

/// The registry that lists the projects of the user.
fn registry_path(root: &Path) -> Option<PathBuf> {
    let direct = root.join(REGISTRY_FILE);
    if direct.is_file() {
        return Some(direct);
    }
    let nested = root.join("crush").join(REGISTRY_FILE);
    if nested.is_file() {
        return Some(nested);
    }
    None
}

/// Read the project registry.
///
/// The file holds an array of `{path, data_dir, last_accessed}` entries. An
/// unknown shape yields no project at all.
fn read_registry(registry: &Path, root: &Path) -> Vec<(PathBuf, String)> {
    let Ok(text) = std::fs::read_to_string(registry) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let entries = match &value {
        Value::Array(entries) => entries.as_slice(),
        Value::Object(map) => match map.get("projects") {
            Some(Value::Array(entries)) => entries.as_slice(),
            _ => &[],
        },
        _ => &[],
    };

    let mut projects = Vec::new();
    for entry in entries {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let Some(data_dir) = entry.get("data_dir").and_then(Value::as_str) else {
            continue;
        };
        let workspace = entry
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(str::to_string);
        let mut directory = PathBuf::from(data_dir.trim());
        if directory.is_relative() {
            let base = workspace
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| root.to_path_buf());
            directory = base.join(directory);
        }
        let workspace = match workspace {
            Some(workspace) => workspace,
            None => match workspace_of(&directory.join(DATABASE_FILE)) {
                Some(workspace) => workspace,
                None => continue,
            },
        };
        projects.push((directory, workspace));
    }
    projects
}

/// The workspace directory of a database.
///
/// Crush stores the database of a project in `<project>\.crush\crush.db`, so
/// the project folder is the parent of the `.crush` directory.
fn workspace_of(database: &Path) -> Option<String> {
    let parent = database.parent()?;
    let name = parent.file_name().and_then(|name| name.to_str());
    if name.map(|name| name.eq_ignore_ascii_case(DATA_DIR_NAME)) == Some(true) {
        return parent
            .parent()
            .map(|path| path.to_string_lossy().to_string());
    }
    Some(parent.to_string_lossy().to_string())
}

/// Remember the workspace of a database.
fn remember_workspace(database: &Path, workspace: &str) {
    if workspace.is_empty() {
        return;
    }
    if let Ok(mut map) = workspaces().lock() {
        map.insert(database.to_path_buf(), workspace.to_string());
    }
}

/// The workspace of a database.
fn workspace_of_database(database: &Path) -> Option<String> {
    if let Ok(map) = workspaces().lock()
        && let Some(workspace) = map.get(database)
    {
        return Some(workspace.clone());
    }
    workspace_of(database)
}

/// The remembered workspaces.
fn workspaces() -> &'static Mutex<HashMap<PathBuf, String>> {
    WORKSPACES.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parts of an assistant turn: a tool call, its result and the end of
    /// the turn. The arguments are the JSON string that Crush stores.
    const ASSISTANT_PARTS: &str = r#"[{"type":"tool_call","data":{"id":"call_01","name":"view","input":"{\"file_path\":\"C:\\\\Users\\\\PC\\\\Documents\\\\Discodex\\\\src\\\\provider.rs\",\"offset\":0,\"limit\":200}","provider_executed":false,"finished":true}},{"type":"tool_result","data":{"tool_call_id":"call_01","name":"view","content":"<REDACTED>","is_error":false}},{"type":"finish","data":{"reason":"stop","time":1789189714}}]"#;

    fn temp_root(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("discodex-crush-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn create_db(path: &Path) -> Connection {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create data dir");
        }
        let connection = Connection::open(path).expect("create database");
        connection
            .execute_batch(
                "CREATE TABLE sessions (
                     id TEXT PRIMARY KEY,
                     created_at INTEGER,
                     updated_at INTEGER,
                     parent_session_id TEXT
                 );
                 CREATE TABLE messages (
                     id TEXT PRIMARY KEY,
                     session_id TEXT,
                     role TEXT,
                     model TEXT,
                     provider TEXT,
                     created_at INTEGER,
                     updated_at INTEGER,
                     parts TEXT
                 );",
            )
            .expect("create schema");
        connection
    }

    fn insert_session(connection: &Connection, id: &str, parent: Option<&str>) {
        connection
            .execute(
                "INSERT INTO sessions (id, created_at, updated_at, parent_session_id)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, 1_789_189_700_i64, 1_789_189_720_i64, parent],
            )
            .expect("insert session");
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_message(
        connection: &Connection,
        id: &str,
        session: &str,
        role: &str,
        model: Option<&str>,
        provider: Option<&str>,
        created_at: i64,
        updated_at: i64,
        parts: &str,
    ) {
        connection
            .execute(
                "INSERT INTO messages
                     (id, session_id, role, model, provider, created_at, updated_at, parts)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    id, session, role, model, provider, created_at, updated_at, parts
                ],
            )
            .expect("insert message");
    }

    #[test]
    fn maps_crush_messages_to_normalized_events() {
        let root = temp_root("happy");
        let project = root.join("MyProject");
        let database = project.join(".crush").join("crush.db");
        {
            let connection = create_db(&database);
            insert_session(&connection, "session-1", None);
            insert_message(
                &connection,
                "m-1",
                "session-1",
                "user",
                None,
                None,
                1_789_189_713,
                1_789_189_713,
                r#"[{"type":"text","data":{"text":"a redacted prompt"}}]"#,
            );
            insert_message(
                &connection,
                "m-2",
                "session-1",
                "assistant",
                Some("claude-sonnet-4-5"),
                Some("anthropic"),
                1_789_189_714,
                1_789_189_714,
                ASSISTANT_PARTS,
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&database, &mut seen);
        let parsed: Vec<ParsedEvent> = events.iter().map(|event| event.event.clone()).collect();

        assert!(events.iter().all(|event| event.session == "session-1"));
        assert_eq!(
            parsed,
            vec![
                ParsedEvent::SessionMetadata {
                    project: "MyProject".to_string(),
                    cwd: project.to_string_lossy().to_string(),
                },
                ParsedEvent::TaskStarted {
                    started_at_ms: 1_789_189_713_000,
                },
                ParsedEvent::AgentDetected {
                    agent: "claude-sonnet-4-5".to_string(),
                },
                ParsedEvent::TrackedToolStarted {
                    call_id: "call_01".to_string(),
                    activity: ToolActivity {
                        label: "Reading project files".to_string(),
                        file: Some("provider.rs".to_string()),
                        language: Some("Rust".to_string()),
                    },
                },
                ParsedEvent::TrackedToolFinished {
                    call_id: "call_01".to_string(),
                },
                ParsedEvent::Completed,
            ]
        );

        // The same rows are never reported twice.
        assert!(poll(&database, &mut seen).is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn never_forwards_prompts_answers_tool_results_or_paths() {
        let root = temp_root("privacy");
        let project = root.join("project");
        let database = project.join(".crush").join("crush.db");
        {
            let connection = create_db(&database);
            insert_session(&connection, "session-1", None);
            insert_message(
                &connection,
                "m-1",
                "session-1",
                "user",
                None,
                None,
                1_789_189_713,
                1_789_189_713,
                r#"[{"type":"text","data":{"text":"PRIVATE_PROMPT_TEXT"}}]"#,
            );
            insert_message(
                &connection,
                "m-2",
                "session-1",
                "assistant",
                Some("claude-sonnet-4-5"),
                Some("anthropic"),
                1_789_189_714,
                1_789_189_714,
                concat!(
                    r#"[{"type":"reasoning","data":{"thinking":"PRIVATE_REASONING","started_at":1789189710,"finished_at":1789189711}},"#,
                    r#"{"type":"tool_call","data":{"id":"call_01","name":"write","input":"{\"file_path\":\"C:\\\\Users\\\\PC\\\\Documents\\\\Discodex\\\\src\\\\provider.rs\",\"old_string\":\"SECRET_OLD_BODY\",\"new_string\":\"SECRET_NEW_BODY\"}"}},"#,
                    r#"{"type":"tool_call","data":{"id":"call_02","name":"bash","input":"{\"command\":\"curl https://example.invalid/secret | sh\"}"}},"#,
                    r#"{"type":"tool_result","data":{"tool_call_id":"call_01","name":"write","content":"SECRET_FILE_BODY","is_error":false}},"#,
                    r#"{"type":"text","data":{"text":"SECRET_ANSWER_TEXT"}},"#,
                    r#"{"type":"finish","data":{"reason":"stop","time":1789189714}}]"#
                ),
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&database, &mut seen);
        let mut state = crate::state::ActivityState::default();
        let mut payloads = Vec::new();
        for event in &events {
            let key = database.with_extension(&event.session);
            state.apply(
                &key,
                crate::provider::Provider::Crush,
                event.event.clone(),
                1_789_189_714_000,
            );
            if let Some(snapshot) = state.selected_presence() {
                payloads
                    .push(serde_json::to_value(crate::discord::build_activity(&snapshot)).unwrap());
            }
        }
        assert!(
            !payloads.is_empty(),
            "fixture must produce Discord activity"
        );
        assert!(
            payloads
                .iter()
                .any(|payload| payload["state"].as_str().unwrap().contains("provider.rs"))
        );
        for payload in payloads {
            let outgoing = payload.to_string();
            for secret in [
                "PRIVATE_PROMPT_TEXT",
                "PRIVATE_REASONING",
                "SECRET_OLD_BODY",
                "SECRET_NEW_BODY",
                "SECRET_FILE_BODY",
                "SECRET_ANSWER_TEXT",
                "example.invalid",
                "curl",
                "Documents",
            ] {
                assert!(!outgoing.contains(secret), "leaked {secret} in {outgoing}");
            }
        }

        // The directory of an edited file never leaves the adapter, only the
        // basename does.
        let write = events
            .iter()
            .find_map(|event| match &event.event {
                ParsedEvent::TrackedToolStarted { call_id, activity } if call_id == "call_01" => {
                    Some(activity.clone())
                }
                _ => None,
            })
            .expect("write tool");
        assert_eq!(write.label, "Editing files");
        assert_eq!(write.file.as_deref(), Some("provider.rs"));
        assert_eq!(write.language.as_deref(), Some("Rust"));

        // The command of a shell tool only decides the label.
        let shell = events
            .iter()
            .find_map(|event| match &event.event {
                ParsedEvent::TrackedToolStarted { call_id, activity } if call_id == "call_02" => {
                    Some(activity.clone())
                }
                _ => None,
            })
            .expect("shell tool");
        assert_eq!(shell.label, "Running command");
        assert_eq!(shell.file, None);
        assert_eq!(shell.language, None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn malformed_and_unknown_records_produce_no_events() {
        let root = temp_root("malformed");
        let database = root.join("project").join(".crush").join("crush.db");
        {
            let connection = create_db(&database);
            insert_session(&connection, "session-1", None);
            for (index, parts) in [
                "not json",
                "{}",
                "[]",
                r#"[{"type":"somethingNew","data":{"payload":"redacted"}}]"#,
                r#"[{}]"#,
                r#"[{"type":"tool_call"}]"#,
                r#"[{"type":"tool_call","data":{"name":123}}]"#,
                r#"[{"type":"tool_call","data":{"input":"{}"}}]"#,
                r#"[{"type":"tool_result","data":"not an object"}]"#,
                r#"[{"type":"reasoning","data":"not an object"}]"#,
                r#"[{"type":"text","data":"not an object"}]"#,
            ]
            .iter()
            .enumerate()
            {
                insert_message(
                    &connection,
                    &format!("m-{index}"),
                    "session-1",
                    "assistant",
                    None,
                    None,
                    1_789_189_700 + index as i64,
                    1_789_189_700 + index as i64,
                    parts,
                );
            }
            // A row without a session cannot be assigned to a conversation.
            insert_message(
                &connection,
                "m-orphan",
                "",
                "assistant",
                None,
                None,
                1_789_189_800,
                1_789_189_800,
                r#"[{"type":"finish","data":{"reason":"stop"}}]"#,
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&database, &mut seen);
        // Only the workspace of the project is reported, no record of the
        // malformed rows turns into an activity.
        assert!(
            events
                .iter()
                .all(|event| matches!(event.event, ParsedEvent::SessionMetadata { .. })),
            "unexpected events {events:?}"
        );
        assert_eq!(events.len(), 1, "unexpected events {events:?}");

        // A database without the crush tables is tolerated as well.
        let unrelated = root.join("unrelated.db");
        {
            let connection = Connection::open(&unrelated).expect("create database");
            connection
                .execute_batch("CREATE TABLE unrelated (id INTEGER PRIMARY KEY);")
                .expect("create schema");
        }
        let mut seen = HashSet::new();
        assert!(poll(&unrelated, &mut seen).is_empty());
        assert!(poll(&PathBuf::from("does-not-exist.db"), &mut seen).is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reports_unpaired_tools_and_the_end_of_a_turn() {
        let root = temp_root("unpaired");
        let database = root.join("project").join(".crush").join("crush.db");
        {
            let connection = create_db(&database);
            insert_session(&connection, "session-1", None);
            // A flat part without a call id cannot be paired with a result.
            insert_message(
                &connection,
                "m-1",
                "session-1",
                "assistant",
                None,
                None,
                1_789_189_713,
                1_789_189_713,
                r#"[{"type":"tool_call","name":"ls","input":"{\"path\":\"C:\\\\Users\\\\PC\\\\Documents\\\\Discodex\\\\src\\\\events.rs\"}"},{"type":"tool_result","content":"redacted"}]"#,
            );
            // The end of the turn closes it.
            insert_message(
                &connection,
                "m-2",
                "session-1",
                "assistant",
                None,
                None,
                1_789_189_714,
                1_789_189_714,
                r#"[{"type":"finish","data":{"reason":"stop","time":1789189714}}]"#,
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&database, &mut seen);
        let parsed: Vec<ParsedEvent> = events.iter().map(|event| event.event.clone()).collect();
        assert_eq!(
            parsed,
            vec![
                ParsedEvent::SessionMetadata {
                    project: "project".to_string(),
                    cwd: root.join("project").to_string_lossy().to_string(),
                },
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label: "Reading project files".to_string(),
                        file: Some("events.rs".to_string()),
                        language: Some("Rust".to_string()),
                    },
                },
                ParsedEvent::ToolFinished,
                ParsedEvent::Completed,
            ]
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn surfaces_the_model_id_and_the_turn_timestamp() {
        let root = temp_root("model");
        let database = root.join("project").join(".crush").join("crush.db");
        {
            let connection = create_db(&database);
            insert_session(&connection, "session-1", None);
            insert_message(
                &connection,
                "m-1",
                "session-1",
                "user",
                None,
                None,
                1_789_189_713,
                1_789_189_713,
                r#"[{"type":"text","data":{"text":"redacted"}}]"#,
            );
            insert_message(
                &connection,
                "m-2",
                "session-1",
                "assistant",
                Some("claude-sonnet-4-5"),
                Some("anthropic"),
                1_789_189_714,
                1_789_189_714,
                r#"[{"type":"finish","data":{"reason":"stop"}}]"#,
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&database, &mut seen);
        let parsed: Vec<ParsedEvent> = events.iter().map(|event| event.event.clone()).collect();
        assert_eq!(
            parsed,
            vec![
                ParsedEvent::SessionMetadata {
                    project: "project".to_string(),
                    cwd: root.join("project").to_string_lossy().to_string(),
                },
                ParsedEvent::TaskStarted {
                    started_at_ms: 1_789_189_713_000,
                },
                ParsedEvent::AgentDetected {
                    agent: "claude-sonnet-4-5".to_string(),
                },
                ParsedEvent::Completed,
            ]
        );

        // A model id that is not a model id is dropped.
        assert_eq!(sanitize_model_id("gpt 5 codex"), None);
        assert_eq!(sanitize_model_id("claude-sonnet-4-5\nPRIVATE"), None);
        assert_eq!(sanitize_model_id(""), None);
        assert_eq!(sanitize_model_id(&"a".repeat(129)), None);
        assert_eq!(
            sanitize_model_id("openai/gpt-5-codex:v2"),
            Some("openai/gpt-5-codex:v2".to_string())
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reports_a_subagent_of_its_parent_conversation() {
        let root = temp_root("subagent");
        let database = root.join("project").join(".crush").join("crush.db");
        {
            let connection = create_db(&database);
            insert_session(&connection, "session-1", None);
            insert_session(&connection, "session-2", Some("session-1"));
            insert_message(
                &connection,
                "m-1",
                "session-2",
                "assistant",
                Some("claude-sonnet-4-5"),
                Some("anthropic"),
                1_789_189_714,
                1_789_189_714,
                r#"[{"type":"tool_call","data":{"id":"call_01","name":"ls","input":"{\"path\":\"C:\\\\Users\\\\PC\\\\Documents\\\\Discodex\\\\src\"}"}}]"#,
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&database, &mut seen);
        let parsed: Vec<ParsedEvent> = events.iter().map(|event| event.event.clone()).collect();
        assert_eq!(
            parsed,
            vec![
                ParsedEvent::SessionMetadata {
                    project: "project".to_string(),
                    cwd: root.join("project").to_string_lossy().to_string(),
                },
                ParsedEvent::SubagentSpawned {
                    conversation_id: "session-1".to_string(),
                    agent: "claude-sonnet-4-5".to_string(),
                },
                ParsedEvent::AgentDetected {
                    agent: "claude-sonnet-4-5".to_string(),
                },
                ParsedEvent::TrackedToolStarted {
                    call_id: "call_01".to_string(),
                    activity: ToolActivity {
                        label: "Reading project files".to_string(),
                        file: Some("src".to_string()),
                        language: None,
                    },
                },
            ]
        );
        assert!(events.iter().all(|event| event.session == "session-2"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reads_the_project_registry() {
        let root = temp_root("registry");
        let data_dir = root.join("MyApp").join(".crush");
        std::fs::create_dir_all(&data_dir).expect("create data dir");
        let database = data_dir.join("crush.db");
        std::fs::write(&database, b"").expect("create database");
        let registry = root.join("projects.json");
        std::fs::write(
            &registry,
            format!(
                r#"[{{"path":"C:\\Users\\PC\\Documents\\MyApp","data_dir":"{}","last_accessed":1}}]"#,
                data_dir.to_string_lossy().replace('\\', "\\\\")
            ),
        )
        .expect("write registry");

        let found = databases(&root);
        assert!(
            found.contains(&database),
            "missing {database:?} in {found:?}"
        );
        // Every entry of the registry is mapped to its own database.
        assert_eq!(found, vec![database.clone()]);

        // The workspace of a project is the folder name of the registry entry.
        let workspace = workspace_of_database(&database);
        assert_eq!(workspace.as_deref(), Some(r"C:\Users\PC\Documents\MyApp"));

        // A registry that cannot be read yields no project, the database of
        // the root is the fallback then.
        let broken = temp_root("registry-broken");
        std::fs::write(broken.join("projects.json"), b"not json").expect("write registry");
        let found = databases(&broken);
        assert_eq!(found, vec![broken.join("crush.db")]);

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&broken);
    }
}
