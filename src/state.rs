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
    agent: Option<String>,
    reasoning: Option<String>,
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
            agent: None,
            reasoning: None,
        }
    }
}

#[derive(Debug, Clone)]
struct SubagentMeta {
    agent: String,
    project: Option<String>,
    project_stack: Option<String>,
}

#[derive(Debug, Default)]
pub struct ActivityState {
    sessions: HashMap<PathBuf, SessionState>,
    subagents: HashMap<String, SubagentMeta>,
}

fn is_default_project(project: &str, provider: Provider) -> bool {
    project == "AI session" || project == provider.display_name()
}

impl ActivityState {
    pub fn apply(&mut self, path: &Path, provider: Provider, event: ParsedEvent, now_ms: i64) {
        let session = self.sessions.entry(path.to_path_buf()).or_default();
        session.provider = provider;

        let path_lower = path.to_string_lossy().to_ascii_lowercase();
        for (id, meta) in &self.subagents {
            if path_lower.contains(id) {
                if session.agent.is_none() {
                    session.agent = Some(meta.agent.clone());
                }
                if is_default_project(&session.project, session.provider)
                    && let Some(proj) = &meta.project
                {
                    session.project = proj.clone();
                    session.project_stack = meta.project_stack.clone();
                }
                break;
            }
        }

        match event {
            ParsedEvent::AgentDetected { agent } => {
                session.agent = Some(agent);
            }
            ParsedEvent::SubagentSpawned {
                conversation_id,
                agent,
            } => {
                let resolved_agent = if !agent.is_empty() {
                    agent
                } else if let Some(a) = &session.agent {
                    a.clone()
                } else {
                    "DeepCoder".to_string()
                };
                let resolved_project = if !is_default_project(&session.project, session.provider) {
                    Some(session.project.clone())
                } else {
                    None
                };
                let resolved_stack = session.project_stack.clone();

                let meta = SubagentMeta {
                    agent: resolved_agent,
                    project: resolved_project,
                    project_stack: resolved_stack,
                };

                let id_lower = conversation_id.to_ascii_lowercase();
                self.subagents.insert(id_lower.clone(), meta.clone());
                for (sess_path, sess) in &mut self.sessions {
                    if sess_path
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .contains(&id_lower)
                    {
                        sess.agent = Some(meta.agent.clone());
                        if is_default_project(&sess.project, sess.provider)
                            && let Some(proj) = &meta.project
                        {
                            sess.project = proj.clone();
                            sess.project_stack = meta.project_stack.clone();
                        }
                    }
                }
            }
            ParsedEvent::SessionMetadata { project, cwd } => {
                let stack = detect_project_stack(Path::new(&cwd));
                session.project = project.clone();
                session.project_stack = stack.clone();

                for meta in self.subagents.values_mut() {
                    if meta.project.is_none() {
                        meta.project = Some(project.clone());
                        meta.project_stack = stack.clone();
                    }
                }
                for sess in self.sessions.values_mut() {
                    if sess.provider == provider && is_default_project(&sess.project, sess.provider)
                    {
                        sess.project = project.clone();
                        sess.project_stack = stack.clone();
                    }
                }
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
            ParsedEvent::Thinking { reasoning } => {
                session.active = true;
                if session.started_at_ms == 0 {
                    session.started_at_ms = now_ms;
                }
                session.phase = Phase::Thinking;
                if let Some(r) = reasoning {
                    session.activity = format!("Reasoning: {r}");
                    session.reasoning = Some(r);
                } else if session.reasoning.is_none() {
                    session.activity = "Thinking through changes".to_string();
                }
                session.last_activity_ms = now_ms;
                session.terminal_deadline_ms = None;
            }
            ParsedEvent::ToolStarted { activity } => {
                session.active = true;
                if session.started_at_ms == 0 {
                    session.started_at_ms = now_ms;
                }
                session.phase = Phase::RunningTools;
                session.activity = activity.label;
                session.current_file = activity.file;
                session.current_language = activity.language;
                session.last_activity_ms = now_ms;
                session.terminal_deadline_ms = None;
            }
            ParsedEvent::ToolFinished => {
                if session.active {
                    session.phase = Phase::Thinking;
                    session.activity = match &session.reasoning {
                        Some(r) => format!("Reasoning: {r}"),
                        None => "Reviewing tool results".to_string(),
                    };
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
                session.reasoning = None;
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

    pub fn next_stale_deadline_ms(&self, max_age_ms: i64) -> Option<i64> {
        self.sessions
            .values()
            .filter(|session| session.active)
            .map(|session| session.last_activity_ms.saturating_add(max_age_ms))
            .min()
    }

    #[allow(dead_code)]
    pub fn selected_presence(&self) -> Option<PresenceSnapshot> {
        self.selected_presence_for_foreground(None)
    }

    pub fn selected_presence_for_foreground(
        &self,
        foreground_provider: Option<Provider>,
    ) -> Option<PresenceSnapshot> {
        self.sessions
            .values()
            .filter(|session| session.active)
            .max_by_key(|session| {
                (
                    session.phase == Phase::RunningTools,
                    foreground_provider == Some(session.provider),
                    session.last_activity_ms,
                )
            })
            .map(|session| PresenceSnapshot {
                provider: session.provider,
                project: session.project.clone(),
                project_stack: session.project_stack.clone(),
                current_file: session.current_file.clone(),
                current_language: session.current_language.clone(),
                activity: session.activity.clone(),
                phase: session.phase,
                started_at_ms: session.started_at_ms,
                agent: session.agent.clone(),
                reasoning: session.reasoning.clone(),
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
    fn antigravity_state_machine_tracks_tools_and_project() {
        let mut state = ActivityState::default();
        let path = Path::new("transcript.jsonl");
        state.apply(
            path,
            Provider::Antigravity,
            ParsedEvent::TaskStarted {
                started_at_ms: 1_000,
            },
            1_000,
        );
        state.apply(
            path,
            Provider::Antigravity,
            ParsedEvent::SessionMetadata {
                project: "Discodex".to_string(),
                cwd: env!("CARGO_MANIFEST_DIR").to_string(),
            },
            1_000,
        );

        let presence = state.selected_presence().unwrap();
        assert_eq!(presence.provider, Provider::Antigravity);
        assert_eq!(presence.project, "Discodex");
        assert_eq!(presence.project_stack, Some("Rust".to_string()));
        assert_eq!(presence.phase, Phase::Thinking);

        state.apply(
            path,
            Provider::Antigravity,
            ParsedEvent::ToolStarted {
                activity: crate::events::ToolActivity {
                    label: "Reading project files".to_string(),
                    file: Some("provider.rs".to_string()),
                    language: Some("Rust".to_string()),
                },
            },
            2_000,
        );

        let presence = state.selected_presence().unwrap();
        assert_eq!(presence.phase, Phase::RunningTools);
        assert_eq!(presence.activity, "Reading project files");
        assert_eq!(presence.current_file.as_deref(), Some("provider.rs"));
        assert_eq!(presence.current_language.as_deref(), Some("Rust"));

        state.apply(
            path,
            Provider::Antigravity,
            ParsedEvent::ToolFinished,
            3_000,
        );
        assert_eq!(state.selected_presence().unwrap().phase, Phase::Thinking);

        state.apply(
            path,
            Provider::Antigravity,
            ParsedEvent::TerminalCandidate,
            4_000,
        );
        state.expire_terminal_candidates(7_999);
        assert!(state.selected_presence().is_some());
        state.expire_terminal_candidates(8_000);
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
            ParsedEvent::Thinking { reasoning: None },
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

    #[test]
    fn running_tools_prioritized_over_idle_thinking() {
        let mut state = ActivityState::default();
        let path_tools = Path::new("tools");
        let path_thinking = Path::new("thinking");

        state.apply(
            path_tools,
            Provider::Codex,
            ParsedEvent::TaskStarted {
                started_at_ms: 1_000,
            },
            1_000,
        );
        state.apply(
            path_tools,
            Provider::Codex,
            ParsedEvent::ToolStarted {
                activity: crate::events::ToolActivity {
                    label: "Running Rust tests".to_string(),
                    file: None,
                    language: None,
                },
            },
            1_000,
        );

        state.apply(
            path_thinking,
            Provider::Antigravity,
            ParsedEvent::TaskStarted {
                started_at_ms: 2_000,
            },
            2_000,
        );
        state.apply(
            path_thinking,
            Provider::Antigravity,
            ParsedEvent::Thinking { reasoning: None },
            2_000,
        );

        // The session running tools wins even if thinking has a slightly newer event
        let selected = state.selected_presence().unwrap();
        assert_eq!(selected.provider, Provider::Codex);
        assert_eq!(selected.phase, Phase::RunningTools);
    }

    #[test]
    fn state_machine_tracks_agent_and_reasoning() {
        let mut state = ActivityState::default();
        let path = Path::new("transcript.jsonl");

        state.apply(
            path,
            Provider::Antigravity,
            ParsedEvent::TaskStarted {
                started_at_ms: 1_000,
            },
            1_000,
        );
        state.apply(
            path,
            Provider::Antigravity,
            ParsedEvent::SessionMetadata {
                project: "Discodex".to_string(),
                cwd: env!("CARGO_MANIFEST_DIR").to_string(),
            },
            1_000,
        );
        state.apply(
            path,
            Provider::Antigravity,
            ParsedEvent::AgentDetected {
                agent: "DeepCoder".to_string(),
            },
            1_000,
        );
        state.apply(
            path,
            Provider::Antigravity,
            ParsedEvent::Thinking {
                reasoning: Some("Evaluating Request and Identity".to_string()),
            },
            1_500,
        );

        let presence = state.selected_presence().unwrap();
        assert_eq!(presence.agent.as_deref(), Some("DeepCoder"));
        assert_eq!(
            presence.reasoning.as_deref(),
            Some("Evaluating Request and Identity")
        );
        assert_eq!(presence.phase, Phase::Thinking);
        assert_eq!(
            presence.activity,
            "Reasoning: Evaluating Request and Identity"
        );
    }

    #[test]
    fn foreground_provider_prioritized_when_both_active() {
        let mut state = ActivityState::default();
        let codex_path = Path::new("codex");
        let anti_path = Path::new("antigravity");

        state.apply(
            codex_path,
            Provider::Codex,
            ParsedEvent::TaskStarted {
                started_at_ms: 1_000,
            },
            1_000,
        );
        state.apply(
            anti_path,
            Provider::Antigravity,
            ParsedEvent::TaskStarted {
                started_at_ms: 1_500,
            },
            1_500,
        );

        // Without foreground focus, latest activity wins (Antigravity)
        assert_eq!(
            state.selected_presence().unwrap().provider,
            Provider::Antigravity
        );

        // When focused on Codex, Codex wins
        assert_eq!(
            state
                .selected_presence_for_foreground(Some(Provider::Codex))
                .unwrap()
                .provider,
            Provider::Codex
        );
    }

    #[test]
    fn next_stale_deadline_tracks_earliest_stale_expiration() {
        let mut state = ActivityState::default();
        start(&mut state, "one", 1_000);
        start(&mut state, "two", 3_000);

        // Earliest stale deadline should be 1_000 + 60_000 = 61_000
        assert_eq!(state.next_stale_deadline_ms(60_000), Some(61_000));
    }

    #[test]
    fn subagent_session_inherits_agent_from_spawn_event() {
        let mut state = ActivityState::default();
        let parent_path = Path::new(
            r"C:\Users\PC\.gemini\antigravity\brain\parent-uuid\.system_generated\logs\transcript.jsonl",
        );
        let worker_path = Path::new(
            r"C:\Users\PC\.gemini\antigravity\brain\worker-uuid-456\.system_generated\logs\transcript.jsonl",
        );

        state.apply(
            parent_path,
            Provider::Antigravity,
            ParsedEvent::TaskStarted {
                started_at_ms: 1_000,
            },
            1_000,
        );
        state.apply(
            parent_path,
            Provider::Antigravity,
            ParsedEvent::SessionMetadata {
                project: "Discodex".to_string(),
                cwd: env!("CARGO_MANIFEST_DIR").to_string(),
            },
            1_000,
        );
        state.apply(
            parent_path,
            Provider::Antigravity,
            ParsedEvent::AgentDetected {
                agent: "DeepCoder".to_string(),
            },
            1_000,
        );
        state.apply(
            parent_path,
            Provider::Antigravity,
            ParsedEvent::SubagentSpawned {
                conversation_id: "worker-uuid-456".to_string(),
                agent: String::new(),
            },
            1_000,
        );

        // Worker session starts thinking before any tools are called
        state.apply(
            worker_path,
            Provider::Antigravity,
            ParsedEvent::Thinking {
                reasoning: Some("Validating requirements".to_string()),
            },
            1_500,
        );

        let worker_initial = state.selected_presence().unwrap();
        assert_eq!(worker_initial.provider, Provider::Antigravity);
        assert_eq!(worker_initial.agent.as_deref(), Some("DeepCoder"));
        assert_eq!(worker_initial.project, "Discodex");
        assert_eq!(worker_initial.project_stack.as_deref(), Some("Rust"));
        assert_eq!(worker_initial.phase, Phase::Thinking);
        assert_eq!(
            worker_initial.reasoning.as_deref(),
            Some("Validating requirements")
        );

        // Worker session receives tools without containing any agent name in its own lines
        state.apply(
            worker_path,
            Provider::Antigravity,
            ParsedEvent::ToolStarted {
                activity: crate::events::ToolActivity {
                    label: "Editing files".to_string(),
                    file: Some("main.rs".to_string()),
                    language: Some("Rust".to_string()),
                },
            },
            2_000,
        );

        let presence = state.selected_presence().unwrap();
        assert_eq!(presence.provider, Provider::Antigravity);
        assert_eq!(presence.agent.as_deref(), Some("DeepCoder"));
        assert_eq!(presence.project, "Discodex");
        assert_eq!(presence.project_stack.as_deref(), Some("Rust"));
        assert_eq!(presence.phase, Phase::RunningTools);
        assert_eq!(presence.activity, "Editing files");
        assert_eq!(presence.current_file.as_deref(), Some("main.rs"));
    }
}
