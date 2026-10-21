//! Aider adapter: the Markdown transcript of the chat
//!
//! Aider writes a human readable Markdown transcript into the working tree of
//! the repository it edits (`.aider.chat.history.md`, `aider/args.py:274-276`).
//! The file is opened in append mode (`aider/io.py:1128-1132`), so it is an
//! append only log of every session of that repository.
//!
//! The transcript holds the prompts of the user, the answers of the model, the
//! diffs of the edits and, for the coders that call functions, the raw
//! arguments of the tool calls. The adapter is therefore an allow list: only
//! the shape of a line decides what is published. A line that is not known
//! produces no event at all, and no text of a line is ever copied into an
//! event.
//!
//! The known shapes are the ones of the upstream sources:
//!
//! - `# aider chat started at 2026-10-21 08:13:45` - the session header
//!   (`aider/io.py:335-336`). It carries the local start time without a
//!   timezone, so only the workspace is taken from it.
//! - `#### <user message>` - a new user turn (`aider/io.py:775-789`). The
//!   message is user content and never leaves this module.
//! - `> Applied edit to <path>` and `> Did not apply edit to <path>` - an edit
//!   of a file (`aider/coders/base_coder.py:2330-2334`).
//! - `> Added <path> to the chat.` - a file that joined the context
//!   (`aider/coders/base_coder.py:283-287`).
//! - `> Committing <path> before applying edits.` - a commit of a file
//!   (`aider/coders/base_coder.py:2188`).
//! - `> Running <command>` - a shell command (`aider/coders/base_coder.py:2472`).
//!   The raw command must never be published, only a fixed label is.
//! - `> Tokens: 5,244 sent, 604 received. Cost: $0.02` - the end of a model
//!   turn (`aider/coders/base_coder.py:2023`).
//! - `> Main model: <model> with <format> edit format` - the model of the
//!   session (`aider/coders/base_coder.py:207-248`).

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::events::{
    ParsedEvent, ToolActivity, basename, classify_tool, language_from_file, project_name,
};

/// The prefix of the session header.
const SESSION_HEADER: &str = "# aider chat started at ";

/// The prefix of a user turn.
const USER_TURN: &str = "####";

/// The prefix of every tool line.
const TOOL_LINE: &str = "> ";

/// The longest model id that is published.
const MAX_MODEL_ID_LEN: usize = 128;

/// The longest file name that is published.
const MAX_BASENAME_LEN: usize = 128;

/// Parse one line of the harness' Markdown transcript.
///
/// Aider writes a human readable transcript into the working tree of the
/// repository it edits, so the workspace is resolved from the path of the
/// transcript.
pub fn parse_line_at(path: &Path, line: &str) -> Vec<ParsedEvent> {
    // Every blockquote line ends with two spaces, the Markdown hard break of
    // `aider/io.py:1117-1127`, and the transcript may use CRLF endings.
    let line = line.trim_end();

    // A new user turn starts the elapsed timer. The text of the turn is user
    // content: it is not read at all.
    if line.starts_with(USER_TURN) {
        return vec![ParsedEvent::TaskStarted {
            started_at_ms: now_ms(),
        }];
    }

    // The header names the session. Its timestamp is local time without a
    // timezone, so it is not used as the start of a turn: only the workspace
    // comes from this line.
    if let Some(rest) = line.strip_prefix(SESSION_HEADER)
        && is_session_timestamp(rest)
    {
        return session_metadata(path);
    }

    // Every tool line is a blockquote.
    if let Some(rest) = line.strip_prefix(TOOL_LINE) {
        return parse_tool_line(rest);
    }

    // Everything else is assistant prose and is dropped.
    Vec::new()
}

/// The session of a transcript.
///
/// The transcript lives in the root of the repository, so its parent
/// directory is the workspace of the session.
fn session_metadata(path: &Path) -> Vec<ParsedEvent> {
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    let cwd = parent.to_string_lossy().to_string();
    if cwd.is_empty() {
        return Vec::new();
    }
    let Some(project) = project_name(&cwd) else {
        return Vec::new();
    };
    vec![ParsedEvent::SessionMetadata { project, cwd }]
}

/// Parse the text of a blockquote line.
///
/// Only the fixed prefixes of the upstream messages are known. Everything else
/// is dropped, so a line of the assistant that looks like a tool line cannot
/// smuggle text into an event.
fn parse_tool_line(text: &str) -> Vec<ParsedEvent> {
    // An edit that was applied to a file.
    if let Some(path) = text.strip_prefix("Applied edit to ") {
        return file_activity(path, "Editing files", true);
    }

    // An edit that was only tried out.
    if let Some(path) = text.strip_prefix("Did not apply edit to ") {
        let path = path.strip_suffix(" (--dry-run)").unwrap_or(path);
        return file_activity(path, "Editing files", true);
    }

    // A file that joined the context of the chat.
    if let Some(rest) = text.strip_prefix("Added ")
        && let Some(path) = rest.strip_suffix(" to the chat.")
    {
        return file_activity(path, "Reading project files", true);
    }

    // A commit before the edits are applied.
    if let Some(rest) = text.strip_prefix("Committing ")
        && let Some(path) = rest.strip_suffix(" before applying edits.")
    {
        return file_activity(path, "Reviewing repository", false);
    }

    // A command that was run. The command itself only decides the label and
    // is dropped here, it never leaves this function.
    if let Some(command) = text.strip_prefix("Running ") {
        return vec![
            ParsedEvent::ToolStarted {
                activity: ToolActivity {
                    label: classify_tool("bash", command).to_string(),
                    file: None,
                    language: None,
                },
            },
            ParsedEvent::ToolFinished,
        ];
    }

    // The end of a model turn.
    if let Some(rest) = text.strip_prefix("Tokens:") {
        return if is_token_report(rest) {
            vec![ParsedEvent::TerminalCandidate]
        } else {
            Vec::new()
        };
    }

    // The model of the session. The banner of the current version reads
    // "<model> with <edit format> edit format", the older one reads
    // "<model> with <edit format> edit format, weak model <weak>".
    //
    // Only the main model is published. The weak model and the editor model
    // are helper models of Aider and would replace the model of the session in
    // the presence.
    for prefix in ["Main model:", "Models:", "Model:"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return model_event(rest);
        }
    }

    // Unknown shape.
    Vec::new()
}

/// The activity of a file of the repository.
///
/// Only the name of the file is published, the path of the repository is
/// dropped.
fn file_activity(path: &str, label: &str, with_language: bool) -> Vec<ParsedEvent> {
    let file = basename(path).and_then(publishable_basename);
    let language = if with_language {
        file.as_deref()
            .and_then(language_from_file)
            .map(str::to_string)
    } else {
        None
    };
    vec![
        ParsedEvent::ToolStarted {
            activity: ToolActivity {
                label: label.to_string(),
                file,
                language,
            },
        },
        ParsedEvent::ToolFinished,
    ]
}

/// The model of the banner.
///
/// The model id is the part before the edit format. It is published only when
/// it looks like a model id, so a line of prose is dropped.
fn model_event(rest: &str) -> Vec<ParsedEvent> {
    let model = rest.split(" with ").next().unwrap_or_default().trim();
    match sanitize_model_id(model) {
        Some(agent) => vec![ParsedEvent::AgentDetected { agent }],
        None => Vec::new(),
    }
}

/// Keep only a model id that looks like a model id.
///
/// Model ids are metadata. Prose, control characters and long strings are
/// dropped instead of being forwarded.
fn sanitize_model_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_MODEL_ID_LEN {
        return None;
    }
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'/' | b'@' | b'-'))
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// Keep only a file name that looks like a file name.
///
/// A name with whitespace or with a control character is prose, not a file
/// name, and is dropped instead of being forwarded.
fn publishable_basename(name: String) -> Option<String> {
    if name.is_empty() || name.len() > MAX_BASENAME_LEN {
        return None;
    }
    if name == "." || name == ".." {
        return None;
    }
    if !name.bytes().all(|byte| byte.is_ascii_graphic()) {
        return None;
    }
    Some(name)
}

/// Whether the text of a header is a timestamp.
///
/// The header carries the local wall clock of the session in the shape
/// `YYYY-MM-DD HH:MM:SS`. A header without that shape is not a header.
fn is_session_timestamp(text: &str) -> bool {
    let text = text.trim();
    let bytes = text.as_bytes();
    if bytes.len() != 19 {
        return false;
    }
    // The separators of the shape.
    let separators = [4usize, 7, 10, 13, 16];
    for index in separators {
        if !matches!(bytes[index], b'-' | b' ' | b':') {
            return false;
        }
    }
    bytes
        .iter()
        .enumerate()
        .all(|(index, byte)| separators.contains(&index) || byte.is_ascii_digit())
}

/// Whether the text of a line is the report of the tokens of a turn.
///
/// The report reads `<sent> sent, <received> received`, followed by the cost
/// of the turn.
fn is_token_report(text: &str) -> bool {
    let text = text.trim();
    let Some((sent, rest)) = text.split_once(" sent") else {
        return false;
    };
    let Some((received, _)) = rest.split_once(" received") else {
        return false;
    };
    is_count(sent) && is_count(received)
}

/// Whether the text is a number of tokens.
///
/// Aider groups the thousands of a number, so the separators are part of the
/// number.
fn is_count(text: &str) -> bool {
    let text = text.trim().trim_matches(',').trim();
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b',' || byte == b'.')
}

/// The current time in milliseconds.
///
/// The transcript has no timestamp for a turn, so the time of the observation
/// is used for the elapsed timer.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// The path of a transcript inside a repository.
    fn transcript() -> PathBuf {
        PathBuf::from(r"C:\Users\PC\Documents\MyRepo").join(".aider.chat.history.md")
    }

    /// The workspace of the transcript of the tests.
    fn workspace() -> String {
        r"C:\Users\PC\Documents\MyRepo".to_string()
    }

    #[test]
    fn maps_a_transcript_to_normalized_events() {
        let path = transcript();
        let lines = [
            "# aider chat started at 2026-10-21 08:13:45",
            "> Aider v0.90.0",
            "> Main model: gpt-4o with diff edit format",
            "> Git repo: .git with 300 files",
            "> Repo-map: using 1024 tokens, auto refresh",
            "> Added src/main.rs to the chat.",
            "#### refactor the parser please",
            "> Running cargo test",
            "> Applied edit to src/main.rs",
            "> Tokens: 1,024 sent, 128 received. Cost: $0.0123 request, $0.02 session.",
        ];

        let events: Vec<ParsedEvent> = lines
            .iter()
            .flat_map(|line| parse_line_at(&path, line))
            .collect();

        // The transcript has no timestamp for a turn, so the time of the
        // observation starts the elapsed timer.
        let turns: Vec<&ParsedEvent> = events
            .iter()
            .filter(|event| matches!(event, ParsedEvent::TaskStarted { .. }))
            .collect();
        assert_eq!(turns.len(), 1);
        match turns[0] {
            ParsedEvent::TaskStarted { started_at_ms } => {
                assert!(*started_at_ms > 0);
                assert!((now_ms() - *started_at_ms).abs() < 60_000);
            }
            _ => unreachable!(),
        }

        let others: Vec<ParsedEvent> = events
            .iter()
            .filter(|event| !matches!(event, ParsedEvent::TaskStarted { .. }))
            .cloned()
            .collect();

        assert_eq!(
            others,
            vec![
                ParsedEvent::SessionMetadata {
                    project: "MyRepo".to_string(),
                    cwd: workspace(),
                },
                ParsedEvent::AgentDetected {
                    agent: "gpt-4o".to_string(),
                },
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label: "Reading project files".to_string(),
                        file: Some("main.rs".to_string()),
                        language: Some("Rust".to_string()),
                    },
                },
                ParsedEvent::ToolFinished,
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label: "Running Rust tests".to_string(),
                        file: None,
                        language: None,
                    },
                },
                ParsedEvent::ToolFinished,
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label: "Editing files".to_string(),
                        file: Some("main.rs".to_string()),
                        language: Some("Rust".to_string()),
                    },
                },
                ParsedEvent::ToolFinished,
                ParsedEvent::TerminalCandidate,
            ]
        );
    }

    #[test]
    fn never_forwards_prompts_answers_commands_or_paths() {
        let path = transcript();
        let lines = [
            "#### my password is hunter2 and my key is sk-abc123",
            "Sure, I will run that command for you right away.",
            "> Running rm -rf C:/Users/PC/Vaults --token=SECRET_TOKEN",
            "> Applied edit to Vaults/important.rs",
            "I changed the file and the tests pass now.",
        ];

        let events: Vec<ParsedEvent> = lines
            .iter()
            .flat_map(|line| parse_line_at(&path, line))
            .collect();
        let debug = format!("{events:?}");

        for secret in [
            "hunter2",
            "sk-abc123",
            "SECRET_TOKEN",
            "rm -rf",
            "Vaults",
            "the command",
            "tests pass",
        ] {
            assert!(!debug.contains(secret), "leaked {secret} in {debug}");
        }

        // The path of the edited file never leaves the adapter, only its name
        // does.
        let edit = events
            .iter()
            .find_map(|event| match event {
                ParsedEvent::ToolStarted { activity } if activity.label == "Editing files" => {
                    Some(activity.clone())
                }
                _ => None,
            })
            .expect("edit activity");
        assert_eq!(edit.file.as_deref(), Some("important.rs"));
        assert_eq!(edit.language.as_deref(), Some("Rust"));

        // The command of the shell line only decides the label.
        let shell = events
            .iter()
            .find_map(|event| match event {
                ParsedEvent::ToolStarted { activity } if activity.label == "Running command" => {
                    Some(activity.clone())
                }
                _ => None,
            })
            .expect("shell activity");
        assert_eq!(shell.file, None);
        assert_eq!(shell.language, None);

        // Only the turn and the two tools are reported, no line of the
        // transcript is forwarded.
        assert_eq!(events.len(), 5);
    }

    #[test]
    fn unknown_lines_produce_no_events() {
        let path = transcript();
        let lines = [
            "",
            " ",
            "This is the answer of the model.",
            "> Aider v0.90.0",
            "> Git repo: .git with 300 files",
            "> Repo-map: using 1024 tokens",
            "> Use /help <question> for help",
            "> Commit fd43cd4 feat: use the spinner",
            "<<<<<<< SEARCH",
            "=======",
            ">>>>>>> REPLACE",
            r#"{"tool": "write", "path": "src/secret.rs"}"#,
            "> Tokens: not a number",
            "# a heading of the answer",
        ];

        for line in lines {
            assert!(
                parse_line_at(&path, line).is_empty(),
                "unexpected events for {line:?}"
            );
        }
    }

    #[test]
    fn reads_the_model_of_the_banner_and_drops_prose() {
        let path = transcript();

        // The banner of the current version.
        assert_eq!(
            parse_line_at(&path, "> Main model: gpt-4o with diff edit format"),
            vec![ParsedEvent::AgentDetected {
                agent: "gpt-4o".to_string()
            }]
        );

        // The banner of the older versions.
        assert_eq!(
            parse_line_at(
                &path,
                "> Models: claude-3-5-sonnet-20240620 with diff edit format, weak model claude-3-haiku-20240307"
            ),
            vec![ParsedEvent::AgentDetected {
                agent: "claude-3-5-sonnet-20240620".to_string()
            }]
        );

        // A model of another vendor keeps its prefix.
        assert_eq!(
            parse_line_at(
                &path,
                "> Main model: openai/gpt-5-codex:v2 with diff edit format"
            ),
            vec![ParsedEvent::AgentDetected {
                agent: "openai/gpt-5-codex:v2".to_string()
            }]
        );

        // A banner that looks like prose is dropped.
        assert!(
            parse_line_at(
                &path,
                "> Main model: the best model is gpt-4o for this task"
            )
            .is_empty()
        );

        // A model id that is not a model id is dropped.
        assert!(parse_line_at(&path, "> Main model: gpt 4o").is_empty());
        assert!(parse_line_at(&path, "> Main model: ").is_empty());
        assert!(parse_line_at(&path, &format!("> Main model: {}", "a".repeat(129))).is_empty());
    }

    #[test]
    fn resolves_the_project_of_the_header_from_the_parent_directory() {
        let path = transcript();

        assert_eq!(
            parse_line_at(&path, "# aider chat started at 2026-10-21 08:13:45"),
            vec![ParsedEvent::SessionMetadata {
                project: "MyRepo".to_string(),
                cwd: workspace(),
            }]
        );

        // A header without a timestamp is not a header.
        assert!(parse_line_at(&path, "# aider chat started at some time").is_empty());
        assert!(parse_line_at(&path, "# aider chat started at 2026-10-21").is_empty());

        // A transcript without a parent directory has no project.
        assert!(
            parse_line_at(
                Path::new(".aider.chat.history.md"),
                "# aider chat started at 2026-10-21 08:13:45"
            )
            .is_empty()
        );
    }

    #[test]
    fn maps_every_tool_shape_of_the_transcript() {
        let path = transcript();

        // An edit that was only tried out.
        assert_eq!(
            parse_line_at(&path, "> Did not apply edit to src/lib.rs (--dry-run)"),
            vec![
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label: "Editing files".to_string(),
                        file: Some("lib.rs".to_string()),
                        language: Some("Rust".to_string()),
                    },
                },
                ParsedEvent::ToolFinished,
            ]
        );

        // A commit before the edits are applied.
        assert_eq!(
            parse_line_at(&path, "> Committing src/main.rs before applying edits."),
            vec![
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label: "Reviewing repository".to_string(),
                        file: Some("main.rs".to_string()),
                        language: None,
                    },
                },
                ParsedEvent::ToolFinished,
            ]
        );

        // A command of the shell.
        assert_eq!(
            parse_line_at(&path, "> Running git status"),
            vec![
                ParsedEvent::ToolStarted {
                    activity: ToolActivity {
                        label: "Reviewing repository".to_string(),
                        file: None,
                        language: None,
                    },
                },
                ParsedEvent::ToolFinished,
            ]
        );

        // The end of a model turn.
        assert_eq!(
            parse_line_at(
                &path,
                "> Tokens: 5,244 sent, 604 received. Cost: $0.02 request, $0.02 session."
            ),
            vec![ParsedEvent::TerminalCandidate]
        );

        // The hard break of the Markdown and the line ending of the file do
        // not change the result.
        assert_eq!(
            parse_line_at(&path, "> Applied edit to src/main.rs  \r"),
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
    }
}
