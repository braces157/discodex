//! Roo Code adapter: task messages in the editor global storage
//! `tasks/<taskId>/ui_messages.json` plus the `history_item.json` manifest.
//!
//! Roo Code is a Cline fork. It rewrites `ui_messages.json` as a whole on
//! every message (safeWriteJson replaces the file atomically), so the
//! document is re-read and diffed instead of tailed. The file holds a JSON
//! array of `ClineMessage` records:
//!
//! ```json
//! {"ts": 1792540721885, "type": "say", "say": "text", "text": "..."}
//! {"ts": 1792540722100, "type": "ask", "ask": "tool", "text": "{\"tool\":\"readFile\",...}"}
//! ```
//!
//! `history_item.json` is a single object that carries the workspace folder
//! and the Roo mode. `api_conversation_history.json` holds the raw
//! conversation and is never read by this adapter.
//!
//! Only the record shape is used here. Prompts, assistant answers, reasoning,
//! tool arguments, tool results, diffs, shell commands and full paths never
//! leave the parser; only a workspace folder name, a file basename, an
//! inferred language, a fixed activity label, a mode name and a timestamp do.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::events::{
    ParsedEvent, ToolActivity, classify_tool, extract_file_basename, language_from_file,
    project_name, timestamp_ms,
};

/// Parse the harness' session document and report only records that were not
/// seen before.
///
/// The document is rewritten in place, so the adapter keys every record and
/// skips keys that are already present in `seen`.
pub fn parse_document(doc: &Value, seen: &mut HashSet<String>) -> Vec<ParsedEvent> {
    let mut events = Vec::new();

    match doc {
        // The transcript: a JSON array of ClineMessage records.
        Value::Array(items) => parse_messages(items, seen, &mut events),
        Value::Object(map) => {
            if let Some(items) = map.get("messages").and_then(Value::as_array) {
                // An object that wraps the message array.
                parse_messages(items, seen, &mut events);
            } else if map.get("workspace").is_some() {
                // The manifest: one task folder with the workspace and mode.
                parse_history_item(map, seen, &mut events);
            } else if is_message(map) {
                // A single message record.
                let items = [Value::Object(map.clone())];
                parse_messages(&items, seen, &mut events);
            }
            // Any other shape is unknown and yields no events.
        }
        _ => {}
    }

    events
}

/// Parse the message records of a task.
fn parse_messages(items: &[Value], seen: &mut HashSet<String>, events: &mut Vec<ParsedEvent>) {
    for (index, item) in items.iter().enumerate() {
        let Some(record) = item.as_object() else {
            continue;
        };
        if !is_message(record) {
            continue;
        }
        let key = message_key(index, record);
        if !seen.insert(key) {
            continue;
        }
        parse_message(record, index, index == 0, seen, events);
    }
}

/// True when the record is a ClineMessage.
///
/// A record without a `say` or `ask` kind is not a message; it is ignored
/// instead of being guessed at.
fn is_message(record: &Map<String, Value>) -> bool {
    let kind = matches!(
        record.get("type").and_then(Value::as_str),
        Some("ask") | Some("say")
    );
    let has_kind = record.get("say").and_then(Value::as_str).is_some()
        || record.get("ask").and_then(Value::as_str).is_some();
    kind && has_kind
}

/// A stable key for a message record.
///
/// The index is stable because Roo only appends to the array. The `partial`
/// marker separates a streamed record from its final form, which Roo rewrites
/// in place without changing the timestamp.
fn message_key(index: usize, record: &Map<String, Value>) -> String {
    let partial = record
        .get("partial")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if partial {
        format!("m:{index}:partial")
    } else {
        format!("m:{index}")
    }
}

/// Map one ClineMessage to normalized events.
fn parse_message(
    record: &Map<String, Value>,
    index: usize,
    is_first: bool,
    seen: &mut HashSet<String>,
    events: &mut Vec<ParsedEvent>,
) {
    let kind = record.get("type").and_then(Value::as_str).unwrap_or("");
    let say = record.get("say").and_then(Value::as_str);
    let ask = record.get("ask").and_then(Value::as_str);

    // The first record of a task holds the user turn that opened it, and a
    // "user_feedback" record is a follow-up turn of the same task. Any other
    // record is not a user turn, so it never starts the timer.
    let user_turn = match say {
        Some("task") | Some("user_feedback") => true,
        Some("text") => is_first,
        _ => false,
    };

    if user_turn && seen.insert(format!("task:{index}")) {
        events.push(ParsedEvent::TaskStarted {
            started_at_ms: record
                .get("ts")
                .and_then(timestamp_ms)
                .unwrap_or_else(current_time_ms),
        });
    }

    match kind {
        "say" => match say {
            Some(
                "api_req_started" | "api_req_finished" | "reasoning" | "text" | "user_edit_todos",
            ) => {
                // Roo's reasoning is free-form prose, so no reasoning text is
                // forwarded.
                events.push(ParsedEvent::Thinking { reasoning: None });
            }
            Some("command_output") | Some("subtask_result") => {
                events.push(ParsedEvent::ToolFinished);
            }
            Some("completion_result") => {
                events.push(ParsedEvent::TerminalCandidate);
            }
            _ => {}
        },
        "ask" => match ask {
            Some("tool") => push_tool(record, events),
            Some("command") => push_command(record, events),
            Some("use_mcp_server") => events.push(ParsedEvent::ToolStarted {
                activity: ToolActivity {
                    label: "Using MCP tool".to_string(),
                    file: None,
                    language: None,
                },
            }),
            // Roo asks the user a question; the model waits for the answer.
            Some("followup") => events.push(ParsedEvent::Thinking { reasoning: None }),
            Some("completion_result") => events.push(ParsedEvent::Completed),
            _ => {}
        },
        _ => {}
    }
}

/// Map a tool request to a safe activity.
///
/// The payload of a tool request holds the file body, the diff and the
/// reason of the tool. Only the tool token and the path are read.
fn push_tool(record: &Map<String, Value>, events: &mut Vec<ParsedEvent>) {
    let Some(payload) = tool_payload(record) else {
        return;
    };
    let Some(token) = payload.get("tool").and_then(Value::as_str) else {
        return;
    };

    // A mode switch is the only place where the running mode is visible
    // while the task is running.
    if token == "switchMode"
        && let Some(mode) = payload.get("mode").and_then(Value::as_str)
        && let Some(agent) = mode_label(mode)
    {
        events.push(ParsedEvent::AgentDetected { agent });
    }

    let file = tool_file(&payload);
    let language = file
        .as_deref()
        .and_then(language_from_file)
        .map(str::to_string);

    events.push(ParsedEvent::ToolStarted {
        activity: ToolActivity {
            label: tool_label(token).to_string(),
            file,
            language,
        },
    });
}

/// The JSON payload of a tool request.
fn tool_payload(record: &Map<String, Value>) -> Option<Value> {
    match record.get("text") {
        Some(Value::String(text)) => serde_json::from_str::<Value>(text).ok(),
        Some(value @ Value::Object(_)) => Some(value.clone()),
        _ => None,
    }
}

/// The file a tool touched, as a basename only.
fn tool_file(payload: &Value) -> Option<String> {
    let path = payload.get("path").and_then(Value::as_str).or_else(|| {
        payload
            .get("batchFiles")
            .and_then(Value::as_array)
            .and_then(|files| files.first())
            .and_then(|file| file.get("path"))
            .and_then(Value::as_str)
    })?;

    // Only the path is handed to the helper, so the file body, the diff and
    // the reason of the tool can never be read.
    let holder = serde_json::json!({ "path": path });
    extract_file_basename(&holder)
}

/// A fixed label for a Roo tool token.
///
/// The token itself is never published; an unknown token falls back to a
/// generic label.
fn tool_label(token: &str) -> &'static str {
    match token {
        "appliedDiff" | "editedExistingFile" | "newFileCreated" => "Editing files",
        "readFile" | "listFilesTopLevel" | "listFilesRecursive" => "Reading project files",
        "searchFiles" | "codebaseSearch" => "Searching project",
        "switchMode" | "updateTodoList" => "Planning changes",
        "newTask" => "Running coding agent",
        "readCommandOutput" => "Reviewing terminal output",
        "generateImage" | "imageGenerated" => "Inspecting image",
        "runSlashCommand" | "skill" => "Using development tools",
        _ => "Using development tools",
    }
}

/// Map a shell command request to a safe activity.
///
/// The command text is only used to pick a fixed label; it never leaves the
/// adapter.
fn push_command(record: &Map<String, Value>, events: &mut Vec<ParsedEvent>) {
    let Some(command) = record.get("text").and_then(Value::as_str) else {
        return;
    };

    events.push(ParsedEvent::ToolStarted {
        activity: ToolActivity {
            label: classify_tool("execute_command", command).to_string(),
            file: None,
            language: None,
        },
    });
}

/// Map the task manifest to the session metadata and the running mode.
///
/// The manifest also holds the user prompt and the name of the provider
/// profile; both are deliberately ignored.
fn parse_history_item(
    record: &Map<String, Value>,
    seen: &mut HashSet<String>,
    events: &mut Vec<ParsedEvent>,
) {
    if seen.insert("h:workspace".to_string())
        && let Some(workspace) = record.get("workspace").and_then(Value::as_str)
        && let Some(project) = project_name(workspace)
    {
        events.push(ParsedEvent::SessionMetadata {
            project,
            cwd: workspace.to_string(),
        });
    }

    if let Some(mode) = record.get("mode").and_then(Value::as_str)
        && let Some(agent) = mode_label(mode)
    {
        let key = format!("h:mode:{agent}");
        if seen.insert(key) {
            events.push(ParsedEvent::AgentDetected { agent });
        }
    }
}

/// The honest stand-in for a model id: the built-in Roo mode.
///
/// Roo Code does not persist the selected model, and custom modes are
/// user-authored, so only the fixed upstream vocabulary is published.
fn mode_label(mode: &str) -> Option<String> {
    let label = match mode {
        "architect" => "Architect",
        "code" => "Code",
        "ask" => "Ask",
        "debug" => "Debug",
        "orchestrator" => "Orchestrator",
        _ => return None,
    };
    sanitize_agent(label)
}

/// Keep only safe model or agent identifiers.
fn sanitize_agent(value: &str) -> Option<String> {
    if value.is_empty() || value.len() > 128 {
        return None;
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"._:/@-".contains(&byte))
    {
        return None;
    }
    Some(value.to_string())
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

    fn tool_text(payload: Value) -> Value {
        serde_json::Value::String(payload.to_string())
    }

    fn sample_transcript() -> Value {
        serde_json::json!([
            {
                "ts": 1_792_540_721_885i64,
                "type": "say",
                "say": "text",
                "text": "PRIVATE_USER_PROMPT"
            },
            {
                "ts": 1_792_540_721_901i64,
                "type": "say",
                "say": "api_req_started"
            },
            {
                "ts": 1_792_540_722_100i64,
                "type": "ask",
                "ask": "tool",
                "text": tool_text(serde_json::json!({
                    "tool": "readFile",
                    "path": "src/main.rs",
                    "isOutsideWorkspace": false,
                    "content": r"C:\Users\PC\Documents\Discodex\src\main.rs",
                    "reason": "PRIVATE_REASON"
                })),
                "partial": false,
                "isAnswered": true
            },
            {
                "ts": 1_792_540_723_400i64,
                "type": "ask",
                "ask": "tool",
                "text": tool_text(serde_json::json!({
                    "tool": "editedExistingFile",
                    "path": "src/main.rs",
                    "content": "PRIVATE_FILE_BODY",
                    "diff": "PRIVATE_DIFF"
                })),
                "partial": false,
                "isAnswered": true
            },
            {
                "ts": 1_792_540_723_500i64,
                "type": "ask",
                "ask": "command",
                "text": "cargo test -- --nocapture"
            },
            {
                "ts": 1_792_540_724_100i64,
                "type": "say",
                "say": "command_output",
                "text": "PRIVATE_SHELL_OUTPUT"
            },
            {
                "ts": 1_792_540_724_200i64,
                "type": "say",
                "say": "reasoning",
                "reasoning": "PRIVATE_REASONING"
            },
            {
                "ts": 1_792_540_725_000i64,
                "type": "ask",
                "ask": "completion_result",
                "text": "PRIVATE_FINAL_RESULT"
            }
        ])
    }

    #[test]
    fn maps_a_roo_task_to_safe_activity() {
        let document = sample_transcript();
        let mut seen = HashSet::new();
        let events = parse_document(&document, &mut seen);

        assert_eq!(
            events,
            vec![
                ParsedEvent::TaskStarted {
                    started_at_ms: 1_792_540_721_885
                },
                ParsedEvent::Thinking { reasoning: None },
                ParsedEvent::Thinking { reasoning: None },
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label: "Reading project files".to_string(),
                        file: Some("main.rs".to_string()),
                        language: Some("Rust".to_string()),
                    }
                },
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label: "Editing files".to_string(),
                        file: Some("main.rs".to_string()),
                        language: Some("Rust".to_string()),
                    }
                },
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label: "Running Rust tests".to_string(),
                        file: None,
                        language: None,
                    }
                },
                ParsedEvent::ToolFinished,
                ParsedEvent::Thinking { reasoning: None },
                ParsedEvent::Completed,
            ]
        );
    }

    #[test]
    fn never_publishes_prompts_answers_results_or_full_paths() {
        let document = serde_json::json!([
            {
                "ts": 1_700_000_000_000i64,
                "type": "say",
                "say": "text",
                "text": "PRIVATE_USER_PROMPT"
            },
            {
                "ts": 1_700_000_001_000i64,
                "type": "say",
                "say": "reasoning",
                "reasoning": "PRIVATE_REASONING"
            },
            {
                "ts": 1_700_000_002_000i64,
                "type": "ask",
                "ask": "tool",
                "text": tool_text(serde_json::json!({
                    "tool": "write_to_file",
                    "path": r"C:\Users\PC\Documents\PRIVATE_REPO_MARKER\src\secret.rs",
                    "content": "PRIVATE_FILE_BODY"
                })),
                "partial": false,
                "isAnswered": true
            },
            {
                "ts": 1_700_000_003_000i64,
                "type": "ask",
                "ask": "command",
                "text": "echo PRIVATE_SHELL_TOKEN"
            },
            {
                "ts": 1_700_000_004_000i64,
                "type": "say",
                "say": "command_output",
                "text": "PRIVATE_TOOL_RESULT"
            },
            {
                "ts": 1_700_000_005_000i64,
                "type": "ask",
                "ask": "completion_result",
                "text": "PRIVATE_FINAL_ANSWER"
            }
        ]);

        let mut seen = HashSet::new();
        let events = parse_document(&document, &mut seen);
        let rendered = format!("{events:?}");

        for secret in [
            "PRIVATE_USER_PROMPT",
            "PRIVATE_REASONING",
            "PRIVATE_FILE_BODY",
            "PRIVATE_SHELL_TOKEN",
            "PRIVATE_TOOL_RESULT",
            "PRIVATE_FINAL_ANSWER",
            "PRIVATE_REPO_MARKER",
            "Users",
            "Documents",
            r"C:\Users\PC\Documents\PRIVATE_REPO_MARKER\src\secret.rs",
        ] {
            assert!(
                !rendered.contains(secret),
                "the event leaked {secret}: {rendered}"
            );
        }

        // The manifest holds the user prompt and the provider profile name.
        let manifest = serde_json::json!({
            "id": "0198f3a2-7c1e-7b44-9f10-2a6c1d5e8b31",
            "number": 1,
            "ts": 1_792_540_725_000i64,
            "task": "PRIVATE_HISTORY_PROMPT",
            "tokensIn": 4211,
            "apiConfigName": "PRIVATE_PROFILE_NAME",
            "workspace": r"C:\Users\PC\Documents\Discodex",
            "mode": "code",
            "status": "active"
        });
        let mut seen = HashSet::new();
        let events = parse_document(&manifest, &mut seen);
        let rendered = format!("{events:?}");
        assert!(!rendered.contains("PRIVATE_HISTORY_PROMPT"));
        assert!(!rendered.contains("PRIVATE_PROFILE_NAME"));
        assert_eq!(
            events,
            vec![
                ParsedEvent::SessionMetadata {
                    project: "Discodex".to_string(),
                    cwd: r"C:\Users\PC\Documents\Discodex".to_string(),
                },
                ParsedEvent::AgentDetected {
                    agent: "Code".to_string()
                },
            ]
        );
    }

    #[test]
    fn malformed_and_unknown_records_produce_no_events() {
        let mut seen = HashSet::new();
        let documents = vec![
            Value::Null,
            Value::Bool(true),
            Value::Number(42.into()),
            Value::String("not json".to_string()),
            Value::Array(Vec::new()),
            serde_json::json!({}),
            serde_json::json!({ "type": "say" }),
            serde_json::json!({ "type": "say", "say": "brand_new_say", "text": "PRIVATE" }),
            serde_json::json!({ "type": "ask", "ask": "brand_new_ask", "text": "PRIVATE" }),
            serde_json::json!([{ "type": "ask", "ask": "tool", "text": "not json at all" }]),
            serde_json::json!([{ "type": "ask", "ask": "tool", "text": "{\"path\":\"src/main.rs\"}" }]),
            serde_json::json!([{ "type": "ask", "ask": "command" }]),
            serde_json::json!([{ "ts": 1, "type": "note", "text": "PRIVATE" }]),
            serde_json::json!([{ "ts": 1, "type": "say", "say": "text", "text": "PRIVATE" }, "junk"]),
        ];

        for document in documents {
            assert!(
                parse_document(&document, &mut seen).is_empty(),
                "unexpected events for {document}"
            );
        }
    }

    #[test]
    fn reports_the_roo_mode_and_the_record_timestamp() {
        let manifest = serde_json::json!({
            "id": "0198f3a2-7c1e-7b44-9f10-2a6c1d5e8b31",
            "ts": 1_792_540_725_000i64,
            "task": "PRIVATE_PROMPT",
            "workspace": r"C:\work\Repo",
            "mode": "architect"
        });
        let mut seen = HashSet::new();
        assert_eq!(
            parse_document(&manifest, &mut seen),
            vec![
                ParsedEvent::SessionMetadata {
                    project: "Repo".to_string(),
                    cwd: r"C:\work\Repo".to_string(),
                },
                ParsedEvent::AgentDetected {
                    agent: "Architect".to_string()
                },
            ]
        );

        // A custom mode is user-authored and is therefore not publishable.
        let custom = serde_json::json!({ "workspace": r"C:\work\Repo", "mode": "my-mode" });
        let mut seen = HashSet::new();
        assert_eq!(
            parse_document(&custom, &mut seen),
            vec![ParsedEvent::SessionMetadata {
                project: "Repo".to_string(),
                cwd: r"C:\work\Repo".to_string(),
            }]
        );

        // The timer uses the timestamp of the record that opened the turn.
        let messages = serde_json::json!([{
            "ts": 1_700_000_123_456i64,
            "type": "say",
            "say": "text",
            "text": "PRIVATE"
        }]);
        let mut seen = HashSet::new();
        let events = parse_document(&messages, &mut seen);
        assert_eq!(
            events[0],
            ParsedEvent::TaskStarted {
                started_at_ms: 1_700_000_123_456
            }
        );

        // A mode switch reports the new mode.
        let switch = serde_json::json!([{
            "ts": 1_700_000_123_456i64,
            "type": "ask",
            "ask": "tool",
            "text": tool_text(serde_json::json!({
                "tool": "switchMode",
                "mode": "debug",
                "reason": "PRIVATE_REASON"
            }))
        }]);
        let mut seen = HashSet::new();
        let events = parse_document(&switch, &mut seen);
        assert_eq!(
            events[0],
            ParsedEvent::AgentDetected {
                agent: "Debug".to_string()
            }
        );
    }

    #[test]
    fn a_rewritten_document_only_reports_new_records() {
        let document = sample_transcript();
        let mut seen = HashSet::new();

        let first = parse_document(&document, &mut seen);
        assert!(!first.is_empty());
        assert!(parse_document(&document, &mut seen).is_empty());

        // Roo rewrites a streamed record in place when the stream finishes.
        let streamed = serde_json::json!([{
            "ts": 1_700_000_000_000i64,
            "type": "say",
            "say": "text",
            "text": "PRIVATE",
            "partial": true
        }]);
        let finished = serde_json::json!([{
            "ts": 1_700_000_000_000i64,
            "type": "say",
            "say": "text",
            "text": "PRIVATE",
            "partial": false
        }]);
        let mut seen = HashSet::new();
        assert_eq!(parse_document(&streamed, &mut seen).len(), 2);
        // The final form of the stream is reported again, but the task start
        // is not repeated.
        assert_eq!(parse_document(&finished, &mut seen).len(), 1);
        assert!(parse_document(&finished, &mut seen).is_empty());
    }
}
