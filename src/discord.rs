use discord_rich_presence::{DiscordIpc, DiscordIpcClient, activity};

use crate::{provider::Provider, state::Phase};

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
    let details = match (snapshot.project == provider, context) {
        (true, _) => format!("Using {provider}"),
        (false, Some(context)) => format!("{provider} • {} • {context}", snapshot.project),
        (false, None) => format!("{provider} • {}", snapshot.project),
    };
    let state = match snapshot.current_file.as_deref() {
        Some(file) => format!("{file} • {}", snapshot.activity),
        None => snapshot.activity.clone(),
    };
    activity::Activity::new()
        .name(provider)
        .details(details)
        .state(state)
        .timestamps(activity::Timestamps::new().start(snapshot.started_at_ms))
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
        };
        assert_eq!(snapshot.project, "Discodex");
        assert_eq!(snapshot.activity, "Editing files");
        assert_eq!(snapshot.started_at_ms, 1234);
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
        };
        let value = serde_json::to_value(build_activity(&snapshot)).unwrap();
        assert_eq!(value["name"], "Claude Code");
        assert_eq!(value["details"], "Claude Code • Discodex • Rust");
        assert_eq!(value["state"], "discord.rs • Editing files");
        assert_eq!(value["timestamps"]["start"], 1_789_187_508_000_i64);
    }
}
