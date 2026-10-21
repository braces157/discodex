//! Goose adapter: session messages in the shared sessions database
//!
//! Goose keeps every session of every project in one SQLite database
//! (`%APPDATA%\Block\goose\data\sessions\sessions.db`, WAL mode) that it
//! writes while the agent runs. The `messages` table is the only reliable
//! cursor: `messages.id` is a global AUTOINCREMENT insertion counter and
//! `messages.content_json` holds the tagged content blocks of the message.
//!
//! Only structural facts leave this adapter: the workspace folder name, a file
//! basename, an inferred language, a normalized activity label, the model id,
//! the conversation id and timing. Prompts, answers, reasoning, tool
//! arguments, tool results and path prefixes never leave the database.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;

use super::{StoreEvent, column_json, column_text_opt, open_database};
use crate::events::{
    ParsedEvent, ToolActivity, classify_tool, extract_file_basename, language_from_file,
    project_name, timestamp_ms,
};

/// Number of message rows that are read per poll.
const ROW_LIMIT: i64 = 200;

/// Places below the goose root where the shared database may live.
const DATABASE_PATHS: &[&str] = &[
    "data/sessions/sessions.db",
    "sessions/sessions.db",
    "data/sessions.db",
    "sessions.db",
];

/// The session columns the adapter reads.
///
/// `name` (the auto generated conversation title), `description`,
/// `extension_data` and `system_prompt_override` are deliberately ignored:
/// they carry conversation text.
#[derive(Default, Clone)]
struct SessionInfo {
    working_dir: Option<String>,
    model_config_json: Option<String>,
    session_type: Option<String>,
}

/// One row of the `messages` table.
struct MessageRow {
    id: i64,
    session_id: String,
    role: String,
    content: Option<Value>,
    started_at_ms: Option<i64>,
    session: SessionInfo,
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
/// The `seen` set holds the row keys that were already reported. Every key
/// is namespaced (`row:`, `meta:`, `model:`, `subagent:`, `final:`) so the
/// conversation scoped bookkeeping never collides with the row cursor.
pub fn poll(db: &Path, seen: &mut HashSet<String>) -> Vec<StoreEvent> {
    let Ok(connection) = open_database(db) else {
        return Vec::new();
    };
    let Some(rows) = read_rows(&connection) else {
        return Vec::new();
    };

    let mut events = Vec::new();
    // The query returns the newest rows first; report them oldest first so the
    // presence follows the order of the conversation.
    for row in rows.into_iter().rev() {
        let key = format!("row:{}:{}", row.session_id, row.id);
        if !seen.insert(key) {
            continue;
        }
        for event in row_events(&row, seen) {
            events.push(StoreEvent {
                session: row.session_id.clone(),
                event,
            });
        }
    }
    events
}

/// Read the newest message rows, tolerating a changed schema.
///
/// The full query joins the session table. When a column was renamed the
/// query fails and the reader falls back to the messages table alone and
/// resolves the session columns by name.
fn read_rows(connection: &Connection) -> Option<Vec<MessageRow>> {
    query_joined(connection).or_else(|| query_split(connection))
}

const JOINED_QUERY: &str = "
    SELECT m.id, m.session_id, m.role, m.content_json, m.created_timestamp, m.timestamp,
           s.working_dir, s.model_config_json, s.session_type
    FROM messages m
    JOIN sessions s ON s.id = m.session_id
    ORDER BY m.id DESC
    LIMIT ?";

const MESSAGES_QUERY: &str = "SELECT * FROM messages ORDER BY id DESC LIMIT ?";
const SESSIONS_QUERY: &str = "SELECT * FROM sessions";

fn query_joined(connection: &Connection) -> Option<Vec<MessageRow>> {
    let mut statement = connection.prepare(JOINED_QUERY).ok()?;
    let mut rows = statement.query(rusqlite::params![ROW_LIMIT]).ok()?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().ok()? {
        let session = session_row(row);
        // The joined query fixes the column order, so the content is read by
        // index here and by name in the fallback query.
        let content = column_json(row, 3);
        if let Some(message) = message_row(row, Some(session), content) {
            out.push(message);
        }
    }
    Some(out)
}

fn query_split(connection: &Connection) -> Option<Vec<MessageRow>> {
    let mut sessions: HashMap<String, SessionInfo> = HashMap::new();
    {
        let mut statement = connection.prepare(SESSIONS_QUERY).ok()?;
        let mut rows = statement.query([]).ok()?;
        while let Some(row) = rows.next().ok()? {
            if let Some(id) = column_text_opt(row, "id") {
                sessions.insert(id, session_row(row));
            }
        }
    }

    let mut statement = connection.prepare(MESSAGES_QUERY).ok()?;
    let mut rows = statement.query(rusqlite::params![ROW_LIMIT]).ok()?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().ok()? {
        let session = column_text_opt(row, "session_id").and_then(|id| sessions.get(&id).cloned());
        let content = column_text_opt(row, "content_json")
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        if let Some(message) = message_row(row, session, content) {
            out.push(message);
        }
    }
    Some(out)
}

/// Read the session columns of a row by name.
///
/// A renamed column yields `None` instead of an error, so the adapter
/// degrades to "no activity" instead of guessing.
fn session_row(row: &rusqlite::Row<'_>) -> SessionInfo {
    SessionInfo {
        working_dir: column_text_opt(row, "working_dir"),
        model_config_json: column_text_opt(row, "model_config_json"),
        session_type: column_text_opt(row, "session_type"),
    }
}

/// Read one message row by name.
fn message_row(
    row: &rusqlite::Row<'_>,
    session: Option<SessionInfo>,
    content: Option<Value>,
) -> Option<MessageRow> {
    let id = column_text_opt(row, "id")?.trim().parse::<i64>().ok()?;
    let session_id = column_text_opt(row, "session_id")?;
    Some(MessageRow {
        id,
        session_id,
        role: column_text_opt(row, "role").unwrap_or_default(),
        content,
        started_at_ms: message_timestamp_ms(row),
        session: session.unwrap_or_default(),
    })
}

/// The point in time the message was created.
///
/// `created_timestamp` holds epoch seconds, `timestamp` is the SQLite UTC
/// text of the same moment.
fn message_timestamp_ms(row: &rusqlite::Row<'_>) -> Option<i64> {
    column_text_opt(row, "created_timestamp")
        .and_then(|text| text.trim().parse::<i64>().ok())
        .and_then(|seconds| timestamp_ms(&Value::from(seconds)))
        .or_else(|| column_text_opt(row, "timestamp").and_then(|text| sqlite_utc_ms(&text)))
}

/// Convert a SQLite UTC timestamp (`YYYY-MM-DD HH:MM:SS`) into milliseconds.
fn sqlite_utc_ms(text: &str) -> Option<i64> {
    let naive = chrono::NaiveDateTime::parse_from_str(text.trim(), "%Y-%m-%d %H:%M:%S").ok()?;
    Some(naive.and_utc().timestamp_millis())
}

/// Convert one message row into normalized events.
fn row_events(row: &MessageRow, seen: &mut HashSet<String>) -> Vec<ParsedEvent> {
    let mut events = Vec::new();

    // Facts that belong to the conversation and not to a single message are
    // reported once per session.
    if seen.insert(format!("meta:{}", row.session_id)) {
        if let Some(cwd) = row.session.working_dir.as_deref()
            && let Some(project) = project_name(cwd)
        {
            events.push(ParsedEvent::SessionMetadata {
                project,
                cwd: cwd.to_string(),
            });
        }
        if row.session.session_type.as_deref() == Some("subagent") {
            // The conversation id is the session that carries the subagent, so
            // the presence of that conversation shows the subagent.
            events.push(ParsedEvent::SubagentSpawned {
                conversation_id: row.session_id.clone(),
                agent: "goose subagent".to_string(),
            });
        }
    }
    if let Some(agent) = model_id(&row.session)
        && seen.insert(format!("model:{}:{}", row.session_id, agent))
    {
        events.push(ParsedEvent::AgentDetected { agent });
    }

    let Some(Value::Array(blocks)) = row.content.as_ref() else {
        return events;
    };

    let role = row.role.to_ascii_lowercase();
    let mut has_text = false;
    let mut has_tool_request = false;
    let mut has_thinking = false;
    let mut turn_completed = false;
    let mut tool_events: Vec<ParsedEvent> = Vec::new();

    for block in blocks {
        let Some(kind) = block.get("type").and_then(Value::as_str) else {
            continue;
        };
        match kind {
            "text" => has_text = true,
            "thinking" => has_thinking = true,
            "toolRequest" => {
                has_tool_request = true;
                let Some(call) = block.get("toolCall").and_then(|call| call.get("value")) else {
                    continue;
                };
                let name = call.get("name").and_then(Value::as_str).unwrap_or("");
                let activity = tool_activity(name, call.get("arguments"));
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(sanitize_call_id);
                if base_tool_name(name).eq_ignore_ascii_case("final_output")
                    && let Some(id) = id.as_deref()
                {
                    // The final answer tool closes the turn; remember the call
                    // so that its response can end the conversation.
                    seen.insert(format!("final:{}:{}", row.session_id, id));
                }
                match id {
                    Some(id) => tool_events.push(ParsedEvent::TrackedToolStarted {
                        call_id: id,
                        activity,
                    }),
                    None => tool_events.push(ParsedEvent::ToolStarted { activity }),
                }
            }
            "toolResponse" => {
                match block
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(sanitize_call_id)
                {
                    Some(id) => {
                        tool_events.push(ParsedEvent::TrackedToolFinished {
                            call_id: id.clone(),
                        });
                        if seen.remove(&format!("final:{}:{}", row.session_id, id)) {
                            turn_completed = true;
                        }
                    }
                    None => tool_events.push(ParsedEvent::ToolFinished),
                }
            }
            // Every other block type (image, document, error, actionRequired,
            // toolConfirmationRequest, systemNotification, ...) is ignored.
            _ => {}
        }
    }

    // Goose stores tool observations under the user role, so the block type
    // decides: only a text block is a human prompt.
    if role == "user" && has_text {
        events.push(ParsedEvent::TaskStarted {
            started_at_ms: row.started_at_ms.unwrap_or(0),
        });
    }
    if has_thinking {
        events.push(ParsedEvent::Thinking { reasoning: None });
    }
    events.extend(tool_events);
    // An assistant message that only carries text ends the turn.
    if turn_completed || (role == "assistant" && has_text && !has_tool_request) {
        events.push(ParsedEvent::Completed);
    }
    events
}

/// Build the activity of a tool call.
///
/// The arguments are only inspected for a file path, the tool name only for a
/// fixed label. The command, the prompt and the result never leave here.
fn tool_activity(name: &str, arguments: Option<&Value>) -> ToolActivity {
    let base = base_tool_name(name).to_ascii_lowercase();
    let label = match base.as_str() {
        "tree" => "Exploring project",
        "final_output" => "Finishing up",
        _ => classify_tool(&base, ""),
    };
    let file = arguments
        .and_then(extract_file_basename)
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

/// The tool name without the extension namespace (`developer__shell`).
fn base_tool_name(name: &str) -> &str {
    match name.rsplit_once("__") {
        Some((_, base)) if !base.is_empty() => base,
        _ => name,
    }
}

/// The model id of a session, if the session names one.
fn model_id(session: &SessionInfo) -> Option<String> {
    let config = session.model_config_json.as_deref()?;
    let value: Value = serde_json::from_str(config).ok()?;
    let raw = value
        .get("model_name")
        .and_then(Value::as_str)
        .or_else(|| value.get("model").and_then(Value::as_str))?;
    sanitize_model_id(raw)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("discodex-goose-{name}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn create_db(path: &Path) -> Connection {
        let connection = Connection::open(path).expect("create database");
        connection
            .execute_batch(
                "CREATE TABLE sessions (
                     id TEXT PRIMARY KEY,
                     name TEXT,
                     session_type TEXT,
                     working_dir TEXT,
                     created_at TIMESTAMP,
                     updated_at TIMESTAMP,
                     provider_name TEXT,
                     model_config_json TEXT
                 );
                 CREATE TABLE messages (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     message_id TEXT,
                     session_id TEXT NOT NULL,
                     role TEXT NOT NULL,
                     content_json TEXT NOT NULL,
                     created_timestamp INTEGER NOT NULL,
                     timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     tokens INTEGER,
                     metadata_json TEXT
                 );",
            )
            .expect("create schema");
        connection
    }

    fn insert_session(
        connection: &Connection,
        id: &str,
        working_dir: Option<&str>,
        model: Option<&str>,
        session_type: Option<&str>,
    ) {
        connection
            .execute(
                "INSERT INTO sessions (id, name, session_type, working_dir, provider_name, model_config_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    id,
                    "redacted conversation title",
                    session_type,
                    working_dir,
                    "openai",
                    model
                ],
            )
            .expect("insert session");
    }

    fn insert_message(
        connection: &Connection,
        id: i64,
        session: &str,
        role: &str,
        content: &str,
        created: i64,
    ) {
        connection
            .execute(
                "INSERT INTO messages (id, session_id, role, content_json, created_timestamp)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, session, role, content, created],
            )
            .expect("insert message");
    }

    #[test]
    fn maps_goose_messages_to_normalized_events() {
        let path = temp_db("happy");
        {
            let connection = create_db(&path);
            insert_session(
                &connection,
                "20251021_1",
                Some(r"C:\Users\PC\Documents\Discodex"),
                Some(r#"{"model_name":"gpt-5-codex"}"#),
                Some("user"),
            );
            insert_message(
                &connection,
                1,
                "20251021_1",
                "user",
                r#"[{"type":"text","text":"a redacted prompt"}]"#,
                1_761_000_000,
            );
            insert_message(
                &connection,
                2,
                "20251021_1",
                "assistant",
                r#"[{"type":"thinking","thinking":"private reasoning"},
                    {"type":"toolRequest","id":"toolu_01A2B","toolCall":{"status":"success","value":{"name":"write","arguments":{"path":"src/provider.rs","content":"private body"}}}}]"#,
                1_761_000_010,
            );
            insert_message(
                &connection,
                3,
                "20251021_1",
                "user",
                r#"[{"type":"toolResponse","id":"toolu_01A2B","toolResult":{"status":"success","value":{"content":[{"type":"text","text":"private output"}],"isError":false}}}]"#,
                1_761_000_020,
            );
            insert_message(
                &connection,
                4,
                "20251021_1",
                "assistant",
                r#"[{"type":"text","text":"a redacted answer"}]"#,
                1_761_000_030,
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        let parsed: Vec<ParsedEvent> = events.iter().map(|event| event.event.clone()).collect();

        assert!(events.iter().all(|event| event.session == "20251021_1"));
        assert_eq!(
            parsed,
            vec![
                ParsedEvent::SessionMetadata {
                    project: "Discodex".to_string(),
                    cwd: r"C:\Users\PC\Documents\Discodex".to_string(),
                },
                ParsedEvent::AgentDetected {
                    agent: "gpt-5-codex".to_string(),
                },
                ParsedEvent::TaskStarted {
                    started_at_ms: 1_761_000_000_000,
                },
                ParsedEvent::Thinking { reasoning: None },
                ParsedEvent::TrackedToolStarted {
                    call_id: "toolu_01A2B".to_string(),
                    activity: ToolActivity {
                        label: "Editing files".to_string(),
                        file: Some("provider.rs".to_string()),
                        language: Some("Rust".to_string()),
                    },
                },
                ParsedEvent::TrackedToolFinished {
                    call_id: "toolu_01A2B".to_string(),
                },
                ParsedEvent::Completed,
            ]
        );

        // The same rows are never reported twice.
        assert!(poll(&path, &mut seen).is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn never_forwards_prompts_answers_tool_results_or_paths() {
        let path = temp_db("privacy");
        {
            let connection = create_db(&path);
            insert_session(
                &connection,
                "20251021_2",
                Some(r"C:\workspace\demo-project"),
                None,
                Some("user"),
            );
            insert_message(
                &connection,
                10,
                "20251021_2",
                "user",
                r#"[{"type":"text","text":"my secret token is hunter2"}]"#,
                1_761_000_100,
            );
            insert_message(
                &connection,
                11,
                "20251021_2",
                "assistant",
                r#"[{"type":"toolRequest","id":"toolu_01","toolCall":{"status":"success","value":{"name":"write","arguments":{"path":"C:\\Users\\PC\\Documents\\Discodex\\src\\provider.rs","content":"SECRET FILE CONTENTS"}}}}]"#,
                1_761_000_110,
            );
            insert_message(
                &connection,
                12,
                "20251021_2",
                "assistant",
                r#"[{"type":"toolRequest","id":"toolu_02","toolCall":{"status":"success","value":{"name":"shell","arguments":{"command":"cat C:\\Users\\PC\\secrets\\creds.env","timeout_secs":120}}}}]"#,
                1_761_000_120,
            );
            insert_message(
                &connection,
                13,
                "20251021_2",
                "user",
                r#"[{"type":"toolResponse","id":"toolu_01","toolResult":{"status":"success","value":{"content":[{"type":"text","text":"SECRET FILE CONTENTS"}],"isError":false}}}]"#,
                1_761_000_130,
            );
            insert_message(
                &connection,
                14,
                "20251021_2",
                "assistant",
                r#"[{"type":"text","text":"here is the answer hunter2"}]"#,
                1_761_000_140,
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        let debug = format!("{events:?}");

        for secret in [
            "hunter2",
            "SECRET",
            "creds",
            "Users",
            "Documents",
            "Discodex",
        ] {
            assert!(!debug.contains(secret), "leaked {secret} in {debug}");
        }
        // The directory of an edited file never leaves the adapter, only the
        // basename does.
        assert!(!debug.contains(r"C:\\Users\\PC\\Documents\\Discodex\\src\\provider.rs"));
        assert!(!debug.contains("Documents"));

        // The tool activity keeps the label and the basename only.
        let write = events
            .iter()
            .find(|event| {
                matches!(
                    event.event,
                    ParsedEvent::TrackedToolStarted {
                        activity: ToolActivity { .. },
                        ..
                    }
                )
            })
            .expect("write tool");
        match &write.event {
            ParsedEvent::TrackedToolStarted { activity, .. } => {
                assert_eq!(activity.label, "Editing files");
                assert_eq!(activity.file.as_deref(), Some("provider.rs"));
                assert_eq!(activity.language.as_deref(), Some("Rust"));
            }
            other => panic!("unexpected event {other:?}"),
        }

        let shell = events
            .iter()
            .filter_map(|event| match &event.event {
                ParsedEvent::TrackedToolStarted { activity, .. } => Some(activity),
                _ => None,
            })
            .find(|activity| activity.label == "Running command")
            .expect("shell tool");
        assert_eq!(shell.file, None);
        assert_eq!(shell.language, None);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn malformed_and_unknown_records_produce_no_events() {
        let path = temp_db("malformed");
        {
            let connection = create_db(&path);
            insert_session(&connection, "20251021_3", None, None, Some("user"));
            insert_message(
                &connection,
                20,
                "20251021_3",
                "user",
                "not json",
                1_761_000_200,
            );
            insert_message(&connection, 21, "20251021_3", "user", "{}", 1_761_000_210);
            insert_message(&connection, 22, "20251021_3", "user", "[]", 1_761_000_220);
            insert_message(
                &connection,
                23,
                "20251021_3",
                "assistant",
                r#"[{"type":"somethingNew","payload":"redacted"}]"#,
                1_761_000_230,
            );
            insert_message(
                &connection,
                24,
                "20251021_3",
                "assistant",
                "[{}]",
                1_761_000_240,
            );
            insert_message(
                &connection,
                25,
                "20251021_3",
                "assistant",
                r#"[{"type":"toolRequest"}]"#,
                1_761_000_250,
            );
            insert_message(
                &connection,
                26,
                "20251021_3",
                "system",
                r#"[{"type":"text","text":"redacted"}]"#,
                1_761_000_260,
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        assert!(events.is_empty(), "unexpected events {events:?}");

        // A database without the goose tables is tolerated as well.
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
    fn surfaces_the_model_id_and_the_turn_timestamp() {
        let path = temp_db("model");
        {
            let connection = create_db(&path);
            insert_session(
                &connection,
                "20251021_4",
                Some(r"C:\workspace\goose-demo"),
                Some(r#"{"model_name":"claude-sonnet-4-5","context_limit":200000}"#),
                Some("user"),
            );
            insert_message(
                &connection,
                30,
                "20251021_4",
                "user",
                r#"[{"type":"text","text":"redacted"}]"#,
                1_761_000_300,
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        let parsed: Vec<ParsedEvent> = events.iter().map(|event| event.event.clone()).collect();
        assert_eq!(
            parsed,
            vec![
                ParsedEvent::SessionMetadata {
                    project: "goose-demo".to_string(),
                    cwd: r"C:\workspace\goose-demo".to_string(),
                },
                ParsedEvent::AgentDetected {
                    agent: "claude-sonnet-4-5".to_string(),
                },
                ParsedEvent::TaskStarted {
                    started_at_ms: 1_761_000_300_000,
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

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn pairs_tool_calls_and_reports_a_subagent_conversation() {
        let path = temp_db("subagent");
        {
            let connection = create_db(&path);
            insert_session(
                &connection,
                "20251021_5",
                Some(r"C:\workspace\goose-demo"),
                None,
                Some("subagent"),
            );
            insert_message(
                &connection,
                40,
                "20251021_5",
                "assistant",
                r#"[{"type":"toolRequest","id":"call-1","toolCall":{"status":"success","value":{"name":"developer__tree","arguments":{"path":"."}}}}]"#,
                1_761_000_400,
            );
            insert_message(
                &connection,
                41,
                "20251021_5",
                "user",
                r#"[{"type":"toolResponse","id":"call-1","toolResult":{"status":"success","value":{"content":[]}}}]"#,
                1_761_000_410,
            );
        }

        let mut seen = HashSet::new();
        let events = poll(&path, &mut seen);
        let parsed: Vec<ParsedEvent> = events.iter().map(|event| event.event.clone()).collect();
        assert_eq!(
            parsed,
            vec![
                ParsedEvent::SessionMetadata {
                    project: "goose-demo".to_string(),
                    cwd: r"C:\workspace\goose-demo".to_string(),
                },
                ParsedEvent::SubagentSpawned {
                    conversation_id: "20251021_5".to_string(),
                    agent: "goose subagent".to_string(),
                },
                ParsedEvent::TrackedToolStarted {
                    call_id: "call-1".to_string(),
                    activity: ToolActivity {
                        label: "Exploring project".to_string(),
                        file: None,
                        language: None,
                    },
                },
                ParsedEvent::TrackedToolFinished {
                    call_id: "call-1".to_string(),
                },
            ]
        );

        let _ = std::fs::remove_file(&path);
    }
}
