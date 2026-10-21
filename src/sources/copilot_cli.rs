//! GitHub Copilot CLI adapter: session events in `~/.copilot/session-state/<id>/events.jsonl`
//!
//! The CLI appends one JSON `SessionEvent` envelope per line to
//! `<session-state>/<sessionId>/events.jsonl` (legacy layout: `<sessionId>.jsonl`).
//! The envelope carries `id`, `timestamp` (ISO-8601), `parentId`, an optional
//! `ephemeral` flag, an optional `agentId`, a `type` and a `data` payload.
//!
//! Privacy: the log contains the whole conversation. This adapter reads only
//! the envelope `type` and `timestamp`, the tool name and call id, the model
//! id, the workspace path and the file path of the tool arguments. Prompts,
//! answers, reasoning text, tool results, agent descriptions and shell
//! commands never leave the adapter.

use serde_json::Value;

use crate::events::{
    ParsedEvent, ToolActivity, classify_tool, extract_file_basename, language_from_file,
    project_name, timestamp_ms,
};

/// Parse one line of the harness' append-only activity log.
///
/// Only safe metadata may leave this function: the workspace folder name, a
/// file basename, a normalized activity label, a model id and timing.
pub fn parse_line(line: &str) -> Vec<ParsedEvent> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let Some(event_type) = value.get("type").and_then(Value::as_str) else {
        return Vec::new();
    };

    // Upstream marks transient events with `ephemeral: true` and never writes
    // them to the event log. A persisted one means the format changed, so
    // fail closed instead of guessing what it meant.
    if value.get("ephemeral").and_then(Value::as_bool) == Some(true) {
        return Vec::new();
    }

    let data = value.get("data").unwrap_or(&Value::Null);
    let mut events = Vec::new();

    match event_type {
        // The session bootstrap: workspace, timer and the selected model.
        "session.start" => {
            if let Some(cwd) = workspace_cwd(data)
                && let Some(project) = project_name(cwd)
            {
                events.push(ParsedEvent::SessionMetadata {
                    project,
                    cwd: cwd.to_string(),
                });
            }
            let started_at_ms = data
                .get("startTime")
                .and_then(timestamp_ms)
                .or_else(|| value.get("timestamp").and_then(timestamp_ms))
                .unwrap_or_else(now_ms);
            events.push(ParsedEvent::TaskStarted { started_at_ms });
            if let Some(agent) = data.get("selectedModel").and_then(model_label) {
                events.push(ParsedEvent::AgentDetected { agent });
            }
        }
        // A new agentic turn: the elapsed timer restarts here.
        "assistant.turn_start" => {
            let started_at_ms = value
                .get("timestamp")
                .and_then(timestamp_ms)
                .unwrap_or_else(now_ms);
            events.push(ParsedEvent::TaskStarted { started_at_ms });
            if let Some(agent) = data.get("model").and_then(model_label) {
                events.push(ParsedEvent::AgentDetected { agent });
            }
        }
        // The model is thinking. The reasoning text is never read.
        "assistant.reasoning" => events.push(ParsedEvent::Thinking { reasoning: None }),
        // A tool started. Only the name, the call id and the file path leave.
        "tool.execution_start" => {
            let Some(tool_name) = data.get("toolName").and_then(Value::as_str) else {
                return events;
            };
            if tool_name.trim().is_empty() {
                return events;
            }
            let arguments = tool_arguments(data);
            // The command is only used to pick a fixed label and is never
            // published.
            let command = arguments
                .as_ref()
                .and_then(|args| args.get("command").or_else(|| args.get("cmd")))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let file = arguments.as_ref().and_then(extract_file_basename);
            let language = file
                .as_deref()
                .and_then(language_from_file)
                .map(str::to_string);
            let activity = ToolActivity {
                label: classify_tool(
                    canonical_tool_name(&tool_name.to_ascii_lowercase()),
                    command,
                )
                .to_string(),
                file,
                language,
            };
            match data
                .get("toolCallId")
                .and_then(Value::as_str)
                .and_then(safe_call_id)
            {
                Some(call_id) => events.push(ParsedEvent::TrackedToolStarted { call_id, activity }),
                None => events.push(ParsedEvent::ToolStarted { activity }),
            }
        }
        // A tool finished. The result and the error are never read.
        "tool.execution_complete" => {
            match data
                .get("toolCallId")
                .and_then(Value::as_str)
                .and_then(safe_call_id)
            {
                Some(call_id) => events.push(ParsedEvent::TrackedToolFinished { call_id }),
                None => events.push(ParsedEvent::ToolFinished),
            }
        }
        // The user switched the model.
        "session.model_change" => {
            if let Some(agent) = data.get("newModel").and_then(model_label) {
                events.push(ParsedEvent::AgentDetected { agent });
            }
        }
        // A sub-agent was spawned. The description is never read.
        "subagent.started" => {
            let agent = data
                .get("agentDisplayName")
                .and_then(model_label)
                .or_else(|| data.get("agentName").and_then(model_label));
            if let Some(conversation_id) = data
                .get("toolCallId")
                .and_then(Value::as_str)
                .and_then(safe_call_id)
            {
                events.push(ParsedEvent::SubagentSpawned {
                    conversation_id,
                    agent: agent.clone().unwrap_or_default(),
                });
            }
            if let Some(agent) = data.get("model").and_then(model_label).or(agent) {
                events.push(ParsedEvent::AgentDetected { agent });
            }
        }
        // The turn is over.
        "session.task_complete"
        | "assistant.turn_end"
        | "session.idle"
        | "session.shutdown"
        | "session.next.step.ended" => events.push(ParsedEvent::Completed),
        // Everything else (user messages, assistant messages, permissions,
        // hooks, deltas, ...) carries conversation text and is dropped.
        _ => {}
    }

    events
}

/// The workspace path of a session. Only the path itself is read; no other
/// context field (repository, branch, commit) is touched.
fn workspace_cwd(data: &Value) -> Option<&str> {
    let context = data.get("context");
    ["cwd", "gitRoot", "workingDirectory"]
        .into_iter()
        .find_map(|key| context.and_then(|ctx| ctx.get(key)).and_then(Value::as_str))
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
}

/// Tool arguments are an opaque JSON blob: an object in the log, or the JSON
/// text of an object.
fn tool_arguments(data: &Value) -> Option<Value> {
    match data.get("arguments")? {
        Value::Object(_) => data.get("arguments").cloned(),
        Value::String(text) => serde_json::from_str(text).ok(),
        _ => None,
    }
}

/// A tool call id may only be a short opaque token.
fn safe_call_id(value: &str) -> Option<String> {
    let id = value.trim();
    if id.is_empty() || id.len() > 64 {
        return None;
    }
    if !id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
    {
        return None;
    }
    Some(id.to_string())
}

/// A model (or agent) id may only be a short machine-like token. Anything that
/// looks like prose is dropped instead of being published.
fn model_label(value: &Value) -> Option<String> {
    let model = value.as_str()?.trim();
    if model.is_empty() || model.len() > 128 {
        return None;
    }
    if !model
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:/@".contains(&byte))
    {
        return None;
    }
    Some(model.to_string())
}

/// Copilot's built-in file tools use names the shared label table does not
/// know. Map them onto the generic names the table understands, so the label
/// stays a fixed string and never depends on the tool's own wording.
fn canonical_tool_name(name: &str) -> &str {
    match name {
        "create" | "insert" | "str_replace" | "str_replace_editor" => "edit",
        "write_agent" => "write",
        "rg" | "lexical_code_search" | "semantic_code_search" => "grep",
        _ => name,
    }
}

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

    fn line(value: serde_json::Value) -> String {
        value.to_string()
    }

    #[test]
    fn parses_session_start_tool_and_completion() {
        let start = line(serde_json::json!({
            "id": "11111111-1111-4111-8111-111111111111",
            "timestamp": "2026-10-21T02:00:00.000Z",
            "parentId": null,
            "type": "session.start",
            "data": {
                "sessionId": "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
                "version": 1,
                "producer": "copilot-agent",
                "copilotVersion": "1.0.101",
                "startTime": "2026-10-21T02:00:00.000Z",
                "selectedModel": "claude-sonnet-4.5",
                "context": { "cwd": "C:\\work\\my-repo", "branch": "main" }
            }
        }));
        assert_eq!(
            parse_line(&start),
            vec![
                ParsedEvent::SessionMetadata {
                    project: "my-repo".to_string(),
                    cwd: r"C:\work\my-repo".to_string(),
                },
                ParsedEvent::TaskStarted {
                    started_at_ms: 1_792_548_000_000
                },
                ParsedEvent::AgentDetected {
                    agent: "claude-sonnet-4.5".to_string()
                },
            ]
        );

        let tool = line(serde_json::json!({
            "id": "22222222-2222-4222-8222-222222222222",
            "timestamp": "2026-10-21T02:00:03.500Z",
            "parentId": "11111111-1111-4111-8111-111111111111",
            "type": "tool.execution_start",
            "data": {
                "toolCallId": "call_abc123",
                "toolName": "edit",
                "arguments": { "path": "C:\\work\\my-repo\\src\\main.rs" },
                "model": "claude-sonnet-4.5",
                "turnId": "1"
            }
        }));
        assert_eq!(
            parse_line(&tool),
            vec![ParsedEvent::TrackedToolStarted {
                call_id: "call_abc123".to_string(),
                activity: ToolActivity {
                    label: "Editing files".to_string(),
                    file: Some("main.rs".to_string()),
                    language: Some("Rust".to_string()),
                },
            }]
        );

        let done = line(serde_json::json!({
            "id": "33333333-3333-4333-8333-333333333333",
            "timestamp": "2026-10-21T02:00:05.000Z",
            "type": "tool.execution_complete",
            "data": { "toolCallId": "call_abc123", "success": true, "turnId": "1" }
        }));
        assert_eq!(
            parse_line(&done),
            vec![ParsedEvent::TrackedToolFinished {
                call_id: "call_abc123".to_string()
            }]
        );

        let complete = line(serde_json::json!({
            "id": "44444444-4444-4444-8444-444444444444",
            "timestamp": "2026-10-21T02:00:06.000Z",
            "type": "session.task_complete",
            "data": { "summary": "REDACTED_SUMMARY", "success": true }
        }));
        assert_eq!(parse_line(&complete), vec![ParsedEvent::Completed]);
    }

    #[test]
    fn maps_copilot_tool_names_to_fixed_labels() {
        let tool = |name: &str, arguments: serde_json::Value| {
            line(serde_json::json!({
                "timestamp": "2026-10-21T02:00:03.500Z",
                "type": "tool.execution_start",
                "data": {
                    "toolCallId": "call_1",
                    "toolName": name,
                    "arguments": arguments
                }
            }))
        };

        let create = parse_line(&tool(
            "create",
            serde_json::json!({ "path": "C:\\work\\repo\\app.ts" }),
        ));
        assert_eq!(
            create,
            vec![ParsedEvent::TrackedToolStarted {
                call_id: "call_1".to_string(),
                activity: ToolActivity {
                    label: "Editing files".to_string(),
                    file: Some("app.ts".to_string()),
                    language: Some("TypeScript".to_string()),
                },
            }]
        );

        let grep = parse_line(&tool("rg", serde_json::json!({ "query": "needle" })));
        assert_eq!(
            grep,
            vec![ParsedEvent::TrackedToolStarted {
                call_id: "call_1".to_string(),
                activity: ToolActivity {
                    label: "Searching project".to_string(),
                    file: None,
                    language: None,
                },
            }]
        );

        // A shell tool only reports the fixed label, never the command.
        let shell = parse_line(&tool(
            "local_shell",
            serde_json::json!({ "command": "cargo test --all" }),
        ));
        assert_eq!(
            shell,
            vec![ParsedEvent::TrackedToolStarted {
                call_id: "call_1".to_string(),
                activity: ToolActivity {
                    label: "Running Rust tests".to_string(),
                    file: None,
                    language: None,
                },
            }]
        );

        // Without a call id the tool is reported untracked.
        let untracked = line(serde_json::json!({
            "type": "tool.execution_start",
            "data": { "toolName": "view", "arguments": { "path": "C:\\work\\repo\\README.md" } }
        }));
        assert_eq!(
            parse_line(&untracked),
            vec![ParsedEvent::ToolStarted {
                activity: ToolActivity {
                    label: "Reading project files".to_string(),
                    file: Some("README.md".to_string()),
                    language: Some("Markdown".to_string()),
                },
            }]
        );
    }

    #[test]
    fn maps_turn_boundaries_to_completed() {
        for event_type in [
            "session.task_complete",
            "assistant.turn_end",
            "session.idle",
            "session.shutdown",
            "session.next.step.ended",
        ] {
            let record = line(serde_json::json!({
                "timestamp": "2026-10-21T02:00:09.000Z",
                "type": event_type,
                "data": { "summary": "REDACTED_SUMMARY" }
            }));
            assert_eq!(
                parse_line(&record),
                vec![ParsedEvent::Completed],
                "unexpected result for {event_type}"
            );
        }

        // Upstream marks the idle event as transient, so a persisted one is
        // dropped instead of being guessed at.
        let ephemeral = line(serde_json::json!({
            "timestamp": "2026-10-21T02:00:09.000Z",
            "type": "session.idle",
            "ephemeral": true,
            "data": {}
        }));
        assert!(parse_line(&ephemeral).is_empty());
    }

    #[test]
    fn never_forwards_conversation_text_or_full_paths() {
        let records = [
            serde_json::json!({
                "timestamp": "2026-10-21T02:00:00.000Z",
                "type": "user.message",
                "data": { "content": "PRIVATE_PROMPT_TEXT", "transformedContent": "PRIVATE_PROMPT_TEXT" }
            }),
            serde_json::json!({
                "timestamp": "2026-10-21T02:00:01.000Z",
                "type": "assistant.message",
                "data": {
                    "content": "PRIVATE_ANSWER_TEXT",
                    "reasoningText": "PRIVATE_REASONING_TEXT",
                    "phase": "response"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-10-21T02:00:02.000Z",
                "type": "assistant.reasoning",
                "data": { "reasoningId": "r1", "content": "PRIVATE_REASONING_TEXT" }
            }),
            serde_json::json!({
                "timestamp": "2026-10-21T02:00:03.000Z",
                "type": "tool.execution_start",
                "data": {
                    "toolCallId": "call_private",
                    "toolName": "local_shell",
                    "arguments": { "command": "rm -rf C:\\secret\\PRIVATE_TARGET" }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-10-21T02:00:04.000Z",
                "type": "tool.execution_complete",
                "data": {
                    "toolCallId": "call_private",
                    "success": false,
                    "result": { "content": "PRIVATE_TOOL_RESULT" },
                    "error": { "message": "PRIVATE_TOOL_RESULT" }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-10-21T02:00:05.000Z",
                "type": "session.start",
                "data": {
                    "selectedModel": "claude-sonnet-4.5",
                    "context": { "cwd": "C:\\work\\private-workspace" }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-10-21T02:00:06.000Z",
                "type": "tool.execution_start",
                "data": {
                    "toolCallId": "call_private",
                    "toolName": "edit",
                    "arguments": { "path": "C:\\work\\private-workspace\\src\\private_module.rs" }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-10-21T02:00:07.000Z",
                "type": "subagent.started",
                "data": {
                    "toolCallId": "call_sub_1",
                    "agentName": "explorer",
                    "agentDisplayName": "explorer",
                    "agentDescription": "PRIVATE_AGENT_DESCRIPTION"
                }
            }),
        ];

        for record in records {
            let events = parse_line(&line(record));
            for event in &events {
                let debug = format!("{event:?}");
                assert!(!debug.contains("PRIVATE_"), "private text leaked: {debug}");
                assert!(!debug.contains("rm -rf"), "command leaked: {debug}");
                assert!(!debug.contains("C:\\secret"), "path leaked: {debug}");
                assert!(
                    !debug.contains(r"C:\work\private-workspace\src"),
                    "directory leaked: {debug}"
                );
                assert!(
                    !debug.contains(r"C:\work\private-workspace\src\private_module.rs"),
                    "full path leaked: {debug}"
                );
            }
        }
    }

    #[test]
    fn malformed_and_unknown_records_produce_no_events() {
        for record in [
            "",
            "not json",
            "{",
            "[]",
            "null",
            r#"{"data":{}}"#,
            r#"{}"#,
            r#"{"type":"","data":{}}"#,
            r#"{"type":"unknown.event","data":{"content":"PRIVATE_TEXT"}}"#,
            r#"{"type":"user.message","data":{"content":"PRIVATE_TEXT"}}"#,
            r#"{"type":"assistant.message","data":{"content":"PRIVATE_TEXT"}}"#,
            r#"{"type":"assistant.message_delta","ephemeral":true,"data":{}}"#,
            r#"{"type":"session.start","ephemeral":true,"data":{"context":{"cwd":"C:\\work\\x"}}}"#,
            r#"{"type":"tool.execution_start","data":{"arguments":{"path":"C:\\work\\x\\a.rs"}}}"#,
            r#"{"type":"tool.execution_start","data":{"toolName":"","toolCallId":"call_1"}}"#,
        ] {
            assert!(
                parse_line(record).is_empty(),
                "expected no events for {record}"
            );
        }
    }

    #[test]
    fn reads_model_id_and_record_timestamp() {
        let turn = line(serde_json::json!({
            "id": "55555555-5555-4555-8555-555555555555",
            "timestamp": "2026-10-21T02:00:03.500Z",
            "type": "assistant.turn_start",
            "data": { "turnId": "1", "model": "gpt-5-codex" }
        }));
        assert_eq!(
            parse_line(&turn),
            vec![
                ParsedEvent::TaskStarted {
                    started_at_ms: 1_792_548_003_500
                },
                ParsedEvent::AgentDetected {
                    agent: "gpt-5-codex".to_string()
                },
            ]
        );

        let model_change = line(serde_json::json!({
            "timestamp": "2026-10-21T02:01:00.000Z",
            "type": "session.model_change",
            "data": { "previousModel": "gpt-5-codex", "newModel": "claude-sonnet-4.5" }
        }));
        assert_eq!(
            parse_line(&model_change),
            vec![ParsedEvent::AgentDetected {
                agent: "claude-sonnet-4.5".to_string()
            }]
        );

        // A model id that is prose, or too long, is dropped.
        for bad in [
            r#"{"type":"session.model_change","data":{"newModel":"PRIVATE prose text"}}"#,
            r#"{"type":"session.model_change","data":{"newModel":""}}"#,
        ] {
            assert!(parse_line(bad).is_empty());
        }
        let long = "a".repeat(129);
        let too_long = line(serde_json::json!({
            "type": "session.model_change",
            "data": { "newModel": long }
        }));
        assert!(parse_line(&too_long).is_empty());

        // An epoch-millisecond timestamp is used verbatim.
        let numeric = line(serde_json::json!({
            "timestamp": 1_792_548_003_500i64,
            "type": "assistant.turn_start",
            "data": { "turnId": "1" }
        }));
        assert_eq!(
            parse_line(&numeric),
            vec![ParsedEvent::TaskStarted {
                started_at_ms: 1_792_548_003_500
            }]
        );
    }

    #[test]
    fn reports_subagent_spawn_and_thinking_phase() {
        let spawn = line(serde_json::json!({
            "timestamp": "2026-10-21T02:02:00.000Z",
            "type": "subagent.started",
            "data": {
                "toolCallId": "call_sub_1",
                "agentName": "explorer",
                "agentDisplayName": "explorer",
                "agentDescription": "PRIVATE_AGENT_DESCRIPTION",
                "model": "claude-sonnet-4.5"
            }
        }));
        assert_eq!(
            parse_line(&spawn),
            vec![
                ParsedEvent::SubagentSpawned {
                    conversation_id: "call_sub_1".to_string(),
                    agent: "explorer".to_string(),
                },
                ParsedEvent::AgentDetected {
                    agent: "claude-sonnet-4.5".to_string()
                },
            ]
        );

        let reasoning = line(serde_json::json!({
            "timestamp": "2026-10-21T02:02:01.000Z",
            "type": "assistant.reasoning",
            "data": { "reasoningId": "r1", "content": "PRIVATE_REASONING_TEXT" }
        }));
        assert_eq!(
            parse_line(&reasoning),
            vec![ParsedEvent::Thinking { reasoning: None }]
        );
    }
}
