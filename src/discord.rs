use discord_rich_presence::{DiscordIpc, DiscordIpcClient, activity};

use crate::{provider::Provider, state::Phase};

#[cfg(test)]
const CODEX_IMAGE_URL: &str =
    "https://raw.githubusercontent.com/braces157/discodex/main/assets/codex-bundle-blue.png";

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

pub struct DiscordPublisher {
    application_id: String,
    client: Option<DiscordIpcClient>,
    last_sent: Option<PresenceSnapshot>,
}

impl DiscordPublisher {
    pub fn new(application_id: String) -> Self {
        Self {
            application_id,
            client: None,
            last_sent: None,
        }
    }

    pub fn is_connected(&self) -> bool {
        self.client.is_some()
    }

    pub fn sync(
        &mut self,
        desired: Option<&PresenceSnapshot>,
        force_refresh: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
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
                return Err(Box::new(error));
            }
            self.client = Some(client);
            self.last_sent = None;
        }

        match desired {
            Some(snapshot) if force_refresh || self.last_sent.as_ref() != Some(snapshot) => {
                let activity = build_activity(snapshot);
                if let Err(error) = self.client.as_mut().unwrap().set_activity(activity) {
                    self.disconnect();
                    return Err(Box::new(error));
                }
                self.last_sent = Some(snapshot.clone());
            }
            None if self.last_sent.is_some() => {
                if let Err(error) = self.client.as_mut().unwrap().clear_activity() {
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

fn build_activity(snapshot: &PresenceSnapshot) -> activity::Activity<'_> {
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
            .large_text(large_text),
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
        let value = serde_json::to_value(build_activity(&snapshot)).unwrap();
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
        let value = serde_json::to_value(build_activity(&snapshot)).unwrap();
        assert_eq!(value["assets"]["large_image"], CODEX_IMAGE_URL);
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
            let value = serde_json::to_value(build_activity(&snapshot)).unwrap();
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
        let value = serde_json::to_value(build_activity(&snapshot)).unwrap();
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
        let tool_value = serde_json::to_value(build_activity(&tool_snapshot)).unwrap();
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
