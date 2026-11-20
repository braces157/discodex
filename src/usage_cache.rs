use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use chrono::{Local, TimeZone};
use serde::{Deserialize, Serialize};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_EVENT, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::{
            CreateEventW, EVENT_MODIFY_STATE, GetCurrentThread, OpenEventW, SetEvent,
            SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL, WaitForMultipleObjects,
        },
    },
    core::PCWSTR,
};

use crate::{
    config::AppConfig,
    provider::SessionSource,
    usage::{UsageBundle, UsageScanner, usage_scope_key},
};

pub const CACHE_SCHEMA_VERSION: u32 = 1;
pub const CACHE_FILE_NAME: &str = "usage-cache.json";
pub const RATES_FILE_NAME: &str = "usage-rates.toml";

pub const DIRTY_DEBOUNCE: Duration = Duration::from_secs(5);
pub const DIRTY_MAX_WAIT: Duration = Duration::from_secs(20);
pub const PERIODIC_REFRESH: Duration = Duration::from_secs(180);
pub const FRESH_CACHE_MAX_AGE_MS: i64 = 10_000;
const MAX_WAIT_SLICE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RatesFingerprint {
    pub exists: bool,
    pub bytes: u64,
    pub modified_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedUsage {
    pub schema_version: u32,
    pub scope_key: String,
    pub local_date: String,
    pub generation: u64,
    pub rates_fingerprint: RatesFingerprint,
    pub bundle: UsageBundle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathChange {
    None,
    SessionDirty,
    RatesChanged,
}

/// Derives `(rates_path, cache_path)` in the same directory as `config_path`.
pub fn paths_from_config(config_path: &Path) -> (PathBuf, PathBuf) {
    (
        config_path.with_file_name(RATES_FILE_NAME),
        config_path.with_file_name(CACHE_FILE_NAME),
    )
}

/// Resolves `(rates_path, cache_path)` from the active `%APPDATA%\Discodex\config.toml` location.
pub fn resolve_default_paths() -> io::Result<(PathBuf, PathBuf)> {
    AppConfig::path().map(|config_path| paths_from_config(&config_path))
}

/// Returns the current local calendar date as `YYYY-MM-DD`.
pub fn current_local_date() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

/// Converts a millisecond Unix epoch timestamp to the local `YYYY-MM-DD` date string.
pub fn local_date_for_ms(ms: i64) -> Option<String> {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.format("%Y-%m-%d").to_string())
}

fn system_time_ms(t: SystemTime) -> Option<i64> {
    t.duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
}

/// Captures lightweight existence, size, and modified timestamp metadata for `usage-rates.toml`.
pub fn rates_fingerprint(rates_path: &Path) -> RatesFingerprint {
    match fs::metadata(rates_path) {
        Ok(meta) => RatesFingerprint {
            exists: true,
            bytes: meta.len(),
            modified_ms: meta.modified().ok().and_then(system_time_ms),
        },
        Err(_) => RatesFingerprint::default(),
    }
}

/// Returns `(bytes, modified)` for `cache_path` for cheap change detection.
pub fn cache_file_stamp(cache_path: &Path) -> Option<(u64, Option<SystemTime>)> {
    fs::metadata(cache_path)
        .ok()
        .map(|meta| (meta.len(), meta.modified().ok()))
}

/// Loads and validates `cache_path` against schema version, `expected_scope_key`, and today's local date.
pub fn load_cache(cache_path: &Path, expected_scope_key: &str) -> Option<CachedUsage> {
    load_cache_for_date(cache_path, expected_scope_key, &current_local_date())
}

fn load_cache_for_date(
    cache_path: &Path,
    expected_scope_key: &str,
    expected_local_date: &str,
) -> Option<CachedUsage> {
    let mut bytes = None;
    for attempt in 0..3 {
        match fs::read(cache_path) {
            Ok(data) => {
                bytes = Some(data);
                break;
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
            Err(_) if attempt < 2 => thread::sleep(Duration::from_millis(10)),
            Err(_) => return None,
        }
    }
    let data = bytes?;
    let entry: CachedUsage = serde_json::from_slice(&data).ok()?;
    if entry.schema_version != CACHE_SCHEMA_VERSION
        || entry.scope_key != expected_scope_key
        || entry.local_date != expected_local_date
    {
        return None;
    }
    Some(entry)
}

/// Writes `entry` to `cache_path` atomically via a unique temporary file in the same directory.
pub fn save_cache(cache_path: &Path, entry: &CachedUsage) -> io::Result<()> {
    static NEXT_TMP: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let encoded =
        serde_json::to_vec(entry).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let tmp_id = NEXT_TMP.fetch_add(1, Ordering::Relaxed);
    let tmp_path =
        cache_path.with_file_name(format!("usage-cache.{}.{tmp_id}.tmp", std::process::id()));
    if let Err(err) = fs::write(&tmp_path, encoded) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }
    for attempt in 0..5 {
        match fs::rename(&tmp_path, cache_path) {
            Ok(()) => return Ok(()),
            Err(err)
                if (err.kind() == io::ErrorKind::PermissionDenied
                    || err.raw_os_error() == Some(32))
                    && attempt < 4 =>
            {
                thread::sleep(Duration::from_millis(15));
            }
            Err(err) => {
                let _ = fs::remove_file(&tmp_path);
                return Err(err);
            }
        }
    }
    let _ = fs::remove_file(&tmp_path);
    Ok(())
}

fn normalized_win_path(path: &Path) -> String {
    let mut s: String = path
        .to_string_lossy()
        .chars()
        .map(|c| match c {
            '/' => '\\',
            c => c.to_ascii_lowercase(),
        })
        .collect();
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    s
}

fn same_win_path(a: &Path, b: &Path) -> bool {
    normalized_win_path(a) == normalized_win_path(b)
}

fn is_under_win_dir(path: &Path, dir: &Path) -> bool {
    let p = normalized_win_path(path);
    let d = normalized_win_path(dir);
    if p == d {
        return true;
    }
    let prefix = if d.ends_with('\\') {
        d
    } else {
        format!("{d}\\")
    };
    p.starts_with(&prefix)
}

/// Classifies filesystem notifications, ignoring Discodex's own config/log/cache artifacts
/// while escalating `usage-rates.toml` updates and external session directory writes.
pub fn classify_changed_paths(
    paths: &[PathBuf],
    config_path: &Path,
    rates_path: &Path,
) -> PathChange {
    let config_dir = config_path.parent();
    let mut dirty = false;
    for path in paths {
        if same_win_path(path, rates_path) {
            return PathChange::RatesChanged;
        }
        if config_dir.is_some_and(|dir| is_under_win_dir(path, dir)) {
            continue;
        }
        dirty = true;
    }
    if dirty {
        PathChange::SessionDirty
    } else {
        PathChange::None
    }
}

fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn refresh_event_name(scope_key: &str) -> String {
    format!("Local\\Discodex.UsageRefresh.{scope_key}")
}

struct WinEvent(HANDLE);

// SAFETY: Windows kernel event handles (`HANDLE`) are safe to store, signal, wait on, and
// transfer between threads; ownership remains uniquely inside `WinEvent` until `Drop`.
unsafe impl Send for WinEvent {}
// SAFETY: `SetEvent` and `WaitForMultipleObjects` on a valid kernel event handle are thread-safe.
unsafe impl Sync for WinEvent {}

impl WinEvent {
    fn create(manual_reset: bool, name: Option<&str>) -> io::Result<Self> {
        let wide = name.map(to_wide);
        let name_ptr = wide
            .as_ref()
            .map_or_else(PCWSTR::null, |buf| PCWSTR(buf.as_ptr()));
        // SAFETY: `name_ptr` is either null or points to a NUL-terminated UTF-16 buffer live for this call.
        let handle = unsafe { CreateEventW(None, manual_reset, false, name_ptr) }
            .map_err(|err| io::Error::other(format!("CreateEventW failed: {err}")))?;
        Ok(Self(handle))
    }

    fn handle(&self) -> HANDLE {
        self.0
    }

    fn set(&self) {
        // SAFETY: `self.0` is an open valid kernel event handle owned by this `WinEvent`.
        unsafe {
            let _ = SetEvent(self.0);
        }
    }
}

impl Drop for WinEvent {
    fn drop(&mut self) {
        // SAFETY: `self.0` is uniquely owned and closed exactly once on drop.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Returns true if a tray preloader is currently registered for `scope_key`.
pub fn is_preloader_running(scope_key: &str) -> bool {
    let wide = to_wide(&refresh_event_name(scope_key));
    // SAFETY: `wide` is a valid NUL-terminated UTF-16 event name for the duration of `OpenEventW`.
    unsafe {
        let Ok(handle) = OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(wide.as_ptr())) else {
            return false;
        };
        let _ = CloseHandle(handle);
        true
    }
}

/// Signals the tray preloader for `scope_key` to run an immediate incremental refresh.
pub fn signal_preloader_refresh(scope_key: &str) -> bool {
    let wide = to_wide(&refresh_event_name(scope_key));
    // SAFETY: `wide` is a valid NUL-terminated UTF-16 event name; the opened handle is closed before returning.
    unsafe {
        let Ok(handle) = OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(wide.as_ptr())) else {
            return false;
        };
        let signaled = SetEvent(handle).is_ok();
        let _ = CloseHandle(handle);
        signaled
    }
}

fn set_current_thread_below_normal() {
    // SAFETY: `GetCurrentThread()` returns the pseudo-handle for the calling thread.
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

#[derive(Default)]
struct PreloaderState {
    dirty: bool,
    has_completed_scan: bool,
    last_scan_completed_at: Option<Instant>,
    last_scan_rates: RatesFingerprint,
    last_scan_local_date: String,
}

/// Cloneable control handle for nudging or marking the background usage preloader dirty.
#[derive(Clone)]
pub struct UsagePreloaderHandle {
    rates_path: PathBuf,
    refresh_event: Arc<WinEvent>,
    dirty_event: Arc<WinEvent>,
    shared: Arc<Mutex<PreloaderState>>,
}

impl UsagePreloaderHandle {
    /// Marks local session sources dirty so the preloader refreshes after debounce.
    pub fn mark_dirty(&self) {
        {
            let mut state = self
                .shared
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.dirty = true;
        }
        self.dirty_event.set();
    }

    /// Requests an immediate incremental refresh from the warm scanner.
    pub fn request_refresh(&self) {
        self.refresh_event.set();
    }

    /// Triggers an immediate incremental refresh before opening the popup if cached data may be stale.
    pub fn nudge_before_open(&self) {
        let should_refresh = {
            let state = self
                .shared
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.dirty
                || !state.has_completed_scan
                || state
                    .last_scan_completed_at
                    .is_none_or(|t| t.elapsed() >= DIRTY_DEBOUNCE)
                || rates_fingerprint(&self.rates_path) != state.last_scan_rates
                || current_local_date() != state.last_scan_local_date
        };
        if should_refresh {
            self.request_refresh();
        }
    }
}

/// Owns the low-priority background usage preloader thread inside the tray process.
pub struct UsagePreloader {
    #[cfg(test)]
    scope_key: String,
    cancel: Arc<AtomicBool>,
    stop_event: Arc<WinEvent>,
    handle: UsagePreloaderHandle,
    thread: Option<JoinHandle<()>>,
}

impl UsagePreloader {
    /// Spawns the background preloader thread and immediately warms the usage cache.
    pub fn start(
        sources: Vec<SessionSource>,
        rates_path: PathBuf,
        cache_path: PathBuf,
    ) -> io::Result<Self> {
        let scope_key = usage_scope_key(&sources, &rates_path);
        let cancel = Arc::new(AtomicBool::new(false));
        let stop_event = Arc::new(WinEvent::create(true, None)?);
        let refresh_event = Arc::new(WinEvent::create(
            false,
            Some(&refresh_event_name(&scope_key)),
        )?);
        let dirty_event = Arc::new(WinEvent::create(false, None)?);
        let shared = Arc::new(Mutex::new(PreloaderState::default()));

        let handle = UsagePreloaderHandle {
            rates_path: rates_path.clone(),
            refresh_event: Arc::clone(&refresh_event),
            dirty_event: Arc::clone(&dirty_event),
            shared: Arc::clone(&shared),
        };

        let thread_scope = scope_key.clone();
        let thread_cancel = Arc::clone(&cancel);
        let thread_stop = Arc::clone(&stop_event);
        let thread_refresh = Arc::clone(&refresh_event);
        let thread_dirty = Arc::clone(&dirty_event);
        let thread_shared = Arc::clone(&shared);

        let thread = thread::Builder::new()
            .name("usage-preloader".into())
            .spawn(move || {
                run_preloader_loop(
                    sources,
                    rates_path,
                    cache_path,
                    thread_scope,
                    thread_cancel,
                    thread_stop,
                    thread_refresh,
                    thread_dirty,
                    thread_shared,
                );
            })?;

        Ok(Self {
            #[cfg(test)]
            scope_key,
            cancel,
            stop_event,
            handle,
            thread: Some(thread),
        })
    }

    /// Returns a cloneable control handle for the running preloader.
    pub fn handle(&self) -> UsagePreloaderHandle {
        self.handle.clone()
    }

    /// Returns the opaque 128-bit scope digest isolating this preloader's cache and event.
    #[cfg(test)]
    pub fn scope_key(&self) -> &str {
        &self.scope_key
    }
}

impl Drop for UsagePreloader {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.stop_event.set();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_preloader_loop(
    sources: Vec<SessionSource>,
    rates_path: PathBuf,
    cache_path: PathBuf,
    scope_key: String,
    cancel: Arc<AtomicBool>,
    stop_event: Arc<WinEvent>,
    refresh_event: Arc<WinEvent>,
    dirty_event: Arc<WinEvent>,
    shared: Arc<Mutex<PreloaderState>>,
) {
    set_current_thread_below_normal();
    let mut scanner = UsageScanner::new(sources, rates_path.clone());
    let mut generation = load_cache(&cache_path, &scope_key).map_or(0, |c| c.generation);

    let run_scan = |scanner: &mut UsageScanner, generation: &mut u64| -> Instant {
        {
            let mut state = shared
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.dirty = false;
        }
        let started = Instant::now();
        let rates_fp = rates_fingerprint(&rates_path);
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            scanner.scan_all_periods_cancellable(&cancel)
        })) {
            Ok(Some(bundle)) => {
                let local_date =
                    local_date_for_ms(bundle.refreshed_at_ms()).unwrap_or_else(current_local_date);
                *generation = generation.saturating_add(1);
                let cached = CachedUsage {
                    schema_version: CACHE_SCHEMA_VERSION,
                    scope_key: scope_key.clone(),
                    local_date: local_date.clone(),
                    generation: *generation,
                    rates_fingerprint: rates_fp.clone(),
                    bundle,
                };
                if let Err(err) = save_cache(&cache_path, &cached) {
                    crate::log_line(&format!("usage preloader failed to save cache: {err}"));
                } else {
                    crate::log_line(&format!(
                        "usage preloader updated cache gen={} files={} in {}ms",
                        cached.generation,
                        cached.bundle.week.scanned_files,
                        started.elapsed().as_millis()
                    ));
                }
                let mut state = shared
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.has_completed_scan = true;
                state.last_scan_completed_at = Some(Instant::now());
                state.last_scan_rates = rates_fp;
                state.last_scan_local_date = local_date;
            }
            Ok(None) => {}
            Err(_) => {
                crate::log_line("usage preloader scan panicked; continuing");
            }
        }
        Instant::now()
    };

    let mut last_scan = if cancel.load(Ordering::Relaxed) {
        return;
    } else {
        run_scan(&mut scanner, &mut generation)
    };

    let mut first_dirty_at: Option<Instant> = None;
    let mut last_dirty_at: Option<Instant> = None;
    let handles = [
        stop_event.handle(),
        refresh_event.handle(),
        dirty_event.handle(),
    ];

    while !cancel.load(Ordering::Relaxed) {
        {
            let state = shared
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.dirty && first_dirty_at.is_none() {
                let now = Instant::now();
                first_dirty_at = Some(now);
                last_dirty_at = Some(now);
            }
        }

        let periodic_remaining = PERIODIC_REFRESH.saturating_sub(last_scan.elapsed());
        let mut wait = periodic_remaining.min(MAX_WAIT_SLICE);
        if let (Some(first), Some(last)) = (first_dirty_at, last_dirty_at) {
            let quiet_left = DIRTY_DEBOUNCE.saturating_sub(last.elapsed());
            let max_left = DIRTY_MAX_WAIT.saturating_sub(first.elapsed());
            wait = wait.min(quiet_left.min(max_left));
        }
        let wait_ms = wait.as_millis().min(u32::MAX as u128) as u32;

        // SAFETY: All three handles in `handles` remain open and owned for the loop's lifetime.
        let signaled = unsafe { WaitForMultipleObjects(&handles, false, wait_ms) };
        if signaled == WAIT_OBJECT_0 || signaled == WAIT_FAILED || cancel.load(Ordering::Relaxed) {
            break;
        } else if signaled == WAIT_EVENT(WAIT_OBJECT_0.0 + 1) {
            first_dirty_at = None;
            last_dirty_at = None;
            last_scan = run_scan(&mut scanner, &mut generation);
        } else if signaled == WAIT_EVENT(WAIT_OBJECT_0.0 + 2) {
            let dirty_now = Instant::now();
            if first_dirty_at.is_none() {
                first_dirty_at = Some(dirty_now);
            }
            last_dirty_at = Some(dirty_now);
        } else if signaled == WAIT_TIMEOUT {
            let dirty_due = first_dirty_at
                .zip(last_dirty_at)
                .is_some_and(|(first, last)| {
                    last.elapsed() >= DIRTY_DEBOUNCE || first.elapsed() >= DIRTY_MAX_WAIT
                });
            let periodic_due = last_scan.elapsed() >= PERIODIC_REFRESH;
            let (date_changed, rates_changed) = {
                let state = shared
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                (
                    state.last_scan_local_date != current_local_date(),
                    state.last_scan_rates != rates_fingerprint(&rates_path),
                )
            };
            if dirty_due || periodic_due || date_changed || rates_changed {
                first_dirty_at = None;
                last_dirty_at = None;
                last_scan = run_scan(&mut scanner, &mut generation);
            }
        }
    }
}

static LOCAL_PRELOADER: OnceLock<Mutex<Option<UsagePreloaderHandle>>> = OnceLock::new();

fn local_preloader_slot() -> &'static Mutex<Option<UsagePreloaderHandle>> {
    LOCAL_PRELOADER.get_or_init(|| Mutex::new(None))
}

/// Registers the tray's active preloader handle for fast in-process nudging on tray click.
pub fn register_local_preloader(handle: UsagePreloaderHandle) {
    *local_preloader_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(handle);
}

/// Clears the process-local preloader handle during tray shutdown.
pub fn clear_local_preloader() {
    *local_preloader_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

/// Nudges the in-process tray preloader (if registered) before opening or focusing the popup.
pub fn nudge_local_preloader() {
    let handle = local_preloader_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if let Some(handle) = handle {
        handle.nudge_before_open();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Provider, SessionStore};
    use serde_json::json;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "discodex-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_codex_session(path: &Path, input: u64, output: u64) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let ts = chrono::Utc::now().to_rfc3339();
        let lines = [
            json!({"type":"session_meta","payload":{"id":"preload-test-session"}}),
            json!({"type":"turn_context","payload":{"model":"gpt-5.2","effort":"medium"}}),
            json!({
                "timestamp": ts,
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": input,
                            "cached_input_tokens": 0,
                            "cache_write_input_tokens": 0,
                            "output_tokens": output,
                            "reasoning_output_tokens": 0
                        }
                    }
                }
            }),
        ];
        let body = lines
            .iter()
            .map(|v| serde_json::to_string(v).unwrap() + "\n")
            .collect::<String>();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn cache_round_trips_and_rejects_mismatched_schema_scope_date_or_corrupt_json() {
        let dir = TempDir::new("cache-roundtrip");
        let cache_path = dir.0.join(CACHE_FILE_NAME);
        let rates_path = dir.0.join(RATES_FILE_NAME);
        let sources = vec![SessionSource {
            provider: Provider::Codex,
            root: dir.0.join("sessions"),
            store: SessionStore::LogFiles,
        }];
        write_codex_session(&dir.0.join("sessions/one.jsonl"), 500, 100);
        let scope = usage_scope_key(&sources, &rates_path);
        let mut scanner = UsageScanner::new(sources, rates_path.clone());
        let bundle = scanner.scan_all_periods();

        let entry = CachedUsage {
            schema_version: CACHE_SCHEMA_VERSION,
            scope_key: scope.clone(),
            local_date: "2026-11-21".to_owned(),
            generation: 7,
            rates_fingerprint: rates_fingerprint(&rates_path),
            bundle,
        };
        save_cache(&cache_path, &entry).unwrap();

        let loaded = load_cache_for_date(&cache_path, &scope, "2026-11-21").unwrap();
        assert_eq!(loaded.generation, 7);
        assert_eq!(loaded.bundle.all.rows[0].input_tokens, Some(500));

        assert!(load_cache_for_date(&cache_path, "other-scope", "2026-11-21").is_none());
        assert!(load_cache_for_date(&cache_path, &scope, "2026-11-22").is_none());

        let mut bad_schema = entry.clone();
        bad_schema.schema_version = CACHE_SCHEMA_VERSION + 1;
        save_cache(&cache_path, &bad_schema).unwrap();
        assert!(load_cache_for_date(&cache_path, &scope, "2026-11-21").is_none());

        fs::write(&cache_path, "{broken json").unwrap();
        assert!(load_cache_for_date(&cache_path, &scope, "2026-11-21").is_none());
    }

    #[test]
    fn classify_changed_paths_ignores_discodex_artifacts_and_detects_rates_and_sessions() {
        let dir = TempDir::new("classify-paths");
        let config_dir = dir.0.join("AppData/Roaming/Discodex");
        let config_path = config_dir.join("config.toml");
        let (rates_path, cache_path) = paths_from_config(&config_path);

        assert_eq!(
            classify_changed_paths(
                &[
                    config_dir.join("discodex.log"),
                    cache_path,
                    config_dir.join("usage-cache.123.0.tmp"),
                    config_dir.join("diagnostics.txt"),
                    config_path.clone(),
                ],
                &config_path,
                &rates_path
            ),
            PathChange::None
        );

        assert_eq!(
            classify_changed_paths(
                &[
                    config_dir.join("discodex.log"),
                    dir.0.join(".codex/sessions/2026/11/rollout.jsonl"),
                ],
                &config_path,
                &rates_path
            ),
            PathChange::SessionDirty
        );

        assert_eq!(
            classify_changed_paths(
                &[
                    dir.0.join(".codex/sessions/2026/11/rollout.jsonl"),
                    rates_path.clone(),
                ],
                &config_path,
                &rates_path
            ),
            PathChange::RatesChanged
        );
    }

    #[test]
    fn preloader_warms_cache_before_ui_opens_and_refreshes_on_signal() {
        let dir = TempDir::new("preloader-e2e");
        let session_dir = dir.0.join("sessions");
        let session_file = session_dir.join("rollout.jsonl");
        write_codex_session(&session_file, 1_200, 300);

        let config_path = dir.0.join("AppData/Discodex/config.toml");
        let (rates_path, cache_path) = paths_from_config(&config_path);
        let sources = vec![SessionSource {
            provider: Provider::Codex,
            root: session_dir,
            store: SessionStore::LogFiles,
        }];
        let expected_scope = usage_scope_key(&sources, &rates_path);

        let preloader = UsagePreloader::start(sources, rates_path, cache_path.clone()).unwrap();
        assert_eq!(preloader.scope_key(), expected_scope);
        assert!(is_preloader_running(&expected_scope));

        let deadline = Instant::now() + Duration::from_secs(5);
        let first = loop {
            if let Some(cached) = load_cache(&cache_path, &expected_scope) {
                break cached;
            }
            assert!(
                Instant::now() < deadline,
                "preloader did not create initial cache"
            );
            thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(first.generation, 1);
        assert_eq!(first.bundle.all.rows[0].input_tokens, Some(1_200));

        thread::sleep(Duration::from_millis(25));
        write_codex_session(&session_file, 2_400, 600);
        assert!(signal_preloader_refresh(&expected_scope));

        let deadline = Instant::now() + Duration::from_secs(5);
        let second = loop {
            if let Some(cached) = load_cache(&cache_path, &expected_scope)
                && cached.generation > first.generation
            {
                break cached;
            }
            assert!(
                Instant::now() < deadline,
                "preloader did not refresh on signal"
            );
            thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(second.bundle.all.rows[0].input_tokens, Some(2_400));

        drop(preloader);
        assert!(!is_preloader_running(&expected_scope));
    }
}
