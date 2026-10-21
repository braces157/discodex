//! Kiro adapter: IDE and CLI transcripts under `~/.kiro/sessions`.
//!
//! Kiro writes two transcript shapes into newline delimited JSON files:
//!
//! - IDE / CLI v3 (`sessions/<workspaceKey>/sess_<uuid>/messages.jsonl`):
//!   every line carries a top level RFC3339 `timestamp` and a `payload`
//!   object with `type`, `toolName`, `args` and `status`.
//! - CLI v2 (`sessions/cli/<sessionId>.jsonl`): every line carries a `kind`
//!   string, the tool data nested in `content[].data` and the timestamp in
//!   `data.meta.timestamp` as float seconds.
//!
//! Older IDE builds wrote flat records (`role` plus `content`) into the same
//! file, so that shape is understood as well.
//!
//! Only safe metadata leaves this adapter: the workspace folder name, a file
//! basename, an inferred language, a normalized activity label, a model id
//! and timing. Prompts, answers, thought text, tool arguments, tool results
//! and full paths are never forwarded.

use serde_json::{Map, Value};

use crate::events::{
    ParsedEvent, ToolActivity, classify_tool, extract_file_basename, language_from_file,
    project_name, timestamp_ms,
};

/// Parse one line of the harness' append-only activity log.
///
/// An unknown record shape yields no events at all: a renamed upstream field
/// must degrade to "no activity" and never to a wrong presence.
pub fn parse_line(line: &str) -> Vec<ParsedEvent> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    if !value.is_object() {
        return Vec::new();
    }

    let mut events = Vec::new();

    if let Some(metadata) = session_metadata(&value) {
        events.push(metadata);
    }
    if let Some(agent) = detect_model(&value) {
        events.push(ParsedEvent::AgentDetected { agent });
    }

    if value.get("payload").is_some() {
        // IDE / CLI v3 shape.
        let Some(payload) = value.get("payload").and_then(Value::as_object) else {
            return events;
        };
        events.extend(parse_ide_payload(payload, &value));
    } else if let Some(kind) = value.get("kind").and_then(Value::as_str) {
        // CLI v2 shape.
        events.extend(parse_cli_record(kind, &value));
    } else if let Some(role) = value.get("role").and_then(Value::as_str) {
        // Flat shape of the older IDE builds.
        events.extend(parse_flat_record(role, &value));
    }

    events
}

/// The session project, when the record names a workspace.
///
/// Kiro stores the workspace in an encoded folder key, so the plain project
/// name is only known when a record names the workspace path itself.
fn session_metadata(value: &Value) -> Option<ParsedEvent> {
    let path = workspace_path(value)?;
    let project = project_name(&path)?;
    Some(ParsedEvent::SessionMetadata { project, cwd: path })
}

/// Parse the sidecar of a session.
///
/// The CLI keeps the workspace and the model of a session in a sidecar file
/// next to its transcript, because the transcript itself does not name them.
/// The sidecar is rewritten in place, so only the values that were not
/// reported before are reported.
pub fn parse_document(
    doc: &Value,
    seen: &mut std::collections::HashSet<String>,
) -> Vec<ParsedEvent> {
    let mut events = Vec::new();
    if let Some(path) = workspace_path(doc)
        && let Some(project) = project_name(&path)
        && seen.insert(format!("cwd:{path}"))
    {
        events.push(ParsedEvent::SessionMetadata { project, cwd: path });
    }
    if let Some(model) = detect_model(doc)
        && seen.insert(format!("model:{model}"))
    {
        events.push(ParsedEvent::AgentDetected { agent: model });
    }
    events
}

/// The first workspace path named by the record.
fn workspace_path(value: &Value) -> Option<String> {
    for holder in [Some(value), value.get("payload"), value.get("data")] {
        let Some(holder) = holder else {
            continue;
        };
        for key in ["cwd", "workspacePath", "workspaceDirectory"] {
            if let Some(path) = holder.get(key).and_then(Value::as_str) {
                let trimmed = path.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
        if let Some(paths) = holder.get("workspacePaths").and_then(Value::as_array)
            && let Some(path) = paths
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .find(|path| !path.is_empty())
        {
            return Some(path.to_string());
        }
    }
    None
}

/// The model id, when the record names one.
///
/// Internal placeholders such as `auto` or `qdev::auto` are dropped: they
/// would show a wrong agent in the presence.
fn detect_model(value: &Value) -> Option<String> {
    let mut candidates: Vec<&Value> = Vec::new();
    for holder in [Some(value), value.get("payload"), value.get("data")] {
        let Some(holder) = holder else {
            continue;
        };
        for key in ["model", "modelId", "model_id", "reasoningModelId"] {
            if let Some(candidate) = holder.get(key) {
                candidates.push(candidate);
            }
        }
    }
    // The CLI v2 sidecar keeps the model in the session state.
    if let Some(model) = value
        .get("session_state")
        .and_then(|state| state.get("rts_model_state"))
        .and_then(|state| state.get("model_info"))
        .and_then(|info| info.get("model_id"))
    {
        candidates.push(model);
    }

    candidates
        .iter()
        .find_map(|candidate| sanitize_model(candidate.as_str()?))
}

/// Keep a model id only when it is a short, printable identifier.
fn sanitize_model(model: &str) -> Option<String> {
    let model = model.trim();
    if model.is_empty() || model.len() > 128 {
        return None;
    }
    if !model
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_.:/@".contains(&b))
    {
        return None;
    }
    if is_internal_model(model) {
        return None;
    }
    Some(model.to_string())
}

/// True for the placeholders Kiro uses when no concrete model is selected.
fn is_internal_model(model: &str) -> bool {
    matches!(
        model.to_ascii_lowercase().as_str(),
        "auto" | "agent" | "qdev" | "qdev::auto" | "default" | "none" | "unknown"
    )
}

/// Parse the `payload` of an IDE / CLI v3 record.
fn parse_ide_payload(payload: &Map<String, Value>, root: &Value) -> Vec<ParsedEvent> {
    let Some(kind) = payload.get("type").and_then(Value::as_str) else {
        return Vec::new();
    };

    match kind {
        // A new user turn starts the elapsed timer.
        "turn_start" | "user" => {
            let started_at_ms = root
                .get("timestamp")
                .and_then(timestamp_ms)
                .or_else(|| payload.get("timestamp").and_then(timestamp_ms))
                .unwrap_or_else(current_time_ms);
            vec![ParsedEvent::TaskStarted { started_at_ms }]
        }
        "turn_end" => vec![ParsedEvent::Completed],
        // The answer text and the signed reasoning blob stay private.
        "assistant" => vec![ParsedEvent::Thinking { reasoning: None }],
        "tool_call" => parse_ide_tool_call(payload),
        "tool_result" => match payload.get("toolCallId").and_then(Value::as_str) {
            Some(call_id) => vec![ParsedEvent::TrackedToolFinished {
                call_id: call_id.to_string(),
            }],
            None => vec![ParsedEvent::ToolFinished],
        },
        _ => Vec::new(),
    }
}

/// Parse a tool call of the IDE / CLI v3 store.
///
/// The engine writes one line per action state (`awaiting_approval`,
/// `executing`, `completed`, `failed`, `denied`). Only the terminal states
/// are published, otherwise every tool would be announced twice.
fn parse_ide_tool_call(payload: &Map<String, Value>) -> Vec<ParsedEvent> {
    let failed = match payload.get("status").and_then(Value::as_str) {
        Some("completed") => false,
        Some("failed") => true,
        _ => return Vec::new(),
    };

    let Some(name) = payload
        .get("toolName")
        .or_else(|| payload.get("name"))
        .and_then(Value::as_str)
    else {
        return Vec::new();
    };
    if name.trim().is_empty() {
        return Vec::new();
    }

    let activity = kiro_activity(name, payload.get("args"));
    let call_id = payload.get("toolCallId").and_then(Value::as_str);

    let mut events = Vec::new();
    match call_id {
        Some(call_id) => events.push(ParsedEvent::TrackedToolStarted {
            call_id: call_id.to_string(),
            activity,
        }),
        None => events.push(ParsedEvent::ToolStarted { activity }),
    }
    if failed {
        // A failed tool never gets a result record, so it ends here.
        events.push(match call_id {
            Some(call_id) => ParsedEvent::TrackedToolFinished {
                call_id: call_id.to_string(),
            },
            None => ParsedEvent::ToolFinished,
        });
    }
    events
}

/// Parse a CLI v2 record (`sessions/cli/<sessionId>.jsonl`).
fn parse_cli_record(kind: &str, root: &Value) -> Vec<ParsedEvent> {
    let data = root.get("data").unwrap_or(&Value::Null);

    match kind {
        // A prompt starts the turn; its timestamp is float seconds.
        "Prompt" => {
            let started_at_ms = cli_timestamp_ms(data).unwrap_or_else(current_time_ms);
            vec![ParsedEvent::TaskStarted { started_at_ms }]
        }
        "AssistantMessage" => {
            let mut events = Vec::new();
            for part in data
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                match part.get("kind").and_then(Value::as_str) {
                    // The answer text is never published.
                    Some("text") => events.push(ParsedEvent::Thinking { reasoning: None }),
                    Some("toolUse") => {
                        let tool = part.get("data").unwrap_or(&Value::Null);
                        let Some(name) = tool.get("name").and_then(Value::as_str) else {
                            continue;
                        };
                        let activity = kiro_activity(name, tool.get("input"));
                        match tool.get("toolUseId").and_then(Value::as_str) {
                            Some(call_id) => events.push(ParsedEvent::TrackedToolStarted {
                                call_id: call_id.to_string(),
                                activity,
                            }),
                            None => events.push(ParsedEvent::ToolStarted { activity }),
                        }
                    }
                    _ => {}
                }
            }
            events
        }
        "ToolResults" | "ToolUseResults" => {
            let mut events = Vec::new();
            for part in data
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if part.get("kind").and_then(Value::as_str) != Some("toolResult") {
                    continue;
                }
                let tool = part.get("data").unwrap_or(&Value::Null);
                match tool.get("toolUseId").and_then(Value::as_str) {
                    Some(call_id) => events.push(ParsedEvent::TrackedToolFinished {
                        call_id: call_id.to_string(),
                    }),
                    None => events.push(ParsedEvent::ToolFinished),
                }
            }
            events
        }
        _ => Vec::new(),
    }
}

/// Parse a flat record of the older IDE builds (`role` plus `content`).
fn parse_flat_record(role: &str, root: &Value) -> Vec<ParsedEvent> {
    match role {
        "user" => {
            let started_at_ms = root
                .get("timestamp")
                .and_then(timestamp_ms)
                .unwrap_or_else(current_time_ms);
            vec![ParsedEvent::TaskStarted { started_at_ms }]
        }
        "assistant" => vec![ParsedEvent::TerminalCandidate],
        _ => Vec::new(),
    }
}

/// The safe activity of a Kiro tool call.
///
/// The tool name is mapped onto a fixed label. The command only refines a
/// shell label and never leaves the adapter.
fn kiro_activity(name: &str, args: Option<&Value>) -> ToolActivity {
    let lower = name.trim().to_ascii_lowercase();
    let command = args
        .and_then(|args| args.get("command"))
        .and_then(Value::as_str)
        .unwrap_or("");

    let label = match lower.as_str() {
        "execute_bash" | "execute_pwsh" | "execute_cmd" | "shell" | "run_command"
        | "run_terminal_cmd" => classify_tool("execute_bash", command),
        "fs_write" | "fs_append" | "str_replace" | "write" | "edit" | "delete_file"
        | "fs_delete" => "Editing files",
        "read_file" | "fs_read" | "read" => "Reading project files",
        // Unknown tools keep a fixed generic label.
        other => classify_tool(other, command),
    };

    let file = kiro_file_basename(args);
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

/// The file basename of a Kiro tool call.
///
/// Kiro uses camelCase keys that the shared extractor does not know and it
/// passes directories inside an `operations` list.
fn kiro_file_basename(args: Option<&Value>) -> Option<String> {
    let args = args?;
    if let Some(name) = extract_file_basename(args) {
        return Some(name);
    }

    for key in ["targetFile", "filePath", "fileName", "absolutePath"] {
        if let Some(path) = args.get(key).and_then(Value::as_str)
            && let Some(name) = basename_of(path)
        {
            return Some(name);
        }
    }

    if let Some(operations) = args.get("operations").and_then(Value::as_array) {
        for operation in operations {
            // A directory is not a file.
            if operation.get("mode").and_then(Value::as_str) == Some("Directory") {
                continue;
            }
            if let Some(path) = operation.get("path").and_then(Value::as_str)
                && let Some(name) = basename_of(path)
            {
                return Some(name);
            }
        }
    }

    None
}

/// The last path component of a path.
fn basename_of(path: &str) -> Option<String> {
    let trimmed = path.trim().trim_matches('"');
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed.replace('\\', "/");
    let name = normalized.rsplit('/').find(|part| !part.is_empty())?;
    if name == "." || name == ".." {
        return None;
    }
    Some(name.to_string())
}

/// The timestamp of a CLI v2 record.
///
/// The CLI v2 store keeps float seconds in `data.meta.timestamp`.
fn cli_timestamp_ms(data: &Value) -> Option<i64> {
    let raw = data.get("meta").and_then(|meta| meta.get("timestamp"))?;
    match raw {
        Value::Number(_) => {
            let seconds = raw.as_f64()?;
            let millis = if seconds.abs() < 100_000_000_000.0 {
                seconds * 1000.0
            } else {
                seconds
            };
            Some(millis.round() as i64)
        }
        _ => timestamp_ms(raw),
    }
}

/// The current time in milliseconds, used when a record has no timestamp.
fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;

    fn millis(rfc3339: &str) -> i64 {
        DateTime::parse_from_rfc3339(rfc3339)
            .expect("valid timestamp")
            .timestamp_millis()
    }

    #[test]
    fn ide_tool_call_maps_to_safe_activity() {
        let write = r#"{"id":"fs_write_call","timestamp":"2026-10-08T20:32:46.927Z","payload":{"type":"tool_call","toolCallId":"fs_write_call","toolName":"fs_write","args":{"path":"C:\\Users\\PC\\Documents\\SecretProject\\src\\main.rs","text":"PRIVATE_SOURCE"},"status":"completed","actionType":"write"}}"#;
        assert_eq!(
            parse_line(write),
            vec![ParsedEvent::TrackedToolStarted {
                call_id: "fs_write_call".to_string(),
                activity: ToolActivity {
                    label: "Editing files".to_string(),
                    file: Some("main.rs".to_string()),
                    language: Some("Rust".to_string()),
                },
            }]
        );

        let shell = r#"{"id":"run_command_call","timestamp":"2026-10-08T20:32:47.000Z","payload":{"type":"tool_call","toolCallId":"run_command_call","toolName":"execute_bash","args":{"command":"cargo test","run_in_background":false},"status":"completed"}}"#;
        assert_eq!(
            parse_line(shell),
            vec![ParsedEvent::TrackedToolStarted {
                call_id: "run_command_call".to_string(),
                activity: ToolActivity {
                    label: "Running Rust tests".to_string(),
                    file: None,
                    language: None,
                },
            }]
        );

        let turn = r#"{"id":"turn-1","timestamp":"2026-10-08T20:32:40.000Z","payload":{"type":"turn_start"}}"#;
        assert_eq!(
            parse_line(turn),
            vec![ParsedEvent::TaskStarted {
                started_at_ms: millis("2026-10-08T20:32:40.000Z"),
            }]
        );

        let end = r#"{"id":"turn-end","timestamp":"2026-10-08T20:32:48.809Z","payload":{"type":"turn_end","stopReason":"end_turn"}}"#;
        assert_eq!(parse_line(end), vec![ParsedEvent::Completed]);

        // The result of the tool closes the tracked call.
        let result = r#"{"id":"fs_write_call-result","timestamp":"2026-10-08T20:32:48.100Z","payload":{"type":"tool_result","toolCallId":"fs_write_call","content":"PRIVATE_OUTPUT","success":true}}"#;
        assert_eq!(
            parse_line(result),
            vec![ParsedEvent::TrackedToolFinished {
                call_id: "fs_write_call".to_string(),
            }]
        );
    }

    #[test]
    fn never_forwards_prompts_answers_results_or_paths() {
        let records = [
            r#"{"timestamp":"2026-10-08T20:32:40.000Z","payload":{"type":"user","content":"PRIVATE_PROMPT"}}"#,
            r#"{"timestamp":"2026-10-08T20:32:41.000Z","payload":{"type":"assistant","operationType":"Say","content":"PRIVATE_ANSWER"}}"#,
            r#"{"timestamp":"2026-10-08T20:32:42.000Z","payload":{"type":"assistant","operationType":"Reasoning","content":"PRIVATE_THOUGHT","reasoningSignature":"PRIVATE_SIGNATURE","reasoningModelId":"qdev::auto"}}"#,
            r#"{"timestamp":"2026-10-08T20:32:46.927Z","payload":{"type":"tool_call","toolCallId":"call_1","toolName":"fs_write","args":{"path":"C:\\Users\\PC\\Documents\\SecretProject\\src\\main.rs","text":"PRIVATE_SOURCE"},"status":"completed"}}"#,
            r#"{"timestamp":"2026-10-08T20:32:48.100Z","payload":{"type":"tool_result","toolCallId":"call_1","content":"PRIVATE_TOOL_RESULT","success":true}}"#,
            r#"{"version":"v1","kind":"AssistantMessage","data":{"message_id":"m1","content":[{"kind":"text","data":"PRIVATE_CLI_ANSWER"}],"meta":{"timestamp":1785600018.11}}}"#,
            r#"{"version":"v1","kind":"ToolResults","data":{"message_id":"m2","content":[{"kind":"toolResult","data":{"toolUseId":"t1","content":[{"kind":"json","data":{"stdout":"PRIVATE_STDOUT"}}],"status":"success"}}]}}"#,
        ];

        for record in records {
            let events = parse_line(record);
            let debug = format!("{events:?}");
            for secret in [
                "PRIVATE_PROMPT",
                "PRIVATE_ANSWER",
                "PRIVATE_THOUGHT",
                "PRIVATE_SIGNATURE",
                "PRIVATE_SOURCE",
                "PRIVATE_TOOL_RESULT",
                "PRIVATE_CLI_ANSWER",
                "PRIVATE_STDOUT",
                "SecretProject",
                "C:\\Users\\PC",
            ] {
                assert!(
                    !debug.contains(secret),
                    "{secret} leaked into {debug} for {record}"
                );
            }
        }
    }

    #[test]
    fn malformed_and_unknown_records_produce_no_events() {
        for record in [
            "{",
            "not json",
            "[]",
            "null",
            "{}",
            r#"{"payload":{"type":"unknown_kind"}}"#,
            r#"{"payload":{"type":"session_start","content":"PRIVATE_SYSTEM_PROMPT"}}"#,
            r#"{"payload":{"type":"pending_interaction","content":"PRIVATE_APPROVAL"}}"#,
            r#"{"payload":{"type":"interaction_resolved","content":"PRIVATE_APPROVAL"}}"#,
            r#"{"payload":{"type":"session_metadata","contextUsage":{"PRIVATE":1}}}"#,
            r#"{"payload":{"type":"usage_summary","elapsedTime":39305,"status":"PRIVATE"}}"#,
            r#"{"payload":{"type":"session_event","category":"session_pause"}}"#,
            r#"{"kind":"UnknownKind","data":{"content":"PRIVATE"}}"#,
            r#"{"role":"system","content":"PRIVATE"}"#,
            // The engine writes one line per action state: only the terminal
            // states may be published.
            r#"{"payload":{"type":"tool_call","toolName":"execute_bash","args":{"command":"echo PRIVATE"},"status":"awaiting_approval"}}"#,
            r#"{"payload":{"type":"tool_call","toolName":"execute_bash","args":{"command":"echo PRIVATE"},"status":"executing"}}"#,
            r#"{"payload":{"type":"tool_call","toolName":"execute_bash","args":{"command":"echo PRIVATE"},"status":"denied"}}"#,
            // A renamed tool name yields no activity at all.
            r#"{"payload":{"type":"tool_call","args":{"path":"C:\\Users\\PC\\Documents\\SecretProject\\src\\main.rs"},"status":"completed"}}"#,
        ] {
            assert!(
                parse_line(record).is_empty(),
                "unexpected events for {record}"
            );
        }
    }

    #[test]
    fn reads_model_ids_and_timestamps() {
        let line = r#"{"timestamp":"2026-10-08T20:32:40.000Z","model":"big-pickle","payload":{"type":"user","content":"PRIVATE"}}"#;
        let events = parse_line(line);
        assert_eq!(
            events,
            vec![
                ParsedEvent::AgentDetected {
                    agent: "big-pickle".to_string(),
                },
                ParsedEvent::TaskStarted {
                    started_at_ms: millis("2026-10-08T20:32:40.000Z"),
                },
            ]
        );

        // Internal placeholders and prose are dropped.
        for model in ["auto", "qdev::auto", "agent", "a model with spaces", ""] {
            let record =
                serde_json::json!({"model": model, "payload": {"type": "user"}}).to_string();
            let events = parse_line(&record);
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, ParsedEvent::AgentDetected { .. })),
                "unexpected agent for {model}"
            );
        }

        // The CLI v2 store keeps float seconds in `data.meta.timestamp`.
        let prompt = r#"{"version":"v1","kind":"Prompt","data":{"message_id":"p1","content":[{"kind":"text","data":"PRIVATE"}],"meta":{"timestamp":1785600005.42}}}"#;
        assert_eq!(
            parse_line(prompt),
            vec![ParsedEvent::TaskStarted {
                started_at_ms: 1_785_600_005_420,
            }]
        );
    }

    #[test]
    fn sidecar_reports_the_workspace_and_the_model() {
        let sidecar = serde_json::json!({
            "session_id": "s1",
            "cwd": "C:\\Users\\PC\\Documents\\Project",
            "created_at": "2026-05-14T03:13:26.529260300Z",
            "title": null,
            "session_state": {
                "rts_model_state": {
                    "model_info": {"model_id": "claude-sonnet-4-5", "model_name": "Claude Sonnet 4.5"}
                }
            }
        });
        let mut seen = std::collections::HashSet::new();
        let events = parse_document(&sidecar, &mut seen);
        assert!(events.contains(&ParsedEvent::SessionMetadata {
            project: "Project".to_string(),
            cwd: "C:\\Users\\PC\\Documents\\Project".to_string(),
        }));
        assert!(events.contains(&ParsedEvent::AgentDetected {
            agent: "claude-sonnet-4-5".to_string(),
        }));

        // The sidecar is rewritten in place, so the same values are not
        // reported twice.
        let again = parse_document(&sidecar, &mut seen);
        assert!(again.is_empty());

        // A sidecar without a workspace or a model reports nothing.
        let mut empty_seen = std::collections::HashSet::new();
        assert!(
            parse_document(&serde_json::json!({"session_id": "s2"}), &mut empty_seen).is_empty()
        );
    }

    #[test]
    fn cli_v2_tracks_tool_use_and_results() {
        let assistant = r#"{"version":"v1","kind":"AssistantMessage","data":{"message_id":"m2","content":[{"kind":"text","data":"PRIVATE_ANSWER"},{"kind":"toolUse","data":{"toolUseId":"t1","name":"shell","input":{"command":"go test ./retry"}}}]}}"#;
        assert_eq!(
            parse_line(assistant),
            vec![
                ParsedEvent::Thinking { reasoning: None },
                ParsedEvent::TrackedToolStarted {
                    call_id: "t1".to_string(),
                    activity: ToolActivity {
                        label: "Running command".to_string(),
                        file: None,
                        language: None,
                    },
                },
            ]
        );

        let results = r#"{"version":"v1","kind":"ToolResults","data":{"message_id":"m3","content":[{"kind":"toolResult","data":{"toolUseId":"t1","content":[{"kind":"json","data":{"exit_status":"exit status: 0","stdout":"ok retry 0.01s","stderr":""}}],"status":"success"}}]}}"#;
        assert_eq!(
            parse_line(results),
            vec![ParsedEvent::TrackedToolFinished {
                call_id: "t1".to_string(),
            }]
        );

        let write = r#"{"version":"v1","kind":"AssistantMessage","data":{"message_id":"m4","content":[{"kind":"toolUse","data":{"toolUseId":"t2","name":"write","input":{"command":"create","path":"/tmp/proj/retry.go","content":"PRIVATE"}}}]}}"#;
        assert_eq!(
            parse_line(write),
            vec![ParsedEvent::TrackedToolStarted {
                call_id: "t2".to_string(),
                activity: ToolActivity {
                    label: "Editing files".to_string(),
                    file: Some("retry.go".to_string()),
                    language: Some("Go".to_string()),
                },
            }]
        );

        // A directory in the operation list is not a file.
        let read = r#"{"version":"v1","kind":"AssistantMessage","data":{"message_id":"m5","content":[{"kind":"toolUse","data":{"toolUseId":"t3","name":"read","input":{"operations":[{"mode":"Directory","path":"/tmp/proj"},{"mode":"File","path":"/tmp/proj/main.go"}]}}}]}}"#;
        assert_eq!(
            parse_line(read),
            vec![ParsedEvent::TrackedToolStarted {
                call_id: "t3".to_string(),
                activity: ToolActivity {
                    label: "Reading project files".to_string(),
                    file: Some("main.go".to_string()),
                    language: Some("Go".to_string()),
                },
            }]
        );
    }

    #[test]
    fn reads_the_workspace_from_a_session_document() {
        let sidecar = r#"{"schemaVersion":"1.0.0","id":"sess_1","title":"PRIVATE_TITLE","agentMode":"vibe","modelId":"auto","workspacePaths":["C:\\Users\\PC\\Documents\\KiroWorkspace"],"rootPaths":["C:\\Users\\PC\\Documents\\KiroWorkspace"],"createdAt":"2026-10-08T20:32:08.850Z","status":"idle"}"#;
        let events = parse_line(sidecar);
        assert_eq!(
            events,
            vec![ParsedEvent::SessionMetadata {
                project: "KiroWorkspace".to_string(),
                cwd: r"C:\Users\PC\Documents\KiroWorkspace".to_string(),
            }]
        );
        assert!(!format!("{events:?}").contains("PRIVATE_TITLE"));
    }
}
