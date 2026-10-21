//! Junie adapter: session events in `~/.junie/sessions/<id>/events.jsonl`.
//!
//! The Junie CLI appends one JSON record per line to the session log. A
//! record is either a top level event (`UserPromptEvent`, `TaskState`, ...)
//! or a `SessionA2uxEvent` that wraps an agent block update:
//!
//! ```text
//! {"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS",
//!   "agentEvent":{"kind":"TerminalBlockUpdatedEvent","stepId":"...",
//!                 "status":"IN_PROGRESS","command":"..."}},
//!  "timestampMs":1788257704000}
//! ```
//!
//! Two traps of the format are handled here:
//!
//! - Junie replays every block of a finished task with `event.state ==
//!   "COMPLETED"`. Such a replay is ignored, otherwise every tool of the
//!   task would be announced twice and the phase would flap.
//! - The log also carries prompts, answers, terminal output, task names and
//!   the full source of the edited files. Only the workspace folder name, a
//!   file basename, an inferred language, a fixed activity label, a model id
//!   and timing leave this adapter.
//!
//! An unknown record shape yields no events at all: a renamed upstream field
//! must degrade to "no activity" and never to a wrong presence.

use serde_json::Value;

use crate::events::{
    ParsedEvent, ToolActivity, basename, classify_tool, language_from_file, project_name,
    timestamp_ms,
};

/// Parse one line of the Junie CLI activity log.
pub fn parse_line(line: &str) -> Vec<ParsedEvent> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    if !value.is_object() {
        return Vec::new();
    }
    let Some(kind) = value.get("kind").and_then(Value::as_str) else {
        return Vec::new();
    };

    match kind {
        // A new user turn starts the elapsed timer. The prompt text is read
        // but never published.
        "UserPromptEvent" => vec![ParsedEvent::TaskStarted {
            started_at_ms: record_timestamp_ms(&value).unwrap_or_else(current_time_ms),
        }],
        "TaskStartedEvent" => vec![ParsedEvent::Thinking { reasoning: None }],
        "TaskState" => task_state(&value),
        "SessionA2uxEvent" => parse_session_aux(&value),
        // "UserMessagesCommittedToHistory" and every unknown kind are ignored.
        _ => Vec::new(),
    }
}

/// The end of a task, as reported by the `TaskState` record.
fn task_state(value: &Value) -> Vec<ParsedEvent> {
    match value.get("state").and_then(Value::as_str) {
        Some("COMPLETED") | Some("FAILED") | Some("CANCELED") | Some("CANCELLED") => {
            vec![ParsedEvent::Completed]
        }
        _ => Vec::new(),
    }
}

/// Parse a `SessionA2uxEvent` and the agent block it carries.
fn parse_session_aux(value: &Value) -> Vec<ParsedEvent> {
    let Some(event) = value.get("event") else {
        return Vec::new();
    };
    // The task-end replay repeats every block of the finished task. It must
    // not fire a second time.
    if event.get("state").and_then(Value::as_str) == Some("COMPLETED") {
        return Vec::new();
    }
    let Some(agent_event) = event.get("agentEvent") else {
        return Vec::new();
    };
    let Some(kind) = agent_event.get("kind").and_then(Value::as_str) else {
        return Vec::new();
    };

    match kind {
        // The workspace of the session. Only the folder name is published,
        // the path itself stays local.
        "CurrentDirectoryUpdatedEvent" => current_directory(agent_event),
        // The model of the answer. The id is sanitized before it is used.
        "LlmResponseMetadataEvent" => model_detected(agent_event),
        "TerminalBlockUpdatedEvent" => terminal_block(agent_event),
        "ViewFilesBlockUpdatedEvent" => view_files_block(agent_event),
        "FileChangesBlockUpdatedEvent" => file_changes_block(agent_event),
        "ToolBlockUpdatedEvent" => tool_block(agent_event),
        // The prose of the agent, the compaction notice and the status line.
        // Their text stays private.
        "MarkdownBlockUpdatedEvent"
        | "ContextCompactionBlockUpdatedEvent"
        | "AgentCurrentStatusUpdatedEvent" => vec![ParsedEvent::Thinking { reasoning: None }],
        // The result of the task. Its title and text stay private.
        "ResultBlockUpdatedEvent" => vec![ParsedEvent::TerminalCandidate],
        // "AgentTaskNameUpdatedEvent" carries the user visible task name and
        // every unknown kind is ignored.
        _ => Vec::new(),
    }
}

/// The session metadata, when the record names the workspace directory.
fn current_directory(agent_event: &Value) -> Vec<ParsedEvent> {
    let Some(cwd) = agent_event
        .get("currentDirectory")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
    else {
        return Vec::new();
    };
    let Some(project) = project_name(cwd) else {
        return Vec::new();
    };
    vec![ParsedEvent::SessionMetadata {
        project,
        cwd: cwd.to_string(),
    }]
}

/// The model id of the session, when the record names one.
fn model_detected(agent_event: &Value) -> Vec<ParsedEvent> {
    let mut candidates: Vec<&str> = Vec::new();
    if let Some(usage) = agent_event.get("modelUsage").and_then(Value::as_array) {
        for entry in usage {
            if let Some(model) = entry.get("model").and_then(Value::as_str) {
                candidates.push(model);
            }
        }
    }
    if let Some(model) = agent_event.get("model").and_then(Value::as_str) {
        candidates.push(model);
    }

    candidates
        .iter()
        .find_map(|model| sanitize_model(model))
        .map(|agent| vec![ParsedEvent::AgentDetected { agent }])
        .unwrap_or_default()
}

/// Keep a model id only when it is a short, printable identifier.
///
/// Prose, newlines and control characters are dropped: the id is shown in
/// the presence and must never carry user content.
fn sanitize_model(model: &str) -> Option<String> {
    let trimmed = model.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return None;
    }
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_.:/@".contains(&b))
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// A terminal block: a command that runs in the shell.
fn terminal_block(agent_event: &Value) -> Vec<ParsedEvent> {
    // The command is only used to pick a fixed label, it never leaves here.
    let command = agent_event
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let activity = ToolActivity {
        label: classify_tool("shell", command).to_string(),
        file: None,
        language: None,
    };
    block_events(agent_event, activity)
}

/// A block that shows the files the agent reads.
fn view_files_block(agent_event: &Value) -> Vec<ParsedEvent> {
    let file = first_basename(agent_event.get("files"), &["relativePath", "path"]);
    let activity = ToolActivity {
        label: "Reading project files".to_string(),
        file: file.clone(),
        language: file
            .as_deref()
            .and_then(language_from_file)
            .map(str::to_string),
    };
    block_events(agent_event, activity)
}

/// A block that shows the files the agent changes.
fn file_changes_block(agent_event: &Value) -> Vec<ParsedEvent> {
    let file = first_basename(
        agent_event.get("changes"),
        &[
            "afterRelativePath",
            "beforeRelativePath",
            "afterPath",
            "beforePath",
        ],
    );
    let activity = ToolActivity {
        label: "Editing files".to_string(),
        file: file.clone(),
        language: file
            .as_deref()
            .and_then(language_from_file)
            .map(str::to_string),
    };
    block_events(agent_event, activity)
}

/// A block of a tool whose name Junie does not report.
///
/// The tool title of the block is agent prose and stays private.
fn tool_block(agent_event: &Value) -> Vec<ParsedEvent> {
    let activity = ToolActivity {
        label: "Using development tools".to_string(),
        file: None,
        language: None,
    };
    block_events(agent_event, activity)
}

/// The events of one agent block.
///
/// Junie updates a block in place: the same `stepId` is written again with
/// the next status. The start is published for every update so that the
/// activity and the file are visible even when only the final record was
/// logged, and the finish is published once the block reached a terminal
/// status. A block without a `stepId` cannot be paired and uses the
/// untracked variants.
fn block_events(agent_event: &Value, activity: ToolActivity) -> Vec<ParsedEvent> {
    let Some(status) = block_status(agent_event) else {
        // A renamed status value yields no activity instead of a guess.
        return Vec::new();
    };

    match step_id(agent_event) {
        Some(call_id) => {
            let mut events = vec![ParsedEvent::TrackedToolStarted {
                call_id: call_id.clone(),
                activity,
            }];
            if status == BlockStatus::Finished {
                events.push(ParsedEvent::TrackedToolFinished { call_id });
            }
            events
        }
        None => {
            let mut events = vec![ParsedEvent::ToolStarted { activity }];
            if status == BlockStatus::Finished {
                events.push(ParsedEvent::ToolFinished);
            }
            events
        }
    }
}

/// The status of an agent block.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockStatus {
    /// The block is still running.
    Running,
    /// The block reached a final status.
    Finished,
}

/// The status of a block, when it is one Junie is known to write.
fn block_status(agent_event: &Value) -> Option<BlockStatus> {
    match agent_event.get("status").and_then(Value::as_str)? {
        "IN_PROGRESS" => Some(BlockStatus::Running),
        "COMPLETED" | "FAILED" | "CANCELED" | "CANCELLED" => Some(BlockStatus::Finished),
        _ => None,
    }
}

/// The call id of a block, when it has a usable one.
fn step_id(agent_event: &Value) -> Option<String> {
    let raw = agent_event.get("stepId").and_then(Value::as_str)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return None;
    }
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// The basename of the first path of a list of path objects.
///
/// The paths of Junie are absolute, so only their last component may leave
/// the adapter.
fn first_basename(items: Option<&Value>, keys: &[&str]) -> Option<String> {
    for item in items?.as_array()? {
        for key in keys {
            if let Some(path) = item.get(key).and_then(Value::as_str)
                && let Some(name) = safe_basename(path)
            {
                return Some(name);
            }
        }
    }
    None
}

/// The last component of a path.
fn safe_basename(path: &str) -> Option<String> {
    let trimmed = path.trim().trim_matches('"');
    if trimmed.is_empty() {
        return None;
    }
    let name = basename(trimmed)?;
    if name == "." || name == ".." {
        return None;
    }
    Some(name)
}

/// The timestamp of a record, in milliseconds.
fn record_timestamp_ms(value: &Value) -> Option<i64> {
    value
        .get("timestampMs")
        .or_else(|| value.get("timestamp"))
        .or_else(|| value.get("createdAt"))
        .or_else(|| value.get("time"))
        .and_then(timestamp_ms)
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
    use super::*;

    const PROMPT: &str = r#"{"kind":"UserPromptEvent","requestId":"prompt-1","prompt":"REDACTED_PROMPT","presentablePrompt":"REDACTED_PROMPT","requiresConfirmation":true,"timestampMs":1788257701000}"#;

    const DIRECTORY: &str = r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"CurrentDirectoryUpdatedEvent","agent":{"kind":"MainAgent","id":"main","name":"main","type":"LINEAR"},"currentDirectory":"C:\\Users\\PC\\Documents\\RetryDemo"}},"taskId":"task-1","timestampMs":1788257702010}"#;

    const TERMINAL_RUN: &str = r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"TerminalBlockUpdatedEvent","stepId":"5f0c1d2e-0002","status":"IN_PROGRESS","command":"cargo test"}},"taskId":"task-1","timestampMs":1788257704000}"#;

    const TERMINAL_FAILED: &str = r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"TerminalBlockUpdatedEvent","stepId":"5f0c1d2e-0002","status":"FAILED","command":"cargo test","output":"REDACTED_OUTPUT","presentableOutput":"REDACTED_OUTPUT","outputLinesCount":3,"exitCode":1}},"taskId":"task-1","timestampMs":1788257705000}"#;

    const VIEW_FILES: &str = r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"ViewFilesBlockUpdatedEvent","stepId":"5f0c1d2e-0001","status":"COMPLETED","files":[{"relativePath":"C:\\Users\\PC\\Documents\\RetryDemo\\retry.go"}]}},"taskId":"task-1","timestampMs":1788257703020}"#;

    const FILE_CHANGES: &str = r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"FileChangesBlockUpdatedEvent","stepId":"5f0c1d2e-0003","status":"COMPLETED","changes":[{"beforeContent":{"kind":"TextFileContent","text":"REDACTED_SOURCE"},"afterContent":{"kind":"TextFileContent","text":"REDACTED_SOURCE"},"beforeRelativePath":"C:\\Users\\PC\\Documents\\RetryDemo\\retry.go","afterRelativePath":"C:\\Users\\PC\\Documents\\RetryDemo\\retry.go"}]}},"taskId":"task-1","timestampMs":1788257706010}"#;

    const MODEL: &str = r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"claude-sonnet-4-5","cost":0.0,"inputTokens":1200,"outputTokens":80,"time":0}]}},"taskId":"task-1","timestampMs":1788257705100}"#;

    const REPLAY: &str = r#"{"kind":"SessionA2uxEvent","event":{"state":"COMPLETED","agentEvent":{"kind":"TerminalBlockUpdatedEvent","stepId":"5f0c1d2e-0002","status":"FAILED","command":"cargo test","output":"REDACTED_OUTPUT"}},"taskId":"task-1","timestampMs":1788257709200}"#;

    const TASK_STATE: &str =
        r#"{"kind":"TaskState","state":"COMPLETED","timestampMs":1788257709300}"#;

    #[test]
    fn parses_turn_blocks_files_and_completion() {
        // The prompt starts the turn and the timer uses the record timestamp.
        assert_eq!(
            parse_line(PROMPT),
            vec![ParsedEvent::TaskStarted {
                started_at_ms: 1_788_257_701_000
            }]
        );

        // The workspace of the session: the folder name is published.
        assert_eq!(
            parse_line(DIRECTORY),
            vec![ParsedEvent::SessionMetadata {
                project: "RetryDemo".to_string(),
                cwd: r"C:\Users\PC\Documents\RetryDemo".to_string(),
            }]
        );

        // A running command is announced with a fixed label.
        assert_eq!(
            parse_line(TERMINAL_RUN),
            vec![ParsedEvent::TrackedToolStarted {
                call_id: "5f0c1d2e-0002".to_string(),
                activity: ToolActivity {
                    label: "Running Rust tests".to_string(),
                    file: None,
                    language: None,
                },
            }]
        );

        // A failed command closes the tracked call, its output stays private.
        // The block is written again with its final status, so the activity
        // is announced once more and the call is closed.
        assert_eq!(
            parse_line(TERMINAL_FAILED),
            vec![
                ParsedEvent::TrackedToolStarted {
                    call_id: "5f0c1d2e-0002".to_string(),
                    activity: ToolActivity {
                        label: "Running Rust tests".to_string(),
                        file: None,
                        language: None,
                    },
                },
                ParsedEvent::TrackedToolFinished {
                    call_id: "5f0c1d2e-0002".to_string(),
                },
            ]
        );

        // A read block publishes the basename and the inferred language.
        assert_eq!(
            parse_line(VIEW_FILES),
            vec![
                ParsedEvent::TrackedToolStarted {
                    call_id: "5f0c1d2e-0001".to_string(),
                    activity: ToolActivity {
                        label: "Reading project files".to_string(),
                        file: Some("retry.go".to_string()),
                        language: Some("Go".to_string()),
                    },
                },
                ParsedEvent::TrackedToolFinished {
                    call_id: "5f0c1d2e-0001".to_string(),
                },
            ]
        );

        // A change block publishes the edited basename as well.
        assert_eq!(
            parse_line(FILE_CHANGES),
            vec![
                ParsedEvent::TrackedToolStarted {
                    call_id: "5f0c1d2e-0003".to_string(),
                    activity: ToolActivity {
                        label: "Editing files".to_string(),
                        file: Some("retry.go".to_string()),
                        language: Some("Go".to_string()),
                    },
                },
                ParsedEvent::TrackedToolFinished {
                    call_id: "5f0c1d2e-0003".to_string(),
                },
            ]
        );

        // The model id is published as it is.
        assert_eq!(
            parse_line(MODEL),
            vec![ParsedEvent::AgentDetected {
                agent: "claude-sonnet-4-5".to_string(),
            }]
        );

        // The task-end replay must not fire the tools a second time.
        assert!(parse_line(REPLAY).is_empty());

        // The end of the task.
        assert_eq!(parse_line(TASK_STATE), vec![ParsedEvent::Completed]);
    }

    /// The debug form of the events without the local working directory.
    ///
    /// The `cwd` of the session metadata stays on the machine and is never
    /// published, so it is left out of the leak check.
    fn published_form(events: &[ParsedEvent]) -> String {
        events
            .iter()
            .map(|event| match event {
                ParsedEvent::SessionMetadata { project, .. } => {
                    format!("SessionMetadata {{ project: {project:?} }}")
                }
                other => format!("{other:?}"),
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }

    #[test]
    fn never_forwards_prompt_result_output_or_full_paths() {
        let records = [
            PROMPT,
            DIRECTORY,
            TERMINAL_RUN,
            TERMINAL_FAILED,
            VIEW_FILES,
            FILE_CHANGES,
            MODEL,
            REPLAY,
            TASK_STATE,
            r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"MarkdownBlockUpdatedEvent","stepId":"5f0c1d2e-0004","status":"IN_PROGRESS","text":"REDACTED_PROSE"}},"timestampMs":1788257707000}"#,
            r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"ResultBlockUpdatedEvent","stepId":"5f0c1d2e-0005","status":"COMPLETED","title":"REDACTED_TITLE","text":"REDACTED_RESULT"}},"timestampMs":1788257708000}"#,
            r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"ToolBlockUpdatedEvent","stepId":"5f0c1d2e-0006","status":"IN_PROGRESS","title":"REDACTED_TOOL_TITLE"}},"timestampMs":1788257708100}"#,
            r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"AgentTaskNameUpdatedEvent","taskName":"REDACTED_TASK_NAME"}},"timestampMs":1788257708200}"#,
            r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"TerminalBlockUpdatedEvent","stepId":"5f0c1d2e-0007","status":"IN_PROGRESS","command":"rm -rf REDACTED_BUILD_DIR"}},"timestampMs":1788257708300}"#,
            r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"ViewFilesBlockUpdatedEvent","stepId":"5f0c1d2e-0008","status":"IN_PROGRESS","files":[{"relativePath":"C:\\Users\\PC\\Documents\\SecretProject\\src\\main.rs"}]}},"timestampMs":1788257708400}"#,
            r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"FileChangesBlockUpdatedEvent","stepId":"5f0c1d2e-0009","status":"IN_PROGRESS","changes":[{"beforeContent":{"kind":"TextFileContent","text":"REDACTED_SOURCE"},"afterContent":{"kind":"TextFileContent","text":"REDACTED_SOURCE"},"beforeRelativePath":"C:\\Users\\PC\\Documents\\SecretProject\\src\\lib.rs","afterRelativePath":"C:\\Users\\PC\\Documents\\SecretProject\\src\\lib.rs"}]}},"timestampMs":1788257708500}"#,
        ];

        for record in records {
            let events = parse_line(record);
            let debug = published_form(&events);
            for secret in [
                "REDACTED_PROMPT",
                "REDACTED_PROSE",
                "REDACTED_RESULT",
                "REDACTED_TITLE",
                "REDACTED_TOOL_TITLE",
                "REDACTED_TASK_NAME",
                "REDACTED_OUTPUT",
                "REDACTED_SOURCE",
                "REDACTED_BUILD_DIR",
                "SecretProject",
                "C:\\Users\\PC",
                "src\\main.rs",
                "src\\lib.rs",
            ] {
                assert!(
                    !debug.contains(secret),
                    "{secret} leaked into {debug} for {record}"
                );
            }
        }

        // Only the folder name of the workspace is published.
        let events = parse_line(DIRECTORY);
        assert_eq!(
            events,
            vec![ParsedEvent::SessionMetadata {
                project: "RetryDemo".to_string(),
                cwd: r"C:\Users\PC\Documents\RetryDemo".to_string(),
            }]
        );
        assert_eq!(
            published_form(&events),
            "SessionMetadata { project: \"RetryDemo\" }"
        );
    }

    #[test]
    fn malformed_and_unknown_records_produce_no_events() {
        for record in [
            "",
            "{",
            "not json",
            "[]",
            "null",
            "42",
            "{}",
            r#"{"kind":"UnknownKind","prompt":"REDACTED"}"#,
            r#"{"kind":"UserMessagesCommittedToHistory","messages":["REDACTED"]}"#,
            r#"{"kind":"AgentTaskNameUpdatedEvent","taskName":"REDACTED"}"#,
            // A record without a kind is not a Junie record.
            r#"{"event":{"state":"IN_PROGRESS"}}"#,
            // A renamed wrapper field yields no activity at all.
            r#"{"kind":"SessionA2uxEvent","payload":{"agentEvent":{"kind":"TerminalBlockUpdatedEvent","stepId":"s1","status":"IN_PROGRESS","command":"cargo test"}}}"#,
            // A renamed block kind yields no activity at all.
            r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"TerminalBlock","stepId":"s1","status":"IN_PROGRESS","command":"cargo test"}}}"#,
            // A block without a status is not published.
            r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"TerminalBlockUpdatedEvent","stepId":"s1","command":"cargo test"}}}"#,
            // A renamed status value is not published either.
            r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"TerminalBlockUpdatedEvent","stepId":"s1","status":"RUNNING","command":"cargo test"}}}"#,
            // The task-end replay of every block kind is ignored.
            r#"{"kind":"SessionA2uxEvent","event":{"state":"COMPLETED","agentEvent":{"kind":"ViewFilesBlockUpdatedEvent","stepId":"s1","status":"COMPLETED","files":[{"relativePath":"C:\\secret\\main.rs"}]}}}"#,
            // The session index is not an event log: its task name is content.
            r#"{"sessionId":"s1","createdAt":1788257701000,"updatedAt":1788257709000,"projectDir":"C:\\Users\\PC\\Documents\\RetryDemo","taskName":"REDACTED_TASK_NAME"}"#,
        ] {
            assert!(
                parse_line(record).is_empty(),
                "unexpected events for {record}"
            );
        }
    }

    #[test]
    fn reads_model_ids_and_timestamps() {
        // The model id is published as it is.
        assert_eq!(
            parse_line(MODEL),
            vec![ParsedEvent::AgentDetected {
                agent: "claude-sonnet-4-5".to_string(),
            }]
        );

        // Prose, control characters and overlong ids are dropped.
        for model in [
            "bad model\ntext",
            "gpt 5 pro",
            "model with; semicolon",
            "",
            &"x".repeat(129),
        ] {
            let record = serde_json::json!({
                "kind": "SessionA2uxEvent",
                "event": {
                    "state": "IN_PROGRESS",
                    "agentEvent": {
                        "kind": "LlmResponseMetadataEvent",
                        "modelUsage": [{"model": model}],
                    },
                },
                "timestampMs": 1788257705100i64,
            })
            .to_string();
            assert!(
                !parse_line(&record)
                    .iter()
                    .any(|event| matches!(event, ParsedEvent::AgentDetected { .. })),
                "unexpected agent for {model}"
            );
        }

        // A model id with the allowed punctuation is kept.
        let record = r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"LlmResponseMetadataEvent","modelUsage":[{"model":"openai/gpt-5.1-codex"}]}},"timestampMs":1788257705100}"#;
        assert_eq!(
            parse_line(record),
            vec![ParsedEvent::AgentDetected {
                agent: "openai/gpt-5.1-codex".to_string(),
            }]
        );

        // The timestamp of a record is read in milliseconds.
        assert_eq!(
            record_timestamp_ms(&serde_json::json!({"timestampMs": 1788257701000i64})),
            Some(1_788_257_701_000)
        );
        assert_eq!(
            record_timestamp_ms(&serde_json::json!({"timestampMs": 1788257701})),
            Some(1_788_257_701_000)
        );
        assert_eq!(record_timestamp_ms(&serde_json::json!({})), None);

        // The task starts with the timestamp of its own record.
        assert_eq!(
            parse_line(PROMPT),
            vec![ParsedEvent::TaskStarted {
                started_at_ms: 1_788_257_701_000
            }]
        );
    }
}
