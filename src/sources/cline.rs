//! Cline adapter: SDK session transcripts, session manifests and legacy task files.
//!
//! Upstream Cline 4 keeps one folder per session below
//! `%USERPROFILE%\.cline\data\sessions\<sessionId>\`:
//!
//! - `<sessionId>.messages.json` is the transcript, a document with a
//!   `messages` array.
//! - `<sessionId>.json` is the manifest of the same session. It carries the
//!   workspace, the model and the session state.
//!
//! The two files belong to one session and are handed to the adapter with the
//! same state key, so the manifest contributes the session metadata and the
//! transcript contributes the tool activity.
//!
//! Cline 3 and older wrote `ui_messages.json` into the editor global storage;
//! that shape is read as well, and so is the per task history item that
//! carries the workspace and the model.
//!
//! Only safe metadata leaves the adapter: the workspace folder name, a file
//! basename, an inferred language, a normalized activity label, a model id and
//! timing. Prompts, answers, reasoning, tool arguments, tool results, command
//! text and full paths are never forwarded.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::events::{
    ParsedEvent, ToolActivity, basename, classify_tool, extract_file_basename, language_from_file,
    project_name, timestamp_ms,
};

/// Longest model id or call id that is accepted.
const MAX_ID_LEN: usize = 128;

/// Parse the harness' session document and report only records that were not
/// seen before.
///
/// The provider REWRITES this document in place, so it is re-read on every
/// change. Every record gets a stable key: the tool call id when the store
/// provides one, otherwise the message id, the message time or the index.
/// Records whose key is already in `seen` are skipped and never reported
/// twice.
///
/// Three document shapes are understood:
///
/// - the SDK transcript, an object with a `messages` array,
/// - the SDK manifest, an object with a workspace, a model or a session state,
/// - the legacy task log, an array of task messages.
pub fn parse_document(doc: &Value, seen: &mut HashSet<String>) -> Vec<ParsedEvent> {
    match doc {
        Value::Array(items) => parse_legacy_messages(items, seen),
        Value::Object(map) => {
            if map.contains_key("messages") {
                parse_transcript(doc, seen)
            } else if looks_like_manifest(map) {
                parse_manifest(doc, seen)
            } else {
                // An unknown document shape yields no activity at all.
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

/// The manifest of a session, or the legacy history item of a task.
///
/// Only the keys that carry session metadata are looked at. Everything else,
/// including the prompt and the derived title, is ignored.
fn looks_like_manifest(map: &Map<String, Value>) -> bool {
    const MANIFEST_KEYS: [&str; 12] = [
        "cwd",
        "workspace_root",
        "workspaceRoot",
        "cwdOnTaskInitialization",
        "model",
        "modelId",
        "model_id",
        "provider",
        "apiProvider",
        "started_at",
        "startedAt",
        "status",
    ];
    MANIFEST_KEYS.iter().any(|key| map.contains_key(*key))
}

/// The session manifest: workspace, model, start time and session state.
fn parse_manifest(doc: &Value, seen: &mut HashSet<String>) -> Vec<ParsedEvent> {
    let mut events = Vec::new();

    // The workspace of the session. Only the folder name is published, the
    // path itself stays local.
    if let Some(cwd) = manifest_workspace(doc)
        && let Some(project) = project_name(&cwd)
    {
        let key = format!("manifest:workspace:{}", fingerprint(&cwd));
        if seen.insert(key) {
            events.push(ParsedEvent::SessionMetadata { project, cwd });
        }
    }

    // The model of the session.
    if let Some(agent) = manifest_model(doc) {
        let key = format!("manifest:model:{}", fingerprint(&agent));
        if seen.insert(key) {
            events.push(ParsedEvent::AgentDetected { agent });
        }
    }

    // The start of the session drives the elapsed timer.
    if let Some(started_at_ms) = manifest_started_at(doc) {
        let key = format!("manifest:started:{started_at_ms}");
        if seen.insert(key) {
            events.push(ParsedEvent::TaskStarted { started_at_ms });
        }
    }

    // A session that reached a final state ends the turn.
    if let Some(status) = doc.get("status").and_then(Value::as_str) {
        let status = status.trim().to_ascii_lowercase();
        if matches!(
            status.as_str(),
            "completed" | "failed" | "cancelled" | "canceled" | "error" | "aborted" | "stopped"
        ) {
            let key = format!("manifest:status:{status}");
            if seen.insert(key) {
                events.push(ParsedEvent::Completed);
            }
        }
    }

    events
}

/// The workspace of the session, if the manifest names one.
fn manifest_workspace(doc: &Value) -> Option<String> {
    for key in [
        "cwd",
        "workspace_root",
        "workspaceRoot",
        "cwdOnTaskInitialization",
    ] {
        if let Some(value) = doc.get(key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// The model of the session, if the manifest names one.
fn manifest_model(doc: &Value) -> Option<String> {
    for key in ["model", "modelId", "model_id"] {
        if let Some(agent) = doc.get(key).and_then(Value::as_str).and_then(sanitize_id) {
            return Some(agent);
        }
    }
    for key in ["provider", "apiProvider", "api_provider"] {
        if let Some(agent) = doc.get(key).and_then(Value::as_str).and_then(sanitize_id) {
            return Some(agent);
        }
    }
    None
}

/// The start time of the session, if the manifest names one.
fn manifest_started_at(doc: &Value) -> Option<i64> {
    for key in ["started_at", "startedAt", "ts", "created_at", "createdAt"] {
        if let Some(millis) = doc
            .get(key)
            .and_then(timestamp_ms)
            .filter(|millis| *millis > 0)
        {
            return Some(millis);
        }
    }
    None
}

/// The SDK transcript: a document with a `messages` array.
fn parse_transcript(doc: &Value, seen: &mut HashSet<String>) -> Vec<ParsedEvent> {
    let Some(messages) = doc.get("messages").and_then(Value::as_array) else {
        // A transcript without a message array is not a transcript.
        return Vec::new();
    };

    let mut events = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        events.extend(parse_transcript_message(message, index, seen));
    }
    events
}

/// One message of the transcript.
fn parse_transcript_message(
    message: &Value,
    index: usize,
    seen: &mut HashSet<String>,
) -> Vec<ParsedEvent> {
    let mut events = Vec::new();
    let role = message.get("role").and_then(Value::as_str).unwrap_or("");
    let blocks = message.get("content").and_then(Value::as_array);
    let key = message_key(message, index);

    // The model of the request, when the harness stored one next to it.
    if let Some(agent) = message
        .get("modelInfo")
        .and_then(|info| info.get("id"))
        .and_then(Value::as_str)
        .and_then(sanitize_id)
    {
        let seen_key = format!("msg:{key}:model:{agent}");
        if seen.insert(seen_key) {
            events.push(ParsedEvent::AgentDetected { agent });
        }
    }

    match role {
        "user" => {
            let mut tool_result = false;
            for block in blocks.into_iter().flatten() {
                if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                    continue;
                }
                // A result without its call id cannot be paired, so it is
                // dropped instead of guessed.
                let Some(call_id) = block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .and_then(sanitize_id)
                else {
                    continue;
                };
                tool_result = true;
                let seen_key = format!("tool-result:{call_id}");
                if seen.insert(seen_key) {
                    events.push(ParsedEvent::TrackedToolFinished { call_id });
                }
            }

            // A user message without a tool result is a new turn. Its text is
            // never read.
            if !tool_result && has_text_content(message) {
                let seen_key = format!("turn:{key}");
                if seen.insert(seen_key) {
                    events.push(ParsedEvent::TaskStarted {
                        started_at_ms: message_timestamp_ms(message)
                            .unwrap_or_else(current_time_ms),
                    });
                }
            }
        }
        "assistant" => {
            let mut thinking = false;
            let mut completion = false;
            let mut tool_blocks: Vec<&Value> = Vec::new();
            for block in blocks.into_iter().flatten() {
                match block.get("type").and_then(Value::as_str) {
                    Some("thinking") | Some("redacted_thinking") => thinking = true,
                    Some("tool_use") => {
                        let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                        if is_completion_tool(name) {
                            completion = true;
                        } else {
                            tool_blocks.push(block);
                        }
                    }
                    _ => {}
                }
            }

            // The phase of an assistant message: thinking first, then the
            // tools that follow.
            if thinking {
                let seen_key = format!("msg:{key}:thinking");
                if seen.insert(seen_key) {
                    events.push(ParsedEvent::Thinking { reasoning: None });
                }
            } else if tool_blocks.is_empty() && !completion && has_text_content(message) {
                let seen_key = format!("msg:{key}:text");
                if seen.insert(seen_key) {
                    events.push(ParsedEvent::Thinking { reasoning: None });
                }
            }

            for block in tool_blocks {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                // An input that is not an object is an unknown shape: the
                // tool is not reported at all.
                let Some(input) = tool_input(block) else {
                    continue;
                };
                let activity = tool_activity(name, &input);
                match block
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(sanitize_id)
                {
                    Some(call_id) => {
                        let seen_key = format!("tool:{call_id}");
                        if seen.insert(seen_key) {
                            events.push(ParsedEvent::TrackedToolStarted { call_id, activity });
                        }
                    }
                    // Without a call id the tool cannot be paired with its
                    // result, so the untracked variant is used.
                    None => {
                        let seen_key = format!("tool:{key}:{}", activity.label);
                        if seen.insert(seen_key) {
                            events.push(ParsedEvent::ToolStarted { activity });
                        }
                    }
                }
            }

            if completion {
                let seen_key = format!("msg:{key}:completed");
                if seen.insert(seen_key) {
                    events.push(ParsedEvent::Completed);
                }
            }
        }
        _ => {}
    }

    events
}

/// The arguments of a tool call.
///
/// The SDK writes an object. A build that writes the arguments as a JSON
/// string is accepted when the string decodes into an object, everything else
/// is an unknown shape and yields nothing.
fn tool_input(block: &Value) -> Option<Value> {
    match block.get("input") {
        None => Some(Value::Object(Map::new())),
        Some(Value::Object(map)) => Some(Value::Object(map.clone())),
        Some(Value::String(text)) => serde_json::from_str::<Value>(text)
            .ok()
            .filter(|value| value.is_object()),
        Some(_) => None,
    }
}

/// The normalized activity of one tool call.
///
/// Only the label, the basename and the language leave the adapter. The
/// arguments themselves are never published.
fn tool_activity(name: &str, input: &Value) -> ToolActivity {
    let command = tool_command(input).unwrap_or_default();
    let file = cline_file_basename(input);
    let language = file
        .as_deref()
        .and_then(language_from_file)
        .map(str::to_string);
    ToolActivity {
        label: cline_label(name, &command).to_string(),
        file,
        language,
    }
}

/// The command of a tool call.
///
/// The command is only used to pick a fixed label, it is never published.
fn tool_command(input: &Value) -> Option<String> {
    for key in ["command", "cmd", "commands", "commandLine", "CommandLine"] {
        match input.get(key) {
            Some(Value::String(text)) => return Some(text.clone()),
            Some(Value::Array(items)) => {
                if let Some(text) = items.iter().find_map(Value::as_str) {
                    return Some(text.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// The basename of the file a tool call works on.
fn cline_file_basename(input: &Value) -> Option<String> {
    if let Some(name) = extract_file_basename(input) {
        return Some(name);
    }

    // `read_files` and friends carry a list of paths.
    for key in ["files", "file_paths", "paths"] {
        let Some(items) = input.get(key).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            if let Some(name) = item.as_str().and_then(basename) {
                return Some(name);
            }
            if let Some(name) = item.get("path").and_then(Value::as_str).and_then(basename) {
                return Some(name);
            }
            if let Some(name) = extract_file_basename(item) {
                return Some(name);
            }
        }
    }

    // `apply_patch` carries the patch body in `input`.
    if let Some(body) = input.get("input").and_then(Value::as_str) {
        for line in body.lines() {
            let line = line.trim();
            let path = line
                .strip_prefix("*** Update File: ")
                .or_else(|| line.strip_prefix("*** Add File: "))
                .or_else(|| line.strip_prefix("*** Delete File: "));
            if let Some(name) = path.and_then(|path| basename(path.trim())) {
                return Some(name);
            }
        }
    }

    None
}

/// Map a Cline tool name onto a label of the shared closed table.
///
/// Cline uses its own tool vocabulary, so the names are normalized first. The
/// shell tool keeps the command based labels of the shared table.
fn cline_label(name: &str, command: &str) -> &'static str {
    let lower = name.trim().to_ascii_lowercase();
    match normalize_tool_name(name).as_str() {
        "readfiles" | "readfile" | "listfiles" | "viewfile" => "Reading project files",
        "editor" | "applypatch" | "writetofile" | "replaceinfile" | "writefile" | "editfile" => {
            "Editing files"
        }
        "searchcodebase" | "searchfiles" | "listcodedefinitionnames" | "grep" | "glob" => {
            "Searching project"
        }
        "runcommands" | "executecommand" | "runcommand" | "shell" | "bash" => {
            classify_tool("shell", command)
        }
        "fetchwebcontent" | "browseraction" | "webfetch" | "browser" => "Browsing documentation",
        "skills" | "skill" | "usemcpserver" => "Using development tools",
        "askquestion" | "askfollowupquestion" => "Waiting for input",
        _ => classify_tool(&lower, command),
    }
}

/// Reduce a tool name to its letters and digits.
///
/// `read_files`, `readFiles` and `readFile` all become `readfiles`, so the
/// SDK and the legacy vocabulary share one table.
fn normalize_tool_name(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

/// The tool that ends a turn with its final answer.
fn is_completion_tool(name: &str) -> bool {
    matches!(
        normalize_tool_name(name).as_str(),
        "submitandexit" | "attemptcompletion"
    )
}

/// The legacy task log of Cline 3 and older: an array of task messages.
fn parse_legacy_messages(items: &[Value], seen: &mut HashSet<String>) -> Vec<ParsedEvent> {
    let mut events = Vec::new();
    for (index, item) in items.iter().enumerate() {
        events.extend(parse_legacy_message(item, index, seen));
    }
    events
}

/// One message of the legacy task log.
fn parse_legacy_message(
    item: &Value,
    index: usize,
    seen: &mut HashSet<String>,
) -> Vec<ParsedEvent> {
    let mut events = Vec::new();
    let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
    let label = item
        .get("say")
        .and_then(Value::as_str)
        .or_else(|| item.get("ask").and_then(Value::as_str))
        .unwrap_or("");
    let text = item.get("text").and_then(Value::as_str).unwrap_or("");

    // The model of the request, when the harness stored one.
    if let Some(agent) = item
        .get("modelInfo")
        .and_then(|info| info.get("id"))
        .and_then(Value::as_str)
        .and_then(sanitize_id)
    {
        let seen_key = format!("legacy:{index}:model:{agent}");
        if seen.insert(seen_key) {
            events.push(ParsedEvent::AgentDetected { agent });
        }
    }

    let seen_key = format!("legacy:{index}:{kind}:{label}");
    let event = match (kind, label) {
        ("say", "task") => Some(ParsedEvent::TaskStarted {
            started_at_ms: item
                .get("ts")
                .and_then(timestamp_ms)
                .unwrap_or_else(current_time_ms),
        }),
        ("say", "reasoning") | ("say", "text") | ("ask", "followup") => {
            Some(ParsedEvent::Thinking { reasoning: None })
        }
        ("say", "tool") | ("ask", "tool") => Some(ParsedEvent::ToolStarted {
            activity: legacy_tool_activity(text),
        }),
        ("say", "command") | ("ask", "command") => Some(ParsedEvent::ToolStarted {
            activity: legacy_command_activity(text),
        }),
        ("say", "command_output") => Some(ParsedEvent::ToolFinished),
        ("say", "completion_result") => Some(ParsedEvent::TerminalCandidate),
        ("ask", "completion_result") => Some(ParsedEvent::Completed),
        _ => None,
    };

    if let Some(event) = event
        && seen.insert(seen_key)
    {
        events.push(event);
    }

    events
}

/// The activity of a legacy tool message.
///
/// The payload is a JSON blob with the tool, the path and, for writes, the
/// whole file body. Only the tool name and the basename are read.
fn legacy_tool_activity(text: &str) -> ToolActivity {
    let blob = serde_json::from_str::<Value>(text)
        .ok()
        .filter(|value| value.is_object());
    let name = blob
        .as_ref()
        .and_then(|value| value.get("tool"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let input = blob.unwrap_or(Value::Object(Map::new()));
    tool_activity(&name, &input)
}

/// The activity of a legacy command message.
///
/// The command itself is only used to pick a fixed label.
fn legacy_command_activity(text: &str) -> ToolActivity {
    let mut input = Map::new();
    input.insert("command".to_string(), Value::String(text.to_string()));
    tool_activity("execute_command", &Value::Object(input))
}

/// True when the message carries text content.
///
/// The text itself is never read, only its presence.
fn has_text_content(message: &Value) -> bool {
    match message.get("content") {
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .any(|block| block.get("type").and_then(Value::as_str) == Some("text")),
        _ => false,
    }
}

/// A stable key for one message of the transcript.
///
/// The message id is used when the harness wrote one, then the message time
/// and only as a last resort the index. A transcript that is compacted from
/// the front keeps its keys, so old records are not reported twice.
fn message_key(message: &Value, index: usize) -> String {
    if let Some(id) = message
        .get("id")
        .and_then(Value::as_str)
        .and_then(sanitize_id)
    {
        return format!("id:{id}");
    }
    if let Some(millis) = message_timestamp_ms(message) {
        return format!("ts:{millis}");
    }
    format!("index:{index}")
}

/// The time of a message, if it carries one.
fn message_timestamp_ms(message: &Value) -> Option<i64> {
    for key in ["ts", "timestamp", "created_at", "createdAt", "time"] {
        if let Some(millis) = message
            .get(key)
            .and_then(timestamp_ms)
            .filter(|millis| *millis > 0)
        {
            return Some(millis);
        }
    }
    None
}

/// Accept only a strict identifier: `[A-Za-z0-9._:/@-]` and at most 128 chars.
///
/// Model ids and call ids are metadata. Anything else, prose or a control
/// character, is dropped instead of being published.
fn sanitize_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_ID_LEN {
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

/// A short, non reversible fingerprint of a value.
///
/// It keeps a path or a model id out of the bookkeeping of the adapter.
fn fingerprint(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
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

    const PROJECT_ROOT: &str = "C:\\Users\\me\\Documents\\Discodex";

    /// A redacted transcript in the shape upstream Cline 4 writes.
    fn transcript() -> Value {
        serde_json::json!({
            "version": 1,
            "updated_at": "2026-10-21T01:26:32.412Z",
            "agent": "lead",
            "sessionId": "1758430000000_abcde",
            "system_prompt": "<REDACTED SYSTEM PROMPT>",
            "messages": [
                {
                    "id": "m1",
                    "role": "user",
                    "ts": 1758430000000_i64,
                    "content": [{ "type": "text", "text": "<REDACTED USER PROMPT>" }]
                },
                {
                    "id": "m2",
                    "role": "assistant",
                    "ts": 1758430001000_i64,
                    "content": [{ "type": "thinking", "thinking": "<REDACTED REASONING>" }],
                    "modelInfo": { "id": "claude-sonnet-4-5", "provider": "anthropic", "family": "claude" }
                },
                {
                    "id": "m3",
                    "role": "assistant",
                    "ts": 1758430002000_i64,
                    "content": [{
                        "type": "tool_use",
                        "id": "toolu_01ABC",
                        "name": "read_files",
                        "input": { "files": [{ "path": "C:\\Users\\me\\Documents\\Discodex\\src\\main.rs" }] }
                    }]
                },
                {
                    "id": "m4",
                    "role": "user",
                    "ts": 1758430003000_i64,
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "toolu_01ABC",
                        "name": "read_files",
                        "content": "<REDACTED FILE BODY>"
                    }]
                },
                {
                    "id": "m5",
                    "role": "assistant",
                    "ts": 1758430004000_i64,
                    "content": [{
                        "type": "tool_use",
                        "id": "toolu_01DEF",
                        "name": "editor",
                        "input": {
                            "path": "C:\\Users\\me\\Documents\\Discodex\\src\\events.rs",
                            "old_text": "<REDACTED>",
                            "new_text": "<REDACTED>"
                        }
                    }]
                },
                {
                    "id": "m6",
                    "role": "assistant",
                    "ts": 1758430005000_i64,
                    "content": [{
                        "type": "tool_use",
                        "id": "toolu_01GHI",
                        "name": "submit_and_exit",
                        "input": { "result": "<REDACTED ANSWER>" }
                    }]
                }
            ]
        })
    }

    /// A redacted manifest in the shape upstream Cline 4 writes.
    fn manifest() -> Value {
        serde_json::json!({
            "version": 1,
            "session_id": "1758430000000_abcde",
            "source": "vscode",
            "pid": 4242,
            "started_at": "2026-10-21T01:20:00.000Z",
            "status": "running",
            "interactive": true,
            "provider": "anthropic",
            "model": "claude-sonnet-4-5",
            "cwd": PROJECT_ROOT,
            "workspace_root": PROJECT_ROOT,
            "prompt": "<REDACTED USER PROMPT>",
            "metadata": { "title": "<REDACTED TITLE>" }
        })
    }

    #[test]
    fn maps_transcript_records_to_normalized_events() {
        let mut seen = HashSet::new();
        let events = parse_document(&transcript(), &mut seen);

        assert_eq!(
            events,
            vec![
                // The user turn starts the elapsed timer.
                ParsedEvent::TaskStarted {
                    started_at_ms: 1_758_430_000_000
                },
                // The model of the request.
                ParsedEvent::AgentDetected {
                    agent: "claude-sonnet-4-5".to_string()
                },
                // The reasoning text itself is never published.
                ParsedEvent::Thinking { reasoning: None },
                // A tool call carries a label, a basename and a language.
                ParsedEvent::TrackedToolStarted {
                    call_id: "toolu_01ABC".to_string(),
                    activity: ToolActivity {
                        label: "Reading project files".to_string(),
                        file: Some("main.rs".to_string()),
                        language: Some("Rust".to_string()),
                    }
                },
                // The result closes the call it belongs to.
                ParsedEvent::TrackedToolFinished {
                    call_id: "toolu_01ABC".to_string()
                },
                ParsedEvent::TrackedToolStarted {
                    call_id: "toolu_01DEF".to_string(),
                    activity: ToolActivity {
                        label: "Editing files".to_string(),
                        file: Some("events.rs".to_string()),
                        language: Some("Rust".to_string()),
                    }
                },
                // The final answer ends the turn.
                ParsedEvent::Completed,
            ]
        );

        // The manifest of the same session carries the workspace and the model.
        let mut manifest_seen = HashSet::new();
        let events = parse_document(&manifest(), &mut manifest_seen);
        assert_eq!(
            events,
            vec![
                ParsedEvent::SessionMetadata {
                    project: "Discodex".to_string(),
                    cwd: PROJECT_ROOT.to_string(),
                },
                ParsedEvent::AgentDetected {
                    agent: "claude-sonnet-4-5".to_string()
                },
                ParsedEvent::TaskStarted {
                    started_at_ms: 1_792_545_600_000
                },
            ]
        );
    }

    #[test]
    fn never_leaks_prompts_answers_results_or_paths() {
        const PROMPT: &str = "SECRET-PROMPT-42";
        const ANSWER: &str = "SECRET-ANSWER-42";
        const THINKING: &str = "SECRET-THINKING-42";
        const RESULT: &str = "SECRET-RESULT-42";
        const BODY: &str = "SECRET-BODY-42";
        const SECRET_PATH: &str = "C:\\Users\\me\\Documents\\Discodex\\src\\secret_notes.md";

        let transcript = serde_json::json!({
            "version": 1,
            "system_prompt": PROMPT,
            "messages": [
                { "id": "u1", "role": "user", "content": [{ "type": "text", "text": PROMPT }] },
                { "id": "a1", "role": "assistant",
                  "content": [
                    { "type": "thinking", "thinking": THINKING },
                    { "type": "text", "text": ANSWER },
                    { "type": "tool_use", "id": "call-1", "name": "editor",
                      "input": { "path": SECRET_PATH, "new_text": BODY } },
                    { "type": "tool_use", "id": "call-2", "name": "run_commands",
                      "input": { "commands": ["cargo test -- SECRET-RESULT-42"] } }
                  ] },
                { "id": "u2", "role": "user",
                  "content": [{ "type": "tool_result", "tool_use_id": "call-1", "content": RESULT }] }
            ]
        });

        let manifest = serde_json::json!({
            "version": 1,
            "session_id": "1758430000000_abcde",
            "provider": "anthropic",
            "model": "claude-sonnet-4-5",
            "cwd": PROJECT_ROOT,
            "workspace_root": PROJECT_ROOT,
            "started_at": "2026-10-21T01:20:00.000Z",
            "status": "running",
            "prompt": PROMPT,
            "metadata": { "title": PROMPT }
        });

        let mut seen = HashSet::new();
        let mut events = parse_document(&transcript, &mut seen);
        // The manifest of the same session is a second document.
        events.extend(parse_document(&manifest, &mut HashSet::new()));

        let debug = events
            .iter()
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>()
            .join("\n");

        for secret in [PROMPT, ANSWER, THINKING, RESULT, BODY] {
            assert!(!debug.contains(secret), "leaked secret: {secret}");
        }

        // The full path of a tool never leaves the adapter, only the basename
        // does.
        assert!(!debug.contains(SECRET_PATH));
        assert!(!debug.contains("Documents\\Discodex\\src"));
        assert!(debug.contains("secret_notes.md"));

        // The workspace path may only appear in the session metadata, and the
        // published name is the folder alone.
        let public_debug = events
            .iter()
            .filter(|event| !matches!(event, ParsedEvent::SessionMetadata { .. }))
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!public_debug.contains("Documents"));
        assert!(!public_debug.contains("Discodex"));
        assert!(!public_debug.contains(PROJECT_ROOT));
    }

    #[test]
    fn malformed_and_unknown_records_produce_no_events() {
        for document in [
            serde_json::json!({}),
            serde_json::json!({ "foo": 1 }),
            serde_json::json!({ "messages": "nope" }),
            serde_json::json!({ "messages": [] }),
            serde_json::json!({ "messages": [{ "role": "system", "content": [{ "type": "text", "text": "x" }] }] }),
            serde_json::json!({ "messages": [{ "role": "assistant", "content": [{ "type": "tool_use", "name": "read_files", "input": "not json" }] }] }),
            serde_json::json!({ "messages": [{ "role": "user", "content": [{ "type": "tool_result", "content": "x" }] }] }),
            serde_json::json!([]),
            serde_json::json!([{ "type": "unknown", "text": "x" }]),
            serde_json::json!(null),
            serde_json::json!(42),
            serde_json::json!("text"),
        ] {
            let mut seen = HashSet::new();
            let events = parse_document(&document, &mut seen);
            assert!(
                events.is_empty(),
                "unexpected events for {document}: {events:?}"
            );
        }
    }

    #[test]
    fn reads_model_ids_and_timestamps() {
        // The manifest names the model and the start of the session.
        let mut seen = HashSet::new();
        let events = parse_document(&manifest(), &mut seen);
        assert!(events.contains(&ParsedEvent::AgentDetected {
            agent: "claude-sonnet-4-5".to_string()
        }));
        assert!(events.contains(&ParsedEvent::TaskStarted {
            started_at_ms: 1_792_545_600_000
        }));

        // A model id that is prose, or too long, is dropped entirely.
        for bad in [
            "evil prose\ntext",
            "model/with spaces",
            "a".repeat(129).as_str(),
        ] {
            let document = serde_json::json!({ "model": bad, "cwd": PROJECT_ROOT });
            let mut seen = HashSet::new();
            let events = parse_document(&document, &mut seen);
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, ParsedEvent::AgentDetected { .. })),
                "unexpected agent label for {bad}"
            );
        }

        // The turn of a message uses the time of the record itself.
        let document = serde_json::json!({
            "messages": [
                { "id": "u1", "role": "user", "ts": 1758430000000_i64,
                  "content": [{ "type": "text", "text": "PRIVATE" }] }
            ]
        });
        let mut seen = HashSet::new();
        let events = parse_document(&document, &mut seen);
        assert_eq!(
            events,
            vec![ParsedEvent::TaskStarted {
                started_at_ms: 1_758_430_000_000
            }]
        );
    }

    #[test]
    fn a_rewritten_document_is_not_replayed() {
        let mut seen = HashSet::new();
        let first = parse_document(&transcript(), &mut seen);
        assert!(!first.is_empty());

        // The provider rewrites the whole document, the same records must not
        // be reported a second time.
        let second = parse_document(&transcript(), &mut seen);
        assert!(second.is_empty());

        // A new tool call is reported once.
        let mut document = transcript();
        document["messages"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": "m7",
                "role": "assistant",
                "ts": 1758430006000_i64,
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_01JKL",
                    "name": "search_codebase",
                    "input": { "queries": ["PRIVATE QUERY"] }
                }]
            }));
        let third = parse_document(&document, &mut seen);
        assert_eq!(
            third,
            vec![ParsedEvent::TrackedToolStarted {
                call_id: "toolu_01JKL".to_string(),
                activity: ToolActivity {
                    label: "Searching project".to_string(),
                    file: None,
                    language: None,
                }
            }]
        );
    }

    #[test]
    fn reads_the_legacy_task_log_of_older_versions() {
        let document = serde_json::json!([
            { "ts": 1758430000000_i64, "type": "say", "say": "task", "text": "<REDACTED USER PROMPT>" },
            { "ts": 1758430001000_i64, "type": "say", "say": "reasoning", "reasoning": "<REDACTED>" },
            { "ts": 1758430002000_i64, "type": "say", "say": "tool",
              "text": "{\"tool\":\"readFile\",\"path\":\"src/main.rs\",\"content\":\"<REDACTED FILE BODY>\"}" },
            { "ts": 1758430003000_i64, "type": "ask", "ask": "command", "text": "cargo test" },
            { "ts": 1758430004000_i64, "type": "say", "say": "completion_result", "text": "<REDACTED ANSWER>" }
        ]);

        let mut seen = HashSet::new();
        let events = parse_document(&document, &mut seen);
        assert_eq!(
            events,
            vec![
                ParsedEvent::TaskStarted {
                    started_at_ms: 1_758_430_000_000
                },
                ParsedEvent::Thinking { reasoning: None },
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label: "Reading project files".to_string(),
                        file: Some("main.rs".to_string()),
                        language: Some("Rust".to_string()),
                    }
                },
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label: "Running Rust tests".to_string(),
                        file: None,
                        language: None,
                    }
                },
                ParsedEvent::TerminalCandidate,
            ]
        );

        // The legacy history item of a task names the workspace and the model.
        let document = serde_json::json!({
            "id": "1758430000000",
            "ts": 1758430000000_i64,
            "task": "<REDACTED USER PROMPT>",
            "cwdOnTaskInitialization": PROJECT_ROOT,
            "modelId": "claude-sonnet-4-5",
            "apiProvider": "anthropic"
        });
        let mut seen = HashSet::new();
        let events = parse_document(&document, &mut seen);
        assert_eq!(
            events,
            vec![
                ParsedEvent::SessionMetadata {
                    project: "Discodex".to_string(),
                    cwd: PROJECT_ROOT.to_string(),
                },
                ParsedEvent::AgentDetected {
                    agent: "claude-sonnet-4-5".to_string()
                },
                ParsedEvent::TaskStarted {
                    started_at_ms: 1_758_430_000_000
                },
            ]
        );
    }
}
