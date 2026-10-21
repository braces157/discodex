//! Gemini CLI adapter: session logs in ~/.gemini/tmp/<project-slug>/chats/*.jsonl
//!
//! Upstream reference: google-gemini/gemini-cli, `packages/core/src/services/`
//! `chatRecordingTypes.ts` and `chatRecordingService.ts`. A session file is
//! newline-delimited JSON that the harness appends to live with
//! `fs.appendFileSync`, so the worker can tail it line by line.
//!
//! Record grammar of one file:
//!
//! - line 1 is the session metadata record
//!   `{sessionId, projectHash, startTime, lastUpdated, kind}`. A subagent
//!   record additionally carries the absolute workspace `directories`.
//! - message records `{id, timestamp, type, ...}` where `type` is `user`,
//!   `gemini`, `info`, `error` or `warning`. A `gemini` record may carry
//!   `model`, `thoughts[]` and `toolCalls[{id, name, args, result, status}]`.
//! - patch records `{$set: ...}`, `{$patch: {id, toolCalls: [{id, result}]}}`
//!   and `{$rewindTo: ...}`.
//!
//! Only safe metadata may leave this adapter: a workspace folder name, a file
//! basename, an inferred language, a normalized activity label, a model id and
//! timing. Prompts, assistant answers, thought summaries, tool arguments, tool
//! results, raw shell commands and full paths are never forwarded.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::events::{
    ParsedEvent, ToolActivity, classify_tool, extract_file_basename, language_from_file,
    project_name, timestamp_ms,
};

/// Parse a whole session file that holds JSON lines, with the path of the file.
///
/// The project of a main session is not part of the log: the log only holds a
/// one-way hash of the workspace. The workspace lives in the ownership marker
/// that the harness writes next to the chats folder, so the path is needed to
/// find it.
pub fn parse_snapshot_at(path: &Path, text: &str, seen: &mut HashSet<String>) -> Vec<ParsedEvent> {
    let mut events = parse_snapshot(text, seen);
    if events
        .iter()
        .any(|event| matches!(event, ParsedEvent::SessionMetadata { .. }))
    {
        return events;
    }
    if !seen.insert("project".to_string()) {
        return events;
    }
    if let Some((project, cwd)) = project_root(path) {
        events.insert(0, ParsedEvent::SessionMetadata { project, cwd });
    }
    events
}

/// The workspace of a session, read from the ownership marker of the project.
fn project_root(path: &Path) -> Option<(String, String)> {
    let marker = project_root_marker(path)?;
    let contents = std::fs::read_to_string(marker).ok()?;
    let cwd = contents.trim();
    if cwd.is_empty() {
        return None;
    }
    let project = project_name(cwd)?;
    Some((project, cwd.to_string()))
}

/// The ownership marker of the project a session belongs to.
///
/// The session lives in `<tmp>/<project>/chats/<session>.jsonl` and the marker
/// in `<tmp>/<project>/.project_root`.
fn project_root_marker(path: &Path) -> Option<PathBuf> {
    let project_dir = path.parent()?.parent()?;
    Some(project_dir.join(".project_root"))
}

/// Parse a whole session file that holds JSON lines.
///
/// The harness rewrites the conversation in place: it appends a
/// `{$set: {messages: [...]}}` snapshot for every change, and newer builds
/// append one message record per line. Both shapes are handled here, and every
/// message is keyed by its id, so a snapshot never replays a conversation that
/// was already reported.
pub fn parse_snapshot(text: &str, seen: &mut HashSet<String>) -> Vec<ParsedEvent> {
    let mut events = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(record) = value.as_object() else {
            continue;
        };

        // Snapshot record: {$set: {messages: [...], lastUpdated}}.
        if let Some(set) = record.get("$set") {
            let Some(messages) = set.get("messages").and_then(Value::as_array) else {
                continue;
            };
            for message in messages {
                let Some(message) = message.as_object() else {
                    continue;
                };
                let Some(id) = message.get("id").and_then(Value::as_str) else {
                    continue;
                };
                if !seen.insert(id.to_string()) {
                    continue;
                }
                events.extend(parse_message_record(message));
            }
            continue;
        }

        // Session metadata record: {sessionId, projectHash, ...}.
        if record.get("sessionId").and_then(Value::as_str).is_some()
            && record.get("projectHash").and_then(Value::as_str).is_some()
        {
            let session_id = record
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let kind = record.get("kind").and_then(Value::as_str).unwrap_or("main");
            if !seen.insert(format!("meta:{session_id}:{kind}")) {
                continue;
            }
            events.extend(parse_session_metadata(record));
            continue;
        }

        // Tool result patch: {$patch:{id, toolCalls: [{id, result}]}}.
        if let Some(patch) = record.get("$patch") {
            let patch_id = patch
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if let Some(calls) = patch.get("toolCalls").and_then(Value::as_array) {
                for call in calls {
                    let Some(call_id) = call.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    if !seen.insert(format!("patch:{patch_id}:{call_id}")) {
                        continue;
                    }
                }
            }
            let mut patch_events = Vec::new();
            push_tool_finishes(patch, &mut patch_events);
            events.extend(patch_events);
            continue;
        }

        // Plain message record: {id, timestamp, type, ...}.
        let Some(id) = record.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !seen.insert(id.to_string()) {
            continue;
        }
        events.extend(parse_message_record(record));
    }
    events
}

/// Parse one line of the harness' append-only activity log.
///
/// Only safe metadata may leave this function: the workspace folder name, a
/// file basename, a normalized activity label, a model id and timing.
pub fn parse_line(line: &str) -> Vec<ParsedEvent> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let Some(record) = value.as_object() else {
        return Vec::new();
    };

    // Session metadata record: {sessionId, projectHash, ...}.
    if record.get("sessionId").and_then(Value::as_str).is_some()
        && record.get("projectHash").and_then(Value::as_str).is_some()
    {
        return parse_session_metadata(record);
    }

    // Tool result patch: {$patch:{id, toolCalls:[{id, result}]}}. Only the
    // call ids are read; the result payload is never inspected.
    if let Some(patch) = record.get("$patch") {
        let mut events = Vec::new();
        push_tool_finishes(patch, &mut events);
        return events;
    }

    // {$set: ...} and {$rewindTo: ...} carry no activity. A $set record can
    // hold a conversation summary, so it is never looked at.

    // Message record: {id, timestamp, type, ...}.
    if record.get("id").and_then(Value::as_str).is_none() {
        return Vec::new();
    }
    parse_message_record(record)
}

/// Handle a message record of the harness.
fn parse_message_record(record: &Map<String, Value>) -> Vec<ParsedEvent> {
    let Some(record_type) = record.get("type").and_then(Value::as_str) else {
        return Vec::new();
    };
    if record.get("id").and_then(Value::as_str).is_none() {
        return Vec::new();
    }

    let mut events = Vec::new();
    match record_type {
        "user" => {
            // The prompt itself is never read, only the turn boundary and the
            // record's own timestamp, so the elapsed timer starts correctly.
            let started_at_ms = record
                .get("timestamp")
                .and_then(timestamp_ms)
                .unwrap_or_else(now_ms);
            events.push(ParsedEvent::TaskStarted { started_at_ms });
        }
        "gemini" => parse_model_turn(record, &mut events),
        // "info", "error", "warning" and unknown types carry no activity.
        _ => {}
    }
    events
}

/// Handle the session metadata record.
///
/// A main session record carries only a one-way sha256 of the project root, so
/// the workspace cannot be recovered from the line. No `SessionMetadata` is
/// invented for it; the project has to be resolved from the sibling
/// `.project_root` marker file, which is outside of this line. A subagent
/// record does carry the absolute workspace directories.
fn parse_session_metadata(record: &Map<String, Value>) -> Vec<ParsedEvent> {
    let mut events = Vec::new();
    if record.get("kind").and_then(Value::as_str) != Some("subagent") {
        return events;
    }

    if let Some(directory) = record
        .get("directories")
        .and_then(Value::as_array)
        .and_then(|directories| directories.first())
        .and_then(Value::as_str)
        && let Some(project) = project_name(directory)
    {
        events.push(ParsedEvent::SessionMetadata {
            project,
            cwd: directory.to_string(),
        });
    }

    if let Some(session_id) = record.get("sessionId").and_then(Value::as_str)
        && let Some(conversation_id) = sanitize_id(session_id)
    {
        events.push(ParsedEvent::SubagentSpawned {
            conversation_id,
            agent: "Gemini Subagent".to_string(),
        });
    }

    events
}

/// Handle a `gemini` message record: the model turn of the harness.
fn parse_model_turn(record: &Map<String, Value>, events: &mut Vec<ParsedEvent>) {
    if let Some(model) = record.get("model").and_then(Value::as_str)
        && let Some(agent) = sanitize_model(model)
    {
        events.push(ParsedEvent::AgentDetected { agent });
    }

    let tool_calls = record
        .get("toolCalls")
        .and_then(Value::as_array)
        .filter(|calls| !calls.is_empty());

    if let Some(calls) = tool_calls {
        for call in calls {
            // A tool call without a name cannot be classified, so it is
            // skipped instead of reported as a wrong activity.
            let Some(name) = call.get("name").and_then(Value::as_str) else {
                continue;
            };
            if name.trim().is_empty() {
                continue;
            }

            let args = call.get("args").filter(|args| args.is_object());
            let activity = tool_activity(name, args);

            match call.get("id").and_then(Value::as_str).and_then(sanitize_id) {
                Some(call_id) => {
                    events.push(ParsedEvent::TrackedToolStarted {
                        call_id: call_id.clone(),
                        activity,
                    });
                    // Gemini only persists a tool call once it completed, so
                    // the start and the finish are reported together.
                    events.push(ParsedEvent::TrackedToolFinished { call_id });
                }
                None => {
                    events.push(ParsedEvent::ToolStarted { activity });
                    events.push(ParsedEvent::ToolFinished);
                }
            }
        }
        return;
    }

    // A model turn that carries text and no tool call is the final answer of
    // the turn. The text itself is never read, only its presence.
    if has_text(record.get("content")) || has_text(record.get("message")) {
        events.push(ParsedEvent::TerminalCandidate);
    } else {
        // Thought-only turns land here. The thought text stays unread.
        events.push(ParsedEvent::Thinking { reasoning: None });
    }
}

/// Report the tool calls of a `{$patch: ...}` record as finished.
fn push_tool_finishes(patch: &Value, events: &mut Vec<ParsedEvent>) {
    let Some(calls) = patch.get("toolCalls").and_then(Value::as_array) else {
        return;
    };
    for call in calls {
        if let Some(call_id) = call.get("id").and_then(Value::as_str).and_then(sanitize_id) {
            events.push(ParsedEvent::TrackedToolFinished { call_id });
        }
    }
}

/// Build the activity of a tool call from its name and its arguments.
///
/// Only the label, the file basename and the inferred language are kept. The
/// arguments' body, the tool description and the tool result are dropped.
fn tool_activity(name: &str, args: Option<&Value>) -> ToolActivity {
    let file = args.and_then(extract_file_basename);
    let language = file
        .as_deref()
        .and_then(language_from_file)
        .map(str::to_string);
    ToolActivity {
        label: tool_label(name, args),
        file,
        language,
    }
}

/// Map a Gemini tool name to a normalized activity label.
///
/// The shared classifier mislabels some Gemini names (`replace` would become
/// "Using development tools" and `grep_search` "Browsing documentation"), so
/// the Gemini table is applied first. Only the shell tool falls through to the
/// shared classifier, and it does so with the raw command so that the
/// `cargo`/`npm`/`git` special cases keep working. The command itself never
/// leaves the adapter.
fn tool_label(name: &str, args: Option<&Value>) -> String {
    let lower = name.trim().to_ascii_lowercase();
    match lower.as_str() {
        "read_file" | "read_many_files" | "list_directory" => "Reading project files".to_string(),
        "write_file" | "replace" => "Editing files".to_string(),
        "glob" | "grep_search" => "Searching project".to_string(),
        "google_web_search" | "web_fetch" => "Browsing documentation".to_string(),
        "write_todos" | "enter_plan_mode" | "exit_plan_mode" => "Planning changes".to_string(),
        "ask_user" => "Waiting for input".to_string(),
        "activate_skill" | "invoke_agent" => "Running coding agent".to_string(),
        "run_shell_command" => {
            let command = args
                .and_then(|args| args.get("command"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            classify_tool(&lower, command).to_string()
        }
        // Unknown and MCP tools collapse to a neutral label.
        _ => "Using development tools".to_string(),
    }
}

/// Check whether a record part carries text without reading the text.
fn has_text(content: Option<&Value>) -> bool {
    match content {
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(Value::Array(parts)) => parts.iter().any(|part| match part {
            Value::String(text) => !text.trim().is_empty(),
            Value::Object(_) => part
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.trim().is_empty()),
            _ => false,
        }),
        _ => false,
    }
}

/// Model ids are metadata, not prose: a narrow charset only, or drop it.
fn sanitize_model(model: &str) -> Option<String> {
    let trimmed = model.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return None;
    }
    if !trimmed
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"._:/@-".contains(&byte))
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// Session and tool call ids are opaque identifiers, never free text.
fn sanitize_id(id: &str) -> Option<String> {
    let trimmed = id.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return None;
    }
    if !trimmed
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    {
        return None;
    }
    Some(trimmed.to_string())
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

    /// A model turn that rewrote its message with a completed tool call.
    const TOOL_TURN: &str = r#"{"id":"a1b2c3d4-5e6f-4071-8293-a4b5c6d7e8f9","timestamp":"2026-06-13T05:26:15.310Z","type":"gemini","content":"REDACTED_ASSISTANT_TEXT","model":"gemini-2.5-pro","toolCalls":[{"id":"tc-1","name":"write_file","args":{"file_path":"src/main.rs","content":"REDACTED_FILE_BODY"},"result":[{"functionResponse":{"id":"tc-1","name":"write_file","response":{"output":"REDACTED_TOOL_OUTPUT"}}}],"status":"success","timestamp":"2026-06-13T05:26:15.300Z"}]}"#;

    #[test]
    fn parses_model_turn_with_tool_call_into_safe_activity() {
        let events = parse_line(TOOL_TURN);
        assert_eq!(
            events,
            vec![
                ParsedEvent::AgentDetected {
                    agent: "gemini-2.5-pro".to_string(),
                },
                ParsedEvent::TrackedToolStarted {
                    call_id: "tc-1".to_string(),
                    activity: ToolActivity {
                        label: "Editing files".to_string(),
                        file: Some("main.rs".to_string()),
                        language: Some("Rust".to_string()),
                    },
                },
                ParsedEvent::TrackedToolFinished {
                    call_id: "tc-1".to_string(),
                },
            ]
        );
    }

    #[test]
    fn maps_gemini_tool_names_to_normalized_labels() {
        let cases = [
            ("read_file", "Reading project files"),
            ("read_many_files", "Reading project files"),
            ("list_directory", "Reading project files"),
            ("write_file", "Editing files"),
            ("replace", "Editing files"),
            ("glob", "Searching project"),
            ("grep_search", "Searching project"),
            ("google_web_search", "Browsing documentation"),
            ("web_fetch", "Browsing documentation"),
            ("write_todos", "Planning changes"),
            ("enter_plan_mode", "Planning changes"),
            ("ask_user", "Waiting for input"),
            ("invoke_agent", "Running coding agent"),
            ("activate_skill", "Running coding agent"),
            ("mcp__server__discovered_tool_x", "Using development tools"),
        ];
        for (name, expected) in cases {
            let line = format!(
                r#"{{"id":"m1","type":"gemini","toolCalls":[{{"id":"tc-2","name":"{name}","args":{{}}}}]}}"#
            );
            let events = parse_line(&line);
            assert!(
                events.iter().any(|event| matches!(
                    event,
                    ParsedEvent::TrackedToolStarted { activity, .. }
                        if activity.label == expected
                )),
                "unexpected label for {name}: {events:?}"
            );
        }

        // The shell tool keeps the shared cargo/npm/git special cases.
        let shell = r#"{"id":"m2","type":"gemini","toolCalls":[{"id":"tc-3","name":"run_shell_command","args":{"command":"cargo test --release"}}]}"#;
        let events = parse_line(shell);
        assert!(events.iter().any(|event| matches!(
            event,
            ParsedEvent::TrackedToolStarted { activity, .. }
                if activity.label == "Running Rust tests"
        )));
    }

    #[test]
    fn never_forwards_prompt_answer_thoughts_results_or_paths() {
        let user = r#"{"id":"0e6f1f1e-2c31-4a0b-9d55-1a2b3c4d5e6f","timestamp":"2026-06-13T05:26:12.118Z","type":"user","content":[{"text":"REDACTED_USER_PROMPT"}]}"#;
        let answer = r#"{"id":"a1b2c3d4-5e6f-4071-8293-a4b5c6d7e8f9","timestamp":"2026-06-13T05:26:14.902Z","type":"gemini","content":"REDACTED_ASSISTANT_ANSWER","model":"gemini-2.5-pro","thoughts":[{"subject":"REDACTED_THOUGHT","description":"REDACTED_THOUGHT_BODY"}],"tokens":{"input":1234,"output":56}}"#;
        let tool = r#"{"id":"b2b2c3d4-5e6f-4071-8293-a4b5c6d7e8f9","timestamp":"2026-06-13T05:26:15.310Z","type":"gemini","content":"REDACTED_ASSISTANT_ANSWER","model":"gemini-2.5-pro","toolCalls":[{"id":"tc-9","name":"read_file","args":{"file_path":"C:\\Users\\Private\\Documents\\secret-project\\src\\main.rs"},"result":[{"functionResponse":{"response":{"output":"REDACTED_TOOL_OUTPUT"}}}],"status":"success"}]}"#;
        let shell = r#"{"id":"c3c3c3d4-5e6f-4071-8293-a4b5c6d7e8f9","type":"gemini","toolCalls":[{"id":"tc-10","name":"run_shell_command","args":{"command":"curl https://redacted-host.invalid/exfiltrate"}}]}"#;
        let patch = r#"{"$patch":{"id":"a1b2c3d4-5e6f-4071-8293-a4b5c6d7e8f9","toolCalls":[{"id":"tc-9","result":[{"functionResponse":{"id":"tc-9","response":{"output":"REDACTED_TOOL_OUTPUT"}}}]}]}}"#;
        let summary = r#"{"$set":{"lastUpdated":"2026-06-13T05:26:16.001Z","summary":"REDACTED_CONVERSATION_SUMMARY"}}"#;

        let events: Vec<ParsedEvent> = [user, answer, tool, shell, patch, summary]
            .iter()
            .flat_map(|line| parse_line(line))
            .collect();

        assert!(matches!(
            events.first(),
            Some(ParsedEvent::TaskStarted { .. })
        ));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ParsedEvent::AgentDetected { .. }))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ParsedEvent::TerminalCandidate))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ParsedEvent::TrackedToolStarted { .. }))
        );

        let debug = format!("{events:?}");
        for secret in [
            "REDACTED_USER_PROMPT",
            "REDACTED_ASSISTANT_ANSWER",
            "REDACTED_ASSISTANT_TEXT",
            "REDACTED_THOUGHT",
            "REDACTED_THOUGHT_BODY",
            "REDACTED_TOOL_OUTPUT",
            "REDACTED_FILE_BODY",
            "REDACTED_CONVERSATION_SUMMARY",
            "redacted-host.invalid",
            "exfiltrate",
            "curl",
            "Private",
            "secret-project",
            "Users",
        ] {
            assert!(!debug.contains(secret), "leaked `{secret}`: {debug}");
        }
    }

    #[test]
    fn ignores_malformed_and_unknown_records() {
        for line in [
            "",
            " ",
            "{",
            "not json",
            "[]",
            "null",
            "42",
            "{}",
            r#"{"type":"unknown","id":"x","content":"REDACTED"}"#,
            r#"{"type":"info","id":"x","message":"REDACTED"}"#,
            r#"{"type":"error","id":"x","message":"REDACTED"}"#,
            r#"{"type":"warning","id":"x","message":"REDACTED"}"#,
            r#"{"type":"user","content":[{"text":"REDACTED"}]}"#,
            r#"{"$set":{"summary":"REDACTED","lastUpdated":"2026-06-13T05:26:16.001Z"}}"#,
            r#"{"$rewindTo":"2026-06-13T05:26:16.001Z"}"#,
            r#"{"$patch":{"id":"x"}}"#,
            // A main session record carries no workspace path, so no session
            // metadata is invented for it.
            r#"{"sessionId":"b2e9ca8f-0c5a-496b-bc47-6d59f34497e8","projectHash":"3f9ac41b","startTime":"2026-06-13T05:26:11.412Z","lastUpdated":"2026-06-13T05:26:12.004Z","kind":"main"}"#,
        ] {
            assert!(parse_line(line).is_empty(), "unexpected events for {line}");
        }
    }

    #[test]
    fn detects_model_and_uses_the_record_timestamp() {
        let user = r#"{"id":"0e6f1f1e-2c31-4a0b-9d55-1a2b3c4d5e6f","timestamp":"2026-06-13T05:26:12.118Z","type":"user","content":[{"text":"REDACTED"}]}"#;
        assert_eq!(
            parse_line(user),
            vec![ParsedEvent::TaskStarted {
                started_at_ms: 1_781_328_372_118,
            }]
        );

        let answer = r#"{"id":"a1b2c3d4","timestamp":"2026-06-13T05:26:14.902Z","type":"gemini","content":"REDACTED","model":"gemini-2.5-flash"}"#;
        assert_eq!(
            parse_line(answer),
            vec![
                ParsedEvent::AgentDetected {
                    agent: "gemini-2.5-flash".to_string(),
                },
                ParsedEvent::TerminalCandidate,
            ]
        );

        // A model id that is not an id is dropped instead of being published.
        let prose = r#"{"id":"a1b2c3d4","type":"gemini","content":"REDACTED","model":"Gemini wrote a poem"}"#;
        let events = parse_line(prose);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ParsedEvent::AgentDetected { .. }))
        );

        // A tool call without a name cannot be classified and is skipped.
        let nameless = r#"{"id":"a1b2c3d4","type":"gemini","toolCalls":[{"id":"tc-1","args":{"file_path":"src/main.rs"}}]}"#;
        assert!(parse_line(nameless).is_empty());
    }

    #[test]
    fn subagent_metadata_reports_the_workspace_and_the_spawn() {
        let line = r#"{"sessionId":"b2e9ca8f-0c5a-496b-bc47-6d59f34497e8","projectHash":"3f9ac41b","startTime":"2026-06-13T05:26:11.412Z","kind":"subagent","directories":["C:\\Users\\me\\Documents\\My App"]}"#;
        assert_eq!(
            parse_line(line),
            vec![
                ParsedEvent::SessionMetadata {
                    project: "My App".to_string(),
                    cwd: r"C:\Users\me\Documents\My App".to_string(),
                },
                ParsedEvent::SubagentSpawned {
                    conversation_id: "b2e9ca8f-0c5a-496b-bc47-6d59f34497e8".to_string(),
                    agent: "Gemini Subagent".to_string(),
                },
            ]
        );
    }
}
