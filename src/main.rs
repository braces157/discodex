#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
compile_error!("Discodex currently supports Windows only.");

mod config;
mod discord;
mod events;
mod foreground;
mod instance;
mod provider;
mod sources;
mod state;
mod tray;
mod watcher;

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use config::AppConfig;
use discord::{DiscordPublisher, PresenceSnapshot};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use provider::{Provider, SessionSource, SessionStore};
use state::ActivityState;
use tray::TrayStatus;
use watcher::LogTailer;

const DISCORD_RETRY: Duration = Duration::from_secs(5);
const PRESENCE_HEALTHCHECK: Duration = Duration::from_secs(15);
const TEST_PRESENCE_FOR: Duration = Duration::from_secs(15);
const SESSION_ACTIVE_TIMEOUT: Duration = Duration::from_secs(180);
const FOREGROUND_POLL: Duration = Duration::from_millis(1_000);
const FOREGROUND_DWELL_MS: i64 = 2_000;
const FOREGROUND_HOLD_MS: i64 = 15_000;
/// How often database-backed providers are polled for new rows.
const STORE_POLL: Duration = Duration::from_secs(2);
/// How many session documents of a provider are kept in memory.
const RECENT_DOCUMENTS: usize = 12;
/// How often the Aider transcripts of the workspace folders are scanned.
const AIDER_SCAN_INTERVAL: Duration = Duration::from_secs(15);

/// Modification time and the record keys that were already reported for a
/// session document.
type DocumentState = (SystemTime, HashSet<String>);

#[derive(Debug)]
enum WorkerEvent {
    Paths(Vec<PathBuf>),
    ToggleEnabled,
    ToggleForegroundDetection,
    ToggleStartup,
    TestPresence,
    OpenConfig,
    Exit,
}

#[derive(Debug, Default)]
struct ForegroundState {
    raw_provider: Option<Provider>,
    raw_since_ms: i64,
    established_provider: Option<Provider>,
    started_at_ms: i64,
    hold_deadline_ms: Option<i64>,
}

impl ForegroundState {
    fn refresh(&mut self, now_ms: i64) {
        self.refresh_with_provider(foreground::active_provider(), now_ms);
    }

    fn refresh_with_provider(&mut self, current_raw: Option<Provider>, now_ms: i64) {
        let current_raw = current_raw.filter(|provider| !provider.has_session_tracking());
        if current_raw != self.raw_provider {
            self.raw_provider = current_raw;
            self.raw_since_ms = now_ms;
        }

        match self.raw_provider {
            Some(provider) => {
                if Some(provider) == self.established_provider {
                    self.hold_deadline_ms = None;
                } else if now_ms.saturating_sub(self.raw_since_ms) >= FOREGROUND_DWELL_MS {
                    self.established_provider = Some(provider);
                    self.started_at_ms = self.raw_since_ms;
                    self.hold_deadline_ms = None;
                }
            }
            None => {
                if self.established_provider.is_some() {
                    if let Some(deadline) = self.hold_deadline_ms {
                        if now_ms >= deadline {
                            self.established_provider = None;
                            self.hold_deadline_ms = None;
                        }
                    } else {
                        self.hold_deadline_ms = Some(now_ms + FOREGROUND_HOLD_MS);
                    }
                } else {
                    self.hold_deadline_ms = None;
                }
            }
        }
    }

    fn next_deadline_ms(&self) -> Option<i64> {
        let mut deadlines = Vec::new();
        if self.raw_provider.is_some() && self.raw_provider != self.established_provider {
            deadlines.push(self.raw_since_ms + FOREGROUND_DWELL_MS);
        }
        if let Some(hold) = self.hold_deadline_ms {
            deadlines.push(hold);
        }
        deadlines.into_iter().min()
    }

    fn presence(&self) -> Option<PresenceSnapshot> {
        let provider = self.established_provider?;
        if provider.has_session_tracking() {
            return None;
        }
        Some(PresenceSnapshot {
            provider,
            project: provider.display_name().to_string(),
            project_stack: None,
            current_file: None,
            current_language: None,
            activity: provider.default_activity().to_string(),
            phase: state::Phase::Thinking,
            started_at_ms: self.started_at_ms,
            agent: None,
            reasoning: None,
        })
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn log_line(msg: &str) {
    if let Ok(appdata) = std::env::var("APPDATA") {
        let path = std::path::PathBuf::from(appdata)
            .join("Discodex")
            .join("discodex.log");
        use std::io::Write;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(meta) = std::fs::metadata(&path)
            && meta.len() > 500_000
        {
            let _ = std::fs::remove_file(&path);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "[{}] {}", now_ms(), msg);
        }
    }
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        log_line(&format!("PANIC OCCURRED: {:?}", info));
    }));
    log_line("main() started");
    if let Err(error) = run() {
        log_line(&format!("run() error: {error}"));
        let message = format!("Discodex could not start:\n\n{error}\0");
        let wide: Vec<u16> = message.encode_utf16().collect();
        let title: Vec<u16> = "Discodex\0".encode_utf16().collect();
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
                None,
                windows::core::PCWSTR(wide.as_ptr()),
                windows::core::PCWSTR(title.as_ptr()),
                windows::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
            );
        }
    }
    log_line("main() finished");
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    log_line("run() started, acquiring single instance...");
    let Some(_single_instance) = instance::SingleInstance::acquire()
        .map_err(|error| format!("Could not acquire the single-instance mutex: {error}"))?
    else {
        log_line("single instance mutex already locked! exiting.");
        return Ok(());
    };
    log_line("single instance acquired.");

    let (config, config_path) = AppConfig::load_or_create().map_err(|error| {
        format!("Could not load or create %APPDATA%\\Discodex\\config.toml: {error}")
    })?;
    log_line(&format!("config loaded from {:?}", config_path));
    sync_start_with_windows(config.start_with_windows);

    let session_sources = provider::session_sources();

    let status = Arc::new(Mutex::new(TrayStatus {
        enabled: config.enabled,
        foreground_detection: config.foreground_detection,
        start_with_windows: config.start_with_windows,
        text: "Starting...".to_string(),
    }));

    let (tx, rx) = mpsc::channel::<WorkerEvent>();
    let worker_status = Arc::clone(&status);
    let worker_tx = tx.clone();
    let worker_config_path = config_path.clone();
    let worker = thread::spawn(move || {
        log_line("worker_loop started");
        worker_loop(
            rx,
            worker_tx,
            worker_status,
            config,
            worker_config_path,
            session_sources,
        );
        log_line("worker_loop exited");
    });

    log_line("starting tray::run...");
    let res = tray::run(tx, status);
    log_line(&format!("tray::run returned: {:?}", res));
    res.map_err(|error| format!("Could not create the notification-area tray: {error}"))?;

    let _ = worker.join();
    Ok(())
}

fn sync_start_with_windows(enabled: bool) {
    if let Err(error) = config::set_start_with_windows(enabled) {
        log_line(&format!(
            "Could not update Start with Windows (enabled={enabled}); continuing without changing the startup entry: {error}"
        ));
    }
}

fn worker_loop(
    rx: mpsc::Receiver<WorkerEvent>,
    tx: mpsc::Sender<WorkerEvent>,
    status: Arc<Mutex<TrayStatus>>,
    mut config: AppConfig,
    config_path: PathBuf,
    session_sources: Vec<SessionSource>,
) {
    let mut activity = ActivityState::default();
    let mut tailer = LogTailer::default();
    let mut document_state: HashMap<PathBuf, DocumentState> = HashMap::new();
    let mut database_state: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    let mut effective_application_id = config.effective_discord_application_id();
    let mut publisher = DiscordPublisher::new(effective_application_id.clone());
    let mut test_presence_ms: Option<(i64, i64)> = None;
    let mut next_retry_ms = 0_i64;
    let mut next_healthcheck_ms = 0_i64;
    let mut next_foreground_poll_ms = 0_i64;
    let mut next_store_poll_ms = 0_i64;
    let mut next_aider_scan_ms = 0_i64;
    let mut foreground_state = ForegroundState::default();

    for source in &session_sources {
        if !source.root.exists() {
            continue;
        }
        log_line(&format!(
            "scanning recent logs for {:?} at {:?}",
            source.provider, source.root
        ));
        match source.store {
            SessionStore::LogFiles => {
                let now_sys = SystemTime::now();
                let logs = if source.provider == Provider::Aider {
                    // Aider writes into the working tree of a repository, so
                    // the scan stays shallow.
                    watcher::recent_logs_at_depth(&source.root, source.provider, 8, 2)
                } else {
                    watcher::recent_logs(&source.root, source.provider, 8)
                };
                log_line(&format!(
                    "found {} recent logs for {:?}",
                    logs.len(),
                    source.provider
                ));
                for path in logs {
                    let modified = std::fs::metadata(&path)
                        .and_then(|metadata| metadata.modified())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    if now_sys.duration_since(modified).unwrap_or_default()
                        > Duration::from_secs(900)
                    {
                        continue;
                    }
                    log_line(&format!("reading log {:?}", path));
                    if let Ok(lines) = tailer.read_from_start(&path) {
                        let fallback_observed_at_ms =
                            system_time_ms(modified).unwrap_or_else(now_ms);
                        apply_historical_lines(
                            &mut activity,
                            &path,
                            source.provider,
                            lines,
                            fallback_observed_at_ms,
                        );
                    }
                }
            }
            SessionStore::JsonFiles | SessionStore::JsonLineSnapshot => {
                let documents =
                    watcher::recent_documents(&source.root, source.provider, RECENT_DOCUMENTS);
                log_line(&format!(
                    "found {} recent documents for {:?}",
                    documents.len(),
                    source.provider
                ));
                for path in documents {
                    match source.store {
                        SessionStore::JsonLineSnapshot => apply_snapshot(
                            &mut activity,
                            &mut document_state,
                            &path,
                            source.provider,
                        ),
                        _ => apply_document(
                            &mut activity,
                            &mut document_state,
                            &path,
                            source.provider,
                        ),
                    }
                }
            }
            SessionStore::Sqlite => {
                poll_databases(
                    &mut activity,
                    &mut database_state,
                    source.provider,
                    &source.root,
                );
            }
        }
    }
    let startup_now = now_ms();
    activity.expire_terminal_candidates(startup_now);
    activity.discard_stale_active(startup_now, SESSION_ACTIVE_TIMEOUT.as_millis() as i64);
    log_line(&format!(
        "read historical logs. Initial presence: {:?}",
        activity.selected_presence()
    ));

    let watch_tx = tx.clone();
    let mut fs_watcher = RecommendedWatcher::new(
        move |result: notify::Result<notify::Event>| {
            if let Ok(event) = result
                && !event.paths.is_empty()
            {
                let _ = watch_tx.send(WorkerEvent::Paths(event.paths));
            }
        },
        notify::Config::default(),
    )
    .ok();

    let mut watched_roots = HashSet::new();
    if let Some(watcher) = fs_watcher.as_mut() {
        for source in &session_sources {
            if source.provider == Provider::Aider {
                // Aider writes into the working tree of a repository. Watching
                // the whole home directory would report every file change of
                // the machine, so the transcript is polled instead.
                continue;
            }
            if source.root.exists()
                && watcher
                    .watch(&source.root, RecursiveMode::Recursive)
                    .is_ok()
            {
                log_line(&format!("watching source root: {:?}", source.root));
                watched_roots.insert(source.root.clone());
            }
        }
        if let Some(parent) = config_path.parent() {
            let _ = watcher.watch(parent, RecursiveMode::NonRecursive);
        }
    }

    let mut last_desired: Option<PresenceSnapshot> = None;

    loop {
        let now = now_ms();
        if config.foreground_detection
            && (now >= next_foreground_poll_ms
                || foreground_state
                    .next_deadline_ms()
                    .is_some_and(|dl| now >= dl))
        {
            foreground_state.refresh(now);
            next_foreground_poll_ms = now + FOREGROUND_POLL.as_millis() as i64;
        } else if !config.foreground_detection {
            foreground_state = ForegroundState::default();
            next_foreground_poll_ms = now + FOREGROUND_POLL.as_millis() as i64;
        }

        if now >= next_store_poll_ms {
            next_store_poll_ms = now + STORE_POLL.as_millis() as i64;
            for source in &session_sources {
                match source.store {
                    SessionStore::Sqlite => {
                        if !source.root.exists() {
                            continue;
                        }
                        poll_databases(
                            &mut activity,
                            &mut database_state,
                            source.provider,
                            &source.root,
                        );
                    }
                    SessionStore::JsonFiles | SessionStore::JsonLineSnapshot => {
                        // A rewrite can land in the middle of a write, so the
                        // documents are re-read even without a file event.
                        if !source.root.exists() {
                            continue;
                        }
                        let documents = watcher::recent_documents(
                            &source.root,
                            source.provider,
                            RECENT_DOCUMENTS,
                        );
                        for path in documents {
                            match source.store {
                                SessionStore::JsonLineSnapshot => apply_snapshot(
                                    &mut activity,
                                    &mut document_state,
                                    &path,
                                    source.provider,
                                ),
                                _ => apply_document(
                                    &mut activity,
                                    &mut document_state,
                                    &path,
                                    source.provider,
                                ),
                            }
                        }
                    }
                    SessionStore::LogFiles => {}
                }
            }
        }

        if let Some(watcher) = fs_watcher.as_mut() {
            for source in &session_sources {
                if source.provider == Provider::Aider {
                    continue;
                }
                if source.root.exists()
                    && !watched_roots.contains(&source.root)
                    && watcher
                        .watch(&source.root, RecursiveMode::Recursive)
                        .is_ok()
                {
                    log_line(&format!(
                        "dynamically watching source root: {:?}",
                        source.root
                    ));
                    watched_roots.insert(source.root.clone());
                }
            }
        }

        if now >= next_aider_scan_ms {
            next_aider_scan_ms = now + AIDER_SCAN_INTERVAL.as_millis() as i64;
            for source in &session_sources {
                if source.provider != Provider::Aider || !source.root.exists() {
                    continue;
                }
                let logs = watcher::recent_logs_at_depth(&source.root, source.provider, 8, 2);
                for path in logs {
                    if let Ok(lines) = tailer.read_new(&path) {
                        if !lines.is_empty() {
                            log_line(&format!("read {} lines from {:?}", lines.len(), path));
                        }
                        apply_lines(&mut activity, &path, source.provider, lines);
                    }
                }
            }
        }

        activity.expire_terminal_candidates(now);
        activity.discard_stale_active(now, SESSION_ACTIVE_TIMEOUT.as_millis() as i64);
        if test_presence_ms.is_some_and(|(deadline, _)| now >= deadline) {
            test_presence_ms = None;
        }

        let desired = if !config.enabled {
            None
        } else if let Some((_, started_at_ms)) = test_presence_ms {
            Some(PresenceSnapshot {
                provider: Provider::Codex,
                project: "Discodex Test".to_string(),
                project_stack: Some("Rust".to_string()),
                current_file: None,
                current_language: None,
                activity: "Testing rich presence".to_string(),
                phase: state::Phase::Thinking,
                started_at_ms,
                agent: Some("DeepCoder".to_string()),
                reasoning: Some("Evaluating test presence".to_string()),
            })
        } else {
            activity
                .selected_presence()
                .or_else(|| foreground_state.presence())
        };

        if desired != last_desired {
            log_line(&format!("desired presence changed: {:?}", desired));
            last_desired = desired.clone();
        }

        let can_attempt = desired.is_none() || publisher.is_connected() || now >= next_retry_ms;
        if can_attempt {
            let force_refresh =
                desired.is_some() && publisher.is_connected() && now >= next_healthcheck_ms;
            let sync_res = publisher.sync(desired.as_ref(), force_refresh);
            if let Err(error) = sync_res {
                log_line(&format!("publisher.sync error: {error}"));
                next_retry_ms = now + DISCORD_RETRY.as_millis() as i64;
                next_healthcheck_ms = 0;
            } else if publisher.is_connected() {
                next_retry_ms = 0;
                if next_healthcheck_ms == 0 || force_refresh {
                    next_healthcheck_ms = now + PRESENCE_HEALTHCHECK.as_millis() as i64;
                }
            } else {
                next_healthcheck_ms = 0;
            }
        }

        update_status(
            &status,
            &config,
            &effective_application_id,
            &publisher,
            desired.as_ref(),
        );

        let foreground_deadline = if config.foreground_detection {
            let poll_dl = next_foreground_poll_ms.max(now + 1);
            match foreground_state.next_deadline_ms() {
                Some(state_dl) => Some(poll_dl.min(state_dl.max(now + 1))),
                None => Some(poll_dl),
            }
        } else {
            None
        };

        let store_deadline = if session_sources.iter().any(|source| {
            source.store != SessionStore::LogFiles || source.provider == Provider::Aider
        }) {
            Some(next_store_poll_ms.max(now + 1))
        } else {
            None
        };

        let timeout = next_timeout(
            now,
            &[
                activity.next_terminal_deadline_ms(),
                activity.next_stale_deadline_ms(SESSION_ACTIVE_TIMEOUT.as_millis() as i64),
                test_presence_ms.map(|(deadline, _)| deadline),
                if desired.is_some() && !publisher.is_connected() {
                    Some(next_retry_ms.max(now + 1))
                } else {
                    None
                },
                if desired.is_some() && publisher.is_connected() {
                    Some(next_healthcheck_ms.max(now + 1))
                } else {
                    None
                },
                foreground_deadline,
                store_deadline,
            ],
        );

        let event = match timeout {
            Some(duration) => match rx.recv_timeout(duration) {
                Ok(event) => Some(event),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(event) => Some(event),
                Err(_) => break,
            },
        };

        let Some(event) = event else {
            continue;
        };

        match event {
            WorkerEvent::Paths(paths) => {
                let mut reload_config = false;
                for path in paths {
                    if path == config_path {
                        reload_config = true;
                        continue;
                    }
                    let Some(source) = provider::source_for_path(&session_sources, &path) else {
                        continue;
                    };
                    match source.store {
                        SessionStore::LogFiles => {
                            if let Ok(lines) = tailer.read_new(&path) {
                                if !lines.is_empty() {
                                    log_line(&format!(
                                        "read {} lines from {:?}",
                                        lines.len(),
                                        path
                                    ));
                                }
                                apply_lines(&mut activity, &path, source.provider, lines);
                            }
                        }
                        SessionStore::JsonFiles => {
                            apply_document(
                                &mut activity,
                                &mut document_state,
                                &path,
                                source.provider,
                            );
                        }
                        SessionStore::JsonLineSnapshot => {
                            apply_snapshot(
                                &mut activity,
                                &mut document_state,
                                &path,
                                source.provider,
                            );
                        }
                        SessionStore::Sqlite => {
                            poll_databases(
                                &mut activity,
                                &mut database_state,
                                source.provider,
                                &source.root,
                            );
                        }
                    }
                }
                if reload_config && let Ok(reloaded) = AppConfig::load(&config_path) {
                    let reloaded_application_id = reloaded.effective_discord_application_id();
                    let app_id_changed = reloaded_application_id != effective_application_id;
                    config = reloaded;
                    if app_id_changed {
                        effective_application_id = reloaded_application_id;
                        publisher = DiscordPublisher::new(effective_application_id.clone());
                    }
                    sync_start_with_windows(config.start_with_windows);
                }
            }
            WorkerEvent::ToggleEnabled => {
                config.enabled = !config.enabled;
                let _ = config.save(&config_path);
            }
            WorkerEvent::ToggleForegroundDetection => {
                config.foreground_detection = !config.foreground_detection;
                let _ = config.save(&config_path);
            }
            WorkerEvent::ToggleStartup => {
                config.start_with_windows = !config.start_with_windows;
                match config::set_start_with_windows(config.start_with_windows) {
                    Ok(()) => {
                        let _ = config.save(&config_path);
                    }
                    Err(error) => {
                        log_line(&format!("Could not toggle Start with Windows: {error}"));
                        config.start_with_windows = !config.start_with_windows;
                    }
                }
            }
            WorkerEvent::TestPresence => {
                let started_at_ms = now_ms();
                test_presence_ms = Some((
                    started_at_ms + TEST_PRESENCE_FOR.as_millis() as i64,
                    started_at_ms,
                ));
            }
            WorkerEvent::OpenConfig => {
                let _ = std::process::Command::new("notepad.exe")
                    .arg(&config_path)
                    .spawn();
            }
            WorkerEvent::Exit => break,
        }
    }

    let _ = publisher.sync(None, false);
}

fn apply_lines(
    activity: &mut ActivityState,
    path: &std::path::Path,
    provider: Provider,
    lines: Vec<String>,
) {
    apply_lines_at(activity, path, provider, lines, now_ms());
}

fn apply_lines_at(
    activity: &mut ActivityState,
    path: &std::path::Path,
    provider: Provider,
    lines: Vec<String>,
    observed_at_ms: i64,
) {
    for line in lines {
        for event in sources::parse_log_line_at(provider, path, &line) {
            activity.apply(path, provider, event, observed_at_ms);
        }
    }
}

/// Re-read a session document that the provider rewrites in place.
///
/// Only records that were not reported before are applied, so a rewritten file
/// never replays the whole conversation.
fn apply_document(
    activity: &mut ActivityState,
    state: &mut HashMap<PathBuf, DocumentState>,
    path: &std::path::Path,
    provider: Provider,
) {
    let modified = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    if let Some((seen_modified, _)) = state.get(path)
        && *seen_modified == modified
    {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(document) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    let entry = state
        .entry(path.to_path_buf())
        .or_insert_with(|| (modified, HashSet::new()));
    entry.0 = modified;
    let key = provider::session_key(provider, path);
    for event in sources::parse_json_document(provider, &document, &mut entry.1) {
        activity.apply(&key, provider, event, now_ms());
    }
}

/// Re-read a session file that holds JSON lines with conversation snapshots.
///
/// The harness appends a new snapshot of the whole conversation for every
/// change, so the file is re-read as a whole and only records that were not
/// reported before are applied.
fn apply_snapshot(
    activity: &mut ActivityState,
    state: &mut HashMap<PathBuf, DocumentState>,
    path: &std::path::Path,
    provider: Provider,
) {
    let modified = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    if let Some((seen_modified, _)) = state.get(path)
        && *seen_modified == modified
    {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let entry = state
        .entry(path.to_path_buf())
        .or_insert_with(|| (modified, HashSet::new()));
    entry.0 = modified;
    let key = provider::session_key(provider, path);
    for event in sources::parse_jsonl_snapshot_at(provider, path, &text, &mut entry.1) {
        activity.apply(&key, provider, event, now_ms());
    }
}

/// Poll the databases of a provider for rows that were written recently.
fn poll_databases(
    activity: &mut ActivityState,
    state: &mut HashMap<PathBuf, HashSet<String>>,
    provider: Provider,
    root: &std::path::Path,
) {
    for database in sources::databases(provider, root) {
        let seen = state.entry(database.clone()).or_default();
        for store_event in sources::poll_database(provider, &database, seen) {
            // One database holds many sessions, so every event is keyed by the
            // session it belongs to.
            let key =
                std::path::PathBuf::from(format!("{}#{}", database.display(), store_event.session));
            activity.apply(&key, provider, store_event.event, now_ms());
        }
    }
}

fn apply_historical_lines(
    activity: &mut ActivityState,
    path: &std::path::Path,
    provider: Provider,
    lines: Vec<String>,
    fallback_observed_at_ms: i64,
) {
    for line in lines {
        let observed_at_ms = events::line_timestamp_ms(&line).unwrap_or(fallback_observed_at_ms);
        for event in sources::parse_log_line_at(provider, path, &line) {
            activity.apply(path, provider, event, observed_at_ms);
        }
    }
}

fn system_time_ms(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
}

fn next_timeout(now: i64, deadlines: &[Option<i64>]) -> Option<Duration> {
    deadlines
        .iter()
        .flatten()
        .min()
        .map(|deadline| Duration::from_millis(deadline.saturating_sub(now).max(1) as u64))
}

fn update_status(
    shared: &Arc<Mutex<TrayStatus>>,
    config: &AppConfig,
    effective_application_id: &str,
    publisher: &DiscordPublisher,
    desired: Option<&PresenceSnapshot>,
) {
    let text = if !config.enabled {
        "Presence disabled".to_string()
    } else if effective_application_id.trim().is_empty() {
        "Discord Application ID missing from this build".to_string()
    } else if let Some(presence) = desired {
        let phase = presence.phase.label();
        let agent_part = presence
            .agent
            .as_deref()
            .map(|a| format!(" ({a})"))
            .unwrap_or_default();
        let reasoning_part = match &presence.reasoning {
            Some(r) if presence.phase == state::Phase::Thinking => format!(" — Reasoning: {r}"),
            Some(r) if !r.trim().is_empty() => format!(" — {} — Reasoning: {r}", presence.activity),
            _ => format!(" — {}", presence.activity),
        };
        let formatted = if publisher.is_connected() {
            format!(
                "{}{agent_part} • {phase} — {}{reasoning_part}",
                presence.provider.display_name(),
                presence.project
            )
        } else {
            format!(
                "Discord unavailable — {}{agent_part} • {phase} — {}{reasoning_part}",
                presence.provider.display_name(),
                presence.project
            )
        };
        if formatted.len() > 127 {
            let mut end = 127;
            while end > 0 && !formatted.is_char_boundary(end) {
                end -= 1;
            }
            formatted[..end].to_string()
        } else {
            formatted
        }
    } else {
        "Idle".to_string()
    };

    if let Ok(mut status) = shared.lock() {
        status.enabled = config.enabled;
        status.foreground_detection = config.foreground_detection;
        status.start_with_windows = config.start_with_windows;
        status.text = text;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_uses_nearest_deadline() {
        let timeout = next_timeout(
            1_000,
            &[
                Some(5_000),
                Some(8_000),
                Some(3_000),
                Some(4_000),
                Some(6_000),
                Some(7_000),
                Some(2_500),
            ],
        )
        .unwrap();
        assert_eq!(timeout, Duration::from_millis(1_500));
    }

    #[test]
    fn foreground_state_session_tracked_never_emits_fallback_presence() {
        for provider in Provider::ALL
            .into_iter()
            .filter(|provider| provider.has_session_tracking())
        {
            let mut state = ForegroundState::default();
            state.refresh_with_provider(Some(provider), 1_000);
            state.refresh_with_provider(Some(provider), 3_000);
            assert_eq!(state.raw_provider, None);
            assert_eq!(state.established_provider, None);
            assert!(state.presence().is_none());
        }
    }

    #[test]
    fn foreground_state_requires_dwell_to_establish() {
        let mut state = ForegroundState::default();
        // Alt-tab flicker past Cursor for 500ms
        state.refresh_with_provider(Some(Provider::Cursor), 1_000);
        state.refresh_with_provider(None, 1_500);
        assert_eq!(state.established_provider, None);
        assert!(state.presence().is_none());

        // Now user actually stays on Cursor for 2 seconds
        state.refresh_with_provider(Some(Provider::Cursor), 2_000);
        assert_eq!(state.established_provider, None); // not yet 2s
        state.refresh_with_provider(Some(Provider::Cursor), 4_000); // 2s dwell met
        assert_eq!(state.established_provider, Some(Provider::Cursor));
        assert_eq!(state.started_at_ms, 2_000);
        assert!(state.presence().is_some());
    }

    #[test]
    fn deepseek_harness_foreground_requires_session_activity() {
        let detected = Provider::from_window("DeepSeek Harness.exe", "Private workspace title");
        let mut state = ForegroundState::default();
        state.refresh_with_provider(detected, 1_000);
        assert!(state.presence().is_none());
        state.refresh_with_provider(detected, 3_000);
        assert_eq!(state.established_provider, None);
        assert!(state.presence().is_none());
    }

    #[test]
    fn deepseek_session_tracks_model_parallel_tools_and_all_turn_endings() {
        use serde_json::json;
        let path = std::path::Path::new("session.v4.jsonl.zstd");
        let provider = Provider::DeepSeekHarness;
        let started = 1_792_540_721_885_i64;
        let mut activity = ActivityState::default();
        // The project is the name of the folder the session runs in. The
        // checkout folder of a build agent is not spelled the same way as the
        // repository, so the expectation follows the path of the build.
        let expected_project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Discodex")
            .to_string();
        let header = json!({"type":"session","version":4,"cwd":env!("CARGO_MANIFEST_DIR"),"createdAt":started});
        let model = json!({"type":"request/header","time":started,"data":{"header":{"config":{"model":"deepseek-v4-pro","reasoningEffort":"high","apiKey":"PRIVATE_KEY"}}}});
        apply_historical_lines(
            &mut activity,
            path,
            provider,
            vec![header.to_string(), model.to_string()],
            started,
        );
        assert!(activity.selected_presence().is_none());
        let start = json!({"type":"turn/start","time":started,"data":{"turn":1}});
        let tool = |id: &str, name: &str, file: &str| {
            json!({"type":"tool/call","time":started+1,"data":{"callId":id,"name":name,"arguments":json!({"file_path":file,"content":"PRIVATE_CONTENT"}).to_string()}}).to_string()
        };
        apply_historical_lines(
            &mut activity,
            path,
            provider,
            vec![
                start.to_string(),
                tool("read", "read", "C:/private/src/lib.rs"),
                tool("write", "write", "C:/private/src/main.rs"),
            ],
            started,
        );
        let snapshot = activity.selected_presence().unwrap();
        assert_eq!(snapshot.agent.as_deref(), Some("deepseek-v4-pro (high)"));
        assert_eq!(snapshot.project, expected_project);
        assert_eq!(snapshot.project_stack.as_deref(), Some("Rust"));
        assert_eq!(snapshot.current_file.as_deref(), Some("main.rs"));
        assert_eq!(snapshot.phase, state::Phase::RunningTools);
        assert_eq!(snapshot.started_at_ms, started);
        assert!(snapshot.reasoning.is_none());
        assert!(!format!("{snapshot:?}").contains("PRIVATE"));
        assert!(!format!("{snapshot:?}").contains("C:/private"));

        let result = |id: &str| {
            json!({"type":"tool/result","time":started+2,"data":{"message":{"toolCallId":id,"content":"PRIVATE_OUTPUT"}}}).to_string()
        };
        apply_historical_lines(
            &mut activity,
            path,
            provider,
            vec![result("write"), result("unknown")],
            started,
        );
        let snapshot = activity.selected_presence().unwrap();
        assert_eq!(snapshot.phase, state::Phase::RunningTools);
        assert_eq!(snapshot.current_file.as_deref(), Some("lib.rs"));
        apply_historical_lines(&mut activity, path, provider, vec![result("read")], started);
        assert_eq!(
            activity.selected_presence().unwrap().phase,
            state::Phase::Thinking
        );
        for reason in ["completed", "aborted", "error", "blocked", "interrupted"] {
            apply_historical_lines(&mut activity, path, provider, vec![start.to_string(), json!({"type":"turn/end","time":started+3,"data":{"reason":{"kind":reason,"error":"PRIVATE_ERROR"}}}).to_string()], started);
            assert!(activity.selected_presence().is_none());
        }
        apply_historical_lines(
            &mut activity,
            path,
            provider,
            vec![start.to_string()],
            started,
        );
        activity.discard_stale_active(started + 180_001, 180_000);
        assert!(activity.selected_presence().is_none());
        apply_historical_lines(
            &mut activity,
            path,
            provider,
            vec![
                start.to_string(),
                json!({"type":"session/end-seed"}).to_string(),
            ],
            started,
        );
        assert!(activity.selected_presence().is_none());
    }

    #[test]
    fn foreground_state_holds_across_brief_focus_loss() {
        let mut state = ForegroundState::default();
        // Establish Cursor at t=10_000
        state.refresh_with_provider(Some(Provider::Cursor), 10_000);
        state.refresh_with_provider(Some(Provider::Cursor), 12_000);
        assert_eq!(state.established_provider, Some(Provider::Cursor));
        assert_eq!(state.started_at_ms, 10_000);

        // Alt-tab to Discord/browser (non-AI app) for 5 seconds
        state.refresh_with_provider(None, 15_000);
        assert_eq!(state.established_provider, Some(Provider::Cursor));
        assert_eq!(state.presence().unwrap().provider, Provider::Cursor);
        assert_eq!(state.started_at_ms, 10_000); // timestamp preserved!

        // User returns to Cursor at t=18_000 (within 15s hold)
        state.refresh_with_provider(Some(Provider::Cursor), 18_000);
        assert_eq!(state.established_provider, Some(Provider::Cursor));
        assert_eq!(state.started_at_ms, 10_000); // still preserved!
        assert!(state.hold_deadline_ms.is_none());

        // Now user stays away past the 15s hold window
        state.refresh_with_provider(None, 20_000);
        state.refresh_with_provider(None, 35_000); // 15s passed
        assert_eq!(state.established_provider, None);
        assert!(state.presence().is_none());
    }
}
