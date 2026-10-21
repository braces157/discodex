//! Amp adapter: the JSON lines log of a thread
//!
//! Sourcegraph's Amp CLI writes one redacted diagnostic log per thread into
//! `%LOCALAPPDATA%\amp\logs\threads\<threadId>.log`. Every line is a flat
//! JSON object that LogTape produced with flattened properties, so the
//! record keys (`type`, `toolName`, `role`, `state`, ...) sit next to
//! `@timestamp`, `level`, `message` and `logger`.
//!
//! The log is redacted on purpose. The redaction layer keeps the tool NAME
//! but drops the tool arguments, and no record carries a workspace path, a
//! model id or a file path. A tool lease is therefore the only tool signal
//! of the local log, and the tool name is all this adapter may use.
//!
//! Only a normalized activity label, a phase and timing leave this module.
//! Prompts, answers, tool arguments, tool results, file names and paths are
//! never forwarded, and a record shape that is not known yields no event at
//! all instead of a guess.
//!
//! The adapter is stateless: it is called once per line and never counts
//! records, so the "first record of the file" proxy of the research report
//! cannot be used here. A new user turn is detected from the transcript
//! record instead.

use chrono::DateTime;
use serde_json::Value;

use crate::events::{ParsedEvent, ToolActivity, classify_tool};

/// The longest tool name that is accepted.
const MAX_TOOL_NAME_LEN: usize = 64;

/// Parse one line of the harness' per-thread activity log.
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

    match record_kind(&value) {
        Some("tool_lease") => tool_lease_events(&value),
        Some("delta") => delta_events(&value),
        Some("message_added") => message_added_events(&value),
        Some("compaction_started") => vec![ParsedEvent::TerminalCandidate],
        Some("compaction_complete") => vec![ParsedEvent::Completed],
        _ => Vec::new(),
    }
}

/// The kind of a record.
///
/// Amp writes the server message kind into the flattened `type` property.
/// `kind` is accepted as an alias so that a renamed field degrades to "no
/// activity" instead of to a wrong presence.
fn record_kind(value: &Value) -> Option<&str> {
    value
        .get("type")
        .or_else(|| value.get("kind"))
        .and_then(Value::as_str)
}

/// A server tool lease: a tool of the agent is running.
///
/// The log keeps the tool name but not the arguments, so the activity has no
/// file and no language. A lease that the CLI sent to the server is not a
/// running tool and is ignored, and a tool name that is not a plain name is
/// dropped instead of being classified.
fn tool_lease_events(value: &Value) -> Vec<ParsedEvent> {
    if let Some(direction) = value.get("direction").and_then(Value::as_str)
        && direction != "receive"
    {
        return Vec::new();
    }
    let Some(name) = value.get("toolName").and_then(Value::as_str) else {
        return Vec::new();
    };
    if !is_safe_tool_name(name) {
        return Vec::new();
    }

    let label = classify_tool(&name.to_ascii_lowercase(), "");
    vec![
        ParsedEvent::ToolStarted {
            activity: ToolActivity {
                label: label.to_string(),
                // Amp redacts the tool arguments, so no file is known.
                file: None,
                language: None,
            },
        },
        ParsedEvent::ToolFinished,
    ]
}

/// An assistant delta while the model is generating.
///
/// The delta text is never part of the redacted log, so the reasoning stays
/// empty.
fn delta_events(value: &Value) -> Vec<ParsedEvent> {
    let role = value.get("role").and_then(Value::as_str);
    let state = value.get("state").and_then(Value::as_str);
    if role == Some("assistant") && state == Some("generating") {
        return vec![ParsedEvent::Thinking { reasoning: None }];
    }
    Vec::new()
}

/// A new message in the thread transcript.
///
/// A user message is a new user turn and starts the elapsed timer. A user
/// message that carries a tool result is the answer of a tool and not a turn
/// of the user, so it does not start a task.
fn message_added_events(value: &Value) -> Vec<ParsedEvent> {
    if value.get("role").and_then(Value::as_str) != Some("user") {
        return Vec::new();
    }
    if block_types(value).contains(&"tool_result") {
        return Vec::new();
    }
    vec![ParsedEvent::TaskStarted {
        started_at_ms: record_timestamp_ms(value).unwrap_or_else(current_time_ms),
    }]
}

/// The block types of a transcript message.
fn block_types(value: &Value) -> Vec<&str> {
    value
        .get("blockTypes")
        .and_then(Value::as_array)
        .map(|blocks| blocks.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// The millisecond timestamp of a record.
///
/// Every Amp record carries an ISO-8601 timestamp with milliseconds and a
/// `Z` suffix.
fn record_timestamp_ms(value: &Value) -> Option<i64> {
    let text = value
        .get("@timestamp")
        .or_else(|| value.get("timestamp"))
        .and_then(Value::as_str)?;
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.timestamp_millis())
}

/// Whether a tool name is a plain name that may be classified.
///
/// Only `[A-Za-z0-9._-]` and at most 64 characters are allowed, so a name
/// that carries a path, a space or control characters is dropped.
fn is_safe_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_TOOL_NAME_LEN
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
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

    /// A redacted tool lease of a local Amp thread.
    fn tool_lease(tool_name: &str) -> String {
        format!(
            r#"{{"@timestamp":"2026-10-21T01:23:45.678Z","level":"INFO","message":"websocket message","logger":"amp.cli.thread-client","threadId":"T-f9941a55-3765-421e-972f-05dc1138c3a3","type":"tool_lease","toolName":"{tool_name}","toolCallId":"toolu_019cyniPYrSgaJitUSMyxyNV","parentToolCallId":null,"direction":"receive","clientID":"cli-7f2"}}"#
        )
    }

    #[test]
    fn tool_lease_maps_to_the_normalized_activity() {
        let events = parse_line(&tool_lease("edit_file"));
        assert_eq!(
            events,
            vec![
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label: "Editing files".to_string(),
                        file: None,
                        language: None,
                    },
                },
                ParsedEvent::ToolFinished,
            ]
        );

        // The wire names of Amp are mapped by the shared classifier, the tool
        // name is never used as a label of its own.
        for (tool, label) in [
            ("Read", "Reading project files"),
            ("edit_file", "Editing files"),
            ("apply_patch", "Editing files"),
            ("Bash", "Running command"),
            ("shell_command", "Running command"),
            ("Grep", "Searching project"),
            ("glob", "Searching project"),
            ("web_search", "Browsing documentation"),
            ("read_web_page", "Browsing documentation"),
            ("Task", "Running coding agent"),
            ("todo_write", "Planning changes"),
            ("find_thread", "Searching project"),
            ("create_file", "Using development tools"),
            ("oracle", "Using development tools"),
        ] {
            let events = parse_line(&tool_lease(tool));
            assert_eq!(
                events,
                vec![
                    ParsedEvent::ToolStarted {
                        activity: ToolActivity {
                            label: label.to_string(),
                            file: None,
                            language: None,
                        },
                    },
                    ParsedEvent::ToolFinished,
                ],
                "tool {tool}"
            );
        }
    }

    #[test]
    fn redacted_tool_arguments_never_leave_the_adapter() {
        // A record that carries a tool argument, a file path and message
        // text must not forward any of it.
        let line = r#"{"@timestamp":"2026-10-21T01:23:45.678Z","level":"INFO","message":"PRIVATE_PROMPT_TEXT","logger":"amp.cli.thread-client","threadId":"T-f9941a55-3765-421e-972f-05dc1138c3a3","type":"tool_lease","toolName":"edit_file","toolCallId":"toolu_1","parentToolCallId":null,"direction":"receive","clientID":"cli-7f2","input":{"path":"C:\\Users\\PC\\Documents\\Discodex\\src\\main.rs"},"arguments":{"file_path":"C:\\Users\\PC\\Documents\\Discodex\\src\\main.rs","content":"PRIVATE_CONTENT"},"result":"PRIVATE_OUTPUT","text":"PRIVATE_RESPONSE","cwd":"C:\\Users\\PC\\Documents\\Discodex"}"#;
        let events = parse_line(line);
        let debug = format!("{events:?}");

        for secret in [
            "PRIVATE_PROMPT_TEXT",
            "PRIVATE_CONTENT",
            "PRIVATE_OUTPUT",
            "PRIVATE_RESPONSE",
            "Discodex",
            "main.rs",
            "C:",
            "Users",
            "edit_file",
            "toolu_1",
            "T-f9941a55",
        ] {
            assert!(!debug.contains(secret), "{secret} leaked into {debug}");
        }

        // Only the normalized label and the tool lifecycle leave the adapter.
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0],
            ParsedEvent::ToolStarted {
                activity: ToolActivity {
                    label: "Editing files".to_string(),
                    file: None,
                    language: None,
                },
            }
        );
        assert_eq!(events[1], ParsedEvent::ToolFinished);
    }

    #[test]
    fn malformed_and_unknown_records_produce_no_events() {
        for line in [
            "",
            "{",
            "not json",
            "null",
            "[]",
            r#"{"@timestamp":"2026-10-21T01:23:45.678Z"}"#,
            r#"{"type":"totally_unknown_record","threadId":"T-1"}"#,
            r#"{"type":"message_added","role":"assistant","blockTypes":["text"]}"#,
            r#"{"type":"agent_state","agentMode":"fast","state":"running"}"#,
            r#"{"type":"tool_lease"}"#,
            r#"{"type":"tool_lease","toolName":""}"#,
            r#"{"type":"tool_lease","toolName":"../../etc/passwd"}"#,
            r#"{"type":"tool_lease","toolName":"edit file"}"#,
            r#"{"type":"tool_lease","toolName":"edit_file; rm -rf"}"#,
            r#"{"type":"tool_lease","toolName":"toolu_019cyniPYrSgaJitUSMyxyNVtoolu_019cyniPYrSgaJitUSMyxyNVtoolu"}"#,
            // A lease that the CLI sent to the server is not a running tool.
            r#"{"type":"tool_lease","toolName":"edit_file","direction":"send"}"#,
            // Only an assistant delta in the generating state is a phase.
            r#"{"type":"delta","role":"user","state":"generating"}"#,
            r#"{"type":"delta","role":"assistant","state":"idle"}"#,
        ] {
            assert!(
                parse_line(line).is_empty(),
                "record produced events: {line}"
            );
        }
    }

    #[test]
    fn phase_records_map_to_the_expected_events() {
        let generating = r#"{"@timestamp":"2026-10-21T01:23:47.115Z","level":"INFO","message":"websocket message","logger":"amp.cli.thread-client","threadId":"T-f9941a55-3765-421e-972f-05dc1138c3a3","type":"delta","role":"assistant","state":"generating"}"#;
        assert_eq!(
            parse_line(generating),
            vec![ParsedEvent::Thinking { reasoning: None }]
        );

        let compaction_started = r#"{"@timestamp":"2026-10-21T01:24:00.000Z","level":"INFO","message":"compaction started","logger":"amp.cli.thread-client","threadId":"T-f9941a55-3765-421e-972f-05dc1138c3a3","type":"compaction_started"}"#;
        assert_eq!(
            parse_line(compaction_started),
            vec![ParsedEvent::TerminalCandidate]
        );

        let compaction_complete = r#"{"@timestamp":"2026-10-21T01:24:02.000Z","level":"INFO","message":"compaction complete","logger":"amp.cli.thread-client","threadId":"T-f9941a55-3765-421e-972f-05dc1138c3a3","type":"compaction_complete"}"#;
        assert_eq!(
            parse_line(compaction_complete),
            vec![ParsedEvent::Completed]
        );
    }

    #[test]
    fn a_new_user_turn_starts_the_task_timer() {
        let user_message = r#"{"@timestamp":"2026-10-21T01:23:46.902Z","level":"INFO","message":"websocket message","logger":"amp.cli.thread-client","threadId":"T-f9941a55-3765-421e-972f-05dc1138c3a3","type":"message_added","messageId":"m_01","role":"user","blockCount":1,"blockTypes":["text"],"blockStates":["complete"],"messageState":"done","hasUsage":false}"#;
        assert_eq!(
            parse_line(user_message),
            vec![ParsedEvent::TaskStarted {
                started_at_ms: 1_792_545_826_902
            }]
        );

        // The answer of a tool is delivered as a user message and is not a
        // new turn of the user.
        let tool_result = r#"{"@timestamp":"2026-10-21T01:23:50.000Z","level":"INFO","message":"websocket message","logger":"amp.cli.thread-client","threadId":"T-f9941a55-3765-421e-972f-05dc1138c3a3","type":"message_added","messageId":"m_02","role":"user","blockCount":1,"blockTypes":["tool_result"],"blockStates":["complete"],"messageState":"done","hasUsage":false}"#;
        assert!(parse_line(tool_result).is_empty());
    }
}
