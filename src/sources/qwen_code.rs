//! Qwen Code adapter: session logs in ~/.qwen/projects/<project>/chats/*.jsonl
//!
//! Upstream: <https://github.com/QwenLM/qwen-code> (npm `@qwen-code/qwen-code`).
//! The chat log is a newline delimited JSONL stream of `ChatRecord` objects.
//! Every record carries a top level `cwd` (the workspace root), a top level
//! RFC-3339 `timestamp`, a `type` (+ optional `subtype` / `provenance`) and
//! `message.parts[]` that hold `functionCall { id, name, args }` for tool
//! calls and `functionResponse { id, name, response }` for tool results.
//!
//! Only safe metadata ever leaves this module: the workspace folder name, a
//! file basename, an inferred language, a label from the shared closed table,
//! a sanitized model id and timing. Prompts, assistant answers, thought text,
//! tool arguments, tool output and full paths are never forwarded.

use serde_json::Value;

use crate::events::{
    ParsedEvent, ToolActivity, classify_tool, extract_file_basename, language_from_file,
    project_name, timestamp_ms,
};

/// Maximum length of a published model / agent id.
const MAX_AGENT_ID_LEN: usize = 128;

/// Parse one line of the harness' append-only activity log.
///
/// The line is a single JSON record. A record whose shape is unknown yields no
/// events at all: a renamed upstream field must degrade to "no activity",
/// never to a wrong presence.
pub fn parse_line(line: &str) -> Vec<ParsedEvent> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    if !value.is_object() {
        return Vec::new();
    }
    let Some(record_type) = value.get("type").and_then(Value::as_str) else {
        return Vec::new();
    };
    // The managed / ACP dialect is a different schema (epoch `occurredAt`,
    // free form `payload`). Fail safe instead of mis-parsing it.
    let subtype = value.get("subtype").and_then(Value::as_str).unwrap_or("");
    if subtype.starts_with("managed_session") {
        return Vec::new();
    }
    // Unknown record shapes are ignored entirely.
    if !matches!(record_type, "user" | "assistant" | "tool_result" | "system") {
        return Vec::new();
    }

    let mut events = Vec::new();

    match record_type {
        // A new user turn starts the elapsed timer. Notifications and
        // external agent messages carry a different provenance and are not
        // user turns. Older builds of the harness did not record a
        // provenance at all, so a user record without one is a user turn too.
        "user" => {
            let provenance = value
                .get("provenance")
                .and_then(Value::as_str)
                .unwrap_or("");
            let has_subtype = value
                .get("subtype")
                .is_some_and(|value| !value.is_null() && value.as_str() != Some(""));
            let is_user_turn = match provenance {
                "real_user" => true,
                // No provenance at all: an older harness build.
                "" => true,
                _ => false,
            };
            if is_user_turn && !has_subtype {
                let started_at_ms = value
                    .get("timestamp")
                    .and_then(timestamp_ms)
                    .unwrap_or_else(current_time_ms);
                events.push(ParsedEvent::TaskStarted { started_at_ms });
            }
        }
        "assistant" => {
            if let Some(agent) = agent_label(&value) {
                events.push(ParsedEvent::AgentDetected { agent });
            }

            let mut calls: Vec<&Value> = Vec::new();
            let mut has_thought = false;
            let mut has_text = false;
            for part in value
                .pointer("/message/parts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(call) = part.get("functionCall") {
                    calls.push(call);
                    continue;
                }
                // A thought part carries model reasoning; it is only a phase
                // marker here, the text itself is never read.
                let is_thought = part
                    .get("thought")
                    .is_some_and(|thought| thought.as_bool() == Some(true) || thought.is_object());
                if is_thought {
                    has_thought = true;
                    continue;
                }
                if part
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.trim().is_empty())
                {
                    has_text = true;
                }
            }

            if !calls.is_empty() {
                for call in calls {
                    let name = call.get("name").and_then(Value::as_str).unwrap_or("");
                    let args = call.get("args");
                    let activity = tool_activity(name, args);
                    match call
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|call_id| !call_id.trim().is_empty())
                    {
                        Some(call_id) => events.push(ParsedEvent::TrackedToolStarted {
                            call_id: call_id.to_string(),
                            activity,
                        }),
                        None => events.push(ParsedEvent::ToolStarted { activity }),
                    }
                }
            } else {
                if has_thought {
                    events.push(ParsedEvent::Thinking { reasoning: None });
                }
                if has_text {
                    events.push(ParsedEvent::TerminalCandidate);
                }
            }
        }
        // The tool result closes the matching call. The result payload itself
        // is never read.
        "tool_result" => {
            let call_id = value
                .pointer("/toolCallResult/callId")
                .and_then(Value::as_str)
                .or_else(|| {
                    value
                        .pointer("/message/parts")
                        .and_then(Value::as_array)
                        .and_then(|parts| {
                            parts.iter().find_map(|part| {
                                part.get("functionResponse")
                                    .and_then(|response| response.get("id"))
                                    .and_then(Value::as_str)
                            })
                        })
                })
                .filter(|call_id| !call_id.trim().is_empty());
            match call_id {
                Some(call_id) => events.push(ParsedEvent::TrackedToolFinished {
                    call_id: call_id.to_string(),
                }),
                None => events.push(ParsedEvent::ToolFinished),
            }
        }
        "system" => match subtype {
            // The turn settled.
            "turn_result" => events.push(ParsedEvent::Completed),
            // The last known session model.
            "session_model" => {
                if let Some(agent) = value
                    .pointer("/systemPayload/modelId")
                    .and_then(Value::as_str)
                    .and_then(sanitize_agent_id)
                {
                    events.push(ParsedEvent::AgentDetected { agent });
                }
            }
            // `turn_attempt` is a resumed attempt, not a new turn; every other
            // system subtype is bookkeeping and carries no activity.
            _ => {}
        },
        _ => {}
    }

    // The workspace is only announced together with real activity, so a
    // bookkeeping record never creates a presence of its own.
    if events.is_empty() {
        return events;
    }
    if let Some(metadata) = session_metadata(&value) {
        events.insert(0, metadata);
    }

    events
}

/// Build the session metadata event from the record's workspace path.
///
/// Only the folder name is published; the full path stays local.
fn session_metadata(value: &Value) -> Option<ParsedEvent> {
    let cwd = value.get("cwd").and_then(Value::as_str)?.trim();
    if cwd.is_empty() || is_internal_path(cwd) {
        return None;
    }
    let project = project_name(cwd)?;
    Some(ParsedEvent::SessionMetadata {
        project,
        cwd: cwd.to_string(),
    })
}

/// Ignore workspaces that live inside the harness' own storage directory.
fn is_internal_path(path: &str) -> bool {
    path.split(['\\', '/'])
        .any(|segment| segment.eq_ignore_ascii_case(".qwen"))
}

/// The model (or sub agent) id shown in the presence.
fn agent_label(value: &Value) -> Option<String> {
    let is_sidechain = value
        .get("isSidechain")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if is_sidechain
        && let Some(agent) = value
            .get("agentName")
            .and_then(Value::as_str)
            .and_then(sanitize_agent_id)
    {
        return Some(agent);
    }
    value
        .get("model")
        .and_then(Value::as_str)
        .and_then(sanitize_agent_id)
        .or_else(|| {
            value
                .pointer("/executionContext/modelId")
                .and_then(Value::as_str)
                .and_then(sanitize_agent_id)
        })
}

/// Accept only a strict model id: `[A-Za-z0-9._:/@-]` and at most 128 chars.
fn sanitize_agent_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_AGENT_ID_LEN {
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

/// The normalized activity for one tool call.
fn tool_activity(name: &str, args: Option<&Value>) -> ToolActivity {
    let lower = name.trim().to_ascii_lowercase();
    let canonical = canonical_tool_name(&lower);
    let command = args
        .and_then(|args| {
            args.get("command")
                .or_else(|| args.get("cmd"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default();
    // The label always comes from the shared closed table, never from the
    // tool description or the raw command text.
    let label = classify_tool(label_name(canonical), command).to_string();
    let file = args.and_then(extract_file_basename);
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

/// Upstream canonical tool names (legacy aliases are normalized first).
fn canonical_tool_name(name: &str) -> &str {
    match name {
        "replace" => "edit",
        "search_file_content" => "grep_search",
        "task" => "agent",
        other => other,
    }
}

/// Map a canonical Qwen tool name onto the name the shared label table knows.
///
/// The shared table checks `search` before `grep`, so the Qwen search tool is
/// routed through the `grep` classifier to keep the "Searching project" label
/// instead of the "Browsing documentation" one.
fn label_name(canonical: &str) -> &str {
    match canonical {
        "grep_search" => "grep",
        other => other,
    }
}

/// Milliseconds since the epoch, used when a record has no usable timestamp.
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

    const CWD: &str = "C:\\Users\\PC\\Documents\\Discodex";
    const PROJECT: &str = "Discodex";

    fn user_record() -> Value {
        serde_json::json!({
            "uuid": "8f2c1a40-1111-4111-8111-aaaaaaaaaaaa",
            "parentUuid": null,
            "sessionId": "3f2a9c11-2222-4222-8222-bbbbbbbbbbbb",
            "timestamp": "2026-10-21T07:17:16.123Z",
            "type": "user",
            "provenance": "real_user",
            "cwd": CWD,
            "version": "0.25.3",
            "gitBranch": "main",
            "message": {"role": "user", "parts": [{"text": "<<REDACTED USER PROMPT>>"}]}
        })
    }

    fn assistant_record() -> Value {
        serde_json::json!({
            "uuid": "8f2c1a40-1111-4111-8111-cccccccccccc",
            "parentUuid": "8f2c1a40-1111-4111-8111-aaaaaaaaaaaa",
            "sessionId": "3f2a9c11-2222-4222-8222-bbbbbbbbbbbb",
            "timestamp": "2026-10-21T07:17:18.001Z",
            "type": "assistant",
            "provenance": "assistant_output",
            "cwd": CWD,
            "version": "0.25.3",
            "model": "qwen3-coder-plus",
            "message": {"role": "model", "parts": [{
                "functionCall": {
                    "id": "call-1",
                    "name": "read_file",
                    "args": {"file_path": "C:\\Users\\PC\\Documents\\Discodex\\src\\provider.rs"}
                }
            }]}
        })
    }

    #[test]
    fn maps_qwen_records_to_normalized_events() {
        let events = parse_line(&user_record().to_string());
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0],
            ParsedEvent::SessionMetadata {
                project: PROJECT.to_string(),
                cwd: CWD.to_string(),
            }
        );
        assert_eq!(
            events[1],
            ParsedEvent::TaskStarted {
                started_at_ms: 1_792_567_036_123
            }
        );

        let events = parse_line(&assistant_record().to_string());
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[1],
            ParsedEvent::AgentDetected {
                agent: "qwen3-coder-plus".to_string()
            }
        );
        assert_eq!(
            events[2],
            ParsedEvent::TrackedToolStarted {
                call_id: "call-1".to_string(),
                activity: ToolActivity {
                    label: "Reading project files".to_string(),
                    file: Some("provider.rs".to_string()),
                    language: Some("Rust".to_string()),
                },
            }
        );

        let tool_result = serde_json::json!({
            "uuid": "8f2c1a40-1111-4111-8111-dddddddddddd",
            "timestamp": "2026-10-21T07:17:18.400Z",
            "type": "tool_result",
            "provenance": "tool_result",
            "cwd": CWD,
            "toolCallResult": {"callId": "call-1", "displayName": "ReadFile", "status": "success"},
            "message": {"role": "user", "parts": [{
                "functionResponse": {
                    "id": "call-1",
                    "name": "read_file",
                    "response": {"output": "<<REDACTED TOOL OUTPUT>>"}
                }
            }]}
        });
        let events = parse_line(&tool_result.to_string());
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[1],
            ParsedEvent::TrackedToolFinished {
                call_id: "call-1".to_string()
            }
        );

        let turn_result = serde_json::json!({
            "uuid": "8f2c1a40-1111-4111-8111-eeeeeeeeeeee",
            "timestamp": "2026-10-21T07:17:20.900Z",
            "type": "system",
            "provenance": "system",
            "subtype": "turn_result",
            "cwd": CWD,
            "systemPayload": {"promptId": "<<REDACTED>>"}
        });
        let events = parse_line(&turn_result.to_string());
        assert_eq!(events.len(), 2);
        assert_eq!(events[1], ParsedEvent::Completed);
    }

    #[test]
    fn normalizes_legacy_tool_aliases_and_command_labels() {
        let legacy = serde_json::json!({
            "type": "assistant",
            "cwd": CWD,
            "message": {"parts": [
                {"functionCall": {"id": "c1", "name": "replace", "args": {"file_path": "C:\\Users\\PC\\Documents\\Discodex\\src\\events.rs"}}},
                {"functionCall": {"id": "c2", "name": "search_file_content", "args": {"path": "C:\\Users\\PC\\Documents\\Discodex\\src"}}},
                {"functionCall": {"id": "c3", "name": "task", "args": {"prompt": "<<REDACTED>>"}}},
                {"functionCall": {"id": "c4", "name": "run_shell_command", "args": {"command": "cargo test --lib"}}}
            ]}
        });
        let events = parse_line(&legacy.to_string());
        let activities: Vec<&ToolActivity> = events
            .iter()
            .filter_map(|event| match event {
                ParsedEvent::TrackedToolStarted { activity, .. } => Some(activity),
                _ => None,
            })
            .collect();
        assert_eq!(activities.len(), 4);
        assert_eq!(activities[0].label, "Editing files");
        assert_eq!(activities[0].file.as_deref(), Some("events.rs"));
        assert_eq!(activities[0].language.as_deref(), Some("Rust"));
        assert_eq!(activities[1].label, "Searching project");
        assert_eq!(activities[2].label, "Running coding agent");
        assert_eq!(activities[3].label, "Running Rust tests");
        assert!(activities[3].file.is_none());
    }

    #[test]
    fn never_leaks_prompts_answers_tool_output_or_paths() {
        const PROMPT: &str = "PRIVATE-PROMPT-9f3a";
        const ANSWER: &str = "PRIVATE-ANSWER-71bc";
        const TOOL_OUTPUT: &str = "PRIVATE-OUTPUT-c2d4";
        const TOOL_PATH: &str = "C:\\Users\\PC\\Documents\\Discodex\\src\\private_notes.md";

        let record = serde_json::json!({
            "type": "assistant",
            "provenance": "assistant_output",
            "cwd": CWD,
            "timestamp": "2026-10-21T07:17:18.001Z",
            "model": "qwen3-coder-plus",
            "message": {"role": "model", "parts": [
                {"text": ANSWER},
                {"thought": true, "text": "PRIVATE-THOUGHT-4e8a"},
                {"functionCall": {"id": "call-9", "name": "write_file", "args": {
                    "file_path": TOOL_PATH,
                    "content": PROMPT
                }}}
            ]}
        });
        let result = serde_json::json!({
            "type": "tool_result",
            "cwd": CWD,
            "toolCallResult": {"callId": "call-9", "status": "success"},
            "message": {"parts": [{"functionResponse": {"id": "call-9", "name": "write_file", "response": {"output": TOOL_OUTPUT}}}]}
        });

        let mut events = parse_line(&record.to_string());
        events.extend(parse_line(&result.to_string()));
        let debug = events
            .iter()
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>()
            .join("\n");

        for secret in [
            PROMPT,
            ANSWER,
            TOOL_OUTPUT,
            "PRIVATE-THOUGHT-4e8a",
            TOOL_PATH,
        ] {
            assert!(!debug.contains(secret), "leaked secret: {secret}");
        }
        // Only the session metadata may hold a path, and it stays local: every
        // other event carries the folder name, a basename or a label.
        let public_debug = events
            .iter()
            .filter(|event| !matches!(event, ParsedEvent::SessionMetadata { .. }))
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!public_debug.contains("Users"));
        assert!(!public_debug.contains(PROJECT));
        let metadata: Vec<&ParsedEvent> = events
            .iter()
            .filter(|event| matches!(event, ParsedEvent::SessionMetadata { .. }))
            .collect();
        assert_eq!(metadata.len(), 2);
        for event in metadata {
            match event {
                ParsedEvent::SessionMetadata { project, cwd } => {
                    assert_eq!(project, PROJECT);
                    assert_eq!(cwd, CWD);
                }
                _ => unreachable!(),
            }
        }
        // The basename of the edited file is allowed, the path is not.
        assert!(debug.contains("private_notes.md"));
    }

    #[test]
    fn malformed_and_unknown_records_produce_no_events() {
        for line in [
            "",
            "{",
            "not json",
            "null",
            "[]",
            "42",
            "{}",
            r#"{"type":"unknown_record","cwd":"C:\\Users\\PC\\Documents\\Discodex"}"#,
            r#"{"cwd":"C:\\Users\\PC\\Documents\\Discodex"}"#,
            r#"{"type":"user","provenance":"system","subtype":"notification","cwd":"C:\\Users\\PC\\Documents\\Discodex"}"#,
            r#"{"type":"user","provenance":"external_agent","subtype":"agent_message","cwd":"C:\\Users\\PC\\Documents\\Discodex"}"#,
            r#"{"type":"system","subtype":"managed_session_event_v1","payload":{"kind":"model.attempt"},"cwd":"C:\\Users\\PC\\Documents\\Discodex"}"#,
            r#"{"type":"system","subtype":"chat_compression","cwd":"C:\\Users\\PC\\Documents\\Discodex"}"#,
            r#"{"type":"system","subtype":"turn_attempt","cwd":"C:\\Users\\PC\\Documents\\Discodex"}"#,
        ] {
            assert!(parse_line(line).is_empty(), "unexpected events for {line}");
        }

        // A tool call without a call id cannot be tracked, it still reports
        // the activity.
        let untracked = r#"{"type":"assistant","cwd":"C:\\Users\\PC\\Documents\\Discodex","message":{"parts":[{"functionCall":{"name":"read_file","args":{"file_path":"C:\\Users\\PC\\Documents\\Discodex\\src\\events.rs"}}}]}}"#;
        let events = parse_line(untracked);
        assert!(events.contains(&ParsedEvent::ToolStarted {
            activity: ToolActivity {
                label: "Reading project files".to_string(),
                file: Some("events.rs".to_string()),
                language: Some("Rust".to_string()),
            }
        }));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ParsedEvent::TrackedToolStarted { .. }))
        );
    }

    #[test]
    fn reads_model_ids_and_timestamps() {
        let model = serde_json::json!({
            "type": "assistant",
            "cwd": CWD,
            "timestamp": "2026-10-21T09:30:00.000Z",
            "model": "qwen3-coder-plus",
            "message": {"parts": [{"text": "done"}]}
        });
        let events = parse_line(&model.to_string());
        assert!(events.contains(&ParsedEvent::AgentDetected {
            agent: "qwen3-coder-plus".to_string()
        }));
        // A plain text answer is a final answer candidate, not a leak.
        assert!(events.contains(&ParsedEvent::TerminalCandidate));

        // A model id that is prose, or too long, is dropped entirely.
        for bad in [
            "PRIVATE prose with spaces",
            "model/with spaces",
            "a".repeat(129).as_str(),
        ] {
            let record = serde_json::json!({
                "type": "assistant",
                "cwd": CWD,
                "model": bad,
                "message": {"parts": [{"text": "x"}]}
            });
            let events = parse_line(&record.to_string());
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, ParsedEvent::AgentDetected { .. })),
                "unexpected agent label for {bad}"
            );
        }

        // The session model is reported through a system record.
        let session_model = serde_json::json!({
            "type": "system",
            "subtype": "session_model",
            "cwd": CWD,
            "systemPayload": {"modelId": "qwen3-max-preview", "authType": "oauth"}
        });
        assert!(
            parse_line(&session_model.to_string()).contains(&ParsedEvent::AgentDetected {
                agent: "qwen3-max-preview".to_string()
            })
        );

        // The turn start uses the record's own timestamp.
        let turn = serde_json::json!({
            "type": "user",
            "provenance": "real_user",
            "cwd": CWD,
            "timestamp": "2026-10-21T09:30:00.000Z",
            "message": {"parts": [{"text": "hello"}]}
        });
        assert!(
            parse_line(&turn.to_string()).contains(&ParsedEvent::TaskStarted {
                started_at_ms: 1_792_575_000_000
            })
        );
    }
}
