//! Factory Droid adapter: session logs in ~/.factory/sessions/<project>/<uuid>.jsonl
//!
//! Droid keeps every session as plain, append-only newline-delimited JSON.
//! Each line is one record:
//!
//! - `session_start` — the header. It carries the workspace `cwd`, the session
//!   `id`, and on some builds the `model` id plus the parent session of a
//!   spawned sub-agent.
//! - `message` — carries `message.role` and a `message.content` array of
//!   Anthropic-style blocks (`text`, `thinking`, `tool_use`, `tool_result`).
//! - `tool_use` / `tool_result` — the same two blocks in their own record
//!   form, as written by the streaming output formats.
//! - `agent_turn_outcome` — the end of one agent turn.
//!
//! Only safe metadata leaves this adapter: the workspace folder name, a file
//! basename, an inferred language, a normalized activity label, a model id and
//! timing. Prompts, assistant answers, reasoning, tool arguments, tool output,
//! session titles and full paths are never forwarded.

use serde_json::Value;

use crate::events::{
    ParsedEvent, ToolActivity, classify_tool, extract_file_basename, language_from_file,
    project_name, timestamp_ms,
};

/// Longest identifier (model id, tool call id) that may leave the adapter.
const MAX_TOKEN_LEN: usize = 128;

/// Parse one line of the harness' append-only activity log.
///
/// The log is newline-delimited JSON with one record per line. A line that is
/// not valid JSON, or that carries a record shape this adapter does not know,
/// yields no events at all.
pub fn parse_line(line: &str) -> Vec<ParsedEvent> {
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let Some(record_type) = record.get("type").and_then(Value::as_str) else {
        return Vec::new();
    };

    match record_type {
        "session_start" => parse_session_start(&record),
        "message" => parse_message(&record),
        "tool_use" => tool_started_event(&record),
        "tool_result" => tool_result_event(&record),
        "agent_turn_outcome" => vec![ParsedEvent::Completed],
        _ => Vec::new(),
    }
}

/// The header of a session.
///
/// `title`, `owner`, `organizationId` and `hostId` are private and are never
/// read. Only the workspace path, the model id and the parent session of a
/// sub-agent are used.
fn parse_session_start(record: &Value) -> Vec<ParsedEvent> {
    let mut events = Vec::new();

    if let Some(cwd) = record.get("cwd").and_then(Value::as_str)
        && let Some(project) = project_name(cwd)
    {
        events.push(ParsedEvent::SessionMetadata {
            project,
            cwd: cwd.to_string(),
        });
    }

    if let Some(agent) = record
        .get("model")
        .and_then(Value::as_str)
        .and_then(sanitize_token)
    {
        events.push(ParsedEvent::AgentDetected { agent });
    }

    let parent = record
        .get("callingSessionId")
        .or_else(|| record.get("parent"))
        .and_then(Value::as_str)
        .and_then(sanitize_token);
    if let Some(conversation_id) = parent {
        events.push(ParsedEvent::SubagentSpawned {
            conversation_id,
            agent: "Factory subagent".to_string(),
        });
    }

    if let Some(started_at_ms) = record.get("timestamp").and_then(timestamp_ms) {
        events.push(ParsedEvent::TaskStarted { started_at_ms });
    }

    events
}

/// One message of the conversation.
///
/// The role decides what a message means: a user turn starts the task, a user
/// message that only carries tool results closes those calls, and an
/// assistant message either runs tools, thinks, or produces the answer.
fn parse_message(record: &Value) -> Vec<ParsedEvent> {
    let message = record.get("message").unwrap_or(&Value::Null);
    let role = message
        .get("role")
        .or_else(|| record.get("role"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let blocks = content_blocks(message.get("content").or_else(|| record.get("content")));

    let mut events = Vec::new();

    match role {
        "user" => {
            let mut closed: Vec<String> = Vec::new();
            let mut untracked = 0usize;
            let mut has_text = false;

            for block in &blocks {
                match block_type(block) {
                    "tool_result" => match call_id_of_result(block).and_then(sanitize_token) {
                        Some(call_id) => {
                            // A retried call can appear twice in one record.
                            if !closed.contains(&call_id) {
                                closed.push(call_id.clone());
                                events.push(ParsedEvent::TrackedToolFinished { call_id });
                            }
                        }
                        None => untracked += 1,
                    },
                    "text" => has_text = true,
                    _ => {}
                }
            }

            for _ in 0..untracked {
                events.push(ParsedEvent::ToolFinished);
            }

            // A user message that carries tool results continues the current
            // turn; only a real user message starts a new one.
            if closed.is_empty() && untracked == 0 && has_text {
                events.push(ParsedEvent::TaskStarted {
                    started_at_ms: turn_start_ms(record),
                });
            }
        }
        "assistant" => {
            if let Some(agent) = message_model(record) {
                events.push(ParsedEvent::AgentDetected { agent });
            }

            let mut thinking = false;
            let mut has_text = false;
            let mut tool_blocks: Vec<&Value> = Vec::new();
            for block in &blocks {
                match block_type(block) {
                    "thinking" => thinking = true,
                    "tool_use" => tool_blocks.push(block),
                    "text" => has_text = true,
                    _ => {}
                }
            }

            if thinking {
                // The reasoning itself is private, only the phase is published.
                events.push(ParsedEvent::Thinking { reasoning: None });
            }

            for block in &tool_blocks {
                events.extend(tool_started_event(block));
            }

            // Text without a tool call is the answer of the turn.
            if tool_blocks.is_empty() && has_text {
                events.push(ParsedEvent::TerminalCandidate);
            }
        }
        _ => {}
    }

    events
}

/// A tool call that has started.
///
/// The call id pairs the call with its result; without one the call is
/// reported as an untracked activity.
fn tool_started_event(block: &Value) -> Vec<ParsedEvent> {
    let Some(name) = block.get("name").and_then(Value::as_str) else {
        return Vec::new();
    };
    if name.is_empty() {
        return Vec::new();
    }

    let activity = tool_activity(name, &tool_input(block));
    match call_id_of_use(block).and_then(sanitize_token) {
        Some(call_id) => vec![ParsedEvent::TrackedToolStarted { call_id, activity }],
        None => vec![ParsedEvent::ToolStarted { activity }],
    }
}

/// A tool call that has finished.
fn tool_result_event(block: &Value) -> Vec<ParsedEvent> {
    if let Some(call_id) = call_id_of_result(block).and_then(sanitize_token) {
        return vec![ParsedEvent::TrackedToolFinished { call_id }];
    }
    // Without a call id the call cannot be paired. It is still reported when
    // the record carries a result payload; a bare record carries no activity.
    if has_result_payload(block) {
        return vec![ParsedEvent::ToolFinished];
    }
    Vec::new()
}

/// Build the safe activity description of a tool call.
///
/// The tool name is translated into the vocabulary of [`classify_tool`] first:
/// the raw Droid ids (`ApplyPatch`, `Create`, `Execute`, ...) would otherwise
/// degrade into a generic label. The command is only used to pick one of the
/// fixed labels, the command itself never leaves the adapter.
fn tool_activity(name: &str, input: &Value) -> ToolActivity {
    let command = input
        .get("command")
        .or_else(|| input.get("cmd"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let label = classify_tool(&map_tool_name(name), command).to_string();
    let file = extract_file_basename(input);
    let language = file
        .as_deref()
        .and_then(language_from_file)
        .map(str::to_string);

    ToolActivity {
        label,
        file,
        language,
    }
}

/// Translate a Droid tool id into the vocabulary of [`classify_tool`].
fn map_tool_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "applypatch" | "apply_patch" => "apply_patch".to_string(),
        "create" | "createfile" | "create_file" | "write" => "write".to_string(),
        "edit" => "edit".to_string(),
        "multiedit" | "multi_edit" => "edit".to_string(),
        "read" | "readfile" | "read_file" => "read".to_string(),
        "ls" | "list" | "listdirectory" | "list_directory" => "list".to_string(),
        "grep" | "glob" => lower,
        "execute" | "execute-cli" | "execute_cli" | "exec" | "script" | "waitforscript"
        | "wait_for_script" | "bash" | "shell" => "exec".to_string(),
        "todowrite" | "todo_write" | "todo" => "todo".to_string(),
        "task" | "spawnagent" | "spawn_agent" | "sendmessage" | "send_message" | "listagents" => {
            "task".to_string()
        }
        "askuser" | "ask_user" => "ask".to_string(),
        "websearch" | "web_search" => "web_search".to_string(),
        "fetchurl" | "fetch_url" => "web_fetch".to_string(),
        "generateimage" | "generate_image" => "generate_image".to_string(),
        _ => lower,
    }
}

/// The content blocks of a message.
fn content_blocks(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::Array(items)) => items.clone(),
        Some(Value::Object(_)) => content.cloned().into_iter().collect(),
        Some(Value::String(text)) => vec![serde_json::json!({ "type": "text", "text": text })],
        _ => Vec::new(),
    }
}

/// The type of a content block.
fn block_type(block: &Value) -> &str {
    block.get("type").and_then(Value::as_str).unwrap_or("")
}

/// The input of a tool call.
fn tool_input(block: &Value) -> Value {
    match block.get("input") {
        Some(Value::String(text)) => serde_json::from_str::<Value>(text).unwrap_or(Value::Null),
        Some(value) => value.clone(),
        None => Value::Null,
    }
}

/// The call id of a tool call.
fn call_id_of_use(block: &Value) -> Option<&str> {
    block
        .get("id")
        .or_else(|| block.get("call_id"))
        .or_else(|| block.get("callId"))
        .and_then(Value::as_str)
}

/// The call id of a tool result.
///
/// The on-disk blocks use `tool_use_id`, the live protocol `toolUseId`.
fn call_id_of_result(block: &Value) -> Option<&str> {
    block
        .get("tool_use_id")
        .or_else(|| block.get("toolUseId"))
        .or_else(|| block.get("call_id"))
        .or_else(|| block.get("callId"))
        .and_then(Value::as_str)
}

/// True when a tool result carries a payload besides its type.
fn has_result_payload(block: &Value) -> bool {
    [
        "content",
        "is_error",
        "isError",
        "model_content",
        "modelContent",
    ]
    .iter()
    .any(|key| block.get(key).is_some())
}

/// The model id of a message, if the build records one.
fn message_model(record: &Value) -> Option<String> {
    let message = record.get("message").unwrap_or(&Value::Null);
    record
        .get("model")
        .or_else(|| message.get("model"))
        .or_else(|| record.get("metadata").and_then(|meta| meta.get("model")))
        .and_then(Value::as_str)
        .and_then(sanitize_token)
}

/// Keep only id-like tokens: model ids and call ids are metadata, never prose.
fn sanitize_token(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_TOKEN_LEN {
        return None;
    }
    if !trimmed
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:/@".contains(&byte))
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// The start of the current turn, taken from the record's own timestamp.
fn turn_start_ms(record: &Value) -> i64 {
    record
        .get("timestamp")
        .and_then(timestamp_ms)
        .unwrap_or_else(current_time_ms)
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

    /// Every event as text, so the privacy tests can look for leaks.
    fn debug_of(events: &[ParsedEvent]) -> String {
        format!("{events:?}")
    }

    #[test]
    fn parses_a_realistic_session_into_safe_events() {
        let header = r#"{"type":"session_start","id":"0f7d6c2a-9a1b-4d3e-8c55-2f1c9a7b4e10","title":"PRIVATE_TITLE","owner":"PRIVATE_USER","version":2,"cwd":"C:\\Users\\me\\Documents\\Workspace"}"#;
        assert_eq!(
            parse_line(header),
            vec![ParsedEvent::SessionMetadata {
                project: "Workspace".to_string(),
                cwd: r"C:\Users\me\Documents\Workspace".to_string(),
            }]
        );

        let read = r#"{"type":"message","id":"m2","seq":1,"parentId":"m1","timestamp":"2026-10-20T09:30:12.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"call_7HydtXwKFsJOaYUsa0IJAx38","name":"Read","input":{"file_path":"C:\\Users\\me\\Documents\\Workspace\\src\\tray.rs"}}]}}"#;
        assert_eq!(
            parse_line(read),
            vec![ParsedEvent::TrackedToolStarted {
                call_id: "call_7HydtXwKFsJOaYUsa0IJAx38".to_string(),
                activity: ToolActivity {
                    label: "Reading project files".to_string(),
                    file: Some("tray.rs".to_string()),
                    language: Some("Rust".to_string()),
                },
            }]
        );

        let exec = r#"{"type":"message","id":"m3","seq":2,"parentId":"m2","timestamp":"2026-10-20T09:30:20.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"call_exec_1","name":"Execute","input":{"command":"cargo test --release"}}]}}"#;
        assert_eq!(
            parse_line(exec),
            vec![ParsedEvent::TrackedToolStarted {
                call_id: "call_exec_1".to_string(),
                activity: ToolActivity {
                    label: "Running Rust tests".to_string(),
                    file: None,
                    language: None,
                },
            }]
        );

        let patch = r#"{"type":"message","id":"m4","seq":3,"timestamp":"2026-10-20T09:30:25.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"call_patch_1","name":"ApplyPatch","input":{"patch":"*** Update File: src/main.rs\n@@\n"}}]}}"#;
        assert_eq!(
            parse_line(patch),
            vec![ParsedEvent::TrackedToolStarted {
                call_id: "call_patch_1".to_string(),
                activity: ToolActivity {
                    label: "Editing files".to_string(),
                    file: Some("main.rs".to_string()),
                    language: Some("Rust".to_string()),
                },
            }]
        );

        let result = r#"{"type":"message","id":"m5","seq":4,"parentId":"m4","timestamp":"2026-10-20T09:30:26.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_7HydtXwKFsJOaYUsa0IJAx38","is_error":false,"content":"PRIVATE_TOOL_OUTPUT"}]}}"#;
        assert_eq!(
            parse_line(result),
            vec![ParsedEvent::TrackedToolFinished {
                call_id: "call_7HydtXwKFsJOaYUsa0IJAx38".to_string(),
            }]
        );

        let answer = r#"{"type":"message","id":"m6","seq":5,"timestamp":"2026-10-20T09:30:30.000Z","message":{"role":"assistant","content":[{"type":"text","text":"PRIVATE_ANSWER"}]}}"#;
        assert_eq!(parse_line(answer), vec![ParsedEvent::TerminalCandidate]);

        assert_eq!(
            parse_line(r#"{"type":"agent_turn_outcome","turnId":"t1","reason":"end_turn"}"#),
            vec![ParsedEvent::Completed]
        );
    }

    #[test]
    fn never_forwards_private_text() {
        let lines = [
            r#"{"type":"session_start","id":"s1","title":"PRIVATE_TITLE","owner":"PRIVATE_OWNER","organizationId":"PRIVATE_ORG","version":2,"cwd":"C:\\Users\\me\\Documents\\Workspace"}"#,
            r#"{"type":"message","id":"m1","seq":0,"timestamp":"2026-10-20T09:30:00.000Z","message":{"role":"user","content":[{"type":"text","text":"PRIVATE_PROMPT"}]}}"#,
            r#"{"type":"message","id":"m2","seq":1,"timestamp":"2026-10-20T09:30:05.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"PRIVATE_THINKING"},{"type":"text","text":"PRIVATE_ANSWER"},{"type":"tool_use","id":"call_1","name":"Edit","input":{"file_path":"C:\\Users\\me\\Documents\\Workspace\\src\\tray.rs","old_string":"PRIVATE_OLD","new_string":"PRIVATE_NEW"}}]}}"#,
            r#"{"type":"message","id":"m3","seq":2,"timestamp":"2026-10-20T09:30:09.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","is_error":false,"content":"PRIVATE_TOOL_OUTPUT"}]}}"#,
            r#"{"type":"agent_turn_outcome","turnId":"t1","reason":"end_turn"}"#,
        ];

        let events: Vec<ParsedEvent> = lines.iter().flat_map(|line| parse_line(line)).collect();
        let debug = debug_of(&events);

        for secret in [
            "PRIVATE_PROMPT",
            "PRIVATE_THINKING",
            "PRIVATE_ANSWER",
            "PRIVATE_OLD",
            "PRIVATE_NEW",
            "PRIVATE_TOOL_OUTPUT",
            "PRIVATE_TITLE",
            "PRIVATE_OWNER",
            "PRIVATE_ORG",
        ] {
            assert!(!debug.contains(secret), "private text leaked: {secret}");
        }

        // The full path and its directories must not leak, only the basename.
        // The workspace path itself is part of SessionMetadata by design: it
        // stays local, only the folder name is ever published.
        assert!(!debug.contains(r"C:\\Users\\me\\Documents\\Workspace\\src\\tray.rs"));
        assert!(!debug.contains(r"Workspace\\src"));
        assert!(!debug.contains(r"src\\tray.rs"));

        // The safe parts are still published.
        assert!(debug.contains("tray.rs"));
        assert!(debug.contains("Workspace"));
    }

    #[test]
    fn malformed_and_unknown_records_produce_no_events() {
        for line in [
            "",
            "not json",
            "{",
            "null",
            "[]",
            "{}",
            r#"{"type":""}"#,
            r#"{"type":"future_record","payload":"PRIVATE"}"#,
            r#"{"type":"session_start"}"#,
            r#"{"type":"session_start","workDir":"C:\\Users\\me\\Documents\\Workspace"}"#,
            r#"{"type":"message"}"#,
            r#"{"type":"message","message":{"role":"assistant"}}"#,
            r#"{"type":"message","message":{"role":"user"}}"#,
            r#"{"type":"tool_use"}"#,
            r#"{"type":"tool_result"}"#,
            r#"{"type":"todo_state","todos":[]}"#,
            r#"{"type":"compaction_state"}"#,
        ] {
            assert!(parse_line(line).is_empty(), "unexpected events for {line}");
        }
    }

    #[test]
    fn reports_model_id_and_turn_timestamps() {
        let header = r#"{"type":"session_start","id":"s1","cwd":"C:\\Users\\me\\Documents\\Workspace","model":"factory/droid"}"#;
        let events = parse_line(header);
        assert!(
            events.contains(&ParsedEvent::AgentDetected {
                agent: "factory/droid".to_string()
            }),
            "model id missing: {events:?}"
        );

        // A model id that is not id-like is dropped instead of published.
        let prose =
            r#"{"type":"session_start","cwd":"C:\\w\\p","model":"private prose about my project"}"#;
        assert!(
            !parse_line(prose)
                .iter()
                .any(|event| matches!(event, ParsedEvent::AgentDetected { .. }))
        );

        // An over long id is dropped as well.
        let long = format!(
            r#"{{"type":"session_start","cwd":"C:\\w\\p","model":"{}"}}"#,
            "a".repeat(200)
        );
        assert!(
            !parse_line(&long)
                .iter()
                .any(|event| matches!(event, ParsedEvent::AgentDetected { .. }))
        );

        // A new user turn starts the elapsed timer with the record timestamp.
        let user = r#"{"type":"message","id":"m1","seq":0,"timestamp":"2026-10-20T09:30:00.000Z","message":{"role":"user","content":[{"type":"text","text":"PRIVATE_PROMPT"}]}}"#;
        assert_eq!(
            parse_line(user),
            vec![ParsedEvent::TaskStarted {
                started_at_ms: 1_792_488_600_000
            }]
        );

        // A message that carries a tool result continues the turn instead.
        let result = r#"{"type":"message","id":"m2","seq":1,"timestamp":"2026-10-20T09:30:01.000Z","message":{"role":"user","content":[{"type":"tool_result","toolUseId":"call_1","isError":false,"content":"PRIVATE"}]}}"#;
        assert_eq!(
            parse_line(result),
            vec![ParsedEvent::TrackedToolFinished {
                call_id: "call_1".to_string()
            }]
        );
    }

    #[test]
    fn reports_subagent_sessions_and_tool_blocks_in_own_records() {
        let header = r#"{"type":"session_start","id":"s2","parent":"0f7d6c2a-9a1b-4d3e-8c55-2f1c9a7b4e10","cwd":"C:\\Users\\me\\Documents\\Workspace"}"#;
        let events = parse_line(header);
        assert!(
            events.contains(&ParsedEvent::SubagentSpawned {
                conversation_id: "0f7d6c2a-9a1b-4d3e-8c55-2f1c9a7b4e10".to_string(),
                agent: "Factory subagent".to_string(),
            }),
            "subagent missing: {events:?}"
        );

        let use_record = r#"{"type":"tool_use","id":"call_9","name":"Write","input":{"file_path":"C:\\Users\\me\\Documents\\Workspace\\src\\lib.rs"}}"#;
        assert_eq!(
            parse_line(use_record),
            vec![ParsedEvent::TrackedToolStarted {
                call_id: "call_9".to_string(),
                activity: ToolActivity {
                    label: "Editing files".to_string(),
                    file: Some("lib.rs".to_string()),
                    language: Some("Rust".to_string()),
                },
            }]
        );

        let result_record =
            r#"{"type":"tool_result","toolUseId":"call_9","isError":false,"content":"PRIVATE"}"#;
        assert_eq!(
            parse_line(result_record),
            vec![ParsedEvent::TrackedToolFinished {
                call_id: "call_9".to_string()
            }]
        );
    }
}
