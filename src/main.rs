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
const STARTUP_ACTIVE_FRESHNESS: Duration = Duration::from_secs(10 * 60);
const FOREGROUND_POLL: Duration = Duration::from_secs(2);

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
    provider: Option<Provider>,
    started_at_ms: i64,
}

impl ForegroundState {
    fn refresh(&mut self, now_ms: i64) {
        let provider = foreground::active_provider();
        if provider != self.provider {
            self.provider = provider;
            self.started_at_ms = now_ms;
        }
    }

    fn presence(&self) -> Option<PresenceSnapshot> {
        let provider = self.provider?;
        Some(PresenceSnapshot {
            provider,
            project: provider.display_name().to_string(),
            project_stack: None,
            current_file: None,
            current_language: None,
            activity: provider.default_activity().to_string(),
            phase: state::Phase::Thinking,
            started_at_ms: self.started_at_ms,
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

fn main() {
    if let Err(error) = run() {
        let message = format!("Discodex could not start:\n\n{error}");
        let _ = std::process::Command::new("msg")
            .args(["*", &message])
            .status();
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let Some(_single_instance) = instance::SingleInstance::acquire()? else {
        return Ok(());
    };

    let (config, config_path) = AppConfig::load_or_create()?;
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
        worker_loop(
            rx,
            worker_tx,
            worker_status,
            config,
            worker_config_path,
            session_sources,
        )
    });

    tray::run(tx, status)?;

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
        for path in watcher::recent_logs(&source.root, source.provider, 32) {
            if let Ok(lines) = tailer.read_from_start(&path) {
                let fallback_observed_at_ms = std::fs::metadata(&path)
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(system_time_ms)
                    .unwrap_or_else(now_ms);
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
    activity.discard_stale_active(now_ms(), STARTUP_ACTIVE_FRESHNESS.as_millis() as i64);

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
                watched_roots.insert(source.root.clone());
            }
        }
        if let Some(parent) = config_path.parent() {
            let _ = watcher.watch(parent, RecursiveMode::NonRecursive);
        }
    }

    loop {
        let now = now_ms();
        if config.foreground_detection && now >= next_foreground_poll_ms {
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
                    watched_roots.insert(source.root.clone());
                }
            }
        }

        activity.expire_terminal_candidates(now);
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
            })
        } else {
            activity
                .selected_presence()
                .or_else(|| foreground_state.presence())
        };

        let can_attempt = desired.is_none() || publisher.is_connected() || now >= next_retry_ms;
        if can_attempt {
            let force_refresh =
                desired.is_some() && publisher.is_connected() && now >= next_healthcheck_ms;
            if publisher.sync(desired.as_ref(), force_refresh).is_err() {
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

        let timeout = next_timeout(
            now,
            activity.next_terminal_deadline_ms(),
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
            Some(next_foreground_poll_ms.max(now + 1)),
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
    test: Option<i64>,
    reconnect: Option<i64>,
    healthcheck: Option<i64>,
    foreground: Option<i64>,
) -> Option<Duration> {
    [terminal, test, reconnect, healthcheck, foreground]
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
        if publisher.is_connected() {
            format!(
                "{} • {phase} — {}",
                presence.provider.display_name(),
                presence.project
            )
        } else {
            format!(
                "Discord unavailable — {} • {phase} — {}",
                presence.provider.display_name(),
                presence.project
            )
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
            Some(3_000),
            Some(4_000),
            Some(6_000),
            Some(7_000),
        )
        .unwrap();
        assert_eq!(timeout, Duration::from_millis(2_000));
    }
}
