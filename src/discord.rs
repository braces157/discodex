use discord_rich_presence::{DiscordIpc, DiscordIpcClient, activity};

use crate::{config::PrivacySettings, provider::Provider, state::Phase};

const DISCODEX_LOGO_URL: &str =
    "https://raw.githubusercontent.com/braces157/discodex/main/assets/brand/discodex-512.png";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceSnapshot {
    pub provider: Provider,
    pub project: String,
    pub project_stack: Option<String>,
    pub current_file: Option<String>,
    pub current_language: Option<String>,
    pub activity: String,
    pub phase: Phase,
    pub started_at_ms: i64,
    pub agent: Option<String>,
    pub reasoning: Option<String>,
}

impl PresenceSnapshot {
    pub fn redacted(&self, privacy: &PrivacySettings) -> Self {
        let mut snapshot = self.clone();
        if !privacy.show_project_names {
            snapshot.project = snapshot.provider.display_name().to_string();
        }
        if !privacy.show_file_names {
            snapshot.current_file = None;
        }
        if !privacy.share_reasoning || !privacy.show_project_names || !privacy.show_file_names {
            snapshot.reasoning = None;
            if snapshot.activity.starts_with("Reasoning:") {
                snapshot.activity = "Thinking through changes".to_string();
            }
        }
        snapshot
    }
}

pub struct DiscordPublisher {
    application_id: String,
    client: Option<DiscordIpcClient>,
    last_sent: Option<PresenceSnapshot>,
    last_error: Option<String>,
}

impl DiscordPublisher {
    pub fn new(application_id: String) -> Self {
        Self {
            application_id,
            client: None,
            last_sent: None,
            last_error: None,
        }
    }

    pub fn is_connected(&self) -> bool {
        self.client.is_some()
    }

    pub fn diagnostic_status(&self) -> String {
        if self.application_id.trim().is_empty() {
            "Application ID missing".to_string()
        } else if self.is_connected() {
            "Connected".to_string()
        } else if let Some(error) = &self.last_error {
            format!("Disconnected. Last IPC error: {error}")
        } else {
            "Not connected. No presence has required a connection yet".to_string()
        }
    }

    pub fn sync(
        &mut self,
        desired: Option<&PresenceSnapshot>,
        force_refresh: bool,
        privacy: &PrivacySettings,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let redacted = desired.map(|snapshot| snapshot.redacted(privacy));
        let desired = redacted.as_ref();
        if self.application_id.trim().is_empty() {
            self.disconnect();
            return Ok(());
        }
        if desired.is_none() && self.client.is_none() {
            self.last_sent = None;
            return Ok(());
        }
        if desired.is_some() && self.client.is_none() {
            let mut client = DiscordIpcClient::new(&self.application_id);
            if let Err(error) = client.connect() {
                self.client = None;
                self.last_error = Some(error.to_string());
                return Err(Box::new(error));
            }
            self.client = Some(client);
            self.last_sent = None;
        }

        match desired {
            Some(snapshot) if force_refresh || self.last_sent.as_ref() != Some(snapshot) => {
                let activity = build_activity(snapshot, privacy);
                if let Err(error) = self.client.as_mut().unwrap().set_activity(activity) {
                    self.last_error = Some(error.to_string());
                    self.disconnect();
                    return Err(Box::new(error));
                }
                self.last_sent = Some(snapshot.clone());
                self.last_error = None;
            }
            None if self.last_sent.is_some() => {
                if let Err(error) = self.client.as_mut().unwrap().clear_activity() {
                    self.last_error = Some(error.to_string());
                    self.disconnect();
                    return Err(Box::new(error));
                }
                self.last_sent = None;
            }
            _ => {}
        }
        Ok(())
    }

    fn disconnect(&mut self) {
        if let Some(mut client) = self.client.take() {
            let _ = client.close();
        }
        self.last_sent = None;
    }
}

impl Drop for DiscordPublisher {
    fn drop(&mut self) {
        self.disconnect();
    }
}

pub(crate) fn build_activity(
    snapshot: &PresenceSnapshot,
    privacy: &PrivacySettings,
) -> activity::Activity<'static> {
    let snapshot = snapshot.redacted(privacy);
    let context = snapshot
        .current_language
        .as_deref()
        .or(snapshot.project_stack.as_deref());
    let provider = snapshot.provider.display_name();

    let mut details = match (snapshot.project == provider, &snapshot.agent, context) {
        (true, Some(agent), Some(ctx)) => format!("{agent} • {ctx}"),
        (true, Some(agent), None) => agent.clone(),
        (true, None, Some(ctx)) => ctx.to_string(),
        (true, None, None) => snapshot.project.clone(),
        (false, Some(agent), Some(ctx)) => {
            format!("{agent} • {} • {ctx}", snapshot.project)
        }
        (false, Some(agent), None) => {
            format!("{agent} • {}", snapshot.project)
        }
        (false, None, Some(ctx)) => {
            format!("{} • {ctx}", snapshot.project)
        }
        (false, None, None) => snapshot.project.clone(),
    };
    truncate_in_place(&mut details, 128);

    let mut state = if snapshot.phase == Phase::Thinking {
        let reasoning_label = match &snapshot.reasoning {
            Some(r) if !r.trim().is_empty() => r.clone(),
            _ => snapshot.activity.clone(),
        };
        match snapshot.current_file.as_deref() {
            Some(file) => format!("{file} • {reasoning_label}"),
            None => reasoning_label,
        }
    } else {
        let tool_state = match snapshot.current_file.as_deref() {
            Some(file) => format!("{file} • {}", snapshot.activity),
            None => snapshot.activity.clone(),
        };
        match &snapshot.reasoning {
            Some(r) if !r.trim().is_empty() => format!("{tool_state} • {r}"),
            _ => tool_state,
        }
    };
    truncate_in_place(&mut state, 128);

    let mut activity = activity::Activity::new()
        .name(provider)
        .details(details)
        .state(state)
        .timestamps(activity::Timestamps::new().start(snapshot.started_at_ms));

    let mut large_text = match &snapshot.agent {
        Some(agent) => format!("{provider} • {agent}"),
        None => provider.to_string(),
    };
    truncate_in_place(&mut large_text, 128);
    activity = activity.assets(
        activity::Assets::new()
            .large_image(snapshot.provider.logo_url())
            .large_text(large_text)
            .small_image(DISCODEX_LOGO_URL)
            .small_text("Discodex"),
    );

    activity
}

fn truncate_in_place(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracted_confidential_heading_requires_opt_in_in_every_payload() {
        use crate::{events::ParsedEvent, state::ActivityState};
        let mut state = ActivityState::default();
        let path = std::path::Path::new("session.jsonl");
        state.apply(
            path,
            Provider::Antigravity,
            ParsedEvent::TaskStarted {
                started_at_ms: 1234,
            },
            1234,
        );
        let line = r#"{"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","thinking":"**confidential-client acquisition**\nPRIVATE_BODY","tool_calls":[]}"#;
        for event in crate::sources::parse_log_line(Provider::Antigravity, line) {
            state.apply(path, Provider::Antigravity, event, 1235);
        }
        let mut snapshot = state.selected_presence().unwrap();
        assert_eq!(
            snapshot.reasoning.as_deref(),
            Some("confidential-client acquisition")
        );
        snapshot.project = "SECRET_PROJECT".to_string();
        snapshot.current_file = Some("SECRET_FILE.rs".to_string());
        for phase in [Phase::Thinking, Phase::RunningTools] {
            snapshot.phase = phase;
            for share_reasoning in [false, true] {
                for show_project_names in [false, true] {
                    for show_file_names in [false, true] {
                        let privacy = PrivacySettings {
                            share_reasoning,
                            show_project_names,
                            show_file_names,
                        };
                        let payload =
                            serde_json::to_value(build_activity(&snapshot, &privacy)).unwrap();
                        let outgoing = payload.to_string();
                        assert_eq!(
                            outgoing.contains("confidential-client acquisition"),
                            share_reasoning && show_project_names && show_file_names
                        );
                        assert_eq!(outgoing.contains("SECRET_PROJECT"), show_project_names);
                        assert_eq!(outgoing.contains("SECRET_FILE.rs"), show_file_names);
                        assert!(!outgoing.contains("PRIVATE_BODY"));
                        assert_eq!(payload["timestamps"]["start"], 1234);
                    }
                }
            }
        }
    }

    #[test]
    fn snapshot_has_only_safe_presence_fields() {
        let snapshot = PresenceSnapshot {
            provider: Provider::Codex,
            project: "Discodex".to_string(),
            project_stack: Some("Rust".to_string()),
            current_file: Some("tray.rs".to_string()),
            current_language: Some("Rust".to_string()),
            activity: "Editing files".to_string(),
            phase: Phase::RunningTools,
            started_at_ms: 1234,
            agent: Some("DeepCoder".to_string()),
            reasoning: None,
        };
        assert_eq!(snapshot.project, "Discodex");
        assert_eq!(snapshot.activity, "Editing files");
        assert_eq!(snapshot.started_at_ms, 1234);
        assert_eq!(snapshot.agent.as_deref(), Some("DeepCoder"));
    }

    #[test]
    fn activity_uses_provider_name_and_exact_timestamp() {
        let snapshot = PresenceSnapshot {
            provider: Provider::ClaudeCode,
            project: "Discodex".to_string(),
            project_stack: Some("Rust".to_string()),
            current_file: Some("discord.rs".to_string()),
            current_language: Some("Rust".to_string()),
            activity: "Editing files".to_string(),
            phase: Phase::Thinking,
            started_at_ms: 1_789_187_508_000,
            agent: None,
            reasoning: None,
        };
        let value =
            serde_json::to_value(build_activity(&snapshot, &PrivacySettings::default())).unwrap();
        assert_eq!(value["name"], "Claude Code");
        assert_eq!(value["details"], "Discodex • Rust");
        assert_eq!(value["state"], "discord.rs • Editing files");
        assert_eq!(value["timestamps"]["start"], 1_789_187_508_000_i64);
        assert_eq!(
            value["assets"]["large_image"],
            Provider::ClaudeCode.logo_url()
        );
        assert_eq!(value["assets"]["large_text"], "Claude Code");
    }

    #[test]
    fn codex_activity_uses_public_codex_artwork() {
        let snapshot = PresenceSnapshot {
            provider: Provider::Codex,
            project: "Discodex".to_string(),
            project_stack: Some("Rust".to_string()),
            current_file: None,
            current_language: None,
            activity: "Reviewing tool results".to_string(),
            phase: Phase::Thinking,
            started_at_ms: 1_789_187_508_000,
            agent: None,
            reasoning: None,
        };
        let value =
            serde_json::to_value(build_activity(&snapshot, &PrivacySettings::default())).unwrap();
        let large_image = value["assets"]["large_image"].as_str().unwrap();
        assert_eq!(large_image, Provider::Codex.logo_url());
        assert!(
            large_image.ends_with("/assets/logos/codex.png"),
            "Codex should use its own mark, got {large_image}"
        );
        assert_eq!(value["assets"]["large_text"], "OpenAI Codex");
    }

    #[test]
    fn every_supported_provider_has_rich_presence_logo_asset() {
        for provider in Provider::ALL {
            let snapshot = PresenceSnapshot {
                provider,
                project: "Discodex".to_string(),
                project_stack: Some("Rust".to_string()),
                current_file: None,
                current_language: None,
                activity: "Coding".to_string(),
                phase: Phase::Thinking,
                started_at_ms: 1_789_187_508_000,
                agent: None,
                reasoning: None,
            };
            let value =
                serde_json::to_value(build_activity(&snapshot, &PrivacySettings::default()))
                    .unwrap();
            let assets = &value["assets"];
            assert!(
                assets.is_object(),
                "Provider {provider:?} missing assets in activity"
            );
            assert_eq!(
                assets["large_image"],
                provider.logo_url(),
                "Provider {provider:?} large_image mismatch"
            );
            assert_eq!(
                assets["large_text"],
                provider.display_name(),
                "Provider {provider:?} large_text mismatch"
            );
            assert_eq!(
                assets["small_image"],
                "https://raw.githubusercontent.com/braces157/discodex/main/assets/brand/discodex-512.png",
                "Provider {provider:?} missing the Discodex badge"
            );
            assert_eq!(assets["small_text"], "Discodex");
        }
    }

    #[test]
    fn activity_includes_agent_and_reasoning_in_discord_presence() {
        let snapshot = PresenceSnapshot {
            provider: Provider::Antigravity,
            project: "Discodex".to_string(),
            project_stack: Some("Rust".to_string()),
            current_file: None,
            current_language: None,
            activity: "Reasoning: Evaluating Request and Identity".to_string(),
            phase: Phase::Thinking,
            started_at_ms: 1_789_187_508_000,
            agent: Some("DeepCoder".to_string()),
            reasoning: Some("Evaluating Request and Identity".to_string()),
        };
        let value = serde_json::to_value(build_activity(
            &snapshot,
            &PrivacySettings {
                share_reasoning: true,
                ..PrivacySettings::default()
            },
        ))
        .unwrap();
        assert_eq!(value["name"], "Google Antigravity");
        assert_eq!(value["details"], "DeepCoder • Discodex • Rust");
        assert_eq!(value["state"], "Evaluating Request and Identity");
        assert_eq!(
            value["assets"]["large_image"],
            Provider::Antigravity.logo_url()
        );
        assert_eq!(
            value["assets"]["large_text"],
            "Google Antigravity • DeepCoder"
        );

        // When running tools with agent
        let tool_snapshot = PresenceSnapshot {
            provider: Provider::Antigravity,
            project: "Discodex".to_string(),
            project_stack: Some("Rust".to_string()),
            current_file: Some("discord.rs".to_string()),
            current_language: Some("Rust".to_string()),
            activity: "Editing files".to_string(),
            phase: Phase::RunningTools,
            started_at_ms: 1_789_187_508_000,
            agent: Some("DeepCoder".to_string()),
            reasoning: Some("Evaluating Request and Identity".to_string()),
        };
        let tool_value = serde_json::to_value(build_activity(
            &tool_snapshot,
            &PrivacySettings {
                share_reasoning: true,
                ..PrivacySettings::default()
            },
        ))
        .unwrap();
        assert_eq!(tool_value["details"], "DeepCoder • Discodex • Rust");
        assert_eq!(
            tool_value["state"],
            "discord.rs • Editing files • Evaluating Request and Identity"
        );
        assert_eq!(
            tool_value["assets"]["large_image"],
            Provider::Antigravity.logo_url()
        );
        assert_eq!(
            tool_value["assets"]["large_text"],
            "Google Antigravity • DeepCoder"
        );
    }
}
