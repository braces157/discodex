use std::path::Path;

use chrono::DateTime;
use serde_json::Value;

use crate::provider::Provider;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolActivity {
    pub label: String,
    pub file: Option<String>,
    pub language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedEvent {
    SessionMetadata { project: String, cwd: String },
    TaskStarted { started_at_ms: i64 },
    Thinking,
    ToolStarted { activity: ToolActivity },
    ToolFinished,
    TerminalCandidate,
    Completed,
}

pub fn parse_line_for(provider: Provider, line: &str) -> Vec<ParsedEvent> {
    match provider {
        Provider::Codex => parse_line(line).into_iter().collect(),
        Provider::ClaudeCode => parse_claude_line(line),
        _ => Vec::new(),
    }
}

pub fn parse_line(line: &str) -> Option<ParsedEvent> {
    let value: Value = serde_json::from_str(line).ok()?;
    let top_type = value.get("type")?.as_str()?;
    let payload = value.get("payload").unwrap_or(&Value::Null);
    let payload_type = payload.get("type").and_then(Value::as_str).unwrap_or("");

    match (top_type, payload_type) {
        ("session_meta", _) => {
            let cwd = payload.get("cwd")?.as_str()?;
            let project = project_name(cwd)?;
            Some(ParsedEvent::SessionMetadata {
                project,
                cwd: cwd.to_string(),
            })
        }
        ("event_msg", "task_started") => {
            let started_at_ms = payload
                .get("started_at")
                .and_then(timestamp_ms)
                .or_else(|| value.get("timestamp").and_then(timestamp_ms))
                .unwrap_or_else(current_time_ms);
            Some(ParsedEvent::TaskStarted { started_at_ms })
        }
        ("event_msg", "task_complete" | "task_completed" | "task_cancelled" | "task_failed") => {
            Some(ParsedEvent::Completed)
        }
        ("response_item", "reasoning") => Some(ParsedEvent::Thinking),
        ("response_item", "function_call" | "custom_tool_call") => Some(ParsedEvent::ToolStarted {
            activity: tool_activity(payload),
        }),
        ("response_item", "function_call_output" | "custom_tool_call_output") => {
            Some(ParsedEvent::ToolFinished)
        }
        ("response_item", "message") => {
            if payload.get("role").and_then(Value::as_str) == Some("assistant") {
                match payload.get("phase").and_then(Value::as_str) {
                    Some("final_answer" | "final") => Some(ParsedEvent::TerminalCandidate),
                    _ => Some(ParsedEvent::Thinking),
                }
            } else {
                Some(ParsedEvent::Thinking)
            }
        }
        _ => None,
    }
}

fn parse_claude_line(line: &str) -> Vec<ParsedEvent> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let Some(top_type) = value.get("type").and_then(Value::as_str) else {
        return Vec::new();
    };

    let mut events = Vec::new();
    if let Some(cwd) = value.get("cwd").and_then(Value::as_str)
        && let Some(project) = project_name(cwd)
    {
        events.push(ParsedEvent::SessionMetadata {
            project,
            cwd: cwd.to_string(),
        });
    }

    let message = value.get("message").unwrap_or(&Value::Null);
    let content = message.get("content").and_then(Value::as_array);
    match top_type {
        "user" => {
            if content.is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.get("type").and_then(Value::as_str) == Some("tool_result"))
            }) {
                events.push(ParsedEvent::ToolFinished);
            } else {
                let started_at_ms = value
                    .get("timestamp")
                    .and_then(timestamp_ms)
                    .unwrap_or_else(current_time_ms);
                events.push(ParsedEvent::TaskStarted { started_at_ms });
            }
        }
        "assistant" => {
            let tool = content.and_then(|items| {
                items
                    .iter()
                    .find(|item| item.get("type").and_then(Value::as_str) == Some("tool_use"))
            });
            if let Some(tool) = tool {
                let name = tool.get("name").and_then(Value::as_str).unwrap_or("");
                let arguments = tool.get("input").cloned();
                events.push(ParsedEvent::ToolStarted {
                    activity: tool_activity_from_parts(name, arguments),
                });
            } else if content.is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.get("type").and_then(Value::as_str) == Some("text"))
            }) {
                events.push(ParsedEvent::TerminalCandidate);
            } else {
                events.push(ParsedEvent::Thinking);
            }
        }
        "system" | "progress" => events.push(ParsedEvent::Thinking),
        "result" => events.push(ParsedEvent::Completed),
        _ => {}
    }

    events
}

fn tool_activity(payload: &Value) -> ToolActivity {
    let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = tool_arguments(payload);
    tool_activity_from_parts(name, arguments)
}

fn tool_activity_from_parts(name: &str, arguments: Option<Value>) -> ToolActivity {
    let name = name.to_ascii_lowercase();
    let command = arguments
        .as_ref()
        .and_then(|value| value.get("cmd").or_else(|| value.get("command")))
        .and_then(Value::as_str)
        .unwrap_or("");

    let label = classify_tool(&name, command).to_string();
    let file = arguments.as_ref().and_then(extract_file_basename);
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

fn tool_arguments(payload: &Value) -> Option<Value> {
    let raw = payload.get("arguments").or_else(|| payload.get("input"))?;
    match raw {
        Value::String(text) => serde_json::from_str(text).ok(),
        Value::Object(_) => Some(raw.clone()),
        _ => None,
    }
}

fn classify_tool(name: &str, command: &str) -> &'static str {
    let command = command.to_ascii_lowercase();

    if name.contains("apply_patch")
        || name == "patch"
        || name == "edit"
        || name == "write"
        || name.contains("multiedit")
    {
        return "Editing files";
    }
    if name.contains("view_image") || name.contains("image") {
        return "Inspecting image";
    }
    if name.contains("browser") || name.contains("web") || name.contains("search") {
        return "Browsing documentation";
    }
    if name == "grep" || name == "glob" {
        return "Searching project";
    }
    if name.contains("read") || name.contains("open") || name.contains("find") {
        return "Reading project files";
    }
    if name == "task" || name.contains("agent") {
        return "Running coding agent";
    }
    if name.contains("todo") || name.contains("plan") {
        return "Planning changes";
    }
    if name.contains("request_user_input") {
        return "Waiting for input";
    }

    if name.contains("exec") || name.contains("shell") || !command.is_empty() {
        if command.contains("cargo test") {
            return "Running Rust tests";
        }
        if command.contains("cargo build") {
            return "Building Rust project";
        }
        if command.contains("cargo check") {
            return "Checking Rust project";
        }
        if command.contains("cargo clippy") {
            return "Running Clippy";
        }
        if command.contains("cargo fmt") {
            return "Formatting Rust code";
        }
        if command.contains("npm test")
            || command.contains("pnpm test")
            || command.contains("yarn test")
            || command.contains("vitest")
            || command.contains("jest")
        {
            return "Running JavaScript tests";
        }
        if command.contains("npm run build")
            || command.contains("pnpm build")
            || command.contains("yarn build")
        {
            return "Building web app";
        }
        if command.contains("dotnet test") {
            return "Running .NET tests";
        }
        if command.contains("dotnet build") {
            return "Building .NET project";
        }
        if command.contains("mvn test") || command.contains("gradle test") {
            return "Running Java tests";
        }
        if command.contains("mvn ") || command.contains("gradle ") {
            return "Building Java project";
        }
        if command.contains("git diff")
            || command.contains("git status")
            || command.contains("git log")
        {
            return "Reviewing repository";
        }
        if command.contains("rg ") || command.contains("ripgrep") {
            return "Searching project";
        }
        if command.contains("python ") || command.contains("python3 ") {
            return "Running Python";
        }
        return "Running command";
    }

    match name {
        "write_file" | "edit_file" | "replace_file_content" => "Editing files",
        "read_file" | "list_files" => "Reading project files",
        _ => "Using development tools",
    }
}

fn extract_file_basename(arguments: &Value) -> Option<String> {
    for key in ["path", "file", "file_path", "filepath"] {
        if let Some(path) = arguments.get(key).and_then(Value::as_str)
            && let Some(name) = basename(path)
        {
            return Some(name);
        }
    }

    let patch = arguments.get("patch").and_then(Value::as_str)?;
    for line in patch.lines() {
        let path = line
            .strip_prefix("*** Update File: ")
            .or_else(|| line.strip_prefix("*** Add File: "))
            .or_else(|| line.strip_prefix("*** Delete File: "));
        if let Some(path) = path
            && let Some(name) = basename(path.trim())
        {
            return Some(name);
        }
    }
    None
}

fn basename(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");
    normalized
        .rsplit('/')
        .find(|part| !part.is_empty())
        .map(str::to_string)
}

fn language_from_file(file: &str) -> Option<&'static str> {
    let extension = file.rsplit_once('.')?.1.to_ascii_lowercase();
    match extension.as_str() {
        "rs" => Some("Rust"),
        "ts" => Some("TypeScript"),
        "tsx" => Some("TypeScript / React"),
        "js" | "mjs" | "cjs" => Some("JavaScript"),
        "jsx" => Some("JavaScript / React"),
        "java" => Some("Java"),
        "kt" | "kts" => Some("Kotlin"),
        "py" => Some("Python"),
        "go" => Some("Go"),
        "cs" => Some("C#"),
        "cpp" | "cc" | "cxx" | "hpp" => Some("C++"),
        "c" | "h" => Some("C"),
        "swift" => Some("Swift"),
        "php" => Some("PHP"),
        "rb" => Some("Ruby"),
        "html" | "htm" => Some("HTML"),
        "css" => Some("CSS"),
        "scss" | "sass" => Some("Sass"),
        "vue" => Some("Vue"),
        "svelte" => Some("Svelte"),
        "sql" => Some("SQL"),
        "md" => Some("Markdown"),
        "json" | "toml" | "yaml" | "yml" => Some("Configuration"),
        _ => None,
    }
}

pub fn line_timestamp_ms(line: &str) -> Option<i64> {
    let value: Value = serde_json::from_str(line).ok()?;
    value.get("timestamp").and_then(timestamp_ms)
}

fn project_name(cwd: &str) -> Option<String> {
    Path::new(cwd)
        .components()
        .next_back()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
}

fn timestamp_ms(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(if number.abs() < 10_000_000_000 {
            number.saturating_mul(1_000)
        } else {
            number
        });
    }
    if let Some(number) = value.as_f64() {
        let millis = if number.abs() < 10_000_000_000.0 {
            number * 1_000.0
        } else {
            number
        };
        return Some(millis as i64);
    }
    let text = value.as_str()?;
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.timestamp_millis())
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

    #[test]
    fn parses_session_metadata_without_exposing_path() {
        let line =
            r#"{"type":"session_meta","payload":{"cwd":"C:\\Users\\me\\Documents\\Discodex"}}"#;
        assert_eq!(
            parse_line(line),
            Some(ParsedEvent::SessionMetadata {
                project: "Discodex".to_string(),
                cwd: r"C:\Users\me\Documents\Discodex".to_string(),
            })
        );
    }

    #[test]
    fn parses_task_start_reasoning_tools_and_completion() {
        let start = r#"{"timestamp":"2026-09-12T04:00:00Z","type":"event_msg","payload":{"type":"task_started"}}"#;
        let reasoning =
            r#"{"type":"response_item","payload":{"type":"reasoning","content":["PRIVATE"]}}"#;
        let tool = r#"{"type":"response_item","payload":{"type":"function_call","name":"exec","arguments":"SECRET"}}"#;
        let output = r#"{"type":"response_item","payload":{"type":"function_call_output","output":"SECRET"}}"#;
        let complete = r#"{"type":"event_msg","payload":{"type":"task_complete"}}"#;

        assert!(matches!(
            parse_line(start),
            Some(ParsedEvent::TaskStarted { .. })
        ));
        assert_eq!(parse_line(reasoning), Some(ParsedEvent::Thinking));
        assert_eq!(
            parse_line(tool),
            Some(ParsedEvent::ToolStarted {
                activity: ToolActivity {
                    label: "Running command".to_string(),
                    file: None,
                    language: None,
                }
            })
        );
        assert_eq!(parse_line(output), Some(ParsedEvent::ToolFinished));
        assert_eq!(parse_line(complete), Some(ParsedEvent::Completed));
    }

    #[test]
    fn turns_tool_details_into_safe_human_readable_activity() {
        let line = r#"{"type":"response_item","payload":{"type":"function_call","name":"apply_patch","arguments":"{\"patch\":\"*** Update File: src/tray.rs\\n@@\"}"}}"#;
        assert_eq!(
            parse_line(line),
            Some(ParsedEvent::ToolStarted {
                activity: ToolActivity {
                    label: "Editing files".to_string(),
                    file: Some("tray.rs".to_string()),
                    language: Some("Rust".to_string()),
                }
            })
        );

        let build = r#"{"type":"response_item","payload":{"type":"function_call","name":"exec","arguments":"{\"cmd\":\"cargo build --release\"}"}}"#;
        assert_eq!(
            parse_line(build),
            Some(ParsedEvent::ToolStarted {
                activity: ToolActivity {
                    label: "Building Rust project".to_string(),
                    file: None,
                    language: None,
                }
            })
        );
    }

    #[test]
    fn numeric_codex_start_time_is_normalized_to_milliseconds() {
        let line =
            r#"{"type":"event_msg","payload":{"type":"task_started","started_at":1789187508}}"#;
        assert_eq!(
            parse_line(line),
            Some(ParsedEvent::TaskStarted {
                started_at_ms: 1_789_187_508_000
            })
        );
    }

    #[test]
    fn extracts_top_level_event_timestamp() {
        let line = r#"{"timestamp":"2026-09-12T05:08:33Z","type":"event_msg","payload":{"type":"task_started","started_at":1789189713}}"#;
        assert_eq!(line_timestamp_ms(line), Some(1_789_189_713_000));
    }

    #[test]
    fn assistant_commentary_stays_active_and_final_answer_becomes_terminal_candidate() {
        let commentary = r#"{"type":"response_item","payload":{"type":"message","role":"assistant","phase":"commentary","content":[{"text":"TOP SECRET PROMPT CONTENT"}]}}"#;
        assert_eq!(parse_line(commentary), Some(ParsedEvent::Thinking));

        let final_answer = r#"{"type":"response_item","payload":{"type":"message","role":"assistant","phase":"final_answer","content":[{"text":"TOP SECRET PROMPT CONTENT"}]}}"#;
        assert_eq!(
            parse_line(final_answer),
            Some(ParsedEvent::TerminalCandidate)
        );
    }

    #[test]
    fn malformed_or_incomplete_json_is_ignored() {
        assert_eq!(parse_line("{"), None);
        assert_eq!(parse_line("not json"), None);
    }

    #[test]
    fn parses_claude_code_session_without_forwarding_message_text() {
        let user = r#"{"type":"user","cwd":"C:\\work\\Discodex","timestamp":"2026-09-12T05:00:00Z","message":{"role":"user","content":"PRIVATE PROMPT"}}"#;
        let events = parse_line_for(Provider::ClaudeCode, user);
        assert_eq!(
            events[0],
            ParsedEvent::SessionMetadata {
                project: "Discodex".to_string(),
                cwd: r"C:\work\Discodex".to_string(),
            }
        );
        assert!(matches!(events[1], ParsedEvent::TaskStarted { .. }));

        let tool = r#"{"type":"assistant","cwd":"C:\\work\\Discodex","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"C:\\work\\Discodex\\src\\main.rs","new_string":"PRIVATE"}}]}}"#;
        let events = parse_line_for(Provider::ClaudeCode, tool);
        assert!(matches!(
            events.last(),
            Some(ParsedEvent::ToolStarted {
                activity: ToolActivity {
                    label,
                    file: Some(file),
                    language: Some(language),
                }
            }) if label == "Editing files" && file == "main.rs" && language == "Rust"
        ));
    }
}
