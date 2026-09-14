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
    SessionMetadata {
        project: String,
        cwd: String,
    },
    TaskStarted {
        started_at_ms: i64,
    },
    Thinking {
        reasoning: Option<String>,
    },
    ToolStarted {
        activity: ToolActivity,
    },
    ToolFinished,
    TerminalCandidate,
    Completed,
    AgentDetected {
        agent: String,
    },
    SubagentSpawned {
        conversation_id: String,
        agent: String,
    },
}

pub fn parse_line_for(provider: Provider, line: &str) -> Vec<ParsedEvent> {
    match provider {
        Provider::Codex => parse_line(line).into_iter().collect(),
        Provider::ClaudeCode => parse_claude_line(line),
        Provider::Antigravity => parse_antigravity_line(line),
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
        ("turn_context", _) => {
            let model = payload.get("model").and_then(Value::as_str);
            let mode = payload
                .get("collaboration_mode")
                .and_then(|cm| cm.get("mode"))
                .and_then(Value::as_str);
            let agent = match (model, mode) {
                (Some(m), Some("plan")) => Some(format!("Planner ({m})")),
                (Some(m), _) => Some(m.to_string()),
                (None, Some("plan")) => Some("Planner".to_string()),
                _ => None,
            };
            agent.map(|agent| ParsedEvent::AgentDetected { agent })
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
        ("response_item", "reasoning") => {
            let reasoning = payload
                .get("summary")
                .and_then(extract_reasoning_from_summary_value)
                .or_else(|| {
                    payload
                        .get("content")
                        .and_then(extract_reasoning_from_value)
                });
            Some(ParsedEvent::Thinking { reasoning })
        }
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
                    _ => Some(ParsedEvent::Thinking { reasoning: None }),
                }
            } else {
                Some(ParsedEvent::Thinking { reasoning: None })
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
            if let Some(model) = message.get("model").and_then(Value::as_str)
                && let Some(agent) = clean_claude_model(model)
            {
                events.push(ParsedEvent::AgentDetected { agent });
            }

            let tool = content.and_then(|items| {
                items
                    .iter()
                    .find(|item| item.get("type").and_then(Value::as_str) == Some("tool_use"))
            });
            let thinking = content.and_then(|items| {
                items
                    .iter()
                    .find(|item| item.get("type").and_then(Value::as_str) == Some("thinking"))
                    .and_then(|item| item.get("thinking").and_then(Value::as_str))
            });
            let reasoning = thinking.and_then(extract_reasoning_summary);

            if let Some(tool) = tool {
                let name = tool.get("name").and_then(Value::as_str).unwrap_or("");
                let arguments = tool.get("input").cloned();
                if let Some(agent) = extract_claude_subagent(name, arguments.as_ref()) {
                    events.push(ParsedEvent::AgentDetected { agent });
                }
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
                events.push(ParsedEvent::Thinking { reasoning });
            }
        }
        "system" | "progress" => events.push(ParsedEvent::Thinking { reasoning: None }),
        "result" => events.push(ParsedEvent::Completed),
        _ => {}
    }

    events
}

fn parse_antigravity_line(line: &str) -> Vec<ParsedEvent> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let Some(top_type) = value.get("type").and_then(Value::as_str) else {
        return Vec::new();
    };

    let mut events = Vec::new();

    match top_type {
        "USER_INPUT" => {
            let started_at_ms = value
                .get("created_at")
                .and_then(timestamp_ms)
                .unwrap_or_else(current_time_ms);
            events.push(ParsedEvent::TaskStarted { started_at_ms });

            let content = value.get("content").and_then(Value::as_str).unwrap_or("");
            if let Some(agent) = extract_antigravity_agent_from_content(content) {
                events.push(ParsedEvent::AgentDetected { agent });
            }
        }
        "PLANNER_RESPONSE" => {
            let thinking = value.get("thinking").and_then(Value::as_str);
            let reasoning = thinking.and_then(extract_reasoning_summary);

            let tool_calls = value.get("tool_calls").and_then(Value::as_array);
            if let Some(tool_calls) = tool_calls
                && !tool_calls.is_empty()
            {
                if let Some(r) = reasoning {
                    events.push(ParsedEvent::Thinking { reasoning: Some(r) });
                }

                for tool in tool_calls {
                    let name = tool.get("name").and_then(Value::as_str).unwrap_or("");
                    let arguments = tool_arguments(tool);

                    if let Some(metadata) = extract_antigravity_metadata(arguments.as_ref())
                        && !events.contains(&metadata)
                    {
                        events.push(metadata);
                    }

                    if let Some(agent) = extract_antigravity_agent(name, arguments.as_ref()) {
                        events.push(ParsedEvent::AgentDetected { agent });
                    }

                    events.push(ParsedEvent::ToolStarted {
                        activity: tool_activity_from_parts(name, arguments),
                    });
                }
            } else {
                let content = value.get("content").and_then(Value::as_str);
                if content.is_some_and(|c| !c.trim().is_empty()) {
                    events.push(ParsedEvent::TerminalCandidate);
                } else if reasoning.is_some() || thinking.is_some_and(|t| !t.trim().is_empty()) {
                    events.push(ParsedEvent::Thinking { reasoning });
                } else if value.get("status").and_then(Value::as_str) == Some("DONE") {
                    events.push(ParsedEvent::TerminalCandidate);
                }
            }
        }
        "GENERIC" => {
            if value.get("status").and_then(Value::as_str) == Some("DONE") {
                events.push(ParsedEvent::ToolFinished);
            }
            if let Some(content) = value.get("content").and_then(Value::as_str) {
                if let Some(conv_id) = extract_conversation_id_from_generic(content) {
                    events.push(ParsedEvent::SubagentSpawned {
                        conversation_id: conv_id,
                        agent: String::new(),
                    });
                }
                if let Some(metadata) = extract_metadata_from_workspace_uris(content)
                    && !events.contains(&metadata)
                {
                    events.push(metadata);
                }
            }
        }
        _ => {}
    }

    events
}

fn is_internal_path(path_str: &str) -> bool {
    let lower = path_str.to_ascii_lowercase();
    lower.contains(".gemini") || lower.contains(".codex") || lower.contains(".claude")
}

fn detect_project_root(path_str: &str) -> Option<(String, String)> {
    if is_internal_path(path_str) {
        return None;
    }
    let path = Path::new(path_str);
    let start_dir = if path.is_file() || path.extension().is_some() {
        path.parent()?
    } else {
        path
    };

    let mut current = Some(start_dir);
    let mut best_root = None;
    while let Some(dir) = current {
        if dir.join(".git").exists()
            || dir.join("Cargo.toml").exists()
            || dir.join("package.json").exists()
            || dir.join("go.mod").exists()
            || dir.join("pyproject.toml").exists()
        {
            best_root = Some(dir);
            break;
        }
        current = dir.parent();
    }

    let root = best_root.unwrap_or(start_dir);
    let cwd = root.to_string_lossy().to_string();
    let project = project_name(&cwd)?;
    Some((project, cwd))
}

fn extract_antigravity_metadata(arguments: Option<&Value>) -> Option<ParsedEvent> {
    let obj = arguments?;

    if let Some(cwd_str) = obj.get("Cwd").and_then(clean_arg_str)
        && !is_internal_path(&cwd_str)
        && let Some(project) = project_name(&cwd_str)
    {
        return Some(ParsedEvent::SessionMetadata {
            project,
            cwd: cwd_str,
        });
    }

    let path_keys = [
        "AbsolutePath",
        "TargetFile",
        "SearchPath",
        "DirectoryPath",
        "SearchDirectory",
    ];
    for key in path_keys {
        if let Some(path_str) = obj.get(key).and_then(clean_arg_str)
            && let Some((project, cwd)) = detect_project_root(&path_str)
        {
            return Some(ParsedEvent::SessionMetadata { project, cwd });
        }
    }

    None
}

fn clean_arg_str(val: &Value) -> Option<String> {
    let s = val.as_str()?;
    let trimmed = s.trim();
    if trimmed.starts_with('"')
        && trimmed.ends_with('"')
        && trimmed.len() >= 2
        && let Ok(unquoted) = serde_json::from_str::<String>(trimmed)
        && !unquoted.is_empty()
    {
        return Some(unquoted);
    }
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(trimmed);
    if unquoted.is_empty() {
        None
    } else {
        Some(unquoted.to_string())
    }
}

fn tool_activity(payload: &Value) -> ToolActivity {
    let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = tool_arguments(payload);
    tool_activity_from_parts(name, arguments)
}

fn tool_activity_from_parts(name: &str, arguments: Option<Value>) -> ToolActivity {
    let lower_name = name.to_ascii_lowercase();
    let command = arguments
        .as_ref()
        .and_then(|value| {
            value
                .get("cmd")
                .or_else(|| value.get("command"))
                .or_else(|| value.get("CommandLine"))
        })
        .and_then(clean_arg_str)
        .unwrap_or_default();

    let agent_detected = arguments
        .as_ref()
        .and_then(|args| extract_antigravity_agent(&lower_name, Some(args)))
        .or_else(|| {
            arguments
                .as_ref()
                .and_then(|args| extract_claude_subagent(&lower_name, Some(args)))
        });

    let label = if let Some(agent) = agent_detected {
        format!("Running {agent}")
    } else {
        classify_tool(&lower_name, &command).to_string()
    };

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
    let raw = payload
        .get("arguments")
        .or_else(|| payload.get("input"))
        .or_else(|| payload.get("args"))?;
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
    if name == "grep" || name == "glob" || name.contains("find") {
        return "Searching project";
    }
    if name.contains("read")
        || name.contains("open")
        || name.contains("view")
        || name.contains("list")
    {
        return "Reading project files";
    }
    if name == "task"
        || name.contains("agent")
        || name.contains("manage_task")
        || name.contains("schedule")
    {
        return "Running coding agent";
    }
    if name.contains("todo") || name.contains("plan") {
        return "Planning changes";
    }
    if name.contains("request_user_input") || name.contains("ask") {
        return "Waiting for input";
    }

    if name.contains("exec")
        || name.contains("shell")
        || name.contains("command")
        || !command.is_empty()
    {
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
        "write_file" | "edit_file" | "replace_file_content" | "write_to_file" => "Editing files",
        "read_file" | "list_files" | "view_file" | "list_dir" => "Reading project files",
        "grep_search" | "find_by_name" => "Searching project",
        _ => "Using development tools",
    }
}

fn extract_file_basename(arguments: &Value) -> Option<String> {
    for key in [
        "path",
        "file",
        "file_path",
        "filepath",
        "AbsolutePath",
        "TargetFile",
        "SearchPath",
        "DirectoryPath",
        "SearchDirectory",
    ] {
        if let Some(path) = arguments.get(key).and_then(clean_arg_str)
            && let Some(name) = basename(&path)
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

fn strip_list_prefix(line: &str) -> &str {
    let s = line.trim_start();
    if let Some(rest) = s
        .strip_prefix("- ")
        .or_else(|| s.strip_prefix("* "))
        .or_else(|| s.strip_prefix("+ "))
        .or_else(|| s.strip_prefix("• "))
    {
        return rest.trim_start();
    }
    let digits_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
    if digits_end > 0 {
        let rest = &s[digits_end..];
        if let Some(after) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return after.trim_start();
        }
    }
    for prefix in ["step ", "phase "] {
        if s.to_ascii_lowercase().starts_with(prefix) {
            let rest = &s[prefix.len()..];
            let num_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
            if num_end > 0 {
                let after_num = rest[num_end..].trim_start();
                if let Some(after_sep) = after_num
                    .strip_prefix(':')
                    .or_else(|| after_num.strip_prefix('-'))
                    .or_else(|| after_num.strip_prefix('—'))
                {
                    return after_sep.trim_start();
                }
            }
        }
    }
    s
}

fn is_generic_callout(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "note" | "warning" | "caution" | "important" | "tip" | "info" | "remember" | "details"
    )
}

pub fn extract_reasoning_summary(thinking: &str) -> Option<String> {
    let trimmed = thinking.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut latest_heading = None;
    let mut in_code_block = false;

    for line in trimmed.lines() {
        let line_trimmed = line.trim();
        if line_trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block || line_trimmed.is_empty() {
            continue;
        }

        let stripped = strip_list_prefix(line_trimmed);

        // Markdown headings: # Title, ## Title, ### Title
        if let Some(heading) = stripped.strip_prefix('#') {
            let heading = heading.trim_start_matches('#').trim();
            if !heading.is_empty() && !heading.starts_with('[') {
                let cleaned = clean_reasoning_text(heading);
                if !cleaned.is_empty() && !is_generic_callout(&cleaned) {
                    latest_heading = Some(cleaned);
                    continue;
                }
            }
        }

        // Bold step heading at the start of a line: **Title** or **Title:**
        if let Some(rest) = stripped.strip_prefix("**")
            && let Some(end) = rest.find("**")
        {
            let heading = rest[..end].trim();
            if !heading.is_empty() && heading.len() <= 80 {
                let cleaned = clean_reasoning_text(heading);
                if !cleaned.is_empty() && !is_generic_callout(&cleaned) {
                    latest_heading = Some(cleaned);
                    continue;
                }
            }
        }
    }

    latest_heading
}

fn clean_reasoning_text(text: &str) -> String {
    let stripped = text
        .trim()
        .trim_start_matches(['*', '-', '#', '`', ' '])
        .trim_end_matches(['*', '`', ':', ' ']);

    let single_line: String = stripped
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();

    let collapsed = single_line.split_whitespace().collect::<Vec<_>>().join(" ");

    if collapsed.len() > 60 {
        let mut end = 60;
        while end > 0 && !collapsed.is_char_boundary(end) {
            end -= 1;
        }
        if let Some(last_space) = collapsed[..end].rfind(' ')
            && last_space >= 30
        {
            end = last_space;
        }
        format!("{}...", collapsed[..end].trim_end())
    } else {
        collapsed
    }
}

fn extract_reasoning_from_summary_value(val: &Value) -> Option<String> {
    match val {
        Value::String(s) => {
            let cleaned = clean_reasoning_text(s);
            if !cleaned.is_empty() {
                Some(cleaned)
            } else {
                None
            }
        }
        Value::Array(arr) => {
            for item in arr.iter().rev() {
                if let Some(s) = item.as_str() {
                    let cleaned = clean_reasoning_text(s);
                    if !cleaned.is_empty() {
                        return Some(cleaned);
                    }
                } else if let Some(text) = item.get("text").and_then(Value::as_str) {
                    let cleaned = clean_reasoning_text(text);
                    if !cleaned.is_empty() {
                        return Some(cleaned);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn extract_antigravity_agent(name: &str, arguments: Option<&Value>) -> Option<String> {
    let obj = arguments?;
    let lower_name = name.to_ascii_lowercase();
    if (lower_name == "invoke_subagent" || lower_name.contains("agent"))
        && let Some(subagents) = obj.get("Subagents")
    {
        let items: Option<Vec<Value>> = match subagents {
            Value::String(s) => serde_json::from_str(s).ok(),
            Value::Array(arr) => Some(arr.clone()),
            _ => None,
        };
        if let Some(items) = items {
            for item in items {
                if let Some(type_name) = item.get("TypeName").and_then(clean_arg_str) {
                    return Some(normalize_agent_name(&type_name));
                }
                if let Some(role) = item.get("Role").and_then(clean_arg_str) {
                    return Some(normalize_agent_name(&role));
                }
            }
        }
    }
    for key in [
        "agent",
        "agent_name",
        "subagent",
        "subagent_type",
        "TypeName",
    ] {
        if let Some(val) = obj.get(key).and_then(clean_arg_str) {
            return Some(normalize_agent_name(&val));
        }
    }
    None
}

fn normalize_agent_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.starts_with("DeepCoder") {
        "DeepCoder".to_string()
    } else if trimmed.starts_with("DeepInvestigator") {
        "DeepInvestigator".to_string()
    } else if trimmed.len() > 24 {
        let mut end = 21;
        while end > 0 && !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", trimmed[..end].trim_end())
    } else {
        trimmed.to_string()
    }
}

fn clean_claude_model(model: &str) -> Option<String> {
    let lower = model.to_ascii_lowercase();
    if lower.contains("opus") {
        Some("Claude Opus".to_string())
    } else if lower.contains("sonnet") {
        Some("Claude Sonnet".to_string())
    } else if lower.contains("haiku") {
        Some("Claude Haiku".to_string())
    } else if !model.trim().is_empty() && model.len() <= 30 {
        Some(model.trim().to_string())
    } else {
        None
    }
}

fn extract_claude_subagent(name: &str, arguments: Option<&Value>) -> Option<String> {
    let lower_name = name.to_ascii_lowercase();
    if lower_name == "agent" || lower_name == "task" {
        let args = arguments?;
        for key in [
            "subagent_type",
            "agent",
            "type",
            "name",
            "agent_type",
            "role",
        ] {
            if let Some(val) = args.get(key).and_then(clean_arg_str) {
                return Some(normalize_agent_name(&val));
            }
        }
    }
    None
}

fn extract_reasoning_from_value(val: &Value) -> Option<String> {
    match val {
        Value::String(s) => extract_reasoning_summary(s),
        Value::Array(arr) => {
            for item in arr.iter().rev() {
                if let Some(s) = item.as_str() {
                    if let Some(summary) = extract_reasoning_summary(s) {
                        return Some(summary);
                    }
                } else if let Some(text) = item.get("text").and_then(Value::as_str)
                    && let Some(summary) = extract_reasoning_summary(text)
                {
                    return Some(summary);
                }
            }
            None
        }
        _ => None,
    }
}

fn extract_model_selection(content: &str) -> Option<String> {
    let key = "Model Selection`";
    let start = content.find(key)?;
    let rest = &content[start + key.len()..];
    let to_idx = rest.find(" to ")?;
    let rest_to = rest[to_idx + 4..].trim_start();
    let end = rest_to
        .find(". ")
        .or_else(|| rest_to.find(".\n"))
        .or_else(|| rest_to.find(".\r"))
        .or_else(|| rest_to.find('\n'))
        .or_else(|| rest_to.find('<'))
        .unwrap_or(rest_to.len());
    let raw_model = rest_to[..end].trim();
    if raw_model.is_empty() || raw_model == "None" {
        return None;
    }
    let model = if let Some((base, _)) = raw_model.split_once('(') {
        base.trim()
    } else {
        raw_model
    };
    let model = model.trim_matches(|c: char| c == '`' || c == '"' || c == '\'');
    if !model.is_empty() {
        Some(model.to_string())
    } else {
        None
    }
}

fn extract_conversation_id_from_generic(content: &str) -> Option<String> {
    let start = content.find("\"conversationId\"")?;
    let rest = &content[start + "\"conversationId\"".len()..];
    let colon = rest.find(':')?;
    let rest_after_colon = rest[colon + 1..].trim_start();
    let quote_start = rest_after_colon.find('"')?;
    let quote_end = rest_after_colon[quote_start + 1..].find('"')?;
    let id = &rest_after_colon[quote_start + 1..quote_start + 1 + quote_end];
    if !id.is_empty() {
        Some(id.to_string())
    } else {
        None
    }
}

fn extract_metadata_from_workspace_uris(content: &str) -> Option<ParsedEvent> {
    let key = "workspaceUris";
    let start = content.find(key)?;
    let rest = &content[start + key.len()..];
    let uri_start = rest.find("file://")?;
    let rest_uri = &rest[uri_start..];
    let uri_end = rest_uri.find('"')?;
    let uri = &rest_uri[..uri_end];
    let s = uri.strip_prefix("file:///").unwrap_or(uri);
    let s = s.replace("%3A", ":").replace("%3a", ":").replace('/', "\\");
    detect_project_root(&s).map(|(project, cwd)| ParsedEvent::SessionMetadata { project, cwd })
}

fn extract_antigravity_agent_from_content(content: &str) -> Option<String> {
    if let Some(model) = extract_model_selection(content) {
        return Some(model);
    }
    if content.contains("/boost") || content.contains("DeepCoder") {
        return Some("DeepCoder".to_string());
    }
    if content.contains("DeepInvestigator") || content.contains("/investigate") {
        return Some("DeepInvestigator".to_string());
    }
    if content.contains("/planner") {
        return Some("Planner".to_string());
    }
    if content.contains("/fast") {
        return Some("Gemini Flash".to_string());
    }
    None
}

pub fn line_timestamp_ms(line: &str) -> Option<i64> {
    let value: Value = serde_json::from_str(line).ok()?;
    value
        .get("timestamp")
        .or_else(|| value.get("created_at"))
        .and_then(timestamp_ms)
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
        let reasoning_raw =
            r#"{"type":"response_item","payload":{"type":"reasoning","content":["PRIVATE"]}}"#;
        let reasoning_summary = r#"{"type":"response_item","payload":{"type":"reasoning","summary":["Designing schema"]}}"#;
        let tool = r#"{"type":"response_item","payload":{"type":"function_call","name":"exec","arguments":"SECRET"}}"#;
        let output = r#"{"type":"response_item","payload":{"type":"function_call_output","output":"SECRET"}}"#;
        let complete = r#"{"type":"event_msg","payload":{"type":"task_complete"}}"#;

        assert!(matches!(
            parse_line(start),
            Some(ParsedEvent::TaskStarted { .. })
        ));
        // Raw reasoning tokens without headers must never leak
        assert_eq!(
            parse_line(reasoning_raw),
            Some(ParsedEvent::Thinking { reasoning: None })
        );
        // Explicit summaries are safely used
        assert_eq!(
            parse_line(reasoning_summary),
            Some(ParsedEvent::Thinking {
                reasoning: Some("Designing schema".to_string())
            })
        );
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

        let antigravity_line =
            r#"{"created_at":"2026-09-12T06:05:01Z","type":"USER_INPUT","status":"DONE"}"#;
        assert_eq!(line_timestamp_ms(antigravity_line), Some(1_789_193_101_000));
    }

    #[test]
    fn assistant_commentary_stays_active_and_final_answer_becomes_terminal_candidate() {
        let commentary = r#"{"type":"response_item","payload":{"type":"message","role":"assistant","phase":"commentary","content":[{"text":"TOP SECRET PROMPT CONTENT"}]}}"#;
        assert_eq!(
            parse_line(commentary),
            Some(ParsedEvent::Thinking { reasoning: None })
        );

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

    #[test]
    fn parses_antigravity_session_events() {
        let start = r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-09-12T06:05:01Z","content":"<USER_REQUEST>\nimplement it\n</USER_REQUEST>"}"#;
        let events = parse_line_for(Provider::Antigravity, start);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(events[0], ParsedEvent::TaskStarted { started_at_ms } if started_at_ms == 1_789_193_101_000)
        );

        let view_file = r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-09-12T06:05:04Z","tool_calls":[{"name":"view_file","args":{"AbsolutePath":"\"c:\\\\Users\\\\PC\\\\Documents\\\\Discodex\\\\src\\\\provider.rs\"","StartLine":"1","EndLine":"150"}}]}"#;
        let events = parse_line_for(Provider::Antigravity, view_file);
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0],
            ParsedEvent::SessionMetadata {
                project: "Discodex".to_string(),
                cwd: r"c:\Users\PC\Documents\Discodex".to_string(),
            }
        );
        assert_eq!(
            events[1],
            ParsedEvent::ToolStarted {
                activity: ToolActivity {
                    label: "Reading project files".to_string(),
                    file: Some("provider.rs".to_string()),
                    language: Some("Rust".to_string()),
                }
            }
        );

        let command = r#"{"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-09-12T06:05:10Z","tool_calls":[{"name":"run_command","args":{"CommandLine":"cargo test","Cwd":"c:\\Users\\PC\\Documents\\Discodex"}}]}"#;
        let events = parse_line_for(Provider::Antigravity, command);
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0],
            ParsedEvent::SessionMetadata {
                project: "Discodex".to_string(),
                cwd: r"c:\Users\PC\Documents\Discodex".to_string(),
            }
        );
        assert_eq!(
            events[1],
            ParsedEvent::ToolStarted {
                activity: ToolActivity {
                    label: "Running Rust tests".to_string(),
                    file: None,
                    language: None,
                }
            }
        );

        let tool_finished = r#"{"step_index":3,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-09-12T06:05:12Z","content":"Created At: ...\nCompleted At: ..."}"#;
        let events = parse_line_for(Provider::Antigravity, tool_finished);
        assert_eq!(events, vec![ParsedEvent::ToolFinished]);

        let thinking = r#"{"step_index":4,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-09-12T06:05:13Z","thinking":"**Planning next changes**\nReviewing files...","tool_calls":[]}"#;
        let events = parse_line_for(Provider::Antigravity, thinking);
        assert_eq!(
            events,
            vec![ParsedEvent::Thinking {
                reasoning: Some("Planning next changes".to_string())
            }]
        );

        let final_answer = r#"{"step_index":5,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-09-12T06:05:15Z","content":"I have implemented the changes.","tool_calls":[]}"#;
        let events = parse_line_for(Provider::Antigravity, final_answer);
        assert_eq!(events, vec![ParsedEvent::TerminalCandidate]);
    }

    #[test]
    fn extracts_reasoning_summary_from_bold_and_markdown_headers() {
        assert_eq!(
            extract_reasoning_summary(
                "**Evaluating Request and Identity**\n\nThe task involves modifying Discord..."
            ),
            Some("Evaluating Request and Identity".to_string())
        );
        assert_eq!(
            extract_reasoning_summary("# Refactoring notification payload\n\nLooking into..."),
            Some("Refactoring notification payload".to_string())
        );
        assert_eq!(
            extract_reasoning_summary("## Optimizing Cache Layer:\nWorking on cache..."),
            Some("Optimizing Cache Layer".to_string())
        );
        // Returns the latest heading in multi-step thought
        assert_eq!(
            extract_reasoning_summary(
                "**Step 1: Reading Files**\nRead done.\n\n**Step 2: Executing Fix**\nRunning..."
            ),
            Some("Step 2: Executing Fix".to_string())
        );
        // Supports numbered and bulleted step headings
        assert_eq!(
            extract_reasoning_summary("1. **Evaluating Request and Identity**\nTesting..."),
            Some("Evaluating Request and Identity".to_string())
        );
        assert_eq!(
            extract_reasoning_summary("- **Optimizing Cache Layer**\nWorking..."),
            Some("Optimizing Cache Layer".to_string())
        );
        assert_eq!(
            extract_reasoning_summary("Step 1: **Fixing Build**\nCompiling..."),
            Some("Fixing Build".to_string())
        );
        // Ignores generic callouts like Note and Warning
        assert_eq!(
            extract_reasoning_summary("**Note:** This is an implementation detail."),
            None
        );
        assert_eq!(
            extract_reasoning_summary("**Warning:** High memory usage detected."),
            None
        );
        // Rejects inline bold and raw chain of thought to prevent leaking private thoughts
        assert_eq!(
            extract_reasoning_summary("The function is **deprecated** in this version."),
            None
        );
        assert_eq!(
            extract_reasoning_summary("Looking up secret auth token in environment variable"),
            None
        );
        // Rejects headers inside code blocks
        assert_eq!(
            extract_reasoning_summary("```rust\n# [derive(Debug)]\nstruct Test;\n```"),
            None
        );
    }

    #[test]
    fn extracts_agent_from_boost_prompt_and_subagent_invocation() {
        let boost_start = r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-09-12T06:05:01Z","content":"<USER_REQUEST>\n/boost can u add the agent + reasoning in the discord noti\n</USER_REQUEST>"}"#;
        let events = parse_line_for(Provider::Antigravity, boost_start);
        assert!(events.contains(&ParsedEvent::AgentDetected {
            agent: "DeepCoder".to_string()
        }));

        let subagent_call = r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-09-12T06:05:04Z","tool_calls":[{"name":"invoke_subagent","args":{"Subagents":"[{\"TypeName\":\"DeepInvestigator\",\"Role\":\"Investigator\"}]"}}]}"#;
        let events = parse_line_for(Provider::Antigravity, subagent_call);
        assert!(events.contains(&ParsedEvent::AgentDetected {
            agent: "DeepInvestigator".to_string()
        }));
        assert!(events.iter().any(|e| matches!(
            e,
            ParsedEvent::ToolStarted { activity } if activity.label == "Running DeepInvestigator"
        )));
    }

    #[test]
    fn extracts_agent_from_model_selection_and_subagent_generic_response() {
        let model_change = r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-09-12T06:05:01Z","content":"<USER_SETTINGS_CHANGE>\nThe user changed setting `Model Selection` from None to Gemini 3.8 Flash (High). No need to comment...\n</USER_SETTINGS_CHANGE>"}"#;
        let events = parse_line_for(Provider::Antigravity, model_change);
        assert!(events.contains(&ParsedEvent::AgentDetected {
            agent: "Gemini 3.8 Flash".to_string()
        }));

        let subagent_result = r#"{"step_index":3,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-09-12T06:05:05Z","content":"Created the following subagents:\n{\n  \"conversationId\": \"ded4c290-aa9c-4bbe-8021-dbf5bb37ed96\",\n  \"workspaceUris\": [\"file:///c%3A/Users/PC/Documents/Discodex\"]\n}"}"#;
        let events = parse_line_for(Provider::Antigravity, subagent_result);
        assert!(events.contains(&ParsedEvent::SubagentSpawned {
            conversation_id: "ded4c290-aa9c-4bbe-8021-dbf5bb37ed96".to_string(),
            agent: String::new(),
        }));
        assert!(events.contains(&ParsedEvent::SessionMetadata {
            project: "Discodex".to_string(),
            cwd: r"c:\Users\PC\Documents\Discodex".to_string(),
        }));
    }
}
