use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    events, now_ms,
    provider::{self, Provider, SessionSource, SessionStore},
    sources,
    state::ActivityState,
    watcher::{self, LogTailer},
};

const RECENT_DOCUMENTS: usize = 12;
type DocumentState = (SystemTime, HashSet<String>);

#[derive(Debug)]
pub struct ReadObservation {
    pub provider: Provider,
    pub last_read_ms: i64,
    pub result: Result<(), String>,
    pub recognized_events: usize,
}

pub struct Ingestion {
    sources: Vec<SessionSource>,
    tailer: LogTailer,
    documents: HashMap<PathBuf, DocumentState>,
    databases: HashMap<PathBuf, HashSet<String>>,
    antigravity_workspace_checked: HashSet<PathBuf>,
    pub observations: HashMap<(PathBuf, PathBuf), ReadObservation>,
}

impl Ingestion {
    pub fn new(sources: Vec<SessionSource>) -> Self {
        Self {
            sources,
            tailer: LogTailer::default(),
            documents: HashMap::new(),
            databases: HashMap::new(),
            antigravity_workspace_checked: HashSet::new(),
            observations: HashMap::new(),
        }
    }

    pub fn sources(&self) -> &[SessionSource] {
        &self.sources
    }

    pub fn bootstrap(&mut self, activity: &mut ActivityState) {
        for source in self.sources.clone() {
            if source.store == SessionStore::LogFiles {
                for path in recent_logs(&source) {
                    let modified = std::fs::metadata(&path)
                        .and_then(|metadata| metadata.modified())
                        .unwrap_or(UNIX_EPOCH);
                    if SystemTime::now()
                        .duration_since(modified)
                        .unwrap_or_default()
                        <= Duration::from_secs(900)
                    {
                        self.read_log(activity, &source, &path, Some(modified));
                    }
                }
            } else {
                self.poll_source(activity, &source);
            }
        }
    }

    pub fn poll_stores(&mut self, activity: &mut ActivityState) {
        for source in self.sources.clone() {
            if source.store != SessionStore::LogFiles {
                self.poll_source(activity, &source);
            }
        }
    }

    pub fn poll_aider(&mut self, activity: &mut ActivityState) {
        for source in self.sources.clone() {
            if source.provider == Provider::Aider {
                for path in recent_logs(&source) {
                    self.read_log(activity, &source, &path, None);
                }
            }
        }
    }

    pub fn handle_path(&mut self, activity: &mut ActivityState, path: &Path) {
        let Some(source) = provider::source_for_path(&self.sources, path).cloned() else {
            return;
        };
        match source.store {
            SessionStore::LogFiles => self.read_log(activity, &source, path, None),
            SessionStore::JsonFiles | SessionStore::JsonLineSnapshot => {
                self.read_document(activity, &source, path);
            }
            SessionStore::Sqlite => self.poll_source(activity, &source),
        }
    }

    fn observe(&mut self, path: &Path, source: &SessionSource, result: Result<usize, String>) {
        let key = (source.root.clone(), path.to_path_buf());
        let previous = self
            .observations
            .get(&key)
            .map_or(0, |obs| obs.recognized_events);
        let recognized_events = previous + result.as_ref().copied().unwrap_or_default();
        self.observations.insert(
            key,
            ReadObservation {
                provider: source.provider,
                last_read_ms: now_ms(),
                result: result.map(|_| ()),
                recognized_events,
            },
        );
    }

    fn read_log(
        &mut self,
        activity: &mut ActivityState,
        source: &SessionSource,
        path: &Path,
        historical: Option<SystemTime>,
    ) {
        let result = if historical.is_some() {
            self.tailer.read_from_start(path)
        } else {
            self.tailer.read_new(path)
        };
        let result = result
            .map_err(|error| format!("Log read failed ({:?})", error.kind()))
            .map(|lines| {
                let observed = historical.and_then(system_time_ms).unwrap_or_else(now_ms);
                let count = apply_lines(
                    activity,
                    path,
                    source.provider,
                    lines,
                    observed,
                    historical.is_some(),
                );
                if source.provider == Provider::Antigravity
                    && count > 0
                    && !activity.has_direct_project(path)
                    && !self.antigravity_workspace_checked.contains(path)
                {
                    match lookup_antigravity_workspace(path) {
                        AntigravityWorkspaceLookup::Found(event) => {
                            activity.apply(path, source.provider, event, observed);
                        }
                        AntigravityWorkspaceLookup::Empty => {
                            self.antigravity_workspace_checked
                                .insert(path.to_path_buf());
                        }
                        AntigravityWorkspaceLookup::Unavailable => {}
                    }
                }
                count
            });
        self.observe(path, source, result);
    }

    fn read_document(&mut self, activity: &mut ActivityState, source: &SessionSource, path: &Path) {
        let result = (|| {
            let modified = std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .map_err(|error| format!("Session metadata unavailable ({:?})", error.kind()))?;
            if self
                .documents
                .get(path)
                .is_some_and(|(seen, _)| *seen == modified)
            {
                return Ok(0);
            }
            let text = std::fs::read_to_string(path)
                .map_err(|error| format!("Session read failed ({:?})", error.kind()))?;
            let document = if source.store == SessionStore::JsonFiles {
                Some(
                    serde_json::from_str::<serde_json::Value>(&text)
                        .map_err(|_| "Invalid session JSON; retrying".to_string())?,
                )
            } else {
                None
            };
            let entry = self
                .documents
                .entry(path.to_path_buf())
                .or_insert_with(|| (modified, HashSet::new()));
            let events = match document {
                Some(document) => {
                    sources::parse_json_document(source.provider, &document, &mut entry.1)
                }
                None => {
                    sources::parse_jsonl_snapshot_at(source.provider, path, &text, &mut entry.1)
                }
            };
            entry.0 = modified;
            let count = events.len();
            let key = provider::session_key(source.provider, path);
            for event in events {
                activity.apply(&key, source.provider, event, now_ms());
            }
            Ok(count)
        })();
        self.observe(path, source, result);
    }

    fn poll_source(&mut self, activity: &mut ActivityState, source: &SessionSource) {
        if source.store != SessionStore::Sqlite {
            for path in watcher::recent_documents(&source.root, source.provider, RECENT_DOCUMENTS) {
                self.read_document(activity, source, &path);
            }
            return;
        }
        for database in sources::databases(source.provider, &source.root) {
            let result = sources::open_database(&database)
                .and_then(|connection| {
                    connection.query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0))
                })
                .map_err(|_| {
                    "Database cannot be read (access, lock, or invalid SQLite store)".to_string()
                })
                .map(|_| {
                    let seen = self.databases.entry(database.clone()).or_default();
                    let events = sources::poll_database(source.provider, &database, seen);
                    let count = events.len();
                    for event in events {
                        let key =
                            PathBuf::from(format!("{}#{}", database.display(), event.session));
                        activity.apply(&key, source.provider, event.event, now_ms());
                    }
                    count
                });
            self.observe(&database, source, result);
        }
    }
}

pub fn recent_logs(source: &SessionSource) -> Vec<PathBuf> {
    if source.provider == Provider::Aider {
        watcher::recent_logs_at_depth(&source.root, source.provider, 8, 2)
    } else {
        watcher::recent_logs(&source.root, source.provider, 8)
    }
}

fn apply_lines(
    activity: &mut ActivityState,
    path: &Path,
    provider: Provider,
    lines: Vec<String>,
    observed: i64,
    historical: bool,
) -> usize {
    let mut count = 0;
    for line in lines {
        let observed = if historical {
            events::line_timestamp_ms(&line).unwrap_or(observed)
        } else {
            observed
        };
        for event in sources::parse_log_line_at(provider, path, &line) {
            activity.apply(path, provider, event, observed);
            count += 1;
        }
    }
    count
}

#[cfg(test)]
pub(crate) fn apply_historical_lines(
    activity: &mut ActivityState,
    path: &Path,
    provider: Provider,
    lines: Vec<String>,
    observed: i64,
) {
    apply_lines(activity, path, provider, lines, observed, true);
}

fn system_time_ms(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
}

enum AntigravityWorkspaceLookup {
    Found(events::ParsedEvent),
    Empty,
    Unavailable,
}

fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path
    }
}

fn extract_antigravity_db_and_id(path: &Path) -> Option<(PathBuf, String)> {
    use std::path::Component;

    let components: Vec<Component<'_>> = path.components().collect();
    for i in (1..components.len()).rev() {
        let Component::Normal(seg) = components[i] else {
            continue;
        };
        if !seg.to_string_lossy().eq_ignore_ascii_case("brain") {
            continue;
        }
        let Component::Normal(parent) = components[i - 1] else {
            continue;
        };
        if !parent.to_string_lossy().eq_ignore_ascii_case("antigravity") {
            continue;
        }
        let Some(Component::Normal(id_os)) = components.get(i + 1) else {
            continue;
        };
        let id = id_os.to_str()?;
        if id.is_empty() || id.starts_with('.') {
            return None;
        }
        let mut root = PathBuf::new();
        for component in &components[..i] {
            root.push(component);
        }
        if root.as_os_str().is_empty() {
            return None;
        }
        let db_path = strip_verbatim_prefix(root.join("conversation_summaries.db"));
        return Some((db_path, id.to_string()));
    }
    None
}

fn lookup_antigravity_workspace(path: &Path) -> AntigravityWorkspaceLookup {
    let Some((db_path, conversation_id)) = extract_antigravity_db_and_id(path) else {
        return AntigravityWorkspaceLookup::Unavailable;
    };
    if !db_path.is_file() {
        return AntigravityWorkspaceLookup::Unavailable;
    }
    let Ok(connection) = sources::open_database(&db_path) else {
        return AntigravityWorkspaceLookup::Unavailable;
    };
    let result: rusqlite::Result<Option<String>> = connection.query_row(
        "SELECT workspace_uris FROM conversation_summaries WHERE conversation_id = ?1 LIMIT 1",
        [conversation_id.as_str()],
        |row| Ok(sources::column_text(row, 0)),
    );
    match result {
        Ok(Some(raw)) if !raw.trim().is_empty() => {
            match events::metadata_from_workspace_uris_json(&raw) {
                Some(event) => AntigravityWorkspaceLookup::Found(event),
                None => AntigravityWorkspaceLookup::Empty,
            }
        }
        Ok(_) => AntigravityWorkspaceLookup::Empty,
        Err(_) => AntigravityWorkspaceLookup::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_document_is_retried_and_unchanged_records_are_not_replayed() {
        let root = std::env::temp_dir().join(format!(
            "discodex-ingestion-document-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("ui_messages.json");
        std::fs::write(&path, "[").unwrap();
        let source = SessionSource {
            provider: Provider::RooCode,
            root: root.clone(),
            store: SessionStore::JsonFiles,
        };
        let mut ingestion = Ingestion::new(vec![source]);
        let mut activity = ActivityState::default();
        ingestion.bootstrap(&mut activity);
        assert!(activity.selected_presence().is_none());
        let key = (root.clone(), path.clone());
        assert!(ingestion.observations[&key].result.is_err());
        std::fs::write(
            &path,
            r#"[{"ts":1792540721901,"type":"say","say":"api_req_started"}]"#,
        )
        .unwrap();
        ingestion.poll_stores(&mut activity);
        assert!(activity.selected_presence().is_some());
        assert!(ingestion.observations[&key].result.is_ok());
        let count = ingestion.observations[&key].recognized_events;
        activity.discard_stale_active(now_ms() + 180_001, 180_000);
        assert!(activity.selected_presence().is_none());
        ingestion.poll_stores(&mut activity);
        assert!(activity.selected_presence().is_none());
        assert_eq!(ingestion.observations[&key].recognized_events, count);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn log_notifications_tail_only_complete_new_records() {
        use std::io::Write;
        let root =
            std::env::temp_dir().join(format!("discodex-ingestion-log-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("rollout-1.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n",
        )
        .unwrap();
        let mut ingestion = Ingestion::new(vec![SessionSource {
            provider: Provider::Codex,
            root: root.clone(),
            store: SessionStore::LogFiles,
        }]);
        let mut activity = ActivityState::default();
        ingestion.bootstrap(&mut activity);
        assert!(activity.selected_presence().is_some());
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        write!(
            file,
            "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\"}}}}"
        )
        .unwrap();
        ingestion.handle_path(&mut activity, &path);
        assert!(activity.selected_presence().is_some());
        writeln!(file).unwrap();
        ingestion.handle_path(&mut activity, &path);
        assert!(activity.selected_presence().is_none());
        ingestion.handle_path(&mut activity, &path);
        assert!(activity.selected_presence().is_none());
        drop(file);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn extracts_antigravity_db_and_id_for_standard_and_verbatim_paths() {
        let standard = Path::new(
            r"C:\Users\PC\.gemini\antigravity\brain\fe535acc-0687-4b43-ba85-18a33c0f7221\.system_generated\logs\transcript.jsonl",
        );
        assert_eq!(
            extract_antigravity_db_and_id(standard),
            Some((
                PathBuf::from(r"C:\Users\PC\.gemini\antigravity\conversation_summaries.db"),
                "fe535acc-0687-4b43-ba85-18a33c0f7221".to_string(),
            ))
        );

        let verbatim = Path::new(
            r"\\?\C:\Users\PC\.gemini\antigravity\brain\fe535acc-0687-4b43-ba85-18a33c0f7221\.system_generated\logs\transcript.jsonl",
        );
        assert_eq!(
            extract_antigravity_db_and_id(verbatim),
            Some((
                PathBuf::from(r"C:\Users\PC\.gemini\antigravity\conversation_summaries.db"),
                "fe535acc-0687-4b43-ba85-18a33c0f7221".to_string(),
            ))
        );

        let hidden = Path::new(r"C:\Users\PC\.gemini\antigravity\brain\.hidden\transcript.jsonl");
        assert_eq!(extract_antigravity_db_and_id(hidden), None);

        let account_named_brain = Path::new(
            r"C:\Users\brain\.gemini\antigravity\brain\conv-123\.system_generated\logs\transcript.jsonl",
        );
        assert_eq!(
            extract_antigravity_db_and_id(account_named_brain),
            Some((
                PathBuf::from(r"C:\Users\brain\.gemini\antigravity\conversation_summaries.db"),
                "conv-123".to_string(),
            ))
        );

        let verbatim_account_brain = Path::new(
            r"\\?\C:\Users\brain\.gemini\antigravity\brain\conv-123\.system_generated\logs\transcript.jsonl",
        );
        assert_eq!(
            extract_antigravity_db_and_id(verbatim_account_brain),
            Some((
                PathBuf::from(r"C:\Users\brain\.gemini\antigravity\conversation_summaries.db"),
                "conv-123".to_string(),
            ))
        );
    }

    #[test]
    fn antigravity_resolves_workspace_from_conversation_summaries_db_and_keeps_orchestrator_variant()
     {
        let root =
            std::env::temp_dir().join(format!("discodex-ingestion-agy-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let agy_root = root.join("antigravity");
        let conv_id = "conv-neon-123";
        let log_dir = agy_root
            .join("brain")
            .join(conv_id)
            .join(".system_generated")
            .join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        let db_path = agy_root.join("conversation_summaries.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE conversation_summaries (conversation_id TEXT PRIMARY KEY, workspace_uris TEXT NOT NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_summaries (conversation_id, workspace_uris) VALUES (?1, ?2)",
            rusqlite::params![conv_id, r#"["file:///Z:/nonexistent/Discodex"]"#],
        )
        .unwrap();
        drop(conn);

        let transcript = log_dir.join("transcript.jsonl");
        let user_input = serde_json::json!({
            "step_index": 0,
            "source": "USER_EXPLICIT",
            "type": "USER_INPUT",
            "status": "DONE",
            "created_at": "2026-11-19T07:30:12Z",
            "content": "<USER_REQUEST>\nhi\n</USER_REQUEST>\n<USER_SETTINGS_CHANGE>\nThe user changed setting `Model Selection` from None to Orchestrator (Neon). No need to comment.\n</USER_SETTINGS_CHANGE>"
        })
        .to_string();
        let command = serde_json::json!({
            "step_index": 1,
            "source": "MODEL",
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "model": "gemini-pro-orchestrator-neon",
            "created_at": "2026-11-19T07:30:14Z",
            "tool_calls": [{
                "name": "run_command",
                "arguments": { "CommandLine": "cargo test" }
            }]
        })
        .to_string();
        std::fs::write(&transcript, format!("{user_input}\n{command}\n")).unwrap();

        let mut ingestion = Ingestion::new(vec![SessionSource {
            provider: Provider::Antigravity,
            root: agy_root.join("brain"),
            store: SessionStore::LogFiles,
        }]);
        let mut activity = ActivityState::default();
        ingestion.handle_path(&mut activity, &transcript);

        let presence = activity.selected_presence().unwrap();
        assert_eq!(presence.provider, Provider::Antigravity);
        assert_eq!(presence.project, "Discodex");
        assert_eq!(presence.agent.as_deref(), Some("Orchestrator (Neon)"));
        assert_eq!(presence.activity, "Running Rust tests");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn antigravity_empty_workspace_uris_caches_checked_path() {
        let root = std::env::temp_dir().join(format!(
            "discodex-ingestion-agy-empty-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let agy_root = root.join("antigravity");
        let conv_id = "conv-empty-456";
        let log_dir = agy_root
            .join("brain")
            .join(conv_id)
            .join(".system_generated")
            .join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        let db_path = agy_root.join("conversation_summaries.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE conversation_summaries (conversation_id TEXT PRIMARY KEY, workspace_uris TEXT NOT NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_summaries (conversation_id, workspace_uris) VALUES (?1, ?2)",
            rusqlite::params![conv_id, ""],
        )
        .unwrap();
        drop(conn);

        let transcript = log_dir.join("transcript.jsonl");
        let user_input = serde_json::json!({
            "step_index": 0,
            "source": "USER_EXPLICIT",
            "type": "USER_INPUT",
            "status": "DONE",
            "created_at": "2026-11-19T07:30:12Z",
            "content": "<USER_REQUEST>\nhello\n</USER_REQUEST>"
        })
        .to_string();
        std::fs::write(&transcript, format!("{user_input}\n")).unwrap();

        let mut ingestion = Ingestion::new(vec![SessionSource {
            provider: Provider::Antigravity,
            root: agy_root.join("brain"),
            store: SessionStore::LogFiles,
        }]);
        let mut activity = ActivityState::default();
        ingestion.handle_path(&mut activity, &transcript);

        let presence = activity.selected_presence().unwrap();
        assert_eq!(presence.project, "AI session");
        assert!(
            ingestion
                .antigravity_workspace_checked
                .contains(&transcript)
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn antigravity_replaces_inherited_project_with_own_summary_workspace() {
        let root = std::env::temp_dir().join(format!(
            "discodex-ingestion-agy-inherit-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let agy_root = root.join("antigravity");
        let conv_a = "conv-a";
        let conv_b = "conv-b";
        let dir_a = agy_root
            .join("brain")
            .join(conv_a)
            .join(".system_generated")
            .join("logs");
        let dir_b = agy_root
            .join("brain")
            .join(conv_b)
            .join(".system_generated")
            .join("logs");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();

        let db_path = agy_root.join("conversation_summaries.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE conversation_summaries (conversation_id TEXT PRIMARY KEY, workspace_uris TEXT NOT NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_summaries (conversation_id, workspace_uris) VALUES (?1, ?2)",
            rusqlite::params![conv_b, r#"["file:///Z:/nonexistent/ProjectB"]"#],
        )
        .unwrap();
        drop(conn);

        let transcript_b = dir_b.join("transcript.jsonl");
        let b_start = serde_json::json!({
            "step_index": 0,
            "source": "USER_EXPLICIT",
            "type": "USER_INPUT",
            "status": "DONE",
            "created_at": "2026-11-19T07:30:10Z",
            "content": "<USER_REQUEST>\nhi\n</USER_REQUEST>"
        })
        .to_string();
        std::fs::write(&transcript_b, format!("{b_start}\n")).unwrap();

        let mut ingestion = Ingestion::new(vec![SessionSource {
            provider: Provider::Antigravity,
            root: agy_root.join("brain"),
            store: SessionStore::LogFiles,
        }]);
        let mut activity = ActivityState::default();

        activity.apply(
            &transcript_b,
            Provider::Antigravity,
            events::ParsedEvent::TaskStarted {
                started_at_ms: 1_000,
            },
            1_000,
        );
        let transcript_a = dir_a.join("transcript.jsonl");
        activity.apply(
            &transcript_a,
            Provider::Antigravity,
            events::ParsedEvent::SessionMetadata {
                project: "ProjectA".to_string(),
                cwd: r"Z:\nonexistent\ProjectA".to_string(),
            },
            1_000,
        );
        assert_eq!(activity.selected_presence().unwrap().project, "ProjectA");
        assert!(!activity.has_direct_project(&transcript_b));

        ingestion.handle_path(&mut activity, &transcript_b);
        let presence = activity.selected_presence().unwrap();
        assert_eq!(presence.project, "ProjectB");
        assert!(activity.has_direct_project(&transcript_b));

        std::fs::remove_dir_all(root).unwrap();
    }
}
