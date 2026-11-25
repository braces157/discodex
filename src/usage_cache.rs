use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use chrono::{Local, TimeZone};
use serde::{Deserialize, Serialize};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_EVENT, WAIT_FAILED, WAIT_OBJECT_0,
            WAIT_TIMEOUT,
        },
        System::Threading::{
            CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, GetCurrentThread, OpenEventW,
            ReleaseMutex, SetEvent, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
            WaitForMultipleObjects, WaitForSingleObject,
        },
    },
    core::PCWSTR,
};

use crate::{
    config::AppConfig,
    provider::SessionSource,
    usage::{UsageBundle, UsageScanner, current_hour_start_ms, usage_scope_key},
};

pub const CACHE_SCHEMA_VERSION: u32 = 3;
pub const CACHE_FILE_NAME: &str = "usage-cache.json";
pub const RATES_FILE_NAME: &str = "usage-rates.toml";

pub const DIRTY_DEBOUNCE: Duration = Duration::from_secs(5);
pub const DIRTY_MAX_WAIT: Duration = Duration::from_secs(20);
pub const PERIODIC_REFRESH: Duration = Duration::from_secs(180);
pub const FRESH_CACHE_MAX_AGE_MS: i64 = 10_000;
const MAX_WAIT_SLICE: Duration = Duration::from_secs(30);
const CACHE_LOCK_TIMEOUT_MS: u32 = 2_000;
const PRELOADER_DROP_TIMEOUT: Duration = Duration::from_secs(2);

pub type CacheStamp = (u64, Option<SystemTime>);

#[derive(Debug, Clone)]
pub enum PublishOutcome {
    Published(CachedUsage),
    Superseded(CachedUsage),
}

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
pub fn cache_file_stamp(cache_path: &Path) -> Option<CacheStamp> {
    fs::metadata(cache_path)
        .ok()
        .map(|meta| (meta.len(), meta.modified().ok()))
}

/// Returns true if `candidate` represents a newer observation than `(current_refreshed_at_ms, current_generation)`.
pub fn is_newer_bundle(
    current_refreshed_at_ms: i64,
    current_generation: u64,
    candidate: &CachedUsage,
) -> bool {
    let candidate_ms = candidate.bundle.refreshed_at_ms();
    candidate_ms > current_refreshed_at_ms
        || (candidate_ms == current_refreshed_at_ms && candidate.generation > current_generation)
}

/// Loads and validates `cache_path` against schema version, `expected_scope_key`, and today's local date.
pub fn load_cache(cache_path: &Path, expected_scope_key: &str) -> Option<CachedUsage> {
    load_cache_for_date(cache_path, expected_scope_key, &current_local_date())
}

/// Loads `cache_path` along with its metadata stamp, returning `None` for the stamp if the file
/// changed concurrently during the read.
pub fn load_cache_with_stamp(
    cache_path: &Path,
    expected_scope_key: &str,
) -> (Option<CachedUsage>, Option<CacheStamp>) {
    let stamp_before = cache_file_stamp(cache_path);
    let cached = load_cache(cache_path, expected_scope_key);
    let stamp_after = cache_file_stamp(cache_path);
    let stable_stamp = if stamp_before.is_some() && stamp_before == stamp_after {
        stamp_before
    } else {
        None
    };
    (cached, stable_stamp)
}

fn load_cache_any_date(cache_path: &Path, expected_scope_key: &str) -> Option<CachedUsage> {
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
    if entry.schema_version != CACHE_SCHEMA_VERSION || entry.scope_key != expected_scope_key {
        return None;
    }
    Some(entry)
}

fn load_cache_for_date(
    cache_path: &Path,
    expected_scope_key: &str,
    expected_local_date: &str,
) -> Option<CachedUsage> {
    load_cache_any_date(cache_path, expected_scope_key)
        .filter(|entry| entry.local_date == expected_local_date)
}

/// Coordinates cross-process writes to `cache_path` so an older scan that finishes later cannot
/// overwrite a newer scan or regress the monotonic generation counter.
pub fn publish_cache(
    cache_path: &Path,
    scope_key: &str,
    rates_fingerprint: RatesFingerprint,
    bundle: UsageBundle,
    min_generation: u64,
) -> io::Result<PublishOutcome> {
    let local_date = local_date_for_ms(bundle.refreshed_at_ms()).unwrap_or_else(current_local_date);
    let _guard = CacheMutexGuard::acquire(scope_key)?;
    let existing = load_cache_any_date(cache_path, scope_key);
    if let Some(existing) = &existing
        && (existing.local_date > local_date
            || (existing.local_date == local_date
                && existing.bundle.refreshed_at_ms() > bundle.refreshed_at_ms()))
    {
        return Ok(PublishOutcome::Superseded(existing.clone()));
    }
    let generation = existing
        .as_ref()
        .map_or(min_generation, |e| e.generation.max(min_generation))
        .saturating_add(1);
    let cached = CachedUsage {
        schema_version: CACHE_SCHEMA_VERSION,
        scope_key: scope_key.to_owned(),
        local_date,
        generation,
        rates_fingerprint,
        bundle,
    };
    save_cache(cache_path, &cached)?;
    Ok(PublishOutcome::Published(cached))
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

fn cache_lock_name(scope_key: &str) -> String {
    format!("Local\\Discodex.UsageCacheLock.{scope_key}")
}

struct CacheMutexGuard {
    handle: HANDLE,
    locked: bool,
}

impl CacheMutexGuard {
    fn acquire(scope_key: &str) -> io::Result<Self> {
        let wide = to_wide(&cache_lock_name(scope_key));
        // SAFETY: `wide` is a valid NUL-terminated UTF-16 buffer live for this `CreateMutexW` call.
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(wide.as_ptr())) }
            .map_err(|err| io::Error::other(format!("CreateMutexW failed: {err}")))?;
        // SAFETY: `handle` is a valid mutex kernel handle owned by this guard.
        let wait = unsafe { WaitForSingleObject(handle, CACHE_LOCK_TIMEOUT_MS) };
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            return Ok(Self {
                handle,
                locked: true,
            });
        }
        // SAFETY: `handle` is valid and has not been closed yet; ownership ends before returning `Err`.
        unsafe {
            let _ = CloseHandle(handle);
        }
        if wait == WAIT_TIMEOUT {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for usage cache lock",
            ))
        } else {
            Err(io::Error::other("failed waiting for usage cache lock"))
        }
    }
}

impl Drop for CacheMutexGuard {
    fn drop(&mut self) {
        if self.locked {
            // SAFETY: This thread holds ownership of `self.handle` from `WaitForSingleObject`.
            unsafe {
                let _ = ReleaseMutex(self.handle);
            }
        }
        // SAFETY: `self.handle` is uniquely owned by this guard and closed once on drop.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
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
    last_scan_hour_start_ms: Option<i64>,
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
                || current_hour_start_ms(crate::now_ms()) != state.last_scan_hour_start_ms
        };
        if should_refresh {
            self.request_refresh();
        }
    }
}

struct FinishSignal(Arc<(Mutex<bool>, Condvar)>);

impl Drop for FinishSignal {
    fn drop(&mut self) {
        let (lock, ready) = &*self.0;
        let mut done = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *done = true;
        ready.notify_all();
    }
}

fn join_with_timeout(
    thread: JoinHandle<()>,
    finished: &(Mutex<bool>, Condvar),
    timeout: Duration,
) -> bool {
    let (lock, ready) = finished;
    let done = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (guard, _) = ready
        .wait_timeout_while(done, timeout, |is_done| !*is_done)
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let exited = *guard;
    drop(guard);
    if exited {
        let _ = thread.join();
    }
    exited
}

fn dirty_scan_due(
    first_dirty_at: Option<Instant>,
    last_dirty_at: Option<Instant>,
    now: Instant,
) -> bool {
    first_dirty_at
        .zip(last_dirty_at)
        .is_some_and(|(first, last)| {
            now.saturating_duration_since(last) >= DIRTY_DEBOUNCE
                || now.saturating_duration_since(first) >= DIRTY_MAX_WAIT
        })
}

/// Owns the low-priority background usage preloader thread inside the tray process.
pub struct UsagePreloader {
    #[cfg(test)]
    scope_key: String,
    cancel: Arc<AtomicBool>,
    stop_event: Arc<WinEvent>,
    finished: Arc<(Mutex<bool>, Condvar)>,
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
        let finished = Arc::new((Mutex::new(false), Condvar::new()));

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
        let thread_finished = Arc::clone(&finished);

        let thread = thread::Builder::new()
            .name("usage-preloader".into())
            .spawn(move || {
                let _finish = FinishSignal(thread_finished);
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
            finished,
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
        if let Some(thread) = self.thread.take()
            && !join_with_timeout(thread, &self.finished, PRELOADER_DROP_TIMEOUT)
        {
            crate::log_line("usage preloader did not exit within 2s; detaching during shutdown");
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
                let fallback_refreshed_at_ms = bundle.refreshed_at_ms();
                let fallback_date =
                    local_date_for_ms(fallback_refreshed_at_ms).unwrap_or_else(current_local_date);
                let (applied_rates, applied_date, applied_refreshed_at_ms) = match publish_cache(
                    &cache_path,
                    &scope_key,
                    rates_fp.clone(),
                    bundle,
                    *generation,
                ) {
                    Ok(PublishOutcome::Published(cached)) => {
                        *generation = cached.generation;
                        crate::log_line(&format!(
                            "usage preloader updated cache gen={} files={} in {}ms",
                            cached.generation,
                            cached.bundle.week.scanned_files,
                            started.elapsed().as_millis()
                        ));
                        (
                            cached.rates_fingerprint,
                            cached.local_date,
                            cached.bundle.refreshed_at_ms(),
                        )
                    }
                    Ok(PublishOutcome::Superseded(existing)) => {
                        *generation = (*generation).max(existing.generation);
                        crate::log_line(&format!(
                            "usage preloader kept newer cache gen={} after {}ms scan",
                            existing.generation,
                            started.elapsed().as_millis()
                        ));
                        (
                            existing.rates_fingerprint,
                            existing.local_date,
                            existing.bundle.refreshed_at_ms(),
                        )
                    }
                    Err(err) => {
                        crate::log_line(&format!("usage preloader failed to save cache: {err}"));
                        (rates_fp, fallback_date, fallback_refreshed_at_ms)
                    }
                };
                let mut state = shared
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.has_completed_scan = true;
                state.last_scan_completed_at = Some(Instant::now());
                state.last_scan_rates = applied_rates;
                state.last_scan_local_date = applied_date;
                state.last_scan_hour_start_ms = current_hour_start_ms(applied_refreshed_at_ms);
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
        } else {
            if signaled == WAIT_EVENT(WAIT_OBJECT_0.0 + 2) {
                let dirty_now = Instant::now();
                if first_dirty_at.is_none() {
                    first_dirty_at = Some(dirty_now);
                }
                last_dirty_at = Some(dirty_now);
            }
            if signaled == WAIT_EVENT(WAIT_OBJECT_0.0 + 2) || signaled == WAIT_TIMEOUT {
                let dirty_due = dirty_scan_due(first_dirty_at, last_dirty_at, Instant::now());
                let periodic_due = last_scan.elapsed() >= PERIODIC_REFRESH;
                let (date_changed, hour_changed, rates_changed) = {
                    let state = shared
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    (
                        state.last_scan_local_date != current_local_date(),
                        state.last_scan_hour_start_ms != current_hour_start_ms(crate::now_ms()),
                        state.last_scan_rates != rates_fingerprint(&rates_path),
                    )
                };
                if dirty_due || periodic_due || date_changed || hour_changed || rates_changed {
                    first_dirty_at = None;
                    last_dirty_at = None;
                    last_scan = run_scan(&mut scanner, &mut generation);
                }
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

    fn set_bundle_timestamp(bundle: &mut UsageBundle, ts_ms: i64) {
        bundle.last_24h.refreshed_at_ms = ts_ms;
        bundle.week.refreshed_at_ms = ts_ms;
        bundle.month.refreshed_at_ms = ts_ms;
        bundle.all.refreshed_at_ms = ts_ms;
    }

    #[test]
    fn publish_cache_rejects_older_scan_and_returns_superseding_entry() {
        let dir = TempDir::new("publish-coord");
        let cache_path = dir.0.join(CACHE_FILE_NAME);
        let rates_path = dir.0.join(RATES_FILE_NAME);
        let session_file = dir.0.join("sessions/one.jsonl");
        write_codex_session(&session_file, 100, 10);
        let sources = vec![SessionSource {
            provider: Provider::Codex,
            root: dir.0.join("sessions"),
            store: SessionStore::LogFiles,
        }];
        let scope = usage_scope_key(&sources, &rates_path);
        let mut scanner = UsageScanner::new(sources, rates_path.clone());

        let mut older_bundle = scanner.scan_all_periods();
        write_codex_session(&session_file, 900, 90);
        let mut newer_bundle = scanner.scan_all_periods();

        let base_ms = newer_bundle.refreshed_at_ms().max(1_700_000_000_000);
        set_bundle_timestamp(&mut newer_bundle, base_ms);
        set_bundle_timestamp(&mut older_bundle, base_ms - 5_000);

        let rates_fp = rates_fingerprint(&rates_path);
        let published =
            publish_cache(&cache_path, &scope, rates_fp.clone(), newer_bundle, 0).unwrap();
        let first = match published {
            PublishOutcome::Published(entry) => entry,
            PublishOutcome::Superseded(_) => panic!("initial publish must succeed"),
        };
        assert_eq!(first.generation, 1);

        let stale = publish_cache(&cache_path, &scope, rates_fp, older_bundle, 0).unwrap();
        match stale {
            PublishOutcome::Superseded(existing) => {
                assert_eq!(existing.generation, 1);
                assert_eq!(existing.bundle.all.rows[0].input_tokens, Some(900));
            }
            PublishOutcome::Published(_) => panic!("older observation must be superseded"),
        }
    }

    #[test]
    fn load_cache_with_stamp_returns_stable_stamp_and_none_when_missing() {
        let dir = TempDir::new("load-stamp");
        let cache_path = dir.0.join(CACHE_FILE_NAME);
        let rates_path = dir.0.join(RATES_FILE_NAME);
        let sources = vec![SessionSource {
            provider: Provider::Codex,
            root: dir.0.join("sessions"),
            store: SessionStore::LogFiles,
        }];
        let scope = usage_scope_key(&sources, &rates_path);

        assert_eq!(load_cache_with_stamp(&cache_path, &scope).1, None);
        assert!(load_cache_with_stamp(&cache_path, &scope).0.is_none());

        write_codex_session(&dir.0.join("sessions/one.jsonl"), 300, 30);
        let mut scanner = UsageScanner::new(sources, rates_path.clone());
        let bundle = scanner.scan_all_periods();
        publish_cache(
            &cache_path,
            &scope,
            rates_fingerprint(&rates_path),
            bundle,
            0,
        )
        .unwrap();

        let (loaded, stamp) = load_cache_with_stamp(&cache_path, &scope);
        assert!(loaded.is_some());
        assert_eq!(stamp, cache_file_stamp(&cache_path));
    }

    #[test]
    fn dirty_scan_due_triggers_at_max_wait_even_under_continuous_dirty_events() {
        let t0 = Instant::now();
        assert!(!dirty_scan_due(
            Some(t0),
            Some(t0 + Duration::from_secs(19)),
            t0 + Duration::from_secs(19)
        ));
        assert!(dirty_scan_due(
            Some(t0),
            Some(t0 + Duration::from_secs(20)),
            t0 + Duration::from_secs(20)
        ));
        assert!(dirty_scan_due(
            Some(t0),
            Some(t0 + Duration::from_secs(1)),
            t0 + Duration::from_secs(6)
        ));
    }

    #[test]
    fn join_with_timeout_detaches_blocked_thread_without_hanging() {
        let fast_finished = Arc::new((Mutex::new(false), Condvar::new()));
        let fast_flag = Arc::clone(&fast_finished);
        let fast_handle = thread::spawn(move || {
            let _finish = FinishSignal(fast_flag);
        });
        assert!(join_with_timeout(
            fast_handle,
            &fast_finished,
            Duration::from_secs(1)
        ));

        let slow_finished = Arc::new((Mutex::new(false), Condvar::new()));
        let slow_flag = Arc::clone(&slow_finished);
        let (tx, rx) = std::sync::mpsc::channel();
        let slow_handle = thread::spawn(move || {
            let _finish = FinishSignal(slow_flag);
            let _ = rx.recv_timeout(Duration::from_secs(2));
        });
        let start = Instant::now();
        let joined = join_with_timeout(slow_handle, &slow_finished, Duration::from_millis(40));
        assert!(!joined);
        assert!(start.elapsed() < Duration::from_millis(500));
        let _ = tx.send(());
    }
}
