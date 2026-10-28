#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
compile_error!("Discodex currently supports Windows only.");

mod config;
mod diagnostics;
mod discord;
mod events;
mod foreground;
mod ingestion;
mod instance;
mod provider;
mod scheduling;
mod sources;
mod state;
mod tray;
mod watcher;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use config::AppConfig;
use discord::{DiscordPublisher, PresenceSnapshot};
use ingestion::Ingestion;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use provider::{Provider, SessionSource};
use scheduling::{SESSION_ACTIVE_TIMEOUT_MS, Schedule};
use state::ActivityState;
use tray::TrayStatus;

#[derive(Debug)]
enum WorkerEvent {
    Paths(Vec<PathBuf>),
    ToggleEnabled,
    ToggleForegroundDetection,
    ToggleStartup,
    ToggleReasoning,
    ToggleProjectNames,
    ToggleFileNames,
    OpenDiagnostics,
    TestPresence,
    OpenConfig,
    Exit,
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
    if std::env::args().any(|arg| arg == "--diagnostics") {
        let path = AppConfig::path()?;
        let config = if path.exists() {
            AppConfig::load(&path)?
        } else {
            AppConfig::default()
        };
        let mut ingestion = Ingestion::new(provider::session_sources());
        let mut activity = ActivityState::default();
        ingestion.bootstrap(&mut activity);
        let report = diagnostics::DiagnosticSnapshot {
            ingestion: &ingestion,
            activity: &activity,
            config: &config,
            focused: if config.foreground_detection { foreground::active_provider() } else { None },
            discord: "Not probed. Standalone diagnostics do not connect to Discord; use tray Diagnostics for the running connection",
            watches: &HashMap::new(),
        }.render();
        println!("{report}");
        return Ok(());
    }

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
        privacy: config.privacy.clone(),
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
    let mut schedule = Schedule::new(&session_sources);
    let mut ingestion = Ingestion::new(session_sources);
    let mut effective_application_id = config.effective_discord_application_id();
    let mut publisher = DiscordPublisher::new(effective_application_id.clone());
    ingestion.bootstrap(&mut activity);
    let startup_now = now_ms();
    activity.expire_terminal_candidates(startup_now);
    activity.discard_stale_active(startup_now, SESSION_ACTIVE_TIMEOUT_MS);
    log_line(&format!(
        "read historical logs. Initial presence: {:?}",
        activity
            .selected_presence()
            .map(|snapshot| snapshot.redacted(&config.privacy))
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

    let mut watch_status = HashMap::new();
    if let Some(watcher) = fs_watcher.as_mut() {
        for source in ingestion.sources() {
            if source.provider == Provider::Aider {
                // Aider writes into the working tree of a repository. Watching
                // the whole home directory would report every file change of
                // the machine, so the transcript is polled instead.
                continue;
            }
            if source.root.exists() {
                watch_status.insert(
                    source.root.clone(),
                    watcher
                        .watch(&source.root, RecursiveMode::Recursive)
                        .map_err(|_| "Could not subscribe to filesystem changes".to_string()),
                );
            }
        }
        if let Some(parent) = config_path.parent() {
            let _ = watcher.watch(parent, RecursiveMode::NonRecursive);
        }
    }

    if fs_watcher.is_none() {
        for source in ingestion.sources() {
            watch_status.insert(
                source.root.clone(),
                Err("Filesystem watcher unavailable".to_string()),
            );
        }
    }
    let mut last_desired: Option<PresenceSnapshot> = None;

    loop {
        let now = now_ms();
        let due = schedule.due(now, config.foreground_detection);
        if due.stores {
            ingestion.poll_stores(&mut activity);
        }
        if due.aider {
            ingestion.poll_aider(&mut activity);
        }

        if let Some(watcher) = fs_watcher.as_mut() {
            for source in ingestion.sources() {
                if source.provider == Provider::Aider {
                    continue;
                }
                if source.root.exists() && !matches!(watch_status.get(&source.root), Some(Ok(()))) {
                    watch_status.insert(
                        source.root.clone(),
                        watcher
                            .watch(&source.root, RecursiveMode::Recursive)
                            .map_err(|_| "Could not subscribe to filesystem changes".to_string()),
                    );
                }
            }
        }

        activity.expire_terminal_candidates(now);
        activity.discard_stale_active(now, SESSION_ACTIVE_TIMEOUT_MS);

        let desired = if !config.enabled {
            None
        } else if let Some(started_at_ms) = schedule.test_started_at_ms() {
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
                .or_else(|| schedule.foreground_presence())
        };

        let desired = desired.map(|snapshot| snapshot.redacted(&config.privacy));

        if desired != last_desired {
            log_line(&format!("desired presence changed: {:?}", desired));
            last_desired = desired.clone();
        }

        let can_attempt = schedule.can_publish(now, desired.is_some(), publisher.is_connected());
        if can_attempt {
            let force_refresh =
                schedule.force_refresh(now, desired.is_some(), publisher.is_connected());
            let sync_res = publisher.sync(desired.as_ref(), force_refresh, &config.privacy);
            if let Err(error) = sync_res {
                log_line(&format!("publisher.sync error: {error}"));
                schedule.publish_failed(now);
            } else {
                schedule.published(now, publisher.is_connected(), force_refresh);
            }
        }

        update_status(
            &status,
            &config,
            &effective_application_id,
            &publisher,
            desired.as_ref(),
        );

        let timeout = schedule.timeout(now, &activity, desired.is_some(), publisher.is_connected());

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
                    ingestion.handle_path(&mut activity, &path);
                }
                if reload_config && let Ok(reloaded) = config.reload(&config_path) {
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
                schedule.start_test(now_ms());
            }
            WorkerEvent::ToggleReasoning
            | WorkerEvent::ToggleProjectNames
            | WorkerEvent::ToggleFileNames => {
                match event {
                    WorkerEvent::ToggleReasoning => {
                        config.privacy.share_reasoning = !config.privacy.share_reasoning
                    }
                    WorkerEvent::ToggleProjectNames => {
                        config.privacy.show_project_names = !config.privacy.show_project_names
                    }
                    WorkerEvent::ToggleFileNames => {
                        config.privacy.show_file_names = !config.privacy.show_file_names
                    }
                    _ => unreachable!(),
                }
                if let Err(error) = config.save(&config_path) {
                    log_line(&format!("Could not save privacy settings: {error}"));
                }
            }
            WorkerEvent::OpenDiagnostics => {
                let discord = publisher.diagnostic_status();
                let report = diagnostics::DiagnosticSnapshot {
                    ingestion: &ingestion,
                    activity: &activity,
                    config: &config,
                    focused: schedule.detected_provider(),
                    discord: &discord,
                    watches: &watch_status,
                }
                .render();
                let path = config_path.with_file_name("diagnostics.txt");
                match std::fs::write(&path, report) {
                    Ok(()) => {
                        let _ = std::process::Command::new("notepad.exe").arg(path).spawn();
                    }
                    Err(error) => log_line(&format!("Could not write diagnostics: {error}")),
                }
            }
            WorkerEvent::OpenConfig => {
                let _ = std::process::Command::new("notepad.exe")
                    .arg(&config_path)
                    .spawn();
            }
            WorkerEvent::Exit => break,
        }
    }

    let _ = publisher.sync(None, false, &config.privacy);
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
        status.privacy = config.privacy.clone();
        status.text = text;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingestion::apply_historical_lines;

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
}
