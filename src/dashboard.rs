use std::{
    collections::BTreeMap,
    io,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, Command as ProcessCommand},
    sync::{Arc, Condvar, Mutex, OnceLock, mpsc},
    thread,
    time::{Duration, Instant},
};

use chrono::{Datelike, Days, Local, NaiveDate, TimeZone};
use gpui::{
    App, Application, Bounds, Context, FocusHandle, FontWeight, Image, ImageFormat, IntoElement,
    ObjectFit, Render, Subscription, Timer, Window, WindowBackgroundAppearance, WindowBounds,
    WindowKind, WindowOptions, div, img, point, prelude::*, px, relative, rgb, rgba, size,
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, POINT},
        Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint},
        System::Threading::CreateMutexW,
        UI::{
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            WindowsAndMessaging::{
                FindWindowW, GetCursorPos, HWND_TOPMOST, SW_RESTORE, SWP_SHOWWINDOW,
                SetForegroundWindow, SetWindowPos, ShowWindow,
            },
        },
    },
    core::w,
};

use crate::{
    provider::{self, SessionSource},
    usage::{
        UsageBundle, UsageDay, UsagePeriod, UsageRow, UsageScanner, UsageSnapshot, usage_scope_key,
    },
    usage_cache::{
        self, CACHE_SCHEMA_VERSION, CacheStamp, CachedUsage, FRESH_CACHE_MAX_AGE_MS, PublishOutcome,
    },
};

const BG_CANVAS: u32 = 0x0D0F12;
const BG_SURFACE: u32 = 0x14171C;
const BG_RECESSED: u32 = 0x0A0C0F;
const BG_ELEVATED: u32 = 0x1E232B;
const BG_HOVER: u32 = 0xFFFFFF0D;
const BG_ROW_HOVER: u32 = 0xFFFFFF08;

const BORDER_OUTER: u32 = 0xFFFFFF1F;
const BORDER_SUBTLE: u32 = 0xFFFFFF12;
const BORDER_STRONG: u32 = 0xFFFFFF24;
const GRID_LINE: u32 = 0xFFFFFF0A;
const TRACK_BG: u32 = 0xFFFFFF0C;

const TEXT_PRIMARY: u32 = 0xF3F4F6;
const TEXT_SECONDARY: u32 = 0x9CA3AF;
const TEXT_MUTED: u32 = 0x6B7280;

const ACCENT_LIME: u32 = 0xC5F277;
const ACCENT_LIME_SOFT: u32 = 0xC5F2771F;
const ACCENT_LIME_BORDER: u32 = 0xC5F27745;
const ACCENT_LIME_DIM: u32 = 0xC5F2774D;

const ACCENT_COST: u32 = 0x6EE7B7;
const ACCENT_COST_DIM: u32 = 0x6EE7B74D;
const ACCENT_CACHED_BAR: u32 = 0x6EE7B7B8;

const ACCENT_CALLS: u32 = 0x7DD3FC;
const ACCENT_CALLS_DIM: u32 = 0x7DD3FC4D;

const ACCENT_CLAUDE: u32 = 0xE8A87C;
const ACCENT_AMBER: u32 = 0xF5B971;
const ACCENT_AMBER_SOFT: u32 = 0xF5B97116;
const ACCENT_AMBER_BORDER: u32 = 0xF5B97138;
const ACCENT_ERR: u32 = 0xF87171;
const ACCENT_ERR_SOFT: u32 = 0xF871711A;
const ACCENT_ERR_BORDER: u32 = 0xF8717140;

const FONT_UI: &str = "Segoe UI Variable";
const FONT_MONO: &str = "Cascadia Mono";
const FONT_ICON: &str = "Segoe Fluent Icons";

const PANEL_WIDTH: f32 = 460.0;
const PANEL_HEIGHT: f32 = 620.0;

struct Assets {
    brand: Arc<Image>,
    providers: BTreeMap<&'static str, Arc<Image>>,
}

impl Assets {
    fn load() -> Self {
        let icons: [(&'static str, &[u8]); 16] = [
            ("codex", include_bytes!("../assets/logos/codex.png")),
            (
                "claude-code",
                include_bytes!("../assets/logos/claude-code.png"),
            ),
            (
                "antigravity",
                include_bytes!("../assets/logos/antigravity.png"),
            ),
            (
                "deepseek-harness",
                include_bytes!("../assets/logos/deepseek-harness.png"),
            ),
            (
                "gemini-cli",
                include_bytes!("../assets/logos/gemini-cli.png"),
            ),
            ("qwen-code", include_bytes!("../assets/logos/qwen-code.png")),
            ("opencode", include_bytes!("../assets/logos/opencode.png")),
            ("kilo-code", include_bytes!("../assets/logos/kilo-code.png")),
            (
                "factory-droid",
                include_bytes!("../assets/logos/factory-droid.png"),
            ),
            (
                "copilot-cli",
                include_bytes!("../assets/logos/copilot-cli.png"),
            ),
            ("junie", include_bytes!("../assets/logos/junie.png")),
            ("cline", include_bytes!("../assets/logos/cline.png")),
            ("roo-code", include_bytes!("../assets/logos/roo-code.png")),
            ("zed", include_bytes!("../assets/logos/zed.png")),
            ("crush", include_bytes!("../assets/logos/crush.png")),
            ("aider", include_bytes!("../assets/logos/aider.png")),
        ];
        let mut providers = BTreeMap::new();
        for (slug, bytes) in icons {
            providers.insert(
                slug,
                Arc::new(Image::from_bytes(ImageFormat::Png, bytes.to_vec())),
            );
        }
        Self {
            brand: Arc::new(Image::from_bytes(
                ImageFormat::Png,
                include_bytes!("../assets/brand/discodex-64.png").to_vec(),
            )),
            providers,
        }
    }

    fn provider_icon(&self, provider: &str) -> Option<Arc<Image>> {
        self.providers
            .get(provider.trim().to_ascii_lowercase().as_str())
            .cloned()
    }
}

pub fn show() -> io::Result<()> {
    usage_cache::nudge_local_preloader();
    static CHILD: OnceLock<Mutex<Option<Child>>> = OnceLock::new();
    let mut child = CHILD
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if focus_existing() {
        return Ok(());
    }
    if let Some(process) = child.as_mut()
        && process.try_wait()?.is_none()
    {
        return Ok(());
    }
    *child = Some(
        ProcessCommand::new(std::env::current_exe()?)
            .arg("--usage")
            .creation_flags(0x08000000)
            .spawn()?,
    );
    Ok(())
}

fn focus_existing() -> bool {
    unsafe {
        if let Ok(hwnd) = FindWindowW(None, w!("Discodex Usage")) {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
            return true;
        }
    }
    false
}

struct UsageMutex(HANDLE);
impl Drop for UsageMutex {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct Placement {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    scale: f32,
}

impl Placement {
    fn near_cursor() -> Self {
        unsafe {
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);
            let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return Self {
                    x: 40,
                    y: 40,
                    width: PANEL_WIDTH as i32,
                    height: PANEL_HEIGHT as i32,
                    scale: 1.0,
                };
            }
            let (mut dx, mut dy) = (96, 96);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            let scale = dx.max(96) as f32 / 96.;
            let work = info.rcWork;
            let margin = (10. * scale) as i32;
            let width =
                ((PANEL_WIDTH * scale) as i32).min((work.right - work.left - 2 * margin).max(1));
            let height =
                ((PANEL_HEIGHT * scale) as i32).min((work.bottom - work.top - 2 * margin).max(1));
            let x = (cursor.x - width + (24. * scale) as i32).clamp(
                work.left + margin,
                (work.right - width - margin).max(work.left + margin),
            );
            let mid_y = (work.top + work.bottom) / 2;
            let raw_y = if cursor.y < mid_y {
                cursor.y + margin
            } else {
                cursor.y - height - margin
            };
            let y = raw_y.clamp(
                work.top + margin,
                (work.bottom - height - margin).max(work.top + margin),
            );
            Self {
                x,
                y,
                width,
                height,
                scale,
            }
        }
    }

    fn apply(&self) {
        unsafe {
            if let Ok(hwnd) = FindWindowW(None, w!("Discodex Usage")) {
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    self.x,
                    self.y,
                    self.width,
                    self.height,
                    SWP_SHOWWINDOW,
                );
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }
}

pub fn run_standalone() -> Result<(), Box<dyn std::error::Error>> {
    let handle = unsafe { CreateMutexW(None, false, w!("Local\\Discodex.Usage.Popup"))? };
    let _guard = UsageMutex(handle);
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        focus_existing();
        return Ok(());
    }
    let placement = Placement::near_cursor();
    let error = Arc::new(Mutex::new(None));
    let result = error.clone();
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::new(
            point(px(40.), px(40.)),
            size(
                px(placement.width as f32 / placement.scale),
                px(placement.height as f32 / placement.scale),
            ),
        );
        if let Err(problem) = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                kind: WindowKind::Normal,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Opaque,
                ..Default::default()
            },
            |window, cx| {
                window.set_window_title("Discodex Usage");
                window.on_window_should_close(cx, |_, cx| {
                    cx.quit();
                    true
                });
                cx.new(|cx| Dashboard::new(window, cx))
            },
        ) {
            *result
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(problem.to_string());
            cx.quit();
            return;
        }
        placement.apply();
        cx.activate(true);
    });
    if let Some(error) = error
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        return Err(io::Error::other(error).into());
    }
    Ok(())
}

#[derive(Default)]
struct PendingWork {
    scan: bool,
    edit_rates: bool,
}

struct Worker {
    pending: Arc<(Mutex<PendingWork>, Condvar)>,
    updates: mpsc::Receiver<Update>,
}

enum Update {
    Bundle(Box<CachedUsage>),
    ScanError(String),
    Rates(Result<(), String>),
}

impl Worker {
    fn start(
        sources: Vec<SessionSource>,
        rates_path: PathBuf,
        cache_path: PathBuf,
        scope_key: String,
    ) -> io::Result<Self> {
        let pending = Arc::new((Mutex::new(PendingWork::default()), Condvar::new()));
        let work = pending.clone();
        let (send, updates) = mpsc::channel();
        thread::Builder::new()
            .name("usage-scan".into())
            .spawn(move || {
                let mut scanner = UsageScanner::new(sources, rates_path.clone());
                let mut generation =
                    usage_cache::load_cache(&cache_path, &scope_key).map_or(0, |c| c.generation);
                loop {
                    let (scan, edit) = {
                        let (lock, ready) = &*work;
                        let mut pending =
                            lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                        while !pending.scan && !pending.edit_rates {
                            pending = ready
                                .wait(pending)
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                        }
                        (
                            std::mem::take(&mut pending.scan),
                            std::mem::take(&mut pending.edit_rates),
                        )
                    };
                    if edit {
                        let result = crate::usage::ensure_rates_file(&rates_path)
                            .and_then(|()| {
                                ProcessCommand::new("notepad.exe")
                                    .arg(&rates_path)
                                    .spawn()
                                    .map(|_| ())
                            })
                            .map_err(|error| format!("Cannot open custom rates. {error}"));
                        if send.send(Update::Rates(result)).is_err() {
                            break;
                        }
                    }
                    if scan {
                        let rates_fp = usage_cache::rates_fingerprint(&rates_path);
                        let update =
                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                scanner.scan_all_periods()
                            })) {
                                Ok(bundle) => {
                                    let fallback_date =
                                        usage_cache::local_date_for_ms(bundle.refreshed_at_ms())
                                            .unwrap_or_else(usage_cache::current_local_date);
                                    let cached = match usage_cache::publish_cache(
                                        &cache_path,
                                        &scope_key,
                                        rates_fp.clone(),
                                        bundle.clone(),
                                        generation,
                                    ) {
                                        Ok(PublishOutcome::Published(saved)) => {
                                            generation = saved.generation;
                                            saved
                                        }
                                        Ok(PublishOutcome::Superseded(existing)) => {
                                            generation = generation.max(existing.generation);
                                            existing
                                        }
                                        Err(_) => {
                                            generation = generation.saturating_add(1);
                                            CachedUsage {
                                                schema_version: CACHE_SCHEMA_VERSION,
                                                scope_key: scope_key.clone(),
                                                local_date: fallback_date,
                                                generation,
                                                rates_fingerprint: rates_fp,
                                                bundle,
                                            }
                                        }
                                    };
                                    Update::Bundle(Box::new(cached))
                                }
                                Err(_) => Update::ScanError(
                                    "The usage scan stopped unexpectedly. Refresh to retry.".into(),
                                ),
                            };
                        if send.send(update).is_err() {
                            break;
                        }
                    }
                }
            })?;
        Ok(Self { pending, updates })
    }

    fn scan(&self) {
        let (lock, ready) = &*self.pending;
        lock.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .scan = true;
        ready.notify_one();
    }

    fn edit_rates(&self) {
        let (lock, ready) = &*self.pending;
        lock.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .edit_rates = true;
        ready.notify_one();
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Metric {
    Tokens,
    Cost,
    Calls,
}

impl Metric {
    fn accent(self) -> u32 {
        match self {
            Self::Tokens => ACCENT_LIME,
            Self::Cost => ACCENT_COST,
            Self::Calls => ACCENT_CALLS,
        }
    }

    fn dim(self) -> u32 {
        match self {
            Self::Tokens => ACCENT_LIME_DIM,
            Self::Cost => ACCENT_COST_DIM,
            Self::Calls => ACCENT_CALLS_DIM,
        }
    }
}

#[derive(Clone)]
enum Action {
    Period(UsagePeriod),
    Metric(Metric),
    ProviderFilter(Option<String>),
    Refresh,
    Close,
    Info,
    Rates,
    Pin,
}

struct Dashboard {
    focus: FocusHandle,
    _activation: Subscription,
    assets: Assets,
    worker: Option<Worker>,
    period: UsagePeriod,
    metric: Metric,
    bundle: Option<UsageBundle>,
    scope_key: String,
    cache_path: Option<PathBuf>,
    cache_generation: u64,
    cache_stamp: Option<CacheStamp>,
    waiting_for_tray: Option<Instant>,
    hovered_day: Option<usize>,
    provider_filter: Option<String>,
    refreshing: bool,
    last_request: Instant,
    error: Option<String>,
    rates_message: Option<String>,
    info_open: bool,
    has_activated: bool,
    pinned: bool,
}

impl Dashboard {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.has_activated = true;
            } else if this.has_activated && !this.pinned {
                cx.quit();
            }
        });
        let sources = provider::session_sources();
        let (rates_path, cache_path, scope_key, cached, stamp, worker, error) =
            match usage_cache::resolve_default_paths() {
                Ok((rates_path, cache_path)) => {
                    let scope_key = usage_scope_key(&sources, &rates_path);
                    let (cached, stamp) =
                        usage_cache::load_cache_with_stamp(&cache_path, &scope_key);
                    let (worker, error) = match Worker::start(
                        sources,
                        rates_path.clone(),
                        cache_path.clone(),
                        scope_key.clone(),
                    ) {
                        Ok(worker) => (Some(worker), None),
                        Err(error) => (None, Some(format!("Cannot start usage scan. {error}"))),
                    };
                    (
                        Some(rates_path),
                        Some(cache_path),
                        scope_key,
                        cached,
                        stamp,
                        worker,
                        error,
                    )
                }
                Err(error) => (
                    None,
                    None,
                    String::new(),
                    None,
                    None,
                    None,
                    Some(format!("Cannot locate rate settings. {error}")),
                ),
            };
        let mut dashboard = Self {
            focus,
            _activation: activation,
            assets: Assets::load(),
            worker,
            period: UsagePeriod::Week,
            metric: Metric::Tokens,
            bundle: None,
            scope_key,
            cache_path,
            cache_generation: 0,
            cache_stamp: stamp,
            waiting_for_tray: None,
            hovered_day: None,
            provider_filter: None,
            refreshing: false,
            last_request: Instant::now(),
            error,
            rates_message: None,
            info_open: false,
            has_activated: window.is_window_active(),
            pinned: std::env::args().any(|argument| argument == "--pin"),
        };
        if let Some(cached) = cached {
            let rates_changed = rates_path.as_deref().is_some_and(|path| {
                usage_cache::rates_fingerprint(path) != cached.rates_fingerprint
            });
            let age_ms = crate::now_ms().saturating_sub(cached.bundle.refreshed_at_ms());
            dashboard.bundle = Some(cached.bundle);
            dashboard.cache_generation = cached.generation;
            if dashboard.error.is_none() && (rates_changed || age_ms > FRESH_CACHE_MAX_AGE_MS) {
                dashboard.request_refresh(rates_changed);
            }
        } else if dashboard.error.is_none() {
            dashboard.request_refresh(true);
        }
        cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_millis(150)).await;
                if this.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        dashboard
    }

    fn current_snapshot(&self) -> Option<&UsageSnapshot> {
        self.bundle
            .as_ref()
            .map(|bundle| bundle.for_period(self.period))
    }

    fn prune_provider_filter(&mut self) {
        if let Some(filter) = &self.provider_filter
            && !self
                .current_snapshot()
                .is_some_and(|snapshot| snapshot.rows.iter().any(|row| &row.provider == filter))
        {
            self.provider_filter = None;
        }
    }

    fn apply_cached_bundle(&mut self, cached: CachedUsage) -> bool {
        let is_newer = self.bundle.as_ref().is_none_or(|current| {
            usage_cache::is_newer_bundle(current.refreshed_at_ms(), self.cache_generation, &cached)
        });
        if !is_newer {
            return false;
        }
        self.bundle = Some(cached.bundle);
        self.cache_generation = self.cache_generation.max(cached.generation);
        self.prune_provider_filter();
        self.hovered_day = None;
        self.refreshing = false;
        self.waiting_for_tray = None;
        self.error = None;
        true
    }

    fn request_refresh(&mut self, show_indicator: bool) {
        if !self.scope_key.is_empty() && usage_cache::signal_preloader_refresh(&self.scope_key) {
            self.error = None;
            self.last_request = Instant::now();
            if show_indicator || self.bundle.is_none() {
                self.refreshing = true;
            }
            self.waiting_for_tray = Some(Instant::now());
        } else if let Some(worker) = &self.worker {
            self.error = None;
            self.last_request = Instant::now();
            if show_indicator || self.bundle.is_none() {
                self.refreshing = true;
            }
            self.waiting_for_tray = None;
            worker.scan();
        } else {
            self.refreshing = false;
            self.waiting_for_tray = None;
            if self.error.is_none() {
                self.error =
                    Some("The local usage worker is unavailable. Reopen Usage to retry.".into());
            }
        }
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        if let Some(cache_path) = &self.cache_path {
            let stamp = usage_cache::cache_file_stamp(cache_path);
            if stamp.is_some()
                && (stamp != self.cache_stamp
                    || (self.waiting_for_tray.is_some() && self.bundle.is_none()))
            {
                let (cached, stable_stamp) =
                    usage_cache::load_cache_with_stamp(cache_path, &self.scope_key);
                self.cache_stamp = stable_stamp;
                if let Some(cached) = cached
                    && self.apply_cached_bundle(cached)
                {
                    changed = true;
                }
            }
        }
        if let Some(started_at) = self.waiting_for_tray {
            let preloader_gone =
                self.scope_key.is_empty() || !usage_cache::is_preloader_running(&self.scope_key);
            let timed_out = if self.bundle.is_none() {
                started_at.elapsed() >= Duration::from_secs(20)
            } else {
                started_at.elapsed() >= Duration::from_secs(15)
            };
            if preloader_gone || timed_out {
                self.waiting_for_tray = None;
                if let Some(worker) = &self.worker {
                    worker.scan();
                } else {
                    self.refreshing = false;
                    if self.error.is_none() {
                        self.error = Some(
                            "Background usage refresh timed out. Reopen Usage or retry.".into(),
                        );
                    }
                }
                changed = true;
            }
        }
        let mut worker_updates = Vec::new();
        let mut worker_disconnected = false;
        if let Some(worker) = &self.worker {
            loop {
                match worker.updates.try_recv() {
                    Ok(update) => worker_updates.push(update),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        worker_disconnected = true;
                        break;
                    }
                }
            }
        }
        for update in worker_updates {
            match update {
                Update::Bundle(cached) => {
                    let today = usage_cache::current_local_date();
                    let bundle_date =
                        usage_cache::local_date_for_ms(cached.bundle.refreshed_at_ms());
                    if cached.local_date != today || bundle_date.as_deref() != Some(today.as_str())
                    {
                        self.refreshing = false;
                        self.waiting_for_tray = None;
                        self.request_refresh(self.bundle.is_none());
                        changed = true;
                        continue;
                    }
                    if self.apply_cached_bundle(*cached) {
                        self.cache_stamp = None;
                        changed = true;
                    } else if self.waiting_for_tray.is_none() && self.refreshing {
                        self.refreshing = false;
                        self.error = None;
                        changed = true;
                    }
                }
                Update::ScanError(error) => {
                    self.error = Some(error);
                    self.refreshing = false;
                    self.waiting_for_tray = None;
                    changed = true;
                }
                Update::Rates(result) => {
                    self.rates_message = Some(match result {
                        Ok(()) => "Custom rates opened in Notepad. Save, then refresh.".into(),
                        Err(error) => error,
                    });
                    changed = true;
                }
            }
        }
        if worker_disconnected {
            self.worker = None;
            if self.error.is_none() {
                self.error =
                    Some("The local usage worker is unavailable. Reopen Usage to retry.".into());
            }
            self.refreshing = false;
            self.waiting_for_tray = None;
            changed = true;
        }
        if !self.refreshing
            && self.waiting_for_tray.is_none()
            && self.error.is_none()
            && self.last_request.elapsed() >= Duration::from_secs(10)
        {
            self.request_refresh(false);
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        match action {
            Action::Period(period) => {
                if self.period != period {
                    self.period = period;
                    self.hovered_day = None;
                    self.prune_provider_filter();
                }
            }
            Action::Metric(metric) => {
                self.metric = metric;
                self.hovered_day = None;
            }
            Action::ProviderFilter(provider) => {
                self.provider_filter = provider;
                self.hovered_day = None;
            }
            Action::Refresh => self.request_refresh(true),
            Action::Close => cx.quit(),
            Action::Info => self.info_open = !self.info_open,
            Action::Rates => {
                self.pinned = true;
                if let Some(worker) = &self.worker {
                    worker.edit_rates();
                }
            }
            Action::Pin => self.pinned = !self.pinned,
        }
        cx.notify();
    }

    fn icon_button(
        &self,
        id: &'static str,
        glyph: &'static str,
        active: bool,
        action: Action,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .size(px(28.))
            .rounded(px(7.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .font_family(FONT_ICON)
            .text_size(px(12.))
            .border_1()
            .border_color(rgba(if active {
                ACCENT_LIME_BORDER
            } else {
                0x00000000
            }))
            .bg(rgba(if active { ACCENT_LIME_SOFT } else { 0x00000000 }))
            .text_color(rgb(if active { ACCENT_LIME } else { TEXT_SECONDARY }))
            .hover(move |style| {
                style
                    .bg(rgba(if active { ACCENT_LIME_SOFT } else { BG_HOVER }))
                    .text_color(rgb(if active { ACCENT_LIME } else { TEXT_PRIMARY }))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.act(action.clone(), cx)))
            .child(glyph)
    }

    fn segment_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<gpui::SharedString>,
        selected: bool,
        accent: Option<u32>,
        action: Action,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected_text = accent.unwrap_or(TEXT_PRIMARY);
        div()
            .id(id)
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .py(px(5.))
            .px(px(8.))
            .rounded(px(6.))
            .cursor_pointer()
            .text_size(px(12.))
            .border_1()
            .border_color(rgba(if selected { BORDER_STRONG } else { 0x00000000 }))
            .bg(if selected {
                rgba((BG_ELEVATED << 8) | 0xFF)
            } else {
                rgba(0x00000000)
            })
            .text_color(rgb(if selected {
                selected_text
            } else {
                TEXT_SECONDARY
            }))
            .when(selected, |this| this.font_weight(FontWeight::MEDIUM))
            .hover(move |style| {
                let style = if selected {
                    style
                } else {
                    style.bg(rgba(BG_HOVER))
                };
                style.text_color(rgb(if selected {
                    selected_text
                } else {
                    TEXT_PRIMARY
                }))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.act(action.clone(), cx)))
            .child(label.into())
    }

    fn chip_button<T: Into<gpui::ElementId>, U: Into<gpui::SharedString>>(
        &self,
        id: T,
        label: U,
        selected: bool,
        action: Action,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<T, U> {
        div()
            .id(id)
            .px(px(8.))
            .py(px(3.))
            .rounded(px(6.))
            .cursor_pointer()
            .text_size(px(11.))
            .border_1()
            .border_color(rgba(if selected {
                ACCENT_LIME_BORDER
            } else {
                BORDER_SUBTLE
            }))
            .bg(if selected {
                rgba(ACCENT_LIME_SOFT)
            } else {
                rgba(0x00000000)
            })
            .text_color(rgb(if selected {
                ACCENT_LIME
            } else {
                TEXT_SECONDARY
            }))
            .when(selected, |this| this.font_weight(FontWeight::MEDIUM))
            .hover(move |style| {
                let style = if selected {
                    style
                } else {
                    style.bg(rgba(BG_HOVER))
                };
                style.text_color(rgb(if selected { ACCENT_LIME } else { TEXT_PRIMARY }))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.act(action.clone(), cx)))
            .child(label.into())
    }

    fn footer_button(
        &self,
        id: &'static str,
        glyph: &'static str,
        label: &'static str,
        active: bool,
        action: Action,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(5.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(6.))
            .cursor_pointer()
            .border_1()
            .border_color(rgba(if active {
                ACCENT_LIME_BORDER
            } else {
                0x00000000
            }))
            .bg(rgba(if active { ACCENT_LIME_SOFT } else { 0x00000000 }))
            .text_color(rgb(if active { ACCENT_LIME } else { TEXT_SECONDARY }))
            .hover(move |style| {
                style
                    .bg(rgba(if active { ACCENT_LIME_SOFT } else { BG_HOVER }))
                    .text_color(rgb(if active { ACCENT_LIME } else { TEXT_PRIMARY }))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.act(action.clone(), cx)))
            .child(div().font_family(FONT_ICON).text_size(px(11.)).child(glyph))
            .child(div().text_size(px(11.)).child(label))
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (dot_color, subtitle) = if self.error.is_some() {
            (ACCENT_ERR, "Scan issue")
        } else if self.refreshing && self.current_snapshot().is_none() {
            (ACCENT_AMBER, "Scanning logs...")
        } else if self.refreshing {
            (ACCENT_AMBER, "Refreshing...")
        } else {
            (ACCENT_LIME, "Usage")
        };

        div()
            .flex_none()
            .px(px(16.))
            .pt(px(14.))
            .pb(px(12.))
            .border_b_1()
            .border_color(rgba(BORDER_SUBTLE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(
                                img(self.assets.brand.clone())
                                    .size(px(28.))
                                    .rounded(px(7.))
                                    .object_fit(ObjectFit::Contain),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(6.))
                                            .child(
                                                div()
                                                    .text_size(px(14.))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(rgb(TEXT_PRIMARY))
                                                    .child("Discodex"),
                                            )
                                            .child(
                                                div()
                                                    .size(px(6.))
                                                    .rounded_full()
                                                    .bg(rgb(dot_color)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(rgb(TEXT_MUTED))
                                            .child(subtitle),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .child(self.icon_button(
                                "pin",
                                if self.pinned { "\u{E840}" } else { "\u{E718}" },
                                self.pinned,
                                Action::Pin,
                                cx,
                            ))
                            .child(self.icon_button(
                                "refresh",
                                "\u{E72C}",
                                false,
                                Action::Refresh,
                                cx,
                            ))
                            .child(self.icon_button("close", "\u{E8BB}", false, Action::Close, cx)),
                    ),
            )
            .child(
                div()
                    .mt(px(12.))
                    .bg(rgb(BG_RECESSED))
                    .border_1()
                    .border_color(rgba(BORDER_SUBTLE))
                    .rounded(px(9.))
                    .p(px(3.))
                    .flex()
                    .gap(px(2.))
                    .child(self.segment_button(
                        "today",
                        "Today",
                        self.period == UsagePeriod::Today,
                        None,
                        Action::Period(UsagePeriod::Today),
                        cx,
                    ))
                    .child(self.segment_button(
                        "week",
                        "7 days",
                        self.period == UsagePeriod::Week,
                        None,
                        Action::Period(UsagePeriod::Week),
                        cx,
                    ))
                    .child(self.segment_button(
                        "month",
                        "30 days",
                        self.period == UsagePeriod::Month,
                        None,
                        Action::Period(UsagePeriod::Month),
                        cx,
                    ))
                    .child(self.segment_button(
                        "all",
                        "All",
                        self.period == UsagePeriod::All,
                        None,
                        Action::Period(UsagePeriod::All),
                        cx,
                    )),
            )
    }

    fn hero_bento(&self, rows: &[UsageRow], daily: &[UsageDay]) -> impl IntoElement {
        let known = self.current_snapshot().is_some();
        let empty = known && rows.is_empty();

        let tokens = sum_tokens(rows);
        let input = sum_field(rows, |r| r.input_tokens);
        let output = sum_field(rows, |r| r.output_tokens);
        let composition = token_composition(rows);

        let token_value = if !known {
            "—".into()
        } else if empty {
            "0".into()
        } else {
            format_count(tokens)
        };
        let token_sub = if !known {
            "Scanning local sessions".into()
        } else if empty {
            "No tokens in this period".into()
        } else {
            match (input.0, output.0) {
                (Some(in_tok), Some(out_tok)) => {
                    let partial = if input.1 || output.1 { "*" } else { "" };
                    format!("{} in · {} out{partial}", compact(in_tok), compact(out_tok))
                }
                _ => "Partial token breakdown".into(),
            }
        };

        let cost = sum_cost(rows);
        let cost_value = if !known || empty {
            "—".into()
        } else {
            format_cost(cost)
        };
        let cost_color = if !known || empty || cost.0.is_none() {
            TEXT_MUTED
        } else {
            ACCENT_COST
        };
        let cost_sub: &'static str = if !known {
            "Checking catalog rates"
        } else if empty {
            "No billable calls"
        } else if cost.0.is_none() {
            "No catalog rates matched"
        } else if cost.1 {
            "Partial · some unpriced"
        } else {
            "Base-rate approximation"
        };

        let active_days = daily.iter().filter(|d| d.requests > 0).count();
        let sessions: usize = rows.iter().map(|r| r.sessions).sum();
        let cost_meta = if !known || empty {
            None
        } else if let Some(total_usd) = cost.0
            && active_days > 1
        {
            Some(format!(
                "~{} / active day",
                money(total_usd / active_days as f64)
            ))
        } else if sessions > 0 {
            Some(format!(
                "{sessions} {}",
                if sessions == 1 { "session" } else { "sessions" }
            ))
        } else {
            None
        };

        div()
            .flex()
            .gap(px(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .bg(rgb(BG_SURFACE))
                    .border_1()
                    .border_color(rgba(BORDER_SUBTLE))
                    .rounded(px(11.))
                    .p(px(13.))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(TEXT_MUTED))
                                    .child("TOKENS"),
                            )
                            .child(div().size(px(5.)).rounded_full().bg(rgb(ACCENT_LIME))),
                    )
                    .child(
                        div()
                            .mt(px(6.))
                            .font_family(FONT_MONO)
                            .text_size(px(22.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(if !known || empty {
                                TEXT_MUTED
                            } else {
                                TEXT_PRIMARY
                            }))
                            .child(token_value),
                    )
                    .child(
                        div()
                            .mt(px(4.))
                            .font_family(FONT_MONO)
                            .text_size(px(10.5))
                            .text_color(rgb(TEXT_MUTED))
                            .truncate()
                            .child(token_sub),
                    )
                    .when_some(composition, |this, (uncached_r, cached_r, output_r)| {
                        this.child(
                            div()
                                .mt(px(9.))
                                .h(px(3.))
                                .w_full()
                                .rounded_full()
                                .bg(rgba(TRACK_BG))
                                .overflow_hidden()
                                .flex()
                                .gap(px(1.))
                                .when(uncached_r > 0., |bar| {
                                    bar.child(
                                        div().h_full().w(relative(uncached_r)).bg(rgb(ACCENT_LIME)),
                                    )
                                })
                                .when(cached_r > 0., |bar| {
                                    bar.child(
                                        div()
                                            .h_full()
                                            .w(relative(cached_r))
                                            .bg(rgba(ACCENT_CACHED_BAR)),
                                    )
                                })
                                .when(output_r > 0., |bar| {
                                    bar.child(
                                        div().h_full().w(relative(output_r)).bg(rgb(ACCENT_CALLS)),
                                    )
                                }),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .bg(rgb(BG_SURFACE))
                    .border_1()
                    .border_color(rgba(BORDER_SUBTLE))
                    .rounded(px(11.))
                    .p(px(13.))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(TEXT_MUTED))
                                    .child("EST. API COST"),
                            )
                            .child(div().size(px(5.)).rounded_full().bg(rgb(ACCENT_COST))),
                    )
                    .child(
                        div()
                            .mt(px(6.))
                            .font_family(FONT_MONO)
                            .text_size(px(22.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(cost_color))
                            .child(cost_value),
                    )
                    .child(
                        div()
                            .mt(px(4.))
                            .text_size(px(10.5))
                            .text_color(rgb(TEXT_MUTED))
                            .truncate()
                            .child(cost_sub),
                    )
                    .when_some(cost_meta, |this, meta| {
                        this.child(
                            div()
                                .mt(px(6.))
                                .font_family(FONT_MONO)
                                .text_size(px(10.5))
                                .text_color(rgb(TEXT_SECONDARY))
                                .truncate()
                                .child(meta),
                        )
                    }),
            )
    }

    fn telemetry_strip(&self, rows: &[UsageRow]) -> impl IntoElement {
        let calls = compact(rows.iter().map(|r| r.requests).fold(0, u64::saturating_add));
        let (cache_pct, partial_cache) = cache_hit_ratio(rows);
        let (cache_text, cache_color) = match cache_pct {
            Some(pct) => (
                format!("{:.0}%{}", pct, if partial_cache { "*" } else { "" }),
                if pct >= 50. {
                    ACCENT_LIME
                } else {
                    TEXT_PRIMARY
                },
            ),
            None => ("—".into(), TEXT_MUTED),
        };

        let task_time_raw = sum_field(rows, |r| r.task_time_ms);
        let (task_time, time_color) = match task_time_raw.0 {
            Some(ms) => (
                format!("{}{}", duration(ms), if task_time_raw.1 { "*" } else { "" }),
                TEXT_PRIMARY,
            ),
            None => ("—".into(), TEXT_MUTED),
        };

        let cell = |label: &'static str, value: String, color: u32| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .items_center()
                .child(
                    div()
                        .text_size(px(9.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(TEXT_MUTED))
                        .child(label),
                )
                .child(
                    div()
                        .mt(px(3.))
                        .font_family(FONT_MONO)
                        .text_size(px(13.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(color))
                        .child(value),
                )
        };

        let divider = || div().w(px(1.)).h(px(24.)).bg(rgba(BORDER_SUBTLE));

        div()
            .bg(rgb(BG_SURFACE))
            .border_1()
            .border_color(rgba(BORDER_SUBTLE))
            .rounded(px(10.))
            .px(px(12.))
            .py(px(9.))
            .flex()
            .items_center()
            .child(cell("CALLS", calls, TEXT_PRIMARY))
            .child(divider())
            .child(cell("CACHE HIT", cache_text, cache_color))
            .child(divider())
            .child(cell("TASK TIME", task_time, time_color))
    }

    fn chart(&self, daily: &[UsageDay], cx: &mut Context<Self>) -> impl IntoElement {
        let (visible, truncated) = self
            .current_snapshot()
            .map(|snapshot| chart_days(daily, self.period, snapshot.refreshed_at_ms))
            .unwrap_or_default();
        let values: Vec<Option<f64>> = visible
            .iter()
            .map(|day| day_value(day, self.metric))
            .collect();
        let max = values.iter().flatten().copied().fold(0_f64, f64::max);
        let total: f64 = values.iter().flatten().sum();
        let has_unknown = values.iter().any(Option::is_none);
        let metric_color = self.metric.accent();
        let dim_color = self.metric.dim();

        let hovered_info = self
            .hovered_day
            .and_then(|idx| visible.get(idx).zip(values.get(idx)))
            .map(|(day, val)| {
                let formatted = val
                    .map(|v| format_metric_value(self.metric, v))
                    .unwrap_or_else(|| "Unknown".into());
                let calls_label = if day.requests == 1 { "call" } else { "calls" };
                (
                    formatted,
                    format!(
                        "{} · {} {calls_label}",
                        pretty_date(&day.date),
                        compact(day.requests)
                    ),
                )
            });

        let (readout_value, readout_sub) = if let Some((val, sub)) = hovered_info {
            (val, Some(sub))
        } else {
            let mut display_total = if values.is_empty() || values.iter().all(Option::is_none) {
                "—".into()
            } else {
                format_metric_value(self.metric, total)
            };
            if has_unknown && values.iter().any(Option::is_some) {
                display_total.push('*');
            }
            let peak =
                (max > 0.).then(|| format!("Peak {}", format_metric_value(self.metric, max)));
            (display_total, peak)
        };

        let axis_labels = match (visible.first(), visible.last()) {
            (Some(first), Some(last)) => {
                let mid = (visible.len() >= 7)
                    .then(|| visible.get(visible.len() / 2))
                    .flatten()
                    .map(|d| pretty_date(&d.date));
                Some((
                    pretty_date(&first.date),
                    mid,
                    if visible.len() == 1 {
                        None
                    } else {
                        Some(pretty_date(&last.date))
                    },
                ))
            }
            _ => None,
        };

        div()
            .bg(rgb(BG_SURFACE))
            .border_1()
            .border_color(rgba(BORDER_SUBTLE))
            .rounded(px(11.))
            .p(px(13.))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .flex()
                            .gap(px(2.))
                            .bg(rgb(BG_RECESSED))
                            .border_1()
                            .border_color(rgba(BORDER_SUBTLE))
                            .rounded(px(8.))
                            .p(px(2.))
                            .child(self.segment_button(
                                "tokens",
                                "Tokens",
                                self.metric == Metric::Tokens,
                                Some(ACCENT_LIME),
                                Action::Metric(Metric::Tokens),
                                cx,
                            ))
                            .child(self.segment_button(
                                "cost",
                                "Cost",
                                self.metric == Metric::Cost,
                                Some(ACCENT_COST),
                                Action::Metric(Metric::Cost),
                                cx,
                            ))
                            .child(self.segment_button(
                                "calls",
                                "Calls",
                                self.metric == Metric::Calls,
                                Some(ACCENT_CALLS),
                                Action::Metric(Metric::Calls),
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_end()
                            .child(
                                div()
                                    .font_family(FONT_MONO)
                                    .text_size(px(14.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(metric_color))
                                    .child(readout_value),
                            )
                            .when_some(readout_sub, |this, sub| {
                                this.child(
                                    div()
                                        .font_family(FONT_MONO)
                                        .text_size(px(10.))
                                        .text_color(rgb(TEXT_MUTED))
                                        .child(sub),
                                )
                            }),
                    ),
            )
            .child(if visible.is_empty() {
                div()
                    .mt(px(10.))
                    .h(px(56.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(11.5))
                    .text_color(rgb(TEXT_MUTED))
                    .child("No timestamped usage in this period")
            } else {
                div()
                    .mt(px(12.))
                    .h(px(96.))
                    .relative()
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .flex_col()
                            .justify_between()
                            .child(div().h(px(1.)).w_full().bg(rgba(GRID_LINE)))
                            .child(div().h(px(1.)).w_full().bg(rgba(GRID_LINE)))
                            .child(div().h(px(1.)).w_full().bg(rgba(GRID_LINE))),
                    )
                    .child(
                        div()
                            .relative()
                            .size_full()
                            .flex()
                            .items_end()
                            .gap(px(3.))
                            .children(values.iter().enumerate().map(|(index, value)| {
                                let is_hovered = self.hovered_day == Some(index);
                                let dimmed = self.hovered_day.is_some() && !is_hovered;
                                let (height, bar_color) = match value {
                                    Some(v) if *v > 0. && max > 0. => {
                                        let h = ((*v / max) * 88.0).max(3.0) as f32;
                                        let col = if dimmed {
                                            rgba(dim_color)
                                        } else {
                                            rgb(metric_color)
                                        };
                                        (h, col)
                                    }
                                    Some(_) => (2.0, rgba(BORDER_SUBTLE)),
                                    None => (3.0, rgb(ACCENT_AMBER)),
                                };
                                div()
                                    .id(("chart-bar", index))
                                    .flex_1()
                                    .h_full()
                                    .flex()
                                    .items_end()
                                    .justify_center()
                                    .cursor_pointer()
                                    .on_hover(cx.listener(move |this, hovered, _, cx| {
                                        if *hovered {
                                            this.hovered_day = Some(index);
                                            cx.notify();
                                        } else if this.hovered_day == Some(index) {
                                            this.hovered_day = None;
                                            cx.notify();
                                        }
                                    }))
                                    .child(
                                        div()
                                            .w_full()
                                            .max_w(px(24.))
                                            .h(px(height))
                                            .rounded_t(px(3.))
                                            .bg(bar_color),
                                    )
                            })),
                    )
            })
            .when_some(axis_labels, |this, (first, mid, last)| {
                this.child(
                    div()
                        .mt(px(7.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .font_family(FONT_MONO)
                        .text_size(px(10.))
                        .text_color(rgb(TEXT_MUTED))
                        .child(first)
                        .when_some(mid, |row, mid_label| row.child(mid_label))
                        .when_some(last, |row, last_label| row.child(last_label)),
                )
            })
            .when(truncated, |this| {
                this.child(
                    div()
                        .mt(px(5.))
                        .text_size(px(10.))
                        .text_color(rgb(TEXT_MUTED))
                        .child("Showing latest 30 calendar days · summary covers full period"),
                )
            })
            .when(has_unknown, |this| {
                this.child(
                    div()
                        .mt(px(4.))
                        .text_size(px(10.))
                        .text_color(rgb(ACCENT_AMBER))
                        .child("Some daily values are incomplete."),
                )
            })
    }

    fn provider_filter_bar(
        &self,
        distinct_providers: &[String],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut filter_chips = Vec::with_capacity(distinct_providers.len());
        for (idx, provider) in distinct_providers.iter().enumerate() {
            let selected = self.provider_filter.as_deref() == Some(provider.as_str());
            let label = provider_display(provider);
            filter_chips.push(self.chip_button(
                ("provider-filter", idx),
                label,
                selected,
                Action::ProviderFilter(Some(provider.clone())),
                cx,
            ));
        }
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(4.))
            .child(self.chip_button(
                "provider-all",
                "All",
                self.provider_filter.is_none(),
                Action::ProviderFilter(None),
                cx,
            ))
            .children(filter_chips)
    }

    fn models_section(&self, rows: &[UsageRow], show_provider_name: bool) -> impl IntoElement {
        let mut visible_rows: Vec<&UsageRow> = rows.iter().collect();
        visible_rows.sort_by_key(|row| {
            std::cmp::Reverse(
                row.input_tokens
                    .unwrap_or_default()
                    .saturating_add(row.output_tokens.unwrap_or_default()),
            )
        });

        let total_visible_tokens: u64 = visible_rows
            .iter()
            .map(|row| {
                row.input_tokens
                    .unwrap_or_default()
                    .saturating_add(row.output_tokens.unwrap_or_default())
            })
            .sum();

        div()
            .flex()
            .flex_col()
            .gap(px(7.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(TEXT_PRIMARY))
                                    .child("Models"),
                            )
                            .child(
                                div()
                                    .px(px(6.))
                                    .py(px(1.))
                                    .rounded_full()
                                    .bg(rgb(BG_ELEVATED))
                                    .font_family(FONT_MONO)
                                    .text_size(px(10.))
                                    .text_color(rgb(TEXT_SECONDARY))
                                    .child(visible_rows.len().to_string()),
                            ),
                    )
                    .child(
                        div().text_size(px(10.5)).text_color(rgb(TEXT_MUTED)).child(
                            self.provider_filter
                                .as_deref()
                                .map(provider_display)
                                .unwrap_or_else(|| "By tokens".into()),
                        ),
                    ),
            )
            .child(
                div()
                    .bg(rgb(BG_SURFACE))
                    .border_1()
                    .border_color(rgba(BORDER_SUBTLE))
                    .rounded(px(11.))
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .children(visible_rows.into_iter().enumerate().map(|(index, row)| {
                        let row_tokens = row
                            .input_tokens
                            .unwrap_or_default()
                            .saturating_add(row.output_tokens.unwrap_or_default());
                        let tokens = sum_tokens(std::slice::from_ref(row));
                        let share = share_ratio(row_tokens, total_visible_tokens);
                        let share_pct = (share * 100.).round() as u32;
                        let accent = provider_color(&row.provider);

                        let effort = row.effort.trim();
                        let show_effort =
                            !effort.is_empty() && !effort.eq_ignore_ascii_case("unknown");

                        let calls_label = if row.requests == 1 { "call" } else { "calls" };
                        let cache_note = match (row.cached_input_tokens, row.input_tokens) {
                            (Some(c), Some(i)) if i > 0 && c > 0 => {
                                format!(" · {:.0}% cache", c as f64 / i as f64 * 100.)
                            }
                            _ => String::new(),
                        };
                        let prefix = if show_provider_name {
                            format!("{} · ", provider_display(&row.provider))
                        } else {
                            String::new()
                        };
                        let left_meta = format!(
                            "{prefix}{} tok · {} {calls_label}{cache_note}",
                            format_count(tokens),
                            compact(row.requests),
                        );
                        let right_meta = match row.task_time_ms {
                            Some(ms) => format!("{} · {share_pct}%", duration(ms)),
                            None => format!("{share_pct}%"),
                        };

                        let cost_label = row
                            .estimated_cost_usd
                            .map(money)
                            .unwrap_or_else(|| "Unpriced".into());
                        let cost_color = if row.estimated_cost_usd.is_some() {
                            ACCENT_COST
                        } else {
                            TEXT_MUTED
                        };

                        let icon = self.assets.provider_icon(&row.provider);
                        let badge_letter = provider_display(&row.provider)
                            .chars()
                            .next()
                            .unwrap_or('?')
                            .to_uppercase()
                            .to_string();

                        div()
                            .when(index > 0, |this| {
                                this.border_t_1().border_color(rgba(BORDER_SUBTLE))
                            })
                            .px(px(12.))
                            .py(px(10.))
                            .hover(|style| style.bg(rgba(BG_ROW_HOVER)))
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .items_center()
                                            .gap(px(7.))
                                            .child(if let Some(icon) = icon {
                                                img(icon)
                                                    .size(px(16.))
                                                    .rounded(px(4.))
                                                    .object_fit(ObjectFit::Contain)
                                                    .flex_shrink_0()
                                                    .into_any_element()
                                            } else {
                                                div()
                                                    .size(px(16.))
                                                    .rounded(px(4.))
                                                    .bg(rgb(BG_ELEVATED))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .flex_shrink_0()
                                                    .font_family(FONT_MONO)
                                                    .text_size(px(9.5))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(rgb(accent))
                                                    .child(badge_letter)
                                                    .into_any_element()
                                            })
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_size(px(12.5))
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(rgb(TEXT_PRIMARY))
                                                    .child(unknown(&row.model)),
                                            )
                                            .when(show_effort, |name_row| {
                                                name_row.child(
                                                    div()
                                                        .flex_shrink_0()
                                                        .px(px(5.))
                                                        .py(px(1.))
                                                        .rounded(px(4.))
                                                        .bg(rgb(BG_ELEVATED))
                                                        .border_1()
                                                        .border_color(rgba(BORDER_SUBTLE))
                                                        .font_family(FONT_MONO)
                                                        .text_size(px(9.5))
                                                        .text_color(rgb(TEXT_SECONDARY))
                                                        .child(effort.to_string()),
                                                )
                                            }),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .font_family(FONT_MONO)
                                            .text_size(px(12.5))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(rgb(cost_color))
                                            .child(cost_label),
                                    ),
                            )
                            .child(
                                div()
                                    .h(px(3.))
                                    .w_full()
                                    .rounded_full()
                                    .bg(rgba(TRACK_BG))
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .h_full()
                                            .w(relative(share.clamp(0.02, 1.0)))
                                            .rounded_full()
                                            .bg(rgb(accent)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(8.))
                                    .font_family(FONT_MONO)
                                    .text_size(px(10.5))
                                    .text_color(rgb(TEXT_MUTED))
                                    .child(div().truncate().child(left_meta))
                                    .child(div().flex_shrink_0().child(right_meta)),
                            )
                    })),
            )
    }

    fn empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let heading = match self.period {
            UsagePeriod::Today => "No activity recorded today",
            UsagePeriod::Week => "No activity in the last 7 days",
            UsagePeriod::Month => "No activity in the last 30 days",
            UsagePeriod::All => "No recorded usage yet",
        };
        let suggestions: Vec<(&'static str, UsagePeriod)> = match self.period {
            UsagePeriod::Today => vec![
                ("View 7 days", UsagePeriod::Week),
                ("View 30 days", UsagePeriod::Month),
                ("View All", UsagePeriod::All),
            ],
            UsagePeriod::Week => vec![
                ("View 30 days", UsagePeriod::Month),
                ("View All", UsagePeriod::All),
            ],
            UsagePeriod::Month => vec![("View All", UsagePeriod::All)],
            UsagePeriod::All => Vec::new(),
        };

        div()
            .bg(rgb(BG_SURFACE))
            .border_1()
            .border_color(rgba(BORDER_SUBTLE))
            .rounded(px(12.))
            .py(px(28.))
            .px(px(20.))
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .size(px(36.))
                    .rounded_full()
                    .bg(rgb(BG_ELEVATED))
                    .border_1()
                    .border_color(rgba(BORDER_SUBTLE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .font_family(FONT_ICON)
                    .text_size(px(15.))
                    .text_color(rgb(ACCENT_LIME))
                    .child("\u{E9D9}"),
            )
            .child(
                div()
                    .mt(px(12.))
                    .text_size(px(13.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(TEXT_PRIMARY))
                    .child(heading),
            )
            .child(
                div()
                    .mt(px(4.))
                    .text_size(px(11.5))
                    .text_color(rgb(TEXT_MUTED))
                    .text_center()
                    .child("Supported local coding agent sessions appear here automatically."),
            )
            .when(!suggestions.is_empty(), |this| {
                let mut jump_chips = Vec::with_capacity(suggestions.len());
                for (idx, (label, target)) in suggestions.into_iter().enumerate() {
                    jump_chips.push(self.chip_button(
                        ("empty-jump", idx),
                        label,
                        false,
                        Action::Period(target),
                        cx,
                    ));
                }
                this.child(div().mt(px(14.)).flex().gap(px(6.)).children(jump_chips))
            })
    }

    fn notice_pill(&self, warnings: &[String], cx: &mut Context<Self>) -> impl IntoElement {
        let label = if warnings.len() == 1 {
            "Approximate base-rate estimate".to_string()
        } else {
            format!("Partial data · {} notes", warnings.len())
        };
        div()
            .id("notice-toggle")
            .bg(rgba(ACCENT_AMBER_SOFT))
            .border_1()
            .border_color(rgba(ACCENT_AMBER_BORDER))
            .rounded(px(8.))
            .px(px(10.))
            .py(px(7.))
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .on_click(cx.listener(|this, _, _, cx| this.act(Action::Info, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .min_w_0()
                    .child(
                        div()
                            .font_family(FONT_ICON)
                            .text_size(px(12.))
                            .text_color(rgb(ACCENT_AMBER))
                            .child("\u{E7BA}"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(ACCENT_AMBER))
                            .truncate()
                            .child(label),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(10.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(ACCENT_AMBER))
                    .child(if self.info_open { "Hide" } else { "Details" }),
            )
    }

    fn info_drawer(&self, date: &str, warnings: Vec<String>) -> impl IntoElement {
        div()
            .bg(rgb(BG_SURFACE))
            .border_1()
            .border_color(rgba(BORDER_SUBTLE))
            .rounded(px(10.))
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(11.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(TEXT_PRIMARY))
                            .child("Pricing & Notes"),
                    )
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .text_size(px(10.))
                            .text_color(rgb(TEXT_MUTED))
                            .child(format!("Checked {date}")),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(TEXT_SECONDARY))
                    .child("Base-rate API approximation using official standard rates (not a subscription bill). Custom entries in usage-rates.toml override the catalog."),
            )
            .child(
                div()
                    .font_family(FONT_MONO)
                    .text_size(px(10.))
                    .text_color(rgb(TEXT_MUTED))
                    .child("developers.openai.com/api/docs/pricing · platform.claude.com/docs/en/about-claude/pricing · ai.google.dev/gemini-api/docs/pricing · api-docs.deepseek.com/quick_start/pricing"),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(TEXT_MUTED))
                    .child("Calls count recorded observations. Cached input is part of input; reasoning is part of output. Task times may overlap. An asterisk (*) marks a partial total."),
            )
            .children(warnings.into_iter().map(|warning| {
                div()
                    .text_size(px(11.))
                    .text_color(rgb(ACCENT_AMBER))
                    .child(format!("· {warning}"))
            }))
    }

    fn footer(
        &self,
        updated: &str,
        scanned_files: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex_none()
            .bg(rgb(BG_RECESSED))
            .border_t_1()
            .border_color(rgba(BORDER_SUBTLE))
            .px(px(16.))
            .py(px(9.))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .child(
                div()
                    .font_family(FONT_MONO)
                    .text_size(px(10.5))
                    .text_color(rgb(TEXT_MUTED))
                    .truncate()
                    .child(format!("{updated} · {scanned_files} files")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .flex_shrink_0()
                    .child(self.footer_button(
                        "info",
                        "\u{E946}",
                        "Info",
                        self.info_open,
                        Action::Info,
                        cx,
                    ))
                    .child(self.footer_button(
                        "rates",
                        "\u{E70F}",
                        "Edit rates",
                        false,
                        Action::Rates,
                        cx,
                    )),
            )
    }
}

impl Render for Dashboard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.current_snapshot();
        let all_rows: Vec<UsageRow> = snapshot.map(|s| s.rows.clone()).unwrap_or_default();
        let mut provider_totals: BTreeMap<&str, u64> = BTreeMap::new();
        for row in &all_rows {
            let tok = row
                .input_tokens
                .unwrap_or_default()
                .saturating_add(row.output_tokens.unwrap_or_default());
            *provider_totals.entry(row.provider.as_str()).or_default() += tok;
        }
        let mut distinct_providers: Vec<String> =
            provider_totals.keys().map(|s| (*s).to_string()).collect();
        distinct_providers.sort_by_key(|p| {
            std::cmp::Reverse(provider_totals.get(p.as_str()).copied().unwrap_or_default())
        });

        let rows: Vec<UsageRow> = all_rows
            .iter()
            .filter(|row| {
                self.provider_filter
                    .as_ref()
                    .is_none_or(|filter| &row.provider == filter)
            })
            .cloned()
            .collect();
        let daily: Vec<UsageDay> = snapshot
            .map(|s| match &self.provider_filter {
                Some(filter) => s.daily_by_provider.get(filter).cloned().unwrap_or_default(),
                None => s.daily.clone(),
            })
            .unwrap_or_default();
        let warnings = snapshot.map(|s| s.warnings.clone()).unwrap_or_default();
        let date = snapshot
            .map(|s| s.pricing_checked_at.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("Unknown")
            .to_string();
        let updated = snapshot
            .and_then(|s| Local.timestamp_millis_opt(s.refreshed_at_ms).single())
            .map(|time| time.format("%H:%M:%S").to_string())
            .unwrap_or_else(|| "Scanning".into());
        let scanned_files = snapshot
            .map(|snapshot| snapshot.scanned_files)
            .unwrap_or_default();

        let is_loading = snapshot.is_none() && self.error.is_none();
        let is_empty = snapshot.is_some() && all_rows.is_empty();
        let show_provider_filter = !is_loading && !is_empty && distinct_providers.len() > 1;
        let show_provider_name = show_provider_filter && self.provider_filter.is_none();

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG_CANVAS))
            .border_1()
            .border_color(rgba(BORDER_OUTER))
            .rounded(px(12.))
            .overflow_hidden()
            .text_color(rgb(TEXT_PRIMARY))
            .font_family(FONT_UI)
            .text_size(px(13.))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                let mods = &event.keystroke.modifiers;
                if mods.control || mods.alt || mods.platform {
                    return;
                }
                match event.keystroke.key.as_str() {
                    "escape" => cx.quit(),
                    "1" => this.act(Action::Period(UsagePeriod::Today), cx),
                    "2" => this.act(Action::Period(UsagePeriod::Week), cx),
                    "3" => this.act(Action::Period(UsagePeriod::Month), cx),
                    "4" => this.act(Action::Period(UsagePeriod::All), cx),
                    "r" => this.act(Action::Refresh, cx),
                    "p" => this.act(Action::Pin, cx),
                    _ => {}
                }
            }))
            .child(self.header(cx))
            .child(
                div()
                    .id("usage-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(16.))
                    .py(px(14.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .when_some(self.error.clone(), |this, error| {
                                this.child(
                                    div()
                                        .bg(rgba(ACCENT_ERR_SOFT))
                                        .border_1()
                                        .border_color(rgba(ACCENT_ERR_BORDER))
                                        .rounded(px(10.))
                                        .p(px(11.))
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap(px(10.))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .text_size(px(11.5))
                                                .text_color(rgb(ACCENT_ERR))
                                                .child(error),
                                        )
                                        .child(self.chip_button(
                                            "retry",
                                            "Retry",
                                            false,
                                            Action::Refresh,
                                            cx,
                                        )),
                                )
                            })
                            .when(show_provider_filter, |this| {
                                this.child(self.provider_filter_bar(&distinct_providers, cx))
                            })
                            .child(self.hero_bento(&rows, &daily))
                            .when(is_loading, |this| {
                                this.child(
                                    div()
                                        .bg(rgb(BG_SURFACE))
                                        .border_1()
                                        .border_color(rgba(BORDER_SUBTLE))
                                        .rounded(px(11.))
                                        .py(px(28.))
                                        .px(px(16.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_size(px(12.))
                                        .text_color(rgb(TEXT_MUTED))
                                        .child("Reading local session logs..."),
                                )
                            })
                            .when(is_empty, |this| this.child(self.empty_state(cx)))
                            .when(!is_loading && !is_empty, |this| {
                                this.child(self.telemetry_strip(&rows))
                                    .child(self.chart(&daily, cx))
                                    .child(self.models_section(&rows, show_provider_name))
                            })
                            .when(!is_loading && !warnings.is_empty(), |this| {
                                this.child(self.notice_pill(&warnings, cx))
                            })
                            .when_some(self.rates_message.clone(), |this, message| {
                                this.child(
                                    div()
                                        .bg(rgba(ACCENT_LIME_SOFT))
                                        .border_1()
                                        .border_color(rgba(ACCENT_LIME_BORDER))
                                        .rounded(px(8.))
                                        .px(px(10.))
                                        .py(px(7.))
                                        .text_size(px(11.))
                                        .text_color(rgb(ACCENT_LIME))
                                        .child(message),
                                )
                            })
                            .when(self.info_open, |this| {
                                this.child(self.info_drawer(&date, warnings))
                            }),
                    ),
            )
            .child(self.footer(&updated, scanned_files, cx))
    }
}

fn unknown(value: &str) -> String {
    if value.trim().is_empty() {
        "Unknown".into()
    } else {
        value.into()
    }
}

fn provider_display(provider: &str) -> String {
    match provider.trim().to_ascii_lowercase().as_str() {
        "codex" => "Codex".into(),
        "claude-code" => "Claude Code".into(),
        "antigravity" => "Antigravity".into(),
        "deepseek-harness" => "DeepSeek".into(),
        "gemini-cli" => "Gemini CLI".into(),
        "qwen-code" => "Qwen Code".into(),
        "opencode" => "OpenCode".into(),
        "kilo-code" => "Kilo Code".into(),
        "factory-droid" => "Factory Droid".into(),
        "copilot-cli" => "Copilot CLI".into(),
        "junie" => "Junie".into(),
        "cline" => "Cline".into(),
        "roo-code" => "Roo Code".into(),
        "zed" => "Zed".into(),
        "crush" => "Crush".into(),
        "aider" => "Aider".into(),
        "" => "Unknown".into(),
        _ => provider.into(),
    }
}

fn provider_color(provider: &str) -> u32 {
    match provider.trim().to_ascii_lowercase().as_str() {
        "codex" => ACCENT_LIME,
        "claude-code" => ACCENT_CLAUDE,
        "antigravity" => 0x7DD3FC,
        "deepseek-harness" => 0x818CF8,
        "gemini-cli" => 0x60A5FA,
        "qwen-code" => 0xA78BFA,
        "opencode" => 0x6EE7B7,
        "kilo-code" => 0xF472B6,
        "factory-droid" => 0xFBBF24,
        "copilot-cli" => 0x38BDF8,
        "junie" => 0x34D399,
        "cline" => 0xFB923C,
        "roo-code" => 0xC084FC,
        "zed" => 0x93C5FD,
        "crush" => 0xF87171,
        "aider" => 0xA3E635,
        _ => ACCENT_CALLS,
    }
}

fn share_ratio(value: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (value as f32 / total as f32).clamp(0.0, 1.0)
    }
}

fn token_composition(rows: &[UsageRow]) -> Option<(f32, f32, f32)> {
    let input = sum_field(rows, |r| r.input_tokens).0?;
    let output = sum_field(rows, |r| r.output_tokens).0?;
    let total = input.saturating_add(output);
    if total == 0 {
        return None;
    }
    let cached = sum_field(rows, |r| r.cached_input_tokens)
        .0
        .unwrap_or(0)
        .min(input);
    let uncached = input.saturating_sub(cached);
    let denom = total as f32;
    Some((
        uncached as f32 / denom,
        cached as f32 / denom,
        output as f32 / denom,
    ))
}

fn cache_hit_ratio(rows: &[UsageRow]) -> (Option<f64>, bool) {
    let mut cached_total = 0u64;
    let mut input_total = 0u64;
    let mut matched = 0usize;
    for row in rows {
        if let (Some(cached), Some(input)) = (row.cached_input_tokens, row.input_tokens) {
            cached_total = cached_total.saturating_add(cached);
            input_total = input_total.saturating_add(input);
            matched += 1;
        }
    }
    let partial = matched < rows.len();
    let ratio = if matched > 0 && input_total > 0 {
        Some(cached_total as f64 / input_total as f64 * 100.)
    } else {
        None
    };
    (ratio, partial)
}

fn sum_field<T>(rows: &[T], field: impl Fn(&T) -> Option<u64>) -> (Option<u64>, bool) {
    let mut total = None;
    let mut partial = false;
    for row in rows {
        match field(row) {
            Some(value) => total = Some(total.unwrap_or(0u64).saturating_add(value)),
            None => partial = true,
        }
    }
    (total, partial)
}

fn sum_tokens<T: std::borrow::Borrow<UsageRow>>(rows: &[T]) -> (Option<u64>, bool) {
    let mut total = None;
    let mut partial = false;
    for row in rows {
        let row = row.borrow();
        for value in [row.input_tokens, row.output_tokens] {
            if let Some(value) = value {
                total = Some(total.unwrap_or(0u64).saturating_add(value));
            } else {
                partial = true;
            }
        }
    }
    (total, partial)
}

fn sum_cost(rows: &[UsageRow]) -> (Option<f64>, bool) {
    let values: Vec<f64> = rows.iter().filter_map(|r| r.estimated_cost_usd).collect();
    (
        if values.is_empty() {
            None
        } else {
            Some(values.iter().sum())
        },
        values.len() < rows.len(),
    )
}

fn format_count(value: (Option<u64>, bool)) -> String {
    value
        .0
        .map(|n| format!("{}{}", compact(n), if value.1 { "*" } else { "" }))
        .unwrap_or_else(|| "Unknown".into())
}

fn format_cost(value: (Option<f64>, bool)) -> String {
    value
        .0
        .map(|n| format!("{}{}", money(n), if value.1 { "*" } else { "" }))
        .unwrap_or_else(|| "Unpriced".into())
}

fn format_metric_value(metric: Metric, value: f64) -> String {
    match metric {
        Metric::Cost => money(value),
        _ => compact(value.round() as u64),
    }
}

fn compact(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.2}M", value as f64 / 1_000_000.)
    } else if value >= 1_000 {
        format!("{:.1}K", value as f64 / 1_000.)
    } else {
        value.to_string()
    }
}

fn money(value: f64) -> String {
    if value > 0. && value < 0.01 {
        format!("${value:.4}")
    } else {
        format!("${value:.2}")
    }
}

fn duration(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds >= 3600 {
        format!("{}h {:02}m", seconds / 3600, seconds / 60 % 60)
    } else if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}s", seconds)
    }
}

fn day_value(day: &UsageDay, metric: Metric) -> Option<f64> {
    match metric {
        Metric::Tokens => match (day.input_tokens, day.output_tokens) {
            (Some(input), Some(output)) => Some(input.saturating_add(output) as f64),
            _ => None,
        },
        Metric::Cost => day.estimated_cost_usd,
        Metric::Calls => Some(day.requests as f64),
    }
}

fn chart_days(
    daily: &[UsageDay],
    period: UsagePeriod,
    refreshed_at_ms: i64,
) -> (Vec<UsageDay>, bool) {
    let Some(now) = Local.timestamp_millis_opt(refreshed_at_ms).single() else {
        return (Vec::new(), false);
    };
    let parsed: BTreeMap<_, _> = daily
        .iter()
        .filter_map(|day| {
            NaiveDate::parse_from_str(&day.date, "%Y-%m-%d")
                .ok()
                .map(|date| (date, day))
        })
        .collect();
    if parsed.is_empty() {
        return (Vec::new(), false);
    }
    let end = if period == UsagePeriod::All {
        parsed
            .last_key_value()
            .map(|(date, _)| *date)
            .unwrap_or(now.date_naive())
    } else {
        now.date_naive()
    };
    let start = period
        .since_ms(refreshed_at_ms)
        .and_then(|time| Local.timestamp_millis_opt(time).single())
        .map(|time| time.date_naive())
        .or_else(|| parsed.first_key_value().map(|(date, _)| *date))
        .unwrap_or(end);
    let truncated = end.signed_duration_since(start).num_days() >= 30;
    let mut date = if truncated {
        end.checked_sub_days(Days::new(29)).unwrap_or(start)
    } else {
        start
    };
    let mut days = Vec::new();
    while date <= end && days.len() < 30 {
        days.push(
            parsed
                .get(&date)
                .map(|day| (*day).clone())
                .unwrap_or_else(|| UsageDay {
                    date: date.format("%Y-%m-%d").to_string(),
                    input_tokens: Some(0),
                    output_tokens: Some(0),
                    cached_input_tokens: Some(0),
                    estimated_cost_usd: Some(0.0),
                    requests: 0,
                }),
        );
        let Some(next) = date.succ_opt() else {
            break;
        };
        date = next;
    }
    (days, truncated)
}

fn pretty_date(date: &str) -> String {
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map(|d| format!("{} {}", d.format("%b"), d.day()))
        .unwrap_or_else(|_| date.get(5..).unwrap_or(date).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_preserves_calendar_gaps_and_unknown_recorded_values() {
        let at = Local
            .with_ymd_and_hms(2026, 10, 23, 12, 0, 0)
            .single()
            .unwrap()
            .timestamp_millis();
        let records = vec![
            UsageDay {
                date: "2026-10-19".into(),
                input_tokens: Some(100),
                requests: 1,
                ..Default::default()
            },
            UsageDay {
                date: "2026-10-23".into(),
                input_tokens: None,
                requests: 1,
                ..Default::default()
            },
        ];
        let (days, truncated) = chart_days(&records, UsagePeriod::Week, at);
        assert!(!truncated);
        assert_eq!(days.len(), 7);
        assert_eq!(days[2].date, "2026-10-19");
        assert_eq!(days[3].date, "2026-10-20");
        assert_eq!(days[3].input_tokens, Some(0));
        assert_eq!(days[3].estimated_cost_usd, Some(0.0));
        assert_eq!(days[6].input_tokens, None);
        assert_eq!(days.iter().map(|day| day.requests).sum::<u64>(), 2);
        assert!(chart_days(&[], UsagePeriod::Week, at).0.is_empty());
        assert!(chart_days(&[], UsagePeriod::All, at).0.is_empty());
    }

    #[test]
    fn pretty_date_formats_iso_dates_and_falls_back_cleanly() {
        assert_eq!(pretty_date("2026-11-19"), "Nov 19");
        assert_eq!(pretty_date("2026-01-05"), "Jan 5");
        assert_eq!(pretty_date("bad-date"), "ate");
        assert_eq!(pretty_date("raw"), "raw");
    }

    #[test]
    fn token_composition_splits_uncached_cached_and_output_ratios() {
        let rows = vec![UsageRow {
            input_tokens: Some(600),
            cached_input_tokens: Some(400),
            output_tokens: Some(400),
            ..Default::default()
        }];
        let (uncached, cached, output) = token_composition(&rows).unwrap();
        assert!((uncached - 0.2).abs() < 1e-6);
        assert!((cached - 0.4).abs() < 1e-6);
        assert!((output - 0.4).abs() < 1e-6);
        assert!((uncached + cached + output - 1.0).abs() < 1e-6);

        let zero_rows = vec![UsageRow {
            input_tokens: Some(0),
            output_tokens: Some(0),
            ..Default::default()
        }];
        assert!(token_composition(&zero_rows).is_none());
    }

    #[test]
    fn provider_display_and_share_ratio_handle_edge_cases() {
        assert_eq!(provider_display("codex"), "Codex");
        assert_eq!(provider_display("claude-code"), "Claude Code");
        assert_eq!(provider_display("antigravity"), "Antigravity");
        assert_eq!(provider_display("deepseek-harness"), "DeepSeek");
        assert_eq!(provider_display("gemini-cli"), "Gemini CLI");
        assert_eq!(provider_display("qwen-code"), "Qwen Code");
        assert_eq!(provider_display("opencode"), "OpenCode");
        assert_eq!(provider_display("kilo-code"), "Kilo Code");
        assert_eq!(provider_display("factory-droid"), "Factory Droid");
        assert_eq!(provider_display("copilot-cli"), "Copilot CLI");
        assert_eq!(provider_display("junie"), "Junie");
        assert_eq!(provider_display("cline"), "Cline");
        assert_eq!(provider_display("roo-code"), "Roo Code");
        assert_eq!(provider_display("zed"), "Zed");
        assert_eq!(provider_display("crush"), "Crush");
        assert_eq!(provider_display("aider"), "Aider");
        assert_eq!(provider_display("  "), "Unknown");
        assert_eq!(provider_display("custom"), "custom");

        let assets = Assets::load();
        for slug in [
            "codex",
            "claude-code",
            "antigravity",
            "deepseek-harness",
            "gemini-cli",
            "qwen-code",
            "opencode",
            "kilo-code",
            "factory-droid",
            "copilot-cli",
            "junie",
            "cline",
            "roo-code",
            "zed",
            "crush",
            "aider",
        ] {
            assert!(
                assets.provider_icon(slug).is_some(),
                "missing icon for {slug}"
            );
        }

        assert_eq!(share_ratio(0, 0), 0.0);
        assert_eq!(share_ratio(50, 200), 0.25);
        assert_eq!(share_ratio(300, 200), 1.0);
    }

    #[test]
    fn cache_hit_ratio_sums_only_rows_with_both_values_present() {
        let rows = vec![
            UsageRow {
                input_tokens: Some(100),
                cached_input_tokens: None,
                ..Default::default()
            },
            UsageRow {
                input_tokens: None,
                cached_input_tokens: Some(500),
                ..Default::default()
            },
            UsageRow {
                input_tokens: Some(200),
                cached_input_tokens: Some(50),
                ..Default::default()
            },
        ];
        let (ratio, partial) = cache_hit_ratio(&rows);
        assert!(partial);
        assert!((ratio.unwrap() - 25.0).abs() < 1e-6);

        let disjoint = vec![
            UsageRow {
                input_tokens: Some(100),
                cached_input_tokens: None,
                ..Default::default()
            },
            UsageRow {
                input_tokens: None,
                cached_input_tokens: Some(100),
                ..Default::default()
            },
        ];
        let (ratio, partial) = cache_hit_ratio(&disjoint);
        assert!(partial);
        assert!(ratio.is_none());
    }
}
