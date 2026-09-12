#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
compile_error!("Discodex currently supports Windows only.");

mod config;
mod discord;
mod events;
mod foreground;
mod instance;
mod provider;
mod state;
mod tray;
mod watcher;

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use config::AppConfig;
use discord::{DiscordPublisher, PresenceSnapshot};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use provider::{Provider, SessionSource};
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

    fn focused_provider(&self) -> Option<Provider> {
        self.raw_provider.or(self.established_provider)
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
    let Some(_single_instance) = instance::SingleInstance::acquire()? else {
        log_line("single instance mutex already locked! exiting.");
        return Ok(());
    };
    log_line("single instance acquired.");

    let (config, config_path) = AppConfig::load_or_create()?;
    log_line(&format!("config loaded from {:?}", config_path));
    config::set_start_with_windows(config.start_with_windows)?;

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
    res?;

    let _ = worker.join();
    Ok(())
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
    let mut effective_application_id = config.effective_discord_application_id();
    let mut publisher = DiscordPublisher::new(effective_application_id.clone());
    let mut test_presence_ms: Option<(i64, i64)> = None;
    let mut next_retry_ms = 0_i64;
    let mut next_healthcheck_ms = 0_i64;
    let mut next_foreground_poll_ms = 0_i64;
    let mut foreground_state = ForegroundState::default();

    for source in &session_sources {
        if !source.root.exists() {
            continue;
        }
        log_line(&format!(
            "scanning recent logs for {:?} at {:?}",
            source.provider, source.root
        ));
        let now_sys = SystemTime::now();
        let logs = watcher::recent_logs(&source.root, source.provider, 8);
        log_line(&format!(
            "found {} recent logs for {:?}",
            logs.len(),
            source.provider
        ));
        for path in logs {
            let modified = std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            if now_sys.duration_since(modified).unwrap_or_default() > Duration::from_secs(900) {
                continue;
            }
            log_line(&format!("reading log {:?}", path));
            if let Ok(lines) = tailer.read_from_start(&path) {
                let fallback_observed_at_ms = system_time_ms(modified).unwrap_or_else(now_ms);
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

        if let Some(watcher) = fs_watcher.as_mut() {
            for source in &session_sources {
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
                .selected_presence_for_foreground(foreground_state.focused_provider())
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

        let timeout = next_timeout(
            now,
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
                    if let Ok(lines) = tailer.read_new(&path) {
                        if !lines.is_empty() {
                            log_line(&format!("read {} lines from {:?}", lines.len(), path));
                        }
                        apply_lines(&mut activity, &path, source.provider, lines);
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
                    let _ = config::set_start_with_windows(config.start_with_windows);
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
                if config::set_start_with_windows(config.start_with_windows).is_ok() {
                    let _ = config.save(&config_path);
                } else {
                    config.start_with_windows = !config.start_with_windows;
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
        for event in events::parse_line_for(provider, &line) {
            activity.apply(path, provider, event, observed_at_ms);
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
        for event in events::parse_line_for(provider, &line) {
            activity.apply(path, provider, event, observed_at_ms);
        }
    }
}

fn system_time_ms(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
}

fn next_timeout(
    now: i64,
    terminal: Option<i64>,
    stale: Option<i64>,
    test: Option<i64>,
    reconnect: Option<i64>,
    healthcheck: Option<i64>,
    foreground: Option<i64>,
) -> Option<Duration> {
    [terminal, stale, test, reconnect, healthcheck, foreground]
        .into_iter()
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
            Some(5_000),
            Some(8_000),
            Some(3_000),
            Some(4_000),
            Some(6_000),
            Some(7_000),
        )
        .unwrap();
        assert_eq!(timeout, Duration::from_millis(2_000));
    }

    #[test]
    fn foreground_state_session_tracked_never_emits_fallback_presence() {
        let mut state = ForegroundState::default();
        // User focuses Google Antigravity window
        state.refresh_with_provider(Some(Provider::Antigravity), 1_000);
        state.refresh_with_provider(Some(Provider::Antigravity), 3_000); // 2s dwell met
        assert_eq!(state.established_provider, Some(Provider::Antigravity));
        // Session-tracked tools must never emit presence from foreground detection alone!
        assert!(state.presence().is_none());
        // But focused_provider is available for session tie-breaking
        assert_eq!(state.focused_provider(), Some(Provider::Antigravity));
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
