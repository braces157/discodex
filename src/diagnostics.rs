use std::{collections::HashMap, fmt::Write, path::PathBuf};

use crate::{
    config::AppConfig,
    ingestion::{self, Ingestion},
    provider::{Provider, SessionStore},
    sources,
    state::ActivityState,
    watcher,
};

pub struct DiagnosticSnapshot<'a> {
    pub ingestion: &'a Ingestion,
    pub activity: &'a ActivityState,
    pub config: &'a AppConfig,
    pub focused: Option<Provider>,
    pub discord: &'a str,
    pub watches: &'a HashMap<PathBuf, Result<(), String>>,
}

impl DiagnosticSnapshot<'_> {
    pub fn render(&self) -> String {
        let mut report = format!(
            "Discodex diagnostics\n\nPresence enabled: {}\nDiscord: {}\nFocused-app detection enabled: {}\nFocused provider: {}\nReasoning opt-in: {}\nProject names: {}\nFile names: {}\n\n",
            self.config.enabled,
            self.discord,
            self.config.foreground_detection,
            self.focused.map_or("None detected", Provider::display_name),
            self.config.privacy.share_reasoning,
            self.config.privacy.show_project_names,
            self.config.privacy.show_file_names,
        );
        report.push_str("This local snapshot is not sent to Discord. Source paths and conversation text are omitted.\nNo recognized activity can mean an idle store or an unsupported session format.\nDatabase access checks do not verify adapter schema compatibility.\nDocument and database history is observed when read and can predate this run.\n\n");
        for provider in Provider::ALL {
            let _ = writeln!(report, "{}", provider.display_name());
            let last_activity = self.activity.provider_last_activity(provider);
            let _ = writeln!(
                report,
                "  Detected: {}",
                last_activity.is_some() || self.focused == Some(provider)
            );
            let _ = writeln!(
                report,
                "  Focused-app fallback: {}",
                if provider.has_session_tracking() {
                    "Disabled by provider policy; recognized session activity is required"
                } else if self.config.foreground_detection {
                    "Enabled"
                } else {
                    "Disabled in configuration"
                }
            );
            let _ = writeln!(
                report,
                "  Last activity observed by Discodex: {}",
                timestamp(last_activity)
            );
            let mut roots = 0;
            for source in self
                .ingestion
                .sources()
                .iter()
                .filter(|source| source.provider == provider)
            {
                roots += 1;
                let _ = writeln!(report, "  Source {roots} ({:?})", source.store);
                match std::fs::read_dir(&source.root) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        report.push_str("    Session access: source directory not found\n");
                        continue;
                    }
                    Err(error) => {
                        let _ = writeln!(
                            report,
                            "    Session access: directory unavailable ({:?})",
                            error.kind()
                        );
                        continue;
                    }
                    Ok(_) => {}
                }
                let candidates = match source.store {
                    SessionStore::LogFiles => ingestion::recent_logs(source),
                    SessionStore::Sqlite => sources::databases(provider, &source.root),
                    _ => watcher::recent_documents(&source.root, provider, 12),
                };
                let accessible = candidates
                    .iter()
                    .filter(|path| {
                        if source.store == SessionStore::Sqlite {
                            sources::open_database(path)
                                .and_then(|connection| {
                                    connection.query_row("PRAGMA schema_version", [], |row| {
                                        row.get::<_, i64>(0)
                                    })
                                })
                                .is_ok()
                        } else {
                            std::fs::File::open(path).is_ok()
                        }
                    })
                    .count();
                let _ = writeln!(
                    report,
                    "    Session access: {} recent candidates, {accessible} readable",
                    candidates.len()
                );
                if candidates.is_empty() {
                    report.push_str("    No supported session files found; check the provider's log location or format\n");
                }
                let observations: Vec<_> = self
                    .ingestion
                    .observations
                    .iter()
                    .filter(|((root, _), obs)| obs.provider == provider && root == &source.root)
                    .map(|(_, obs)| obs)
                    .collect();
                let count: usize = observations.iter().map(|obs| obs.recognized_events).sum();
                let _ = writeln!(report, "    Recognized events this run: {count}");
                let _ = writeln!(
                    report,
                    "    Last ingestion check: {}",
                    timestamp(observations.iter().map(|obs| obs.last_read_ms).max())
                );
                for obs in observations {
                    if let Err(error) = &obs.result {
                        let _ = writeln!(report, "    Read problem: {error}");
                    }
                }
                if count == 0 {
                    report.push_str("    No recognized events this run; store may be idle, historical, or unsupported\n");
                }
                if provider == Provider::Aider {
                    report.push_str("    Monitoring: periodic transcript scan\n");
                } else {
                    let state = match self.watches.get(&source.root) {
                        Some(Ok(())) => "Watching".to_string(),
                        Some(Err(error)) => format!("Watch unavailable ({error})"),
                        None => "Not watching in this snapshot".to_string(),
                    };
                    let _ = writeln!(report, "    Monitoring: {state}");
                }
            }
            if roots == 0 {
                report.push_str("  No session source configured\n");
            }
            report.push('\n');
        }
        report
    }
}

fn timestamp(value: Option<i64>) -> String {
    value
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|time| time.to_rfc3339())
        .unwrap_or_else(|| "None observed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{events::ParsedEvent, provider::SessionSource};

    #[test]
    fn report_distinguishes_missing_empty_unreadable_and_unrecognized_sources() {
        let root =
            std::env::temp_dir().join(format!("discodex-diagnostics-{}", std::process::id()));
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::create_dir_all(root.join("unknown")).unwrap();
        std::fs::create_dir_all(root.join("broken")).unwrap();
        std::fs::write(root.join("not-a-directory"), "").unwrap();
        std::fs::write(
            root.join("unknown/rollout-1.jsonl"),
            "{\"future_format\":\"PRIVATE_TEXT\"}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("broken/ui_messages.json"),
            "PRIVATE_MALFORMED_JSON",
        )
        .unwrap();
        let sources = [
            (Provider::ClaudeCode, "missing", SessionStore::LogFiles),
            (Provider::Codex, "unknown", SessionStore::LogFiles),
            (Provider::Antigravity, "empty", SessionStore::LogFiles),
            (Provider::Crush, "not-a-directory", SessionStore::Sqlite),
            (Provider::RooCode, "broken", SessionStore::JsonFiles),
        ]
        .into_iter()
        .map(|(provider, name, store)| SessionSource {
            provider,
            root: root.join(name),
            store,
        })
        .collect();
        let mut ingestion = Ingestion::new(sources);
        let mut activity = ActivityState::default();
        ingestion.bootstrap(&mut activity);
        let config = AppConfig::default();
        let report = DiagnosticSnapshot {
            ingestion: &ingestion,
            activity: &activity,
            config: &config,
            focused: Some(Provider::Codex),
            discord: "Disconnected. Last IPC error: test failure",
            watches: &HashMap::new(),
        }
        .render();
        for expected in [
            "source directory not found",
            "directory unavailable",
            "No supported session files found",
            "1 recent candidates, 1 readable",
            "No recognized events this run",
            "Invalid session JSON",
            "Focused provider: OpenAI Codex",
            "Disabled by provider policy",
            "Last IPC error: test failure",
        ] {
            assert!(report.contains(expected), "missing {expected} in {report}");
        }
        assert!(!report.contains("PRIVATE_TEXT"));
        assert!(!report.contains("PRIVATE_MALFORMED_JSON"));
        assert!(!report.contains(root.to_str().unwrap()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completed_sessions_keep_their_last_activity_in_diagnostics() {
        let mut activity = ActivityState::default();
        let path = std::path::Path::new("PRIVATE_SESSION_PATH");
        activity.apply(
            path,
            Provider::Codex,
            ParsedEvent::TaskStarted {
                started_at_ms: 1_000,
            },
            1_000,
        );
        activity.apply(path, Provider::Codex, ParsedEvent::Completed, 2_000);
        assert!(activity.selected_presence().is_none());
        let report = DiagnosticSnapshot {
            ingestion: &Ingestion::new(Vec::new()),
            activity: &activity,
            config: &AppConfig::default(),
            focused: None,
            discord: "Connected",
            watches: &HashMap::new(),
        }
        .render();
        assert!(report.contains("Discord: Connected"));
        assert!(report.contains("Last activity observed by Discodex: 1970-01-01T00:00:02+00:00"));
        assert!(!report.contains("PRIVATE_SESSION_PATH"));
    }

    #[test]
    fn crush_registry_reports_read_failures_for_databases_outside_the_source_root() {
        let root = std::env::temp_dir().join(format!(
            "discodex-diagnostics-registry-{}",
            std::process::id()
        ));
        let registry = root.join("registry");
        let external = root.join("external-project/.crush");
        std::fs::create_dir_all(&registry).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        std::fs::write(external.join("crush.db"), "invalid SQLite database").unwrap();
        std::fs::write(
            registry.join("projects.json"),
            serde_json::to_vec(&serde_json::json!([
                { "path": root.join("external-project"), "data_dir": external, "last_accessed": 1 }
            ]))
            .unwrap(),
        )
        .unwrap();
        let mut ingestion = Ingestion::new(vec![SessionSource {
            provider: Provider::Crush,
            root: registry,
            store: SessionStore::Sqlite,
        }]);
        let mut activity = ActivityState::default();
        ingestion.bootstrap(&mut activity);
        let report = DiagnosticSnapshot {
            ingestion: &ingestion,
            activity: &activity,
            config: &AppConfig::default(),
            focused: None,
            discord: "Not connected",
            watches: &HashMap::new(),
        }
        .render();
        assert!(
            report.contains("Read problem: Database cannot be read"),
            "{report}"
        );
        assert!(
            !report.contains("Last ingestion check: None observed"),
            "{report}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
