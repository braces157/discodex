//! Windsurf adapter: Cascade transcripts in `~/.windsurf/transcripts/*.jsonl`.
//!
//! Cascade keeps one conversation per file and writes one JSON object per step
//! into it:
//!
//! ```text
//! {"type":"<step type>","status":"<done|canceled|error>","<payload>":{...}}
//! ```
//!
//! The payload key is *not* derived from the step type: upstream keeps the
//! payload key `grep_search` for the step type `grep_search_v2`. The payload
//! is the object under the key that is neither `type` nor `status`, and its
//! inner key names are not verified upstream either, so a payload is only
//! searched for a path-like key.
//!
//! The transcript carries neither a timestamp nor a model id nor the workspace
//! path today, so the elapsed timer falls back to the instant the line was
//! observed and neither a project nor an agent is announced. Only the step
//! type, a file basename, an inferred language and a fixed activity label leave
//! this adapter: prompts, answers, thought text, tool arguments, tool results,
//! file contents and full paths are never forwarded.

use serde_json::{Map, Value};

use crate::events::{
    ParsedEvent, ToolActivity, classify_tool, extract_file_basename, language_from_file,
    timestamp_ms,
};

/// The kind of a transcript step.
enum Step {
    /// A new user turn starts the elapsed timer.
    UserTurn,
    /// The planner produced a message.
    Planner,
    /// A tool step with its normalized label.
    ///
    /// `file` marks the steps that work on a single file: only those may
    /// publish the basename of a path. The path of a search or a directory
    /// step names a folder, and a folder name is not published as a file.
    Tool { label: &'static str, file: bool },
}

/// Parse one line of the harness' append-only activity log.
///
/// An unknown record shape yields no events at all: a renamed upstream field
/// must degrade to "no activity" and never to a wrong presence.
pub fn parse_line(line: &str) -> Vec<ParsedEvent> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let Some(record) = value.as_object() else {
        return Vec::new();
    };

    let Some(step_type) = record.get("type").and_then(Value::as_str) else {
        return Vec::new();
    };
    // An unknown step type is an unknown record shape: nothing is published.
    let Some(step) = classify_step(step_type.trim()) else {
        return Vec::new();
    };

    let mut events = Vec::new();

    // The transcript does not name a model today. A build that adds one at the
    // top level is picked up, a renamed field simply yields no agent.
    if let Some(agent) = record
        .get("model_name")
        .or_else(|| record.get("model"))
        .and_then(Value::as_str)
        .and_then(sanitize_model)
    {
        events.push(ParsedEvent::AgentDetected { agent });
    }

    match step {
        // The prompt itself is never read, only the turn boundary.
        Step::UserTurn => {
            let started_at_ms = record_timestamp_ms(record).unwrap_or_else(now_ms);
            events.push(ParsedEvent::TaskStarted { started_at_ms });
        }
        // The answer and the thinking of the planner stay unread: only the
        // fact that the model produced a message is published.
        Step::Planner => events.push(ParsedEvent::Thinking { reasoning: None }),
        Step::Tool { label, file } => {
            // Only the steps that work on a single file publish a basename.
            let file = if file {
                step_file_basename(record)
            } else {
                None
            };
            let language = file
                .as_deref()
                .and_then(language_from_file)
                .map(str::to_string);
            events.push(ParsedEvent::ToolStarted {
                activity: ToolActivity {
                    label: label.to_string(),
                    file,
                    language,
                },
            });
            // The transcript persists a step once it is over, so the start and
            // the finish of the tool are reported together.
            if step_is_finished(record.get("status").and_then(Value::as_str)) {
                events.push(ParsedEvent::ToolFinished);
            }
        }
    }

    events
}

/// Map a step type onto the event it produces.
///
/// Only the step types that Cascade writes are known. Everything else is an
/// unknown shape and yields no activity at all.
fn classify_step(step_type: &str) -> Option<Step> {
    match step_type {
        "user_input" => Some(Step::UserTurn),
        "planner_response" => Some(Step::Planner),
        // The shared classifier would call a code action a command and a grep
        // a web search, so the ambiguous names are mapped explicitly.
        "run_command" => Some(Step::Tool {
            label: classify_tool("run_command", ""),
            file: false,
        }),
        "code_action" | "write_to_file" => Some(Step::Tool {
            label: "Editing files",
            file: true,
        }),
        "view_file" | "view_content_chunk" => Some(Step::Tool {
            label: classify_tool("view_file", ""),
            file: true,
        }),
        "list_directory" => Some(Step::Tool {
            label: classify_tool("list_directory", ""),
            file: false,
        }),
        "grep_search" | "grep_search_v2" | "find" => Some(Step::Tool {
            label: "Searching project",
            file: false,
        }),
        "todo_list" => Some(Step::Tool {
            label: "Planning changes",
            file: false,
        }),
        "mcp_tool" => Some(Step::Tool {
            label: "Using MCP tool",
            file: false,
        }),
        "read_url_content" | "search_web" => Some(Step::Tool {
            label: "Browsing documentation",
            file: false,
        }),
        _ => None,
    }
}

/// The basename of the file a step works on.
///
/// The payload key of a step is unknown, so every object of the record is
/// searched for a path-like key. Only the basename of the path is kept.
fn step_file_basename(record: &Map<String, Value>) -> Option<String> {
    for (key, value) in record {
        if key.as_str() == "type" || key.as_str() == "status" {
            continue;
        }
        if !value.is_object() {
            continue;
        }
        if let Some(name) = extract_file_basename(value).and_then(safe_basename) {
            return Some(name);
        }
        if let Some(name) = extra_path_basename(value) {
            return Some(name);
        }
    }
    None
}

/// The basename of a path under a key the shared extractor does not know.
fn extra_path_basename(value: &Value) -> Option<String> {
    for key in [
        "filePath",
        "file_name",
        "fileName",
        "absolute_path",
        "absolutePath",
        "target_file",
        "targetFile",
    ] {
        if let Some(path) = value.get(key).and_then(Value::as_str)
            && let Some(name) = basename_of(path)
        {
            return Some(name);
        }
    }
    None
}

/// The last component of a path.
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
    safe_basename(name.to_string())
}

/// Reject a basename that cannot be a file name.
fn safe_basename(name: String) -> Option<String> {
    if name.is_empty() || name.len() > 128 {
        return None;
    }
    if name.chars().any(|character| character.is_control()) {
        return None;
    }
    Some(name)
}

/// True when the step of the transcript is over.
///
/// Only a status that says so ends the step. A record without a status (a
/// renamed upstream field) keeps the step open instead of guessing that it is
/// over.
fn step_is_finished(status: Option<&str>) -> bool {
    match status {
        None => false,
        Some(status) => matches!(
            status.trim().to_ascii_lowercase().as_str(),
            "done" | "completed" | "success" | "canceled" | "cancelled" | "error" | "failed"
        ),
    }
}

/// The timestamp of the record, when it has one.
///
/// The transcript lines carry no timestamp today. The instant of the record is
/// only used when it names one explicitly.
fn record_timestamp_ms(record: &Map<String, Value>) -> Option<i64> {
    for key in [
        "timestamp",
        "timestamp_ms",
        "created_at",
        "createdAt",
        "time",
    ] {
        if let Some(value) = record.get(key)
            && let Some(millis) = timestamp_ms(value)
        {
            return Some(millis);
        }
    }
    None
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
    if is_placeholder_model(trimmed) {
        return None;
    }
    Some(trimmed.to_string())
}

/// True for the placeholders that name no concrete model.
fn is_placeholder_model(model: &str) -> bool {
    matches!(
        model.to_ascii_lowercase().as_str(),
        "auto" | "default" | "none" | "unknown" | "agent"
    )
}

/// The current time in milliseconds.
///
/// The transcript line is handed over as soon as it is written, so this is the
/// closest instant to the start of the turn.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use chrono::DateTime;

    use super::*;
    use crate::provider::Provider;
    use crate::state::{ActivityState, Phase};

    fn millis(rfc3339: &str) -> i64 {
        DateTime::parse_from_rfc3339(rfc3339)
            .expect("valid timestamp")
            .timestamp_millis()
    }

    /// The presence state after every event of a line was applied.
    fn presence_states(
        line: &str,
        now_ms: i64,
    ) -> Vec<(Phase, String, Option<String>, Option<String>)> {
        let mut state = ActivityState::default();
        let path = Path::new(r"C:\work\Discodex\transcript.jsonl");
        let mut states = Vec::new();
        for event in parse_line(line) {
            state.apply(path, Provider::Windsurf, event, now_ms);
            if let Some(presence) = state.selected_presence() {
                states.push((
                    presence.phase,
                    presence.activity,
                    presence.current_file,
                    presence.current_language,
                ));
            }
        }
        states
    }

    #[test]
    fn a_tool_step_yields_a_label_a_basename_and_a_language() {
        let code_action = r#"{"type":"code_action","status":"done","code_action":{"path":"C:\\work\\Discodex\\src\\main.rs","new_content":"PRIVATE_CONTENT"}}"#;
        assert_eq!(
            parse_line(code_action),
            vec![
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label: "Editing files".to_string(),
                        file: Some("main.rs".to_string()),
                        language: Some("Rust".to_string()),
                    },
                },
                ParsedEvent::ToolFinished,
            ]
        );

        // The tool runs first and the step is over afterwards.
        let states = presence_states(code_action, 1_789_000_000_000);
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].0, Phase::RunningTools);
        assert_eq!(states[0].1, "Editing files");
        assert_eq!(states[0].2.as_deref(), Some("main.rs"));
        assert_eq!(states[0].3.as_deref(), Some("Rust"));
        assert_eq!(states[1].0, Phase::Thinking);

        // A command step is announced without a file.
        let run_command = r#"{"type":"run_command","status":"done","run_command":{"command":"cargo test","output":"PRIVATE_OUTPUT"}}"#;
        assert_eq!(
            parse_line(run_command),
            vec![
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label: "Running command".to_string(),
                        file: None,
                        language: None,
                    },
                },
                ParsedEvent::ToolFinished,
            ]
        );

        // A read step keeps the file and infers the language.
        let view_file = r#"{"type":"view_file","status":"done","view_file":{"path":"C:\\work\\Discodex\\src\\provider.rs"}}"#;
        let events = parse_line(view_file);
        assert!(
            events.iter().any(|event| matches!(
                event,
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label,
                        file: Some(file),
                        language: Some(language),
                    }
                } if label == "Reading project files" && file == "provider.rs" && language == "Rust"
            )),
            "unexpected events {events:?}"
        );

        // The planner message is a thinking step, its text stays unread.
        let planner = r#"{"type":"planner_response","status":"done","planner_response":{"response":"PRIVATE_ANSWER","thinking":"PRIVATE_THOUGHT"}}"#;
        assert_eq!(
            parse_line(planner),
            vec![ParsedEvent::Thinking { reasoning: None }]
        );
        assert_eq!(
            presence_states(planner, 1_789_000_000_000)[0].0,
            Phase::Thinking
        );
    }

    #[test]
    fn never_forwards_prompts_answers_results_or_paths() {
        let records = [
            r#"{"type":"user_input","status":"done","user_input":{"user_response":"PRIVATE_PROMPT"}}"#,
            r#"{"type":"planner_response","status":"done","planner_response":{"response":"PRIVATE_ANSWER","thinking":"PRIVATE_THOUGHT","tool_calls":[]}}"#,
            r#"{"type":"code_action","status":"done","code_action":{"path":"C:\\work\\SecretProject\\src\\main.rs","new_content":"PRIVATE_FILE_CONTENT"}}"#,
            r#"{"type":"run_command","status":"done","run_command":{"command":"curl -H 'Authorization: Bearer sk-secret' https://example.invalid","stdout":"PRIVATE_STDOUT"}}"#,
            r#"{"type":"mcp_tool","status":"done","mcp_tool":{"tool_name":"private-tool","arguments":{"path":"C:\\work\\SecretProject\\secret.json"},"result":"PRIVATE_MCP_RESULT"}}"#,
            r#"{"type":"grep_search","status":"done","grep_search":{"path":"C:\\work\\SecretProject","pattern":"PRIVATE_PATTERN"}}"#,
            r#"{"type":"list_directory","status":"done","list_directory":{"path":"C:\\work\\SecretProject"}}"#,
        ];

        for record in records {
            let events = parse_line(record);
            let debug = format!("{events:?}");
            for secret in [
                "PRIVATE_PROMPT",
                "PRIVATE_ANSWER",
                "PRIVATE_THOUGHT",
                "PRIVATE_FILE_CONTENT",
                "PRIVATE_STDOUT",
                "PRIVATE_MCP_RESULT",
                "PRIVATE_PATTERN",
                "Authorization",
                "sk-secret",
                "SecretProject",
                "C:\\work",
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
            // Not JSON at all.
            "{",
            "not json",
            "[]",
            "null",
            // No step type.
            "{}",
            r#"{"status":"done"}"#,
            r#"{"type":"","status":"done"}"#,
            // A step type that Cascade does not write.
            r#"{"type":"unknown_step","status":"done","unknown_step":{"path":"C:\\work\\p\\src\\main.rs"}}"#,
            r#"{"type":"custom_tool","status":"done","custom_tool":{"path":"C:\\work\\p\\src\\main.rs"}}"#,
            // A renamed step type degrades to no activity.
            r#"{"kind":"user_input","status":"done","user_input":{"user_response":"PRIVATE"}}"#,
        ] {
            assert!(
                parse_line(record).is_empty(),
                "unexpected events for {record}"
            );
        }
    }

    #[test]
    fn a_renamed_status_field_never_guesses_that_a_step_is_over() {
        // The turn boundary still comes from the step type.
        let turn = r#"{"type":"user_input","state":"done","user_input":{"user_response":"PRIVATE_PROMPT"}}"#;
        assert!(
            parse_line(turn)
                .iter()
                .any(|event| matches!(event, ParsedEvent::TaskStarted { .. }))
        );

        // A tool step without a known status stays open instead of claiming
        // that the tool is over.
        let step =
            r#"{"type":"run_command","state":"done","run_command":{"command":"cargo test"}}"#;
        let events = parse_line(step);
        assert_eq!(
            events,
            vec![ParsedEvent::ToolStarted {
                activity: ToolActivity {
                    label: "Running command".to_string(),
                    file: None,
                    language: None,
                },
            }]
        );

        // A status that is not a terminal one keeps the step open as well.
        let running =
            r#"{"type":"run_command","status":"running","run_command":{"command":"cargo test"}}"#;
        assert_eq!(parse_line(running).len(), 1);
    }

    #[test]
    fn the_payload_key_is_never_derived_from_the_step_type() {
        // The payload key of `grep_search_v2` stays `grep_search`.
        let line = r#"{"type":"grep_search_v2","status":"done","grep_search":{"path":"C:\\work\\p\\src\\lib.rs","pattern":"PRIVATE_PATTERN"}}"#;
        let events = parse_line(line);
        assert_eq!(
            events,
            vec![
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label: "Searching project".to_string(),
                        file: None,
                        language: None,
                    },
                },
                ParsedEvent::ToolFinished,
            ]
        );
        assert!(!format!("{events:?}").contains("PRIVATE_PATTERN"));

        // A payload key that does not match the step type at all still works.
        let renamed = r#"{"type":"code_action","status":"done","payload":{"path":"C:\\work\\p\\src\\main.rs"}}"#;
        let events = parse_line(renamed);
        assert!(
            events.iter().any(|event| matches!(
                event,
                ParsedEvent::ToolStarted {
                    activity: ToolActivity { file: Some(file), .. }
                } if file == "main.rs"
            )),
            "unexpected events {events:?}"
        );
    }

    #[test]
    fn a_record_that_names_a_model_and_a_timestamp_is_used() {
        let turn = r#"{"type":"user_input","status":"done","timestamp":"2026-10-08T20:32:40.000Z","model_name":"claude-sonnet-4-5","user_input":{"user_response":"PRIVATE_PROMPT"}}"#;
        assert_eq!(
            parse_line(turn),
            vec![
                ParsedEvent::AgentDetected {
                    agent: "claude-sonnet-4-5".to_string(),
                },
                ParsedEvent::TaskStarted {
                    started_at_ms: millis("2026-10-08T20:32:40.000Z"),
                },
            ]
        );

        // A numeric timestamp is normalized to milliseconds.
        let numeric = r#"{"type":"user_input","status":"done","timestamp":1789187508,"user_input":{"user_response":"PRIVATE"}}"#;
        assert!(parse_line(numeric).contains(&ParsedEvent::TaskStarted {
            started_at_ms: 1_789_187_508_000
        }));

        // Prose and placeholders are no model id and yield no agent.
        for model in [
            "a model with spaces",
            "Claude Sonnet 4.5",
            "unknown",
            "auto",
            "",
        ] {
            let record = serde_json::json!({
                "type": "user_input",
                "status": "done",
                "model_name": model,
            })
            .to_string();
            let events = parse_line(&record);
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, ParsedEvent::AgentDetected { .. })),
                "unexpected agent for {model}"
            );
        }
    }

    #[test]
    fn a_step_without_a_timestamp_starts_the_timer_at_the_observation_instant() {
        let line = r#"{"type":"user_input","status":"done","user_input":{"user_response":"PRIVATE_PROMPT"}}"#;
        let events = parse_line(line);
        let ParsedEvent::TaskStarted { started_at_ms } = &events[0] else {
            panic!("unexpected events {events:?}");
        };
        let now = now_ms();
        assert!(
            (*started_at_ms - now).abs() < 5_000,
            "the task start {started_at_ms} is not close to {now}"
        );
    }
}
