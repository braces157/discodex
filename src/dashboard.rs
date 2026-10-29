use std::{
    io,
    os::windows::process::CommandExt,
    process::{Child, Command as ProcessCommand},
    sync::{Arc, Condvar, Mutex, OnceLock, mpsc},
    thread,
    time::{Duration, Instant},
};

use chrono::{Days, Local, NaiveDate, TimeZone};
use gpui::{
    App, Application, Bounds, Context, FocusHandle, IntoElement, Render, Subscription, Timer,
    Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, point,
    prelude::*, px, rgb, size,
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
    config::AppConfig,
    provider,
    usage::{UsageDay, UsagePeriod, UsageRow, UsageScanner, UsageSnapshot},
};

const CANVAS: u32 = 0x16171b;
const CARD: u32 = 0x22242b;
const BORDER: u32 = 0x34363f;
const TEXT: u32 = 0xf0eef6;
const MUTED: u32 = 0xa4a3b2;
const LAVENDER: u32 = 0xb9abff;
const GREEN: u32 = 0x8bd7aa;
const AMBER: u32 = 0xe9c58c;

pub fn show() -> io::Result<()> {
    static CHILD: OnceLock<Mutex<Option<Child>>> = OnceLock::new();
    let mut child = CHILD
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| io::Error::other("Usage launcher lock is unavailable"))?;
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
                    width: 480,
                    height: 650,
                    scale: 1.0,
                };
            }
            let (mut dx, mut dy) = (96, 96);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            let scale = dx.max(96) as f32 / 96.;
            let work = info.rcWork;
            let margin = (8. * scale) as i32;
            let width = (480. * scale) as i32;
            let width = width.min((work.right - work.left - 2 * margin).max(1));
            let height = ((650. * scale) as i32).min((work.bottom - work.top - 2 * margin).max(1));
            let x = (cursor.x - width + (24. * scale) as i32).clamp(
                work.left + margin,
                (work.right - width - margin).max(work.left + margin),
            );
            let y = (cursor.y - height - margin).clamp(
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
            *result.lock().unwrap() = Some(problem.to_string());
            cx.quit();
            return;
        }
        placement.apply();
        cx.activate(true);
    });
    if let Some(error) = error.lock().unwrap().take() {
        return Err(io::Error::other(error).into());
    }
    Ok(())
}

#[derive(Default)]
struct PendingWork {
    scan: Option<UsagePeriod>,
    edit_rates: bool,
}
struct Worker {
    pending: Arc<(Mutex<PendingWork>, Condvar)>,
    updates: mpsc::Receiver<Update>,
}
enum Update {
    Snapshot(UsagePeriod, UsageSnapshot),
    ScanError(UsagePeriod, String),
    Rates(Result<(), String>),
}

impl Worker {
    fn start() -> io::Result<Self> {
        let pending = Arc::new((Mutex::new(PendingWork::default()), Condvar::new()));
        let work = pending.clone();
        let (send, updates) = mpsc::channel();
        thread::Builder::new()
            .name("usage-scan".into())
            .spawn(move || {
                let path = match AppConfig::path() {
                    Ok(path) => path.with_file_name("usage-rates.toml"),
                    Err(error) => {
                        let _ = send.send(Update::ScanError(
                            UsagePeriod::Today,
                            format!("Cannot locate rate settings. {error}"),
                        ));
                        return;
                    }
                };
                let mut scanner = UsageScanner::new(provider::session_sources(), path.clone());
                loop {
                    let (scan, edit) = {
                        let (lock, ready) = &*work;
                        let mut pending = lock.lock().unwrap();
                        while pending.scan.is_none() && !pending.edit_rates {
                            pending = ready.wait(pending).unwrap();
                        }
                        (pending.scan.take(), std::mem::take(&mut pending.edit_rates))
                    };
                    if edit {
                        let result = crate::usage::ensure_rates_file(&path)
                            .and_then(|()| {
                                ProcessCommand::new("notepad.exe")
                                    .arg(&path)
                                    .spawn()
                                    .map(|_| ())
                            })
                            .map_err(|error| format!("Cannot open custom rates. {error}"));
                        if send.send(Update::Rates(result)).is_err() {
                            break;
                        }
                    }
                    if let Some(period) = scan {
                        let update =
                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                match period {
                                    UsagePeriod::All => scanner.scan(),
                                    _ => scanner.scan_period(period),
                                }
                            })) {
                                Ok(snapshot) => Update::Snapshot(period, snapshot),
                                Err(_) => Update::ScanError(
                                    period,
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
    fn scan(&self, period: UsagePeriod) {
        let (lock, ready) = &*self.pending;
        lock.lock().unwrap().scan = Some(period);
        ready.notify_one();
    }
    fn edit_rates(&self) {
        let (lock, ready) = &*self.pending;
        lock.lock().unwrap().edit_rates = true;
        ready.notify_one();
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Metric {
    Tokens,
    Cost,
    Calls,
}
#[derive(Clone, Copy)]
enum Action {
    Period(UsagePeriod),
    Metric(Metric),
    Refresh,
    Close,
    Info,
    Rates,
    Pin,
}
struct Dashboard {
    focus: FocusHandle,
    _activation: Subscription,
    worker: Option<Worker>,
    period: UsagePeriod,
    metric: Metric,
    snapshot: Option<UsageSnapshot>,
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
        let (worker, error) = match Worker::start() {
            Ok(worker) => (Some(worker), None),
            Err(error) => (None, Some(format!("Cannot start usage scan. {error}"))),
        };
        let mut dashboard = Self {
            focus,
            _activation: activation,
            worker,
            period: UsagePeriod::Week,
            metric: Metric::Tokens,
            snapshot: None,
            refreshing: false,
            last_request: Instant::now(),
            error,
            rates_message: None,
            info_open: false,
            has_activated: window.is_window_active(),
            pinned: std::env::args().any(|argument| argument == "--pin"),
        };
        dashboard.refresh();
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
    fn refresh(&mut self) {
        if let Some(worker) = &self.worker {
            worker.scan(self.period);
            self.refreshing = true;
            self.error = None;
            self.last_request = Instant::now();
        }
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        if let Some(worker) = &self.worker {
            loop {
                match worker.updates.try_recv() {
                    Ok(Update::Snapshot(period, snapshot)) if period == self.period => {
                        self.snapshot = Some(snapshot);
                        self.refreshing = false;
                        self.error = None;
                        changed = true;
                    }
                    Ok(Update::ScanError(period, error)) if period == self.period => {
                        self.error = Some(error);
                        self.refreshing = false;
                        changed = true;
                    }
                    Ok(Update::Rates(result)) => {
                        self.rates_message = Some(match result {
                            Ok(()) => "Custom rates opened. Save, then refresh.".into(),
                            Err(error) => error,
                        });
                        changed = true;
                    }
                    Ok(_) => {}
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        if self.error.is_none() {
                            self.error = Some(
                                "The local usage worker is unavailable. Reopen Usage to retry."
                                    .into(),
                            );
                            self.refreshing = false;
                            changed = true;
                        }
                        break;
                    }
                }
            }
        }
        if !self.refreshing
            && self.error.is_none()
            && self.last_request.elapsed() >= Duration::from_secs(10)
        {
            self.refresh();
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
                    self.snapshot = None;
                    self.refresh();
                }
            }
            Action::Metric(metric) => self.metric = metric,
            Action::Refresh => self.refresh(),
            Action::Close => cx.quit(),
            Action::Info => self.info_open = !self.info_open,
            Action::Rates => {
                if let Some(worker) = &self.worker {
                    worker.edit_rates();
                }
            }
            Action::Pin => self.pinned = !self.pinned,
        }
        cx.notify();
    }
    fn button(
        &self,
        id: &'static str,
        label: &str,
        selected: bool,
        action: Action,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .px(px(10.))
            .py(px(6.))
            .rounded(px(6.))
            .cursor_pointer()
            .text_size(px(12.))
            .bg(rgb(if selected { 0x393346 } else { CARD }))
            .text_color(rgb(if selected { LAVENDER } else { MUTED }))
            .hover(|style| style.bg(rgb(0x393746)).text_color(rgb(TEXT)))
            .on_click(cx.listener(move |this, _, _, cx| this.act(action, cx)))
            .child(label.to_owned())
    }
    fn summary(&self) -> impl IntoElement {
        let rows = self
            .snapshot
            .as_ref()
            .map(|s| s.rows.as_slice())
            .unwrap_or_default();
        let known = self.snapshot.is_some();
        let tokens = sum_tokens(rows);
        let input = sum_field(rows, |r| r.input_tokens);
        let cached = sum_field(rows, |r| r.cached_input_tokens);
        let partial_cache = cached.1 || input.1;
        let cache = match (cached.0, input.0) {
            (Some(cached), Some(input)) if input > 0 => format!(
                "{:.0}%{}",
                cached as f64 / input as f64 * 100.,
                if partial_cache { "*" } else { "" }
            ),
            _ => "Unknown".into(),
        };
        let cost = sum_cost(rows);
        let cells = [
            (
                "TOTAL TOKENS",
                if known {
                    format_count(tokens)
                } else {
                    "...".into()
                },
                LAVENDER,
            ),
            (
                "CALLS",
                if known {
                    compact(rows.iter().map(|r| r.requests).fold(0, u64::saturating_add))
                } else {
                    "...".into()
                },
                TEXT,
            ),
            (
                "API ESTIMATE",
                if known {
                    format_cost(cost)
                } else {
                    "...".into()
                },
                GREEN,
            ),
            ("CACHE HIT", if known { cache } else { "...".into() }, TEXT),
        ];
        div()
            .flex()
            .gap(px(12.))
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(BORDER))
            .rounded(px(10.))
            .p(px(13.))
            .children(cells.into_iter().map(|(label, value, color)| {
                div()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_size(px(10.)).text_color(rgb(MUTED)).child(label))
                    .child(
                        div()
                            .mt(px(6.))
                            .text_size(px(20.))
                            .text_color(rgb(color))
                            .child(value),
                    )
            }))
    }
    fn chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (visible, truncated) = self
            .snapshot
            .as_ref()
            .map(|snapshot| chart_days(&snapshot.daily, self.period, snapshot.refreshed_at_ms))
            .unwrap_or_default();
        let values: Vec<Option<f64>> = visible
            .iter()
            .map(|day| day_value(day, self.metric))
            .collect();
        let max = values.iter().flatten().copied().fold(0_f64, f64::max);
        let total: f64 = values.iter().flatten().sum();
        let mut display_total = if values.is_empty() || values.iter().all(Option::is_none) {
            "Unknown".into()
        } else {
            match self.metric {
                Metric::Cost => money(total),
                _ => compact(total as u64),
            }
        };
        if values.iter().any(Option::is_none) && values.iter().any(Option::is_some) {
            display_total.push('*');
        }
        let labels = visible
            .first()
            .zip(visible.last())
            .map(|(first, last)| {
                format!(
                    "{}                         {}",
                    short_date(&first.date),
                    short_date(&last.date)
                )
            })
            .unwrap_or_default();
        div()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(BORDER))
            .rounded(px(10.))
            .p(px(13.))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .flex()
                            .gap(px(3.))
                            .child(self.button(
                                "tokens",
                                "Tokens",
                                self.metric == Metric::Tokens,
                                Action::Metric(Metric::Tokens),
                                cx,
                            ))
                            .child(self.button(
                                "cost",
                                "Cost",
                                self.metric == Metric::Cost,
                                Action::Metric(Metric::Cost),
                                cx,
                            ))
                            .child(self.button(
                                "calls",
                                "Calls",
                                self.metric == Metric::Calls,
                                Action::Metric(Metric::Calls),
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .text_size(px(16.))
                            .text_color(rgb(LAVENDER))
                            .child(display_total),
                    ),
            )
            .child(
                div()
                    .mt(px(15.))
                    .h(px(100.))
                    .flex()
                    .items_end()
                    .gap(px(4.))
                    .children(values.iter().map(|value| {
                        let height = value
                            .map(|value| {
                                if max > 0. {
                                    (value / max * 96.) as f32
                                } else {
                                    0.
                                }
                            })
                            .unwrap_or(1.);
                        div()
                            .flex_1()
                            .h(px(100.))
                            .flex()
                            .items_end()
                            .justify_center()
                            .child(
                                div()
                                    .w(px((340. / visible.len().max(1) as f32).min(28.)))
                                    .h(px(height))
                                    .rounded_t(px(3.))
                                    .bg(rgb(if value.is_some() { LAVENDER } else { BORDER })),
                            )
                    })),
            )
            .child(
                div()
                    .mt(px(8.))
                    .text_size(px(10.))
                    .text_color(rgb(MUTED))
                    .child(if visible.is_empty() {
                        "No timestamped usage for this period".into()
                    } else {
                        labels
                    }),
            )
            .when(truncated, |this| {
                this.child(
                    div()
                        .mt(px(4.))
                        .text_size(px(10.))
                        .text_color(rgb(MUTED))
                        .child("Latest 30 calendar days. Summary covers the full period."),
                )
            })
            .when(values.iter().any(Option::is_none), |this| {
                this.child(
                    div()
                        .mt(px(4.))
                        .text_size(px(10.))
                        .text_color(rgb(AMBER))
                        .child("Some daily values are unknown."),
                )
            })
    }
    fn models(&self) -> impl IntoElement {
        let mut rows: Vec<&UsageRow> = self
            .snapshot
            .as_ref()
            .into_iter()
            .flat_map(|s| &s.rows)
            .collect();
        rows.sort_by_key(|row| {
            std::cmp::Reverse(
                row.input_tokens
                    .unwrap_or_default()
                    .saturating_add(row.output_tokens.unwrap_or_default()),
            )
        });
        div()
            .flex()
            .flex_col()
            .gap(px(7.))
            .children(rows.into_iter().enumerate().map(|(index, row)| {
                let tokens = sum_tokens(std::slice::from_ref(row));
                let metadata = format!(
                    "{}  /  {}  /  {} {}",
                    unknown(&row.provider),
                    unknown(&row.effort),
                    compact(row.requests),
                    if row.requests == 1 { "call" } else { "calls" }
                );
                let detail = format!(
                    "{} tokens  /  {} task time",
                    format_count(tokens),
                    row.task_time_ms
                        .map(duration)
                        .unwrap_or_else(|| "Unknown".into())
                );
                div()
                    .bg(rgb(CARD))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .rounded(px(9.))
                    .px(px(12.))
                    .py(px(10.))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .gap(px(10.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(13.))
                                    .text_color(rgb(TEXT))
                                    .child(format!("{:02}  {}", index + 1, unknown(&row.model))),
                            )
                            .child(
                                div().text_size(px(13.)).text_color(rgb(GREEN)).child(
                                    row.estimated_cost_usd
                                        .map(money)
                                        .unwrap_or_else(|| "Unpriced".into()),
                                ),
                            ),
                    )
                    .child(
                        div()
                            .mt(px(4.))
                            .text_size(px(11.))
                            .text_color(rgb(MUTED))
                            .child(metadata),
                    )
                    .child(
                        div()
                            .mt(px(4.))
                            .text_size(px(11.))
                            .text_color(rgb(MUTED))
                            .child(detail),
                    )
            }))
    }
}

impl Render for Dashboard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = if let Some(error) = &self.error {
            error.clone()
        } else if self.refreshing && self.snapshot.is_none() {
            "Reading local session logs...".into()
        } else if self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.rows.is_empty())
        {
            "No recorded usage in this period.".into()
        } else if self.refreshing {
            "Refreshing local usage...".into()
        } else {
            "Local session usage".into()
        };
        let warnings = self
            .snapshot
            .as_ref()
            .map(|s| s.warnings.clone())
            .unwrap_or_default();
        let date = self
            .snapshot
            .as_ref()
            .map(|s| s.pricing_checked_at.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("Unknown");
        let updated = self
            .snapshot
            .as_ref()
            .and_then(|s| Local.timestamp_millis_opt(s.refreshed_at_ms).single())
            .map(|time| time.format("%H:%M:%S").to_string())
            .unwrap_or_else(|| "Waiting for first scan".into());
        let scanned_files = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.scanned_files)
            .unwrap_or_default();
        let task_time = self
            .snapshot
            .as_ref()
            .map(|s| format_count_time(&s.rows))
            .unwrap_or_else(|| "...".into());
        div().size_full().flex().flex_col().bg(rgb(CANVAS)).border_1().border_color(rgb(BORDER)).rounded(px(12.)).overflow_hidden()
            .text_color(rgb(TEXT)).font_family("Segoe UI").text_size(px(13.)).track_focus(&self.focus)
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, _, cx| { if event.keystroke.key == "escape" { cx.quit(); } }))
            .child(div().flex_none().px(px(18.)).pt(px(16.)).pb(px(12.))
                .child(div().flex().items_center().justify_between()
                    .child(div().flex().items_center().gap(px(9.))
                        .child(div().size(px(29.)).rounded(px(8.)).bg(rgb(LAVENDER)).text_color(rgb(CANVAS)).flex().items_center().justify_center().text_size(px(17.)).child("D"))
                        .child(div().child(div().text_size(px(15.)).child("Discodex")).child(div().text_size(px(11.)).text_color(rgb(MUTED)).child("Usage"))))
                    .child(div().flex().gap(px(5.)).child(self.button("pin", "Pin", self.pinned, Action::Pin, cx)).child(self.button("refresh", "Refresh", false, Action::Refresh, cx)).child(self.button("close", "Close", false, Action::Close, cx))))
                .child(div().mt(px(14.)).flex().gap(px(5.))
                    .child(self.button("today", "Today", self.period == UsagePeriod::Today, Action::Period(UsagePeriod::Today), cx))
                    .child(self.button("week", "7 days", self.period == UsagePeriod::Week, Action::Period(UsagePeriod::Week), cx))
                    .child(self.button("month", "30 days", self.period == UsagePeriod::Month, Action::Period(UsagePeriod::Month), cx))
                    .child(self.button("all", "All", self.period == UsagePeriod::All, Action::Period(UsagePeriod::All), cx))))
            .child(div().id("usage-scroll").flex_1().min_h_0().overflow_y_scroll().px(px(18.)).pb(px(16.))
                .child(div().flex().flex_col().gap(px(12.))
                .child(div().text_size(px(11.)).text_color(rgb(if self.error.is_some() { AMBER } else { MUTED })).child(status))
                .child(self.summary())
                .child(div().flex().justify_between().text_size(px(11.)).text_color(rgb(MUTED)).child("Recorded task time").child(task_time))
                .child(self.chart(cx))
                .child(div().flex().justify_between().items_center().child(div().text_size(px(12.)).child("Models")).child(div().text_size(px(10.)).text_color(rgb(MUTED)).child("Ranked by tokens")))
                .child(self.models())
                .when(!warnings.is_empty(), |this| this.child(div().text_size(px(11.)).text_color(rgb(AMBER)).child(format!("Incomplete or approximate data. {}", warnings[0]))))
                .child(div().border_t_1().border_color(rgb(BORDER)).pt(px(10.)).flex().flex_col().gap(px(6.))
                    .child(div().text_size(px(11.)).text_color(rgb(MUTED)).child("Base-rate API approximation. Not a subscription bill."))
                    .child(div().flex().items_center().justify_between()
                        .child(div().text_size(px(10.)).text_color(rgb(MUTED)).child(format!("Pricing checked {date}")))
                        .child(div().flex().gap(px(5.)).child(self.button("info", "Info", self.info_open, Action::Info, cx)).child(self.button("rates", "Edit rates", false, Action::Rates, cx))))
                    .when(self.info_open, |this| this.child(div().text_size(px(11.)).text_color(rgb(MUTED)).flex().flex_col().gap(px(5.))
                        .child("Official provider standard API rates. Exact custom model rates override the catalog. Unknown models remain unpriced.")
                        .child("Sources: developers.openai.com/api/docs/pricing and platform.claude.com/docs/en/about-claude/pricing")
                        .child("Calls count recorded usage observations. Cached input is part of input. Reasoning is part of output. Task times can overlap. An asterisk marks a partial total.")
                        .children(warnings.into_iter().map(|warning| div().text_color(rgb(AMBER)).child(warning)))))
                    .when_some(self.rates_message.clone(), |this, message| this.child(div().text_size(px(11.)).text_color(rgb(AMBER)).child(message)))
                    .child(div().text_size(px(10.)).text_color(rgb(MUTED)).child(format!("Updated {updated}  /  {scanned_files} files  /  Refreshes every 10s"))))
            ))
    }
}

fn unknown(value: &str) -> String {
    if value.trim().is_empty() {
        "Unknown".into()
    } else {
        value.into()
    }
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
fn format_count_time(rows: &[UsageRow]) -> String {
    let value = sum_field(rows, |row| row.task_time_ms);
    value
        .0
        .map(|ms| format!("{}{}", duration(ms), if value.1 { "*" } else { "" }))
        .unwrap_or_else(|| "Unknown".into())
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
    let parsed: std::collections::BTreeMap<_, _> = daily
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

fn short_date(date: &str) -> String {
    date.get(5..).unwrap_or(date).into()
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
}
