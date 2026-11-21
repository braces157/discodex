mod charts;
mod controls;
mod format;
mod heatmap;
mod models;
mod providers;
mod series;
mod sparkline;
mod summary;
mod theme;
mod view;
mod window;
mod worker;

use std::{
    collections::BTreeSet,
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gpui::{
    App, Application, Bounds, Context, FocusHandle, IntoElement, Render, Subscription, Timer,
    Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, point,
    prelude::*, px, rgb, rgba, size,
};

use self::{
    format::{
        ChartStyle, HeatmapTab, LayoutMode, Metric, ModelSort, StackBreakdown, sort_model_rows,
    },
    theme::{Assets, BG_CANVAS, BORDER_OUTER, FONT_UI, TEXT_PRIMARY},
    view::toggle_chart_style,
    window::{Placement, acquire_usage_mutex, focus_existing},
    worker::{Update, Worker},
};
use crate::{
    provider,
    usage::{UsageBundle, UsagePeriod, UsageRow, UsageSnapshot, usage_scope_key},
    usage_cache::{self, CacheStamp, CachedUsage, FRESH_CACHE_MAX_AGE_MS},
};

pub use window::show;

pub fn run_standalone() -> Result<(), Box<dyn std::error::Error>> {
    let Some(_guard) = acquire_usage_mutex()? else {
        return Ok(());
    };
    let initial_layout = if std::env::args().any(|arg| arg == "--wide") {
        LayoutMode::Wide
    } else {
        LayoutMode::Compact
    };
    let (logical_w, logical_h) = initial_layout.dimensions();
    let placement = Placement::near_cursor_for(logical_w, logical_h);
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
                cx.new(|cx| Dashboard::new(window, cx, initial_layout))
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
    let _ = focus_existing;
    Ok(())
}

#[derive(Clone)]
pub enum Action {
    Period(UsagePeriod),
    Metric(Metric),
    ChartStyle(ChartStyle),
    StackBy(StackBreakdown),
    HeatmapTab(HeatmapTab),
    ModelSort(ModelSort),
    ProviderFilter(Option<String>),
    ToggleModel((String, String, String)),
    ToggleLayout,
    Refresh,
    Close,
    Info,
    Rates,
    Pin,
}

pub struct Dashboard {
    focus: FocusHandle,
    _activation: Subscription,
    pub assets: Assets,
    worker: Option<Worker>,
    pub layout: LayoutMode,
    pub period: UsagePeriod,
    pub metric: Metric,
    pub chart_style: ChartStyle,
    pub stack_by: StackBreakdown,
    pub heatmap_tab: HeatmapTab,
    pub model_sort: ModelSort,
    pub bundle: Option<UsageBundle>,
    scope_key: String,
    cache_path: Option<PathBuf>,
    cache_generation: u64,
    cache_stamp: Option<CacheStamp>,
    waiting_for_tray: Option<Instant>,
    pub hovered_chart_idx: Option<usize>,
    pub hovered_heatmap: Option<String>,
    pub provider_filter: Option<String>,
    pub expanded_models: BTreeSet<(String, String, String)>,
    user_touched_model_expand: bool,
    pub refreshing: bool,
    last_request: Instant,
    pub error: Option<String>,
    pub rates_message: Option<String>,
    pub info_open: bool,
    has_activated: bool,
    pub pinned: bool,
}

impl Dashboard {
    fn new(window: &mut Window, cx: &mut Context<Self>, layout: LayoutMode) -> Self {
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
            layout,
            period: UsagePeriod::Week,
            metric: Metric::Tokens,
            chart_style: ChartStyle::StackedBar,
            stack_by: StackBreakdown::TokenMix,
            heatmap_tab: HeatmapTab::Calendar,
            model_sort: ModelSort::Tokens,
            bundle: None,
            scope_key,
            cache_path,
            cache_generation: 0,
            cache_stamp: stamp,
            waiting_for_tray: None,
            hovered_chart_idx: None,
            hovered_heatmap: None,
            provider_filter: None,
            expanded_models: BTreeSet::new(),
            user_touched_model_expand: false,
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
            if layout == LayoutMode::Wide {
                dashboard.maybe_auto_expand_wide();
            }
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

    pub fn current_snapshot(&self) -> Option<&UsageSnapshot> {
        self.bundle
            .as_ref()
            .map(|bundle| bundle.for_period(self.period))
    }

    fn maybe_auto_expand_wide(&mut self) {
        if self.user_touched_model_expand || !self.expanded_models.is_empty() {
            return;
        }
        let Some(snapshot) = self.current_snapshot() else {
            return;
        };
        let rows: Vec<UsageRow> = snapshot
            .rows
            .iter()
            .filter(|row| {
                self.provider_filter
                    .as_ref()
                    .is_none_or(|filter| &row.provider == filter)
            })
            .cloned()
            .collect();
        let sorted = sort_model_rows(&rows, self.model_sort);
        if let Some(row) = sorted.get(1).or_else(|| sorted.first()) {
            self.expanded_models.insert((
                row.provider.clone(),
                row.model.clone(),
                row.effort.clone(),
            ));
        }
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
        if self.layout == LayoutMode::Wide {
            self.maybe_auto_expand_wide();
        }
        self.hovered_chart_idx = None;
        self.hovered_heatmap = None;
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
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
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

    pub fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        match action {
            Action::Period(period) => {
                if self.period != period {
                    self.period = period;
                    self.hovered_chart_idx = None;
                    self.hovered_heatmap = None;
                    self.prune_provider_filter();
                }
            }
            Action::Metric(metric) => {
                self.metric = metric;
                self.hovered_chart_idx = None;
            }
            Action::ChartStyle(style) => {
                self.chart_style = style;
                self.hovered_chart_idx = None;
            }
            Action::StackBy(stack) => self.stack_by = stack,
            Action::HeatmapTab(tab) => {
                self.heatmap_tab = tab;
                self.hovered_heatmap = None;
            }
            Action::ModelSort(sort) => self.model_sort = sort,
            Action::ProviderFilter(provider) => {
                self.provider_filter = provider;
                self.hovered_chart_idx = None;
                self.hovered_heatmap = None;
            }
            Action::ToggleModel(key) => {
                self.user_touched_model_expand = true;
                if !self.expanded_models.remove(&key) {
                    self.expanded_models.insert(key);
                }
            }
            Action::ToggleLayout => {
                self.layout = match self.layout {
                    LayoutMode::Compact => LayoutMode::Wide,
                    LayoutMode::Wide => LayoutMode::Compact,
                };
                if self.layout == LayoutMode::Wide {
                    self.maybe_auto_expand_wide();
                }
                let (w, h) = self.layout.dimensions();
                Placement::resize_active_window(w, h);
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

    pub fn act_with_window(
        &mut self,
        action: Action,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.act(action, cx);
    }
}

impl Render for Dashboard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        let (body, footer) = self.render_shell(cx);

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG_CANVAS))
            .border_1()
            .border_color(rgba(BORDER_OUTER))
            .rounded(px(8.))
            .overflow_hidden()
            .text_color(rgb(TEXT_PRIMARY))
            .font_family(FONT_UI)
            .text_size(px(13.))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
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
                    "w" => this.act_with_window(Action::ToggleLayout, window, cx),
                    "c" => this.act(Action::ChartStyle(toggle_chart_style(this.chart_style)), cx),
                    _ => {}
                }
            }))
            .child(header)
            .child(body)
            .child(footer)
    }
}
