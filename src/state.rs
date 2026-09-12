use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use crate::{discord::PresenceSnapshot, events::ParsedEvent, provider::Provider};

const TERMINAL_DEBOUNCE_MS: i64 = 4_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Thinking,
    RunningTools,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Thinking => "Thinking",
            Self::RunningTools => "Running tools",
        }
    }
}

#[derive(Debug, Clone)]
struct SessionState {
    provider: Provider,
    project: String,
    project_stack: Option<String>,
    current_file: Option<String>,
    current_language: Option<String>,
    activity: String,
    active: bool,
    phase: Phase,
    started_at_ms: i64,
    last_activity_ms: i64,
    terminal_deadline_ms: Option<i64>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            provider: Provider::Codex,
            project: "AI session".to_string(),
            project_stack: None,
            current_file: None,
            current_language: None,
            activity: "Thinking through changes".to_string(),
            active: false,
            phase: Phase::Thinking,
            started_at_ms: 0,
            last_activity_ms: 0,
            terminal_deadline_ms: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct ActivityState {
    sessions: HashMap<PathBuf, SessionState>,
}

impl ActivityState {
    pub fn apply(&mut self, path: &Path, provider: Provider, event: ParsedEvent, now_ms: i64) {
        let session = self.sessions.entry(path.to_path_buf()).or_default();
        session.provider = provider;
        match event {
            ParsedEvent::SessionMetadata { project, cwd } => {
                session.project = project;
                session.project_stack = detect_project_stack(Path::new(&cwd));
            }
            ParsedEvent::TaskStarted { started_at_ms } => {
                session.active = true;
                session.phase = Phase::Thinking;
                session.current_file = None;
                session.current_language = None;
                session.activity = "Planning changes".to_string();
                session.started_at_ms = started_at_ms;
                session.last_activity_ms = now_ms;
                session.terminal_deadline_ms = None;
            }
            ParsedEvent::Thinking => {
                if session.active {
                    session.phase = Phase::Thinking;
                    session.activity = "Thinking through changes".to_string();
                    session.last_activity_ms = now_ms;
                    session.terminal_deadline_ms = None;
                }
            }
            ParsedEvent::ToolStarted { activity } => {
                if session.active {
                    session.phase = Phase::RunningTools;
                    session.activity = activity.label;
                    session.current_file = activity.file;
                    session.current_language = activity.language;
                    session.last_activity_ms = now_ms;
                    session.terminal_deadline_ms = None;
                }
            }
            ParsedEvent::ToolFinished => {
                if session.active {
                    session.phase = Phase::Thinking;
                    session.activity = "Reviewing tool results".to_string();
                    session.last_activity_ms = now_ms;
                    session.terminal_deadline_ms = None;
                }
            }
            ParsedEvent::TerminalCandidate => {
                if session.active {
                    session.phase = Phase::Thinking;
                    session.activity = "Wrapping up".to_string();
                    session.last_activity_ms = now_ms;
                    session.terminal_deadline_ms = Some(now_ms + TERMINAL_DEBOUNCE_MS);
                }
            }
            ParsedEvent::Completed => {
                session.active = false;
                session.terminal_deadline_ms = None;
                session.last_activity_ms = now_ms;
            }
        }
    }

    pub fn expire_terminal_candidates(&mut self, now_ms: i64) {
        for session in self.sessions.values_mut() {
            if session.active
                && session
                    .terminal_deadline_ms
                    .is_some_and(|deadline| now_ms >= deadline)
            {
                session.active = false;
                session.terminal_deadline_ms = None;
            }
        }
    }

    pub fn discard_stale_active(&mut self, now_ms: i64, max_age_ms: i64) {
        for session in self.sessions.values_mut() {
            if session.active && now_ms.saturating_sub(session.last_activity_ms) > max_age_ms {
                session.active = false;
                session.terminal_deadline_ms = None;
            }
        }
    }

    pub fn next_terminal_deadline_ms(&self) -> Option<i64> {
        self.sessions
            .values()
            .filter(|session| session.active)
            .filter_map(|session| session.terminal_deadline_ms)
            .min()
    }

    pub fn selected_presence(&self) -> Option<PresenceSnapshot> {
        self.sessions
            .values()
            .filter(|session| session.active)
            .max_by_key(|session| session.last_activity_ms)
            .map(|session| PresenceSnapshot {
                provider: session.provider,
                project: session.project.clone(),
                project_stack: session.project_stack.clone(),
                current_file: session.current_file.clone(),
                current_language: session.current_language.clone(),
                activity: session.activity.clone(),
                phase: session.phase,
                started_at_ms: session.started_at_ms,
            })
    }
}

fn detect_project_stack(cwd: &Path) -> Option<String> {
    if cwd.join("Cargo.toml").exists() {
        return Some("Rust".to_string());
    }
    if cwd.join("tsconfig.json").exists() {
        if package_json_mentions(cwd, "react") {
            return Some("React • TypeScript".to_string());
        }
        return Some("TypeScript".to_string());
    }
    if cwd.join("package.json").exists() {
        if package_json_mentions(cwd, "react") {
            return Some("React • JavaScript".to_string());
        }
        if package_json_mentions(cwd, "vue") {
            return Some("Vue • JavaScript".to_string());
        }
        if package_json_mentions(cwd, "svelte") {
            return Some("Svelte • JavaScript".to_string());
        }
        return Some("JavaScript".to_string());
    }
    if cwd.join("pyproject.toml").exists()
        || cwd.join("requirements.txt").exists()
        || cwd.join("setup.py").exists()
    {
        return Some("Python".to_string());
    }
    if cwd.join("go.mod").exists() {
        return Some("Go".to_string());
    }
    if cwd.join("pom.xml").exists() || cwd.join("build.gradle").exists() {
        return Some("Java".to_string());
    }
    if cwd.join("Package.swift").exists() {
        return Some("Swift".to_string());
    }
    if let Ok(entries) = std::fs::read_dir(cwd) {
        for entry in entries.flatten().take(128) {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name.ends_with(".csproj") || name.ends_with(".sln") {
                return Some(".NET".to_string());
            }
        }
    }
    None
}

fn package_json_mentions(cwd: &Path, package: &str) -> bool {
    std::fs::read_to_string(cwd.join("package.json"))
        .ok()
        .is_some_and(|text| text.contains(&format!("\"{package}\"")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(state: &mut ActivityState, path: &str, at: i64) {
        state.apply(
            Path::new(path),
            Provider::Codex,
            ParsedEvent::SessionMetadata {
                project: path.to_string(),
                cwd: path.to_string(),
            },
            at,
        );
        state.apply(
            Path::new(path),
            Provider::Codex,
            ParsedEvent::TaskStarted { started_at_ms: at },
            at,
        );
    }

    #[test]
    fn state_machine_thinking_tools_thinking_idle() {
        let mut state = ActivityState::default();
        let path = Path::new("one");
        start(&mut state, "one", 1_000);
        assert_eq!(state.selected_presence().unwrap().phase, Phase::Thinking);
        state.apply(
            path,
            Provider::Codex,
            ParsedEvent::ToolStarted {
                activity: crate::events::ToolActivity {
                    label: "Building Rust project".to_string(),
                    file: None,
                    language: None,
                },
            },
            2_000,
        );
        assert_eq!(
            state.selected_presence().unwrap().phase,
            Phase::RunningTools
        );
        state.apply(path, Provider::Codex, ParsedEvent::ToolFinished, 3_000);
        assert_eq!(state.selected_presence().unwrap().phase, Phase::Thinking);
        state.apply(path, Provider::Codex, ParsedEvent::Completed, 4_000);
        assert!(state.selected_presence().is_none());
    }

    #[test]
    fn terminal_candidate_debounces_then_expires() {
        let mut state = ActivityState::default();
        start(&mut state, "one", 1_000);
        state.apply(
            Path::new("one"),
            Provider::Codex,
            ParsedEvent::TerminalCandidate,
            2_000,
        );
        state.expire_terminal_candidates(5_999);
        assert!(state.selected_presence().is_some());
        state.expire_terminal_candidates(6_000);
        assert!(state.selected_presence().is_none());
    }

    #[test]
    fn latest_active_session_wins() {
        let mut state = ActivityState::default();
        start(&mut state, "older", 1_000);
        start(&mut state, "newer", 2_000);
        assert_eq!(state.selected_presence().unwrap().project, "newer");
        state.apply(
            Path::new("older"),
            Provider::Codex,
            ParsedEvent::Thinking,
            3_000,
        );
        assert_eq!(state.selected_presence().unwrap().project, "older");
    }

    #[test]
    fn stale_incomplete_session_is_not_restored() {
        let mut state = ActivityState::default();
        start(&mut state, "stale", 1_000);
        state.discard_stale_active(61_001, 60_000);
        assert!(state.selected_presence().is_none());
    }
}
