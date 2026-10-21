//! Continue adapter: session documents in ~/.continue/sessions/<sessionId>.json
//!
//! Continue stores one pretty-printed JSON document per session and rewrites it
//! in place whenever the conversation changes (`HistoryManager.save` performs a
//! full `writeFileSync`), so the document is re-read and diffed instead of
//! tailed. The sibling `sessions.json` index is not a session document and
//! therefore yields no events.
//!
//! A session document is an object with a `history` array of chat history
//! items. Every item has a `message` (role and content), optional
//! `toolCallStates` (the tool name, its arguments and its status) and an
//! optional `reasoning` block. Items carry no timestamp, so the elapsed timer
//! starts when the turn is first observed.
//!
//! Only safe metadata leaves this module: the workspace folder name, a file
//! basename, an inferred language, a normalized activity label, a sanitized
//! model id and timing. Prompts, assistant answers, reasoning text, tool
//! arguments, tool results, the session title and absolute paths are dropped.

use std::collections::HashSet;

use serde_json::Value;

use crate::events::{
    ParsedEvent, ToolActivity, classify_tool, extract_file_basename, language_from_file,
    project_name, timestamp_ms,
};

/// Tool states that mean the tool is running or waiting to be approved.
const ACTIVE_TOOL_STATES: [&str; 2] = ["calling", "generated"];
/// Tool states that mean the tool will not run again.
const TERMINAL_TOOL_STATES: [&str; 3] = ["done", "errored", "canceled"];

/// Key of the session wide workspace record.
const KEY_WORKSPACE: &str = "c:workspace";
/// Prefix of the session wide model records.
const KEY_MODEL: &str = "c:model:";

/// Parse the harness' session document and report only records that were not
/// seen before.
///
/// The provider rewrites this document in place, so it is re-read on every
/// change. Every record gets a stable key (the index of the history item plus
/// the slot of the tool inside it), records whose key is already in `seen` are
/// skipped, and the keys of the reported records are inserted.
pub fn parse_document(doc: &Value, seen: &mut HashSet<String>) -> Vec<ParsedEvent> {
    let mut events = Vec::new();

    // Fail closed: only a session document with a `history` array is parsed.
    // The sibling `sessions.json` index, a foreign JSON file or a truncated
    // document yields no activity at all.
    let Some(object) = doc.as_object() else {
        return events;
    };
    let Some(history) = object.get("history").and_then(Value::as_array) else {
        return events;
    };

    if let Some(cwd) = object.get("workspaceDirectory").and_then(Value::as_str)
        && !cwd.trim().is_empty()
        && let Some(project) = project_name(cwd)
        && !seen.contains(KEY_WORKSPACE)
    {
        seen.insert(KEY_WORKSPACE.to_string());
        events.push(ParsedEvent::SessionMetadata {
            project,
            cwd: cwd.to_string(),
        });
    }

    // The model is reported once per document. A title that does not survive
    // the sanitizer is dropped instead of being published.
    if !seen.iter().any(|key| key.starts_with(KEY_MODEL))
        && !report_model(
            object.get("chatModelTitle").and_then(Value::as_str),
            seen,
            &mut events,
        )
    {
        for item in history {
            if let Some(title) = prompt_log_model(item)
                && report_model(Some(title), seen, &mut events)
            {
                break;
            }
        }
    }

    for (index, item) in history.iter().enumerate() {
        collect_item(item, index, seen, &mut events);
    }

    events
}

/// Collect the events of a single chat history item.
fn collect_item(
    item: &Value,
    index: usize,
    seen: &mut HashSet<String>,
    events: &mut Vec<ParsedEvent>,
) {
    let Some(item_object) = item.as_object() else {
        return;
    };
    let message = item_object.get("message");
    let role = message
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str)
        .unwrap_or("");

    match role {
        "user" => {
            // A user message is the start of a new turn.
            let key = format!("c:item:{index}:turn");
            if !seen.contains(&key) {
                seen.insert(key);
                events.push(ParsedEvent::TaskStarted {
                    started_at_ms: started_at_ms(item, message),
                });
            }
        }
        "assistant" => {
            // The model is thinking while the reasoning block is active. The
            // reasoning text itself is never published.
            if item_object
                .get("reasoning")
                .and_then(|reasoning| reasoning.get("active"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                let key = format!("c:item:{index}:reasoning");
                if !seen.contains(&key) {
                    seen.insert(key);
                    events.push(ParsedEvent::Thinking { reasoning: None });
                }
            }

            let tools = collect_tools(item, message);
            for (slot, tool) in tools.iter().enumerate() {
                report_tool(tool, index, slot, seen, events);
            }

            // An assistant message without tool calls is the final answer of
            // the turn. It is a terminal candidate, so the presence fades out
            // after the debounce window instead of vanishing right away.
            if tools.is_empty()
                && has_answer(message)
                && let key = format!("c:item:{index}:answer")
                && !seen.contains(&key)
            {
                seen.insert(key);
                events.push(ParsedEvent::TerminalCandidate);
            }
        }
        // Tool results, system messages and unknown roles carry no activity.
        _ => {}
    }
}

/// A tool call of a history item.
struct ToolCall {
    /// Identifier of the call, used to pair start and finish.
    id: Option<String>,
    /// Name of the tool, used to derive a normalized activity label.
    name: String,
    /// Parsed arguments, used to derive a file basename and a language.
    arguments: Option<Value>,
    /// State of the tool call, when the document tracks one.
    state: Option<String>,
}

/// Collect the tool calls of a history item.
///
/// The tool call states are the authoritative source because only they carry
/// the state of a call. Older documents only keep the requested calls on the
/// message itself.
fn collect_tools(item: &Value, message: Option<&Value>) -> Vec<ToolCall> {
    if let Some(states) = item.get("toolCallStates").and_then(Value::as_array)
        && !states.is_empty()
    {
        return states
            .iter()
            .filter_map(tool_from_state)
            .collect::<Vec<_>>();
    }

    message
        .and_then(|message| message.get("toolCalls"))
        .and_then(Value::as_array)
        .map(|calls| calls.iter().filter_map(tool_from_call).collect::<Vec<_>>())
        .unwrap_or_default()
}

/// Read one entry of the `toolCallStates` array.
fn tool_from_state(state: &Value) -> Option<ToolCall> {
    let call = state.get("toolCall");
    let function = call.and_then(|call| call.get("function"));
    let name = function
        .and_then(|function| function.get("name"))
        .or_else(|| call.and_then(|call| call.get("name")))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let arguments = call_arguments(call, function)
        .or_else(|| state.get("parsedArgs").and_then(parsed_arguments));
    let id = state
        .get("toolCallId")
        .or_else(|| call.and_then(|call| call.get("id")))
        .and_then(Value::as_str)
        .and_then(sanitize_call_id);
    let state = state
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(ToolCall {
        id,
        name,
        arguments,
        state,
    })
}

/// Read one entry of the `toolCalls` array of a message.
fn tool_from_call(call: &Value) -> Option<ToolCall> {
    let function = call.get("function");
    let name = function
        .and_then(|function| function.get("name"))
        .or_else(|| call.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let arguments = call_arguments(Some(call), function);
    let id = call
        .get("id")
        .and_then(Value::as_str)
        .and_then(sanitize_call_id);
    Some(ToolCall {
        id,
        name,
        arguments,
        state: None,
    })
}

/// Read the arguments of a tool call.
///
/// The arguments are stored as a JSON string by most providers and as an
/// object by others. Only the parsed object is used, and only its file name
/// ever leaves this module.
fn call_arguments(call: Option<&Value>, function: Option<&Value>) -> Option<Value> {
    let raw = function
        .and_then(|function| function.get("arguments"))
        .or_else(|| call.and_then(|call| call.get("arguments")))?;
    parsed_arguments(raw)
}

/// Parse a JSON encoded argument object.
fn parsed_arguments(raw: &Value) -> Option<Value> {
    match raw {
        Value::String(text) => serde_json::from_str::<Value>(text).ok(),
        Value::Object(_) => Some(raw.clone()),
        _ => None,
    }
}

/// Report the start or the end of a tool call.
///
/// The key is the slot of the call inside the history item, so a rewritten
/// document reports every state transition exactly once.
fn report_tool(
    tool: &ToolCall,
    index: usize,
    slot: usize,
    seen: &mut HashSet<String>,
    events: &mut Vec<ParsedEvent>,
) {
    let start_key = format!("c:item:{index}:tool:{slot}:start");
    let end_key = format!("c:item:{index}:tool:{slot}:end");
    let state = tool.state.as_deref();
    let active = state.is_none() || state.is_some_and(|state| ACTIVE_TOOL_STATES.contains(&state));
    let terminal = state.is_some_and(|state| TERMINAL_TOOL_STATES.contains(&state));

    if active {
        if seen.contains(&start_key) {
            return;
        }
        seen.insert(start_key);
        let activity = tool_activity(&tool.name, tool.arguments.as_ref());
        match &tool.id {
            Some(call_id) => events.push(ParsedEvent::TrackedToolStarted {
                call_id: call_id.clone(),
                activity,
            }),
            None => events.push(ParsedEvent::ToolStarted { activity }),
        }
        return;
    }

    if terminal {
        // A call that was never seen running must not report a finish.
        if !seen.contains(&start_key) || seen.contains(&end_key) {
            return;
        }
        seen.insert(end_key);
        match &tool.id {
            Some(call_id) => events.push(ParsedEvent::TrackedToolFinished {
                call_id: call_id.clone(),
            }),
            None => events.push(ParsedEvent::ToolFinished),
        }
    }
}

/// Build the activity of a tool call.
///
/// The label comes from the shared classifier, the file is the basename of the
/// path in the arguments and the language is inferred from that basename. The
/// command of a terminal tool is only used to pick a label, never published.
fn tool_activity(name: &str, arguments: Option<&Value>) -> ToolActivity {
    let lower_name = name.to_ascii_lowercase();
    let command = arguments
        .and_then(|arguments| arguments.get("command"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let label = classify_tool(&lower_name, command).to_string();
    let file = arguments
        .and_then(extract_file_basename)
        .or_else(|| arguments.and_then(directory_basename));
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

/// Read the folder name of a directory argument.
///
/// The listing tools of Continue use `dirPath` or `directory_path` instead of
/// the file keys of the shared helper.
fn directory_basename(arguments: &Value) -> Option<String> {
    for key in ["dirPath", "directory_path", "dir_path", "directoryPath"] {
        if let Some(path) = arguments.get(key).and_then(Value::as_str)
            && let Some(name) = basename(path.trim().trim_matches('"'))
        {
            return Some(name);
        }
    }
    None
}

/// The last component of a path.
fn basename(path: &str) -> Option<String> {
    path.replace('\\', "/")
        .rsplit('/')
        .find(|part| !part.is_empty())
        .map(str::to_string)
}

/// Report the model of the session.
fn report_model(
    title: Option<&str>,
    seen: &mut HashSet<String>,
    events: &mut Vec<ParsedEvent>,
) -> bool {
    let Some(agent) = title.and_then(sanitize_model) else {
        return false;
    };
    let key = format!("{KEY_MODEL}{agent}");
    if seen.contains(&key) {
        return false;
    }
    seen.insert(key);
    events.push(ParsedEvent::AgentDetected { agent });
    true
}

/// The model of a history item, when the document logs one.
fn prompt_log_model(item: &Value) -> Option<&str> {
    let logs = item.get("promptLogs")?.as_array()?;
    logs.iter().find_map(|log| {
        log.get("modelTitle")
            .or_else(|| log.get("modelProvider"))
            .and_then(Value::as_str)
    })
}

/// Sanitize a model title.
///
/// Only a short identifier survives. A title with prose in it is dropped
/// instead of being published, because the title is free text.
fn sanitize_model(title: &str) -> Option<String> {
    let trimmed = title.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
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

/// Sanitize the identifier of a tool call.
fn sanitize_call_id(id: &str) -> Option<String> {
    let trimmed = id.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return None;
    }
    if !trimmed
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// Whether an assistant message carries a final answer.
///
/// Only the presence of content is inspected, the content itself is dropped.
fn has_answer(message: Option<&Value>) -> bool {
    match message.and_then(|message| message.get("content")) {
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        _ => false,
    }
}

/// The start of a turn.
///
/// History items carry no timestamp, so the observation time is used when the
/// document does not provide one.
fn started_at_ms(item: &Value, message: Option<&Value>) -> i64 {
    for key in ["timestamp", "createdAt", "created_at", "time"] {
        if let Some(value) = item.get(key)
            && let Some(millis) = timestamp_ms(value)
        {
            return millis;
        }
    }
    if let Some(message) = message {
        for key in ["timestamp", "createdAt", "created_at"] {
            if let Some(value) = message.get(key)
                && let Some(millis) = timestamp_ms(value)
            {
                return millis;
            }
        }
    }
    current_time_ms()
}

/// The current time in milliseconds.
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
    use crate::provider::Provider;
    use crate::state::{ActivityState, Phase};
    use std::path::Path;

    /// A session document with one user turn, one tool call and one answer.
    fn session_document() -> Value {
        serde_json::json!({
            "sessionId": "6f1c1d2e-0b3a-4c5d-8e9f-0011223344aa",
            "title": "PRIVATE_TITLE_FROM_USER_PROMPT",
            "workspaceDirectory": r"C:\Users\PC\Documents\Discodex",
            "history": [
                {
                    "message": {"role": "user", "content": "PRIVATE_PROMPT_TEXT"},
                    "contextItems": []
                },
                {
                    "message": {
                        "role": "assistant",
                        "content": "",
                        "toolCalls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "read_file",
                                "arguments":
                                    r#"{"filepath":"C:\\Users\\PC\\Documents\\Discodex\\src\\provider.rs"}"#
                            }
                        }]
                    },
                    "contextItems": [],
                    "toolCallStates": [{
                        "toolCallId": "call_1",
                        "toolCall": {
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "read_file",
                                "arguments":
                                    r#"{"filepath":"C:\\Users\\PC\\Documents\\Discodex\\src\\provider.rs"}"#
                            }
                        },
                        "status": "calling",
                        "parsedArgs": {
                            "filepath": r"C:\Users\PC\Documents\Discodex\src\provider.rs"
                        }
                    }]
                },
                {
                    "message": {"role": "assistant", "content": "PRIVATE_ASSISTANT_TEXT"},
                    "contextItems": []
                }
            ],
            "mode": "agent",
            "chatModelTitle": "gpt-4o"
        })
    }

    #[test]
    fn parses_metadata_model_tool_activity_and_phase() {
        let mut seen = HashSet::new();
        let events = parse_document(&session_document(), &mut seen);

        assert_eq!(events.len(), 5);
        assert_eq!(
            events[0],
            ParsedEvent::SessionMetadata {
                project: "Discodex".to_string(),
                cwd: r"C:\Users\PC\Documents\Discodex".to_string(),
            }
        );
        assert_eq!(
            events[1],
            ParsedEvent::AgentDetected {
                agent: "gpt-4o".to_string()
            }
        );
        assert!(matches!(events[2], ParsedEvent::TaskStarted { .. }));
        assert_eq!(
            events[3],
            ParsedEvent::TrackedToolStarted {
                call_id: "call_1".to_string(),
                activity: ToolActivity {
                    label: "Reading project files".to_string(),
                    file: Some("provider.rs".to_string()),
                    language: Some("Rust".to_string()),
                },
            }
        );
        assert_eq!(events[4], ParsedEvent::TerminalCandidate);

        // The tool call moves the presence into the running tools phase.
        let mut state = ActivityState::default();
        for event in events.iter().take(4) {
            state.apply(
                Path::new("session"),
                Provider::Continue,
                event.clone(),
                1_000,
            );
        }
        let presence = state.selected_presence().expect("presence");
        assert_eq!(presence.phase, Phase::RunningTools);
        assert_eq!(presence.project, "Discodex");
        assert_eq!(presence.current_file.as_deref(), Some("provider.rs"));
        assert_eq!(presence.current_language.as_deref(), Some("Rust"));
        assert_eq!(presence.agent.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn never_forwards_prompt_answer_output_or_full_path() {
        let document = serde_json::json!({
            "sessionId": "6f1c1d2e-0b3a-4c5d-8e9f-0011223344aa",
            "title": "PRIVATE_TITLE_FROM_USER_PROMPT",
            "workspaceDirectory": r"C:\Users\PC\Documents\Discodex",
            "history": [
                {
                    "message": {"role": "user", "content": "PRIVATE_PROMPT_TEXT"},
                    "contextItems": [{"content": "PRIVATE_CONTEXT_ITEM"}]
                },
                {
                    "message": {
                        "role": "assistant",
                        "content": "PRIVATE_ASSISTANT_TEXT",
                        "toolCalls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "read_file",
                                "arguments":
                                    r#"{"filepath":"C:\\S3CR3T_DIR\\src\\secret_file.rs"}"#
                            }
                        }]
                    },
                    "reasoning": {"active": true, "text": "PRIVATE_REASONING_TEXT"},
                    "toolCallStates": [{
                        "toolCallId": "call_1",
                        "toolCall": {
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "read_file",
                                "arguments":
                                    r#"{"filepath":"C:\\S3CR3T_DIR\\src\\secret_file.rs"}"#
                            }
                        },
                        "status": "calling",
                        "parsedArgs": {"filepath": r"C:\S3CR3T_DIR\src\secret_file.rs"},
                        "output": [{"content": "PRIVATE_TOOL_OUTPUT"}]
                    }]
                }
            ],
            "mode": "agent",
            "chatModelTitle": "gpt-4o"
        });

        let mut seen = HashSet::new();
        let events = parse_document(&document, &mut seen);
        assert!(!events.is_empty());

        let rendered = format!("{events:?}");
        for secret in [
            "PRIVATE_PROMPT_TEXT",
            "PRIVATE_CONTEXT_ITEM",
            "PRIVATE_ASSISTANT_TEXT",
            "PRIVATE_REASONING_TEXT",
            "PRIVATE_TOOL_OUTPUT",
            "PRIVATE_TITLE_FROM_USER_PROMPT",
            "S3CR3T_DIR",
            "secret_file.rs\\",
            "filepath",
        ] {
            assert!(
                !rendered.contains(secret),
                "the event leaked {secret}: {rendered}"
            );
        }

        // Only the basename of the tool path is published.
        let files: Vec<String> = events
            .iter()
            .filter_map(|event| match event {
                ParsedEvent::ToolStarted { activity }
                | ParsedEvent::TrackedToolStarted { activity, .. } => activity.file.clone(),
                _ => None,
            })
            .collect();
        assert_eq!(files, vec!["secret_file.rs".to_string()]);
    }

    #[test]
    fn malformed_or_unknown_records_produce_no_events() {
        let documents = [
            serde_json::json!({}),
            serde_json::json!(null),
            serde_json::json!([]),
            serde_json::json!("garbage"),
            serde_json::json!(42),
            serde_json::json!({"history": "not an array"}),
            serde_json::json!({"history": {}}),
            serde_json::json!({"history": [42, "text", null]}),
            serde_json::json!({"history": [{"unexpected": "shape"}]}),
            serde_json::json!({"history": [{"message": {"role": "unknown", "content": "PRIVATE"}}]}),
            serde_json::json!({"history": [{"message": {"role": "tool", "toolCallId": "call_1"}}]}),
            serde_json::json!({"sessionId": "x", "title": "PRIVATE_TITLE"}),
        ];

        for document in documents {
            let mut seen = HashSet::new();
            let events = parse_document(&document, &mut seen);
            assert!(events.is_empty(), "unexpected events for {document}");
            assert!(seen.is_empty(), "unexpected keys for {document}");
        }
    }

    #[test]
    fn uses_the_model_id_and_the_timestamp_of_the_record() {
        let document = serde_json::json!({
            "workspaceDirectory": r"C:\Users\PC\Documents\Discodex",
            "history": [
                {
                    "message": {"role": "user", "content": "PRIVATE_PROMPT_TEXT"},
                    "timestamp": 1_789_187_508_000i64
                }
            ],
            "chatModelTitle": "gpt-4o"
        });

        let mut seen = HashSet::new();
        let events = parse_document(&document, &mut seen);
        assert_eq!(
            events[1],
            ParsedEvent::AgentDetected {
                agent: "gpt-4o".to_string()
            }
        );
        assert_eq!(
            events[2],
            ParsedEvent::TaskStarted {
                started_at_ms: 1_789_187_508_000
            }
        );

        // A free text title is dropped instead of being published.
        let document = serde_json::json!({
            "workspaceDirectory": r"C:\Users\PC\Documents\Discodex",
            "history": [{"message": {"role": "user", "content": "PRIVATE"}}],
            "chatModelTitle": "PRIVATE prose about my model"
        });
        let mut seen = HashSet::new();
        let events = parse_document(&document, &mut seen);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ParsedEvent::AgentDetected { .. }))
        );
        assert!(!format!("{events:?}").contains("PRIVATE prose"));
    }

    #[test]
    fn a_rewritten_document_reports_every_transition_once() {
        let mut seen = HashSet::new();
        let document = session_document();

        let first = parse_document(&document, &mut seen);
        assert_eq!(first.len(), 5);

        // The same document is rewritten in place and must not replay.
        let second = parse_document(&document, &mut seen);
        assert!(second.is_empty());

        // The tool call finished: exactly one finish and no second start.
        let mut finished = document.clone();
        finished["history"][1]["toolCallStates"][0]["status"] = serde_json::json!("done");
        let third = parse_document(&finished, &mut seen);
        assert_eq!(
            third,
            vec![ParsedEvent::TrackedToolFinished {
                call_id: "call_1".to_string()
            }]
        );

        // Rewriting it again does not repeat the finish.
        let fourth = parse_document(&finished, &mut seen);
        assert!(fourth.is_empty());
    }
}
