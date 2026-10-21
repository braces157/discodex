//! Cursor adapter: agent transcripts in
//! `%USERPROFILE%\.cursor\projects\<sanitized-workspace>\agent-transcripts\<id>\<id>.jsonl`
//!
//! Upstream: the Cursor IDE and the `cursor-agent` CLI share the same agent
//! runtime, which appends one JSON object per line to a transcript:
//!
//! - `{"role":"user","message":{"content":[{"type":"text","text":...}]}}`
//! - `{"role":"assistant","message":{"content":[{"type":"text",...},
//!   {"type":"tool_use","name":...,"input":{...}}]}}`
//! - `{"type":"turn_ended","status":"success"}`
//!
//! Older builds wrote the flat `{"role","content"}` and the type nested
//! `{"type":"user","message":{...}}` shapes; all three are still seen in the
//! wild, so all three are accepted and everything else is ignored.
//!
//! The transcript carries no per record timestamp and no tool results, so the
//! untracked `ToolStarted` / `ToolFinished` pair is used and the tool finish
//! is synthesised at the next turn boundary.
//!
//! Only safe metadata ever leaves this module: the workspace folder name, a
//! file basename, an inferred language, a label from the shared closed table, a
//! sanitized model id and timing. Prompts, assistant answers, thought text,
//! tool arguments, tool output, raw shell commands and full paths are never
//! forwarded.

use serde_json::Value;

use crate::events::{
    ParsedEvent, ToolActivity, classify_tool, detect_project_root, extract_file_basename,
    language_from_file, project_name, timestamp_ms,
};

/// Maximum length of a published model id.
const MAX_AGENT_ID_LEN: usize = 128;

/// Maximum length of the IDE injected `<timestamp>` block.
const MAX_TIMESTAMP_BLOCK_LEN: usize = 96;

/// Keys that may carry the record's own timestamp.
const TIMESTAMP_KEYS: [&str; 6] = [
    "timestamp",
    "created_at",
    "createdAt",
    "time",
    "created_at_ms",
    "createdAtMs",
];

/// Keys that may carry the workspace path of a record.
const CWD_KEYS: [&str; 3] = ["cwd", "workspacePath", "workspace_path"];

/// Keys that may carry the file path of a tool call.
const PATH_KEYS: [&str; 6] = [
    "path",
    "file_path",
    "filePath",
    "file",
    "filepath",
    "absolute_path",
];

/// Keys that may carry the workspace directory of a tool call.
const TOOL_CWD_KEYS: [&str; 3] = ["cwd", "working_directory", "workingDirectory"];

/// Keys that may carry the shell command of a tool call.
const COMMAND_KEYS: [&str; 3] = ["command", "cmd", "CommandLine"];

/// English month abbreviations of the IDE timestamp block.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// English weekday names of the IDE timestamp block.
const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

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

    // The control channel. A turn boundary ends the turn, an error record
    // carries the failure text and never becomes a presence.
    match value.get("type").and_then(Value::as_str) {
        Some("turn_ended") => {
            // Every tool of the turn is done when the turn ends.
            return vec![ParsedEvent::ToolFinished, ParsedEvent::Completed];
        }
        Some("error") => return Vec::new(),
        _ => {}
    }

    let Some(role) = record_role(&value) else {
        return Vec::new();
    };
    let parts = parts_of(&value);

    let mut events = Vec::new();
    let mut metadata = None;

    if role == "user" {
        // A user record without any content carries no information.
        if parts.is_empty() {
            return Vec::new();
        }
        // A user record that carries a tool result closes a tool instead of
        // starting a turn.
        if parts.iter().any(|part| matches!(part, Part::ToolResult)) {
            return vec![ParsedEvent::ToolFinished];
        }
        // The tools of the previous turn are done once a new turn starts.
        events.push(ParsedEvent::ToolFinished);
        metadata = session_metadata(&value);
        let started_at_ms = record_timestamp_ms(&value)
            .or_else(|| {
                parts.iter().find_map(|part| match part {
                    Part::Text(text) => timestamp_block_ms(text),
                    _ => None,
                })
            })
            .unwrap_or_else(current_time_ms);
        events.push(ParsedEvent::TaskStarted { started_at_ms });
    } else {
        // The model id, when the record happens to carry one.
        if let Some(agent) = model_label(&value) {
            events.push(ParsedEvent::AgentDetected { agent });
        }

        let mut narrated = false;
        let mut tool_events = Vec::new();
        for part in &parts {
            match part {
                // The text of the model is dropped: upstream merges the final
                // answer with the interim narration into a single text part, so
                // it cannot be told apart from a reasoning trace.
                Part::Text(_) | Part::Thinking => narrated = true,
                Part::ToolUse { name, input } => {
                    if metadata.is_none() {
                        metadata =
                            session_metadata(&value).or_else(|| session_metadata_from_input(input));
                    }
                    tool_events.push(ParsedEvent::ToolStarted {
                        activity: tool_activity(name, input),
                    });
                }
                Part::ToolResult => {}
            }
        }
        if narrated {
            events.push(ParsedEvent::Thinking { reasoning: None });
        }
        events.extend(tool_events);
    }

    // The workspace is only announced together with real activity.
    if events.is_empty() {
        return Vec::new();
    }
    if let Some(metadata) = metadata {
        events.insert(0, metadata);
    }
    if let Some(conversation_id) = parent_conversation_id(&value) {
        events.push(ParsedEvent::SubagentSpawned {
            conversation_id,
            agent: String::new(),
        });
    }

    events
}

/// A part of a message.
enum Part<'a> {
    /// Free text of the user or of the model. The text itself is never read.
    Text(&'a str),
    /// A reasoning trace. The trace itself is never read.
    Thinking,
    /// A tool call with its name and its input object.
    ToolUse { name: &'a str, input: Value },
    /// A tool result. The result itself is never read.
    ToolResult,
}

/// The role of a record.
///
/// The three historical shapes are accepted: the role nested
/// `{"role":...,"message":{...}}`, the type nested `{"type":"user",...}` and
/// the flat `{"role":...,"content":...}` one. Any other role is unknown and
/// yields no events.
fn record_role(value: &Value) -> Option<&str> {
    if let Some(role) = value.pointer("/message/role").and_then(Value::as_str) {
        return match role {
            "user" | "assistant" => Some(role),
            _ => None,
        };
    }
    if let Some(role) = value.get("role").and_then(Value::as_str) {
        return match role {
            "user" | "assistant" => Some(role),
            _ => None,
        };
    }
    match value.get("type").and_then(Value::as_str) {
        Some("user") => Some("user"),
        Some("assistant") => Some("assistant"),
        _ => None,
    }
}

/// The content of a record, either nested in the message or flat.
fn record_content(value: &Value) -> Option<&Value> {
    for candidate in [value.pointer("/message/content"), value.get("content")] {
        if let Some(content) = candidate
            && !content.is_null()
        {
            return Some(content);
        }
    }
    None
}

/// Split the content of a record into its parts.
fn parts_of(value: &Value) -> Vec<Part<'_>> {
    let mut parts = Vec::new();
    match record_content(value) {
        // The flat shape carries the text as a plain string.
        Some(Value::String(text)) => parts.push(Part::Text(text.as_str())),
        Some(Value::Array(items)) => {
            for item in items {
                let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
                match kind {
                    "text" => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            parts.push(Part::Text(text));
                        }
                    }
                    "thinking" | "reasoning" => parts.push(Part::Thinking),
                    "tool_use" => {
                        if let Some(name) = item.get("name").and_then(Value::as_str) {
                            parts.push(Part::ToolUse {
                                name,
                                input: tool_input(item),
                            });
                        }
                    }
                    "tool_result" => parts.push(Part::ToolResult),
                    // A block without a type is only understood when it is
                    // unambiguously a text or a tool call.
                    "" => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            parts.push(Part::Text(text));
                        } else if let Some(name) = item.get("name").and_then(Value::as_str)
                            && (item.get("input").is_some() || item.get("arguments").is_some())
                        {
                            parts.push(Part::ToolUse {
                                name,
                                input: tool_input(item),
                            });
                        } else if item.get("tool_use_id").is_some() {
                            parts.push(Part::ToolResult);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    parts
}

/// The input object of a tool call.
///
/// Upstream writes an object, older builds wrote a JSON encoded string.
fn tool_input(item: &Value) -> Value {
    let raw = item
        .get("input")
        .or_else(|| item.get("arguments"))
        .or_else(|| item.get("params"));
    match raw {
        Some(Value::String(text)) => serde_json::from_str(text).unwrap_or(Value::Null),
        Some(value) => value.clone(),
        None => Value::Null,
    }
}

/// The normalized activity for one tool call.
fn tool_activity(name: &str, input: &Value) -> ToolActivity {
    let command = COMMAND_KEYS
        .iter()
        .find_map(|key| string_field(input, key))
        .unwrap_or_default();
    // The label always comes from the shared closed table, never from the
    // tool description, the tool arguments or the raw command text.
    let label = cursor_label(name, &command).to_string();
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

/// Map a Cursor tool name onto a label of the shared closed table.
///
/// Cursor uses its own tool vocabulary, so the names are normalized first. The
/// shell tool keeps the command based labels of the shared table.
fn cursor_label(name: &str, command: &str) -> &'static str {
    let lower = name.trim().to_ascii_lowercase();
    match lower.as_str() {
        "read" | "readfile" | "readmanyfiles" | "readlints" | "readlines" | "listfiles"
        | "listdir" | "ls" | "view" | "viewfile" | "open" => "Reading project files",
        "write" | "writefile" | "strreplace" | "replace" | "replacecontent" | "edit"
        | "editfile" | "multiedit" | "delete" | "deletefile" | "remove" | "applypatch"
        | "createfile" | "patch" => "Editing files",
        "grep" | "glob" | "rg" | "semanticsearch" | "codebasesearch" | "search" | "searchfiles"
        | "find" | "listmatching" => "Searching project",
        "websearch" | "webfetch" | "browser" | "browse" | "fetch" | "openurl" => {
            "Browsing documentation"
        }
        "todowrite" | "todoread" | "todo" | "createplan" | "updateplan" | "plan" => {
            "Planning changes"
        }
        "task" | "subagent" | "agent" | "runsubagent" | "dispatchagent" => "Running coding agent",
        "askquestion" | "askuser" | "requestuserinput" => "Waiting for input",
        "viewimage" | "readimage" | "image" => "Inspecting image",
        // The shell tool is the only one whose label depends on its command.
        "shell" | "bash" | "execute" | "exec" | "runcommand" | "runterminalcommand"
        | "terminal" | "terminalcommand" => classify_tool("shell", command),
        _ => classify_tool(&lower, command),
    }
}

/// The workspace of a record, if it is known.
///
/// Only the folder name is published, the full path stays local.
fn session_metadata(value: &Value) -> Option<ParsedEvent> {
    for key in CWD_KEYS {
        if let Some(cwd) = string_field(value, key)
            && let Some(project) = project_name(&cwd)
        {
            return Some(ParsedEvent::SessionMetadata { project, cwd });
        }
    }
    None
}

/// The workspace of a tool call, recovered from an absolute path.
fn session_metadata_from_input(input: &Value) -> Option<ParsedEvent> {
    for key in PATH_KEYS.iter().chain(TOOL_CWD_KEYS.iter()) {
        let Some(path) = string_field(input, key) else {
            continue;
        };
        if !is_absolute_path(&path) {
            continue;
        }
        if let Some((project, cwd)) = detect_project_root(&path) {
            return Some(ParsedEvent::SessionMetadata { project, cwd });
        }
    }
    None
}

/// The model id of a record, if it carries one.
fn model_label(value: &Value) -> Option<String> {
    value
        .get("model")
        .or_else(|| value.pointer("/message/model"))
        .and_then(Value::as_str)
        .and_then(sanitize_agent_id)
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

/// The parent conversation of a subagent run, if the record names one.
///
/// Upstream keeps the parent link in the transcript path only, so this is
/// read from an explicit field when a build provides one. The value must look
/// like the conversation id upstream uses, a UUID.
fn parent_conversation_id(value: &Value) -> Option<String> {
    for key in ["parentConversationId", "parent_conversation_id"] {
        if let Some(id) = value.get(key).and_then(Value::as_str)
            && is_conversation_id(id)
        {
            return Some(id.to_ascii_lowercase());
        }
    }
    None
}

/// A conversation id is a UUID: eight, four, four, four and twelve hex digits.
fn is_conversation_id(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    if groups.len() != 5 {
        return false;
    }
    let lengths = [8usize, 4, 4, 4, 12];
    groups.iter().zip(lengths).all(|(group, length)| {
        group.len() == length && group.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

/// The record's own timestamp, if it has one.
fn record_timestamp_ms(value: &Value) -> Option<i64> {
    TIMESTAMP_KEYS.iter().find_map(|key| {
        value
            .get(key)
            .and_then(timestamp_ms)
            .filter(|millis| *millis > 0)
    })
}

/// Parse the IDE injected `<timestamp>` block of a user text.
///
/// The block is the only in band time the transcript carries. It is accepted
/// only at the very start of the text and only in the documented shape, and
/// only the time itself is read: the surrounding prompt never is.
fn timestamp_block_ms(text: &str) -> Option<i64> {
    let trimmed = text.trim_start();
    let rest = trimmed.strip_prefix("<timestamp>")?;
    let end = rest.find("</timestamp>")?;
    if end > MAX_TIMESTAMP_BLOCK_LEN {
        return None;
    }
    let inner = rest[..end].trim();
    if inner.is_empty() || inner.len() > MAX_TIMESTAMP_BLOCK_LEN {
        return None;
    }
    parse_timestamp_text(inner)
}

/// Read a timestamp that is either RFC 3339 or the IDE's locale shape.
fn parse_timestamp_text(text: &str) -> Option<i64> {
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(time.timestamp_millis());
    }
    parse_locale_timestamp(text)
}

/// Parse `Sunday, Jul 26, 2026, 1:06 PM (UTC+3)`.
fn parse_locale_timestamp(text: &str) -> Option<i64> {
    let trimmed = text.trim();
    let open = trimmed.rfind('(')?;
    if !trimmed.ends_with(')') {
        return None;
    }
    let body = trimmed[..open].trim();
    let offset = normalize_offset(&trimmed[open + 1..trimmed.len() - 1])?;

    let mut fields = body.split(',').map(str::trim);
    let weekday = fields.next()?;
    let month_day = fields.next()?;
    let year = fields.next()?;
    let clock = fields.next()?;
    if fields.next().is_some() {
        return None;
    }
    if !WEEKDAYS
        .iter()
        .any(|name| name.eq_ignore_ascii_case(weekday))
    {
        return None;
    }

    let mut day_parts = month_day.split_whitespace();
    let month_name = day_parts.next()?;
    let day: u32 = day_parts.next()?.parse().ok()?;
    if day_parts.next().is_some() {
        return None;
    }
    let month = MONTHS
        .iter()
        .position(|name| name.eq_ignore_ascii_case(month_name))? as u32
        + 1;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year: i32 = year.parse().ok()?;
    if !(1970..=2999).contains(&year) {
        return None;
    }

    let (clock, is_pm) = split_meridiem(clock)?;
    let mut time_parts = clock.split(':');
    let hour: u32 = time_parts.next()?.parse().ok()?;
    let minute: u32 = time_parts.next()?.parse().ok()?;
    if time_parts.next().is_some() {
        return None;
    }
    let hour = match (is_pm, hour) {
        (false, 12) => 0,
        (false, hour) => hour,
        (true, 12) => 12,
        (true, hour) => hour + 12,
    };
    if hour > 23 || minute > 59 {
        return None;
    }

    let stamp = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:00{offset}");
    chrono::DateTime::parse_from_rfc3339(&stamp)
        .ok()
        .map(|time| time.timestamp_millis())
}

/// Split `1:06 PM` into the clock and the half of the day.
fn split_meridiem(text: &str) -> Option<(&str, bool)> {
    let trimmed = text.trim();
    let (clock, meridiem) = trimmed.rsplit_once(' ')?;
    let meridiem = meridiem.trim();
    if meridiem.eq_ignore_ascii_case("PM") {
        return Some((clock.trim(), true));
    }
    if meridiem.eq_ignore_ascii_case("AM") {
        return Some((clock.trim(), false));
    }
    None
}

/// Turn `UTC+3`, `UTC+03:00` or `GMT-5` into a RFC 3339 offset.
fn normalize_offset(text: &str) -> Option<String> {
    let upper = text.trim().to_ascii_uppercase();
    let rest = upper
        .strip_prefix("UTC")
        .or_else(|| upper.strip_prefix("GMT"))
        .unwrap_or(upper.as_str())
        .trim();
    if rest.is_empty() {
        return Some("+00:00".to_string());
    }
    let (sign, digits) = match rest.strip_prefix('+') {
        Some(digits) => ("+", digits),
        None => match rest.strip_prefix('-') {
            Some(digits) => ("-", digits),
            None => return None,
        },
    };
    let digits: String = digits.chars().filter(|char| *char != ':').collect();
    if digits.is_empty() || digits.len() > 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let (hours, minutes) = if digits.len() <= 2 {
        (digits.as_str(), "00")
    } else {
        (&digits[..2], &digits[2..])
    };
    let hours: u32 = hours.parse().ok()?;
    let minutes: u32 = minutes.parse().ok()?;
    if hours > 14 || minutes > 59 {
        return None;
    }
    Some(format!("{sign}{hours:02}:{minutes:02}"))
}

/// Read a string field, unquoting it when needed.
fn string_field(value: &Value, key: &str) -> Option<String> {
    let text = value.get(key).and_then(Value::as_str)?.trim();
    let unquoted = text
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .unwrap_or(text);
    if unquoted.is_empty() {
        None
    } else {
        Some(unquoted.to_string())
    }
}

/// Whether a path is absolute.
fn is_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/') {
        return true;
    }
    if path.starts_with("\\\\") {
        return true;
    }
    path.starts_with('/')
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

    const PROJECT_ROOT: &str = "Z:\\nonexistent_disk_path\\cursor_demo";
    const PROJECT: &str = "cursor_demo";

    fn user_record(text: &str) -> String {
        serde_json::json!({
            "role": "user",
            "message": {"content": [{"type": "text", "text": text}]}
        })
        .to_string()
    }

    #[test]
    fn maps_cursor_records_to_normalized_events() {
        // A new user turn starts the elapsed timer. The IDE injects the only
        // in band time the transcript carries.
        let events = parse_line(&user_record(
            "<timestamp>Sunday, Jul 26, 2026, 1:06 PM (UTC+3)</timestamp>\n<user_query>\nRead provider.rs\n</user_query>",
        ));
        assert_eq!(
            events,
            vec![
                ParsedEvent::ToolFinished,
                ParsedEvent::TaskStarted {
                    started_at_ms: 1_785_060_360_000
                }
            ]
        );

        // A tool call becomes an activity, a basename and a language, and the
        // absolute path recovers the workspace.
        let assistant = serde_json::json!({
            "role": "assistant",
            "message": {"content": [
                {"type": "text", "text": "Reading the file."},
                {"type": "tool_use", "name": "Read", "input": {
                    "path": "Z:\\nonexistent_disk_path\\cursor_demo\\src\\provider.rs"
                }}
            ]}
        })
        .to_string();
        let events = parse_line(&assistant);
        assert_eq!(
            events,
            vec![
                ParsedEvent::SessionMetadata {
                    project: PROJECT.to_string(),
                    cwd: PROJECT_ROOT.to_string(),
                },
                ParsedEvent::Thinking { reasoning: None },
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label: "Reading project files".to_string(),
                        file: Some("provider.rs".to_string()),
                        language: Some("Rust".to_string()),
                    }
                },
            ]
        );

        // The shell tool keeps the command based labels of the shared table.
        let shell = serde_json::json!({
            "role": "assistant",
            "message": {"content": [
                {"type": "tool_use", "name": "Shell", "input": {
                    "command": "cargo test",
                    "cwd": PROJECT_ROOT
                }}
            ]}
        })
        .to_string();
        let events = parse_line(&shell);
        assert_eq!(
            events,
            vec![
                ParsedEvent::SessionMetadata {
                    project: PROJECT.to_string(),
                    cwd: PROJECT_ROOT.to_string(),
                },
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label: "Running Rust tests".to_string(),
                        file: None,
                        language: None,
                    }
                },
            ]
        );

        // The turn boundary closes the tools and ends the turn.
        assert_eq!(
            parse_line(r#"{"type":"turn_ended","status":"success"}"#),
            vec![ParsedEvent::ToolFinished, ParsedEvent::Completed]
        );
    }

    #[test]
    fn never_leaks_prompts_answers_tool_results_or_paths() {
        const PROMPT: &str = "PRIVATE-PROMPT-9f3a";
        const ANSWER: &str = "PRIVATE-ANSWER-71bc";
        const RESULT: &str = "PRIVATE-RESULT-c2d4";
        const PATH: &str = "Z:\\private-workspace\\cursor_demo\\src\\secret_notes.md";

        let mut events = parse_line(&user_record(PROMPT));
        events.extend(parse_line(
            &serde_json::json!({
                "role": "assistant",
                "message": {"content": [
                    {"type": "text", "text": ANSWER},
                    {"type": "tool_use", "name": "Write", "input": {
                        "path": PATH,
                        "contents": RESULT
                    }}
                ]}
            })
            .to_string(),
        ));
        events.extend(parse_line(
            &serde_json::json!({
                "role": "user",
                "message": {"content": [
                    {"type": "tool_result", "tool_use_id": "call-1", "content": RESULT}
                ]}
            })
            .to_string(),
        ));
        events.extend(parse_line(
            &serde_json::json!({
                "type": "turn_ended",
                "status": "error",
                "error": RESULT
            })
            .to_string(),
        ));

        let debug = events
            .iter()
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>()
            .join("\n");
        for secret in [PROMPT, ANSWER, RESULT] {
            assert!(!debug.contains(secret), "leaked secret: {secret}");
        }

        // Only the session metadata may hold a path, and it stays local: every
        // other event carries a folder name, a basename or a label.
        let public_debug = events
            .iter()
            .filter(|event| !matches!(event, ParsedEvent::SessionMetadata { .. }))
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!public_debug.contains("private-workspace"));
        assert!(!public_debug.contains("cursor_demo"));
        assert!(!public_debug.contains("Z:"));
        // The basename of the edited file is allowed, the path is not.
        assert!(public_debug.contains("secret_notes.md"));

        let metadata: Vec<&ParsedEvent> = events
            .iter()
            .filter(|event| matches!(event, ParsedEvent::SessionMetadata { .. }))
            .collect();
        assert_eq!(metadata.len(), 1);
        match metadata[0] {
            ParsedEvent::SessionMetadata { project, cwd } => {
                assert_eq!(project, PROJECT);
                assert_eq!(cwd, "Z:\\private-workspace\\cursor_demo");
            }
            _ => unreachable!(),
        }
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
            r#"{"type":"unknown_record","message":{"content":[]}}"#,
            r#"{"type":"turn_started","status":"running"}"#,
            r#"{"role":"system","message":{"content":[{"type":"text","text":"x"}]}}"#,
            r#"{"role":"assistant"}"#,
            r#"{"type":"assistant"}"#,
            r#"{"role":"user"}"#,
            r#"{"role":"user","message":{"content":[]}}"#,
            r#"{"type":"error","error":"PRIVATE_ERROR"}"#,
        ] {
            assert!(parse_line(line).is_empty(), "unexpected events for {line}");
        }
        assert!(
            !parse_line(r#"{"type":"error","error":"PRIVATE_ERROR"}"#)
                .iter()
                .any(|event| format!("{event:?}").contains("PRIVATE_ERROR"))
        );

        // The legacy flat and type nested shapes are still accepted.
        let flat = parse_line(r#"{"role":"user","content":"PRIVATE_PROMPT"}"#);
        assert!(matches!(flat[0], ParsedEvent::ToolFinished));
        assert!(matches!(flat[1], ParsedEvent::TaskStarted { .. }));
        let nested = parse_line(r#"{"type":"user","message":{"role":"user","content":"PRIVATE"}}"#);
        assert!(matches!(nested[0], ParsedEvent::ToolFinished));
        assert!(matches!(nested[1], ParsedEvent::TaskStarted { .. }));
    }

    #[test]
    fn reads_model_ids_and_timestamps() {
        // A record that carries its own timestamp drives the elapsed timer.
        let turn = serde_json::json!({
            "role": "user",
            "timestamp": "2026-10-21T09:30:00.000Z",
            "message": {"content": [{"type": "text", "text": "PRIVATE_PROMPT"}]}
        })
        .to_string();
        assert!(parse_line(&turn).contains(&ParsedEvent::TaskStarted {
            started_at_ms: 1_792_575_000_000
        }));

        // The model id is published when the record carries one.
        let model = serde_json::json!({
            "role": "assistant",
            "model": "gpt-5.4-high",
            "message": {"content": [{"type": "text", "text": "PRIVATE_ANSWER"}]}
        })
        .to_string();
        assert!(parse_line(&model).contains(&ParsedEvent::AgentDetected {
            agent: "gpt-5.4-high".to_string()
        }));

        // A prose model id and an over long one are dropped entirely.
        for bad in [
            "PRIVATE prose with spaces",
            "model/with spaces",
            "a".repeat(129).as_str(),
        ] {
            let record = serde_json::json!({
                "role": "assistant",
                "model": bad,
                "message": {"content": [{"type": "text", "text": "x"}]}
            })
            .to_string();
            assert!(
                !parse_line(&record)
                    .iter()
                    .any(|event| matches!(event, ParsedEvent::AgentDetected { .. })),
                "unexpected agent label for {bad}"
            );
        }

        // A timestamp block that is not the documented shape is ignored.
        assert!(timestamp_block_ms("<timestamp>not a date</timestamp>").is_none());
        assert!(timestamp_block_ms("PRIVATE <timestamp>not a date</timestamp>").is_none());
    }
}
