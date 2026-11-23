use std::time::Instant;

use chrono::{Local, TimeZone};
use gpui::{
    AnyElement, Context, FontWeight, IntoElement, ObjectFit, Window, div, img, prelude::*, px, rgb,
    rgba,
};

use super::{
    Action, Dashboard,
    chart_math::compute_nice_scale,
    charts::{ChartParams, render_chart_card},
    controls::{
        chip_button, icon_button, icon_label_button, render_empty_state, render_footer,
        render_info_drawer, render_notice_pill, segment_button,
    },
    format::{ChartStyle, LayoutMode, compute_provider_shares},
    heatmap::render_activity_heatmap,
    models::{ModelsSectionParams, render_models_section},
    providers::render_provider_share,
    series::{Heatmap84, build_chart_series, build_heatmap_84},
    summary::{render_hero_bento, render_telemetry_strip, render_token_mix},
    theme::{
        ACCENT_AMBER, ACCENT_ERR, ACCENT_ERR_BORDER, ACCENT_ERR_SOFT, ACCENT_LIME,
        ACCENT_LIME_BORDER, ACCENT_LIME_SOFT, BG_RECESSED, BG_SURFACE, BORDER_SUBTLE, TEXT_MUTED,
        TEXT_PRIMARY,
    },
};
use crate::usage::{UsageDay, UsagePeriod, UsageRow};

impl Dashboard {
    fn period_selector(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .bg(rgb(BG_RECESSED))
            .border_1()
            .border_color(rgba(BORDER_SUBTLE))
            .rounded(px(9.))
            .p(px(3.))
            .flex()
            .gap(px(2.))
            .child(segment_button(
                "today",
                "Today",
                self.period == UsagePeriod::Today,
                None,
                Action::Period(UsagePeriod::Today),
                cx,
            ))
            .child(segment_button(
                "week",
                "7 days",
                self.period == UsagePeriod::Week,
                None,
                Action::Period(UsagePeriod::Week),
                cx,
            ))
            .child(segment_button(
                "month",
                "30 days",
                self.period == UsagePeriod::Month,
                None,
                Action::Period(UsagePeriod::Month),
                cx,
            ))
            .child(segment_button(
                "all",
                "All",
                self.period == UsagePeriod::All,
                None,
                Action::Period(UsagePeriod::All),
                cx,
            ))
    }

    pub fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (dot_color, subtitle) = if self.error.is_some() {
            (ACCENT_ERR, "Scan issue")
        } else if self.refreshing && self.current_snapshot().is_none() {
            (ACCENT_AMBER, "Scanning logs...")
        } else if self.refreshing {
            (ACCENT_AMBER, "Refreshing...")
        } else {
            (ACCENT_LIME, "Usage")
        };
        let is_wide = self.layout == LayoutMode::Wide;
        let period_el = self.period_selector(cx).into_any_element();
        let period_inline = is_wide.then_some(period_el);
        let period_below = (!is_wide).then(|| self.period_selector(cx).into_any_element());

        div()
            .flex_none()
            .px(px(16.))
            .pt(px(13.))
            .pb(px(11.))
            .border_b_1()
            .border_color(rgba(BORDER_SUBTLE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(10.))
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
                    .when_some(period_inline, |row, selector| {
                        row.child(div().w(px(296.)).child(selector))
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(3.))
                            .child(icon_label_button(
                                "layout-toggle",
                                if is_wide { "\u{E73F}" } else { "\u{E740}" },
                                if is_wide { "Compact" } else { "Wide" },
                                is_wide,
                                Action::ToggleLayout,
                                cx,
                            ))
                            .child(icon_button(
                                "pin",
                                if self.pinned { "\u{E840}" } else { "\u{E718}" },
                                self.pinned,
                                Action::Pin,
                                cx,
                            ))
                            .child(icon_button(
                                "refresh",
                                "\u{E72C}",
                                false,
                                Action::Refresh,
                                cx,
                            ))
                            .child(icon_button("close", "\u{E8BB}", false, Action::Close, cx)),
                    ),
            )
            .when_some(period_below, |hdr, selector| {
                hdr.child(div().mt(px(10.)).child(selector))
            })
    }

    pub fn render_shell(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (AnyElement, AnyElement) {
        let snapshot = self.current_snapshot();
        let all_rows: Vec<UsageRow> = snapshot.map(|s| s.rows.clone()).unwrap_or_default();
        let provider_shares = compute_provider_shares(&all_rows);
        let distinct_providers: Vec<String> =
            provider_shares.iter().map(|p| p.slug.clone()).collect();

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
        let scanned_files = snapshot.map(|s| s.scanned_files).unwrap_or_default();
        let now_ms = snapshot
            .map(|s| s.refreshed_at_ms)
            .filter(|t| *t > 0)
            .unwrap_or_else(crate::now_ms);

        let (chart_points, truncated, is_hourly) = snapshot
            .map(|snap| {
                build_chart_series(
                    snap,
                    self.period,
                    self.provider_filter.as_deref(),
                    &distinct_providers,
                )
            })
            .unwrap_or_default();
        let has_snapshot = snapshot.is_some();
        self.last_chart_len = chart_points.len();
        if self
            .hovered_chart_idx
            .is_some_and(|idx| idx >= chart_points.len())
        {
            self.hovered_chart_idx = None;
        }
        if self
            .pinned_baseline_idx
            .is_some_and(|idx| idx >= chart_points.len())
        {
            self.pinned_baseline_idx = None;
        }
        if self
            .selected_range
            .is_some_and(|(a, b)| a >= chart_points.len() || b >= chart_points.len())
        {
            self.selected_range = None;
            self.drag_anchor_idx = None;
        }

        let dt = self
            .last_anim_tick
            .elapsed()
            .as_secs_f32()
            .clamp(0.001, 0.05);
        self.last_anim_tick = Instant::now();
        let reveal_t = (self.reveal_start.elapsed().as_secs_f32() / 0.65).clamp(0.0, 1.0);
        let reveal_moving = reveal_t < 1.0;

        if let Some(h_idx) = self.hovered_chart_idx
            && !chart_points.is_empty()
        {
            let target_nx = (h_idx as f32 + 0.5) / chart_points.len() as f32;
            self.cursor_x_spring.set_target(target_nx);
            let max_raw = chart_points
                .iter()
                .filter_map(|p| p.value(self.metric))
                .fold(0.0_f64, f64::max);
            let max_scale = compute_nice_scale(max_raw);
            if let Some(v) = chart_points[h_idx].value(self.metric) {
                let target_ny = (v / max_scale).clamp(0.0, 1.0) as f32;
                self.cursor_y_spring.set_target(target_ny);
            } else {
                self.cursor_y_spring.reset();
            }
        } else {
            self.cursor_x_spring.reset();
            self.cursor_y_spring.reset();
        }
        let x_moving = self.cursor_x_spring.step(dt, 30.0, 0.96);
        let y_moving = self.cursor_y_spring.step(dt, 28.0, 0.96);
        if reveal_moving || x_moving || y_moving {
            window.request_animation_frame();
        }
        let cursor_norm_x = self
            .cursor_x_spring
            .initialized
            .then_some(self.cursor_x_spring.value);
        let cursor_norm_y = self
            .cursor_y_spring
            .initialized
            .then_some(self.cursor_y_spring.value);

        let heatmap: Heatmap84 = self
            .bundle
            .as_ref()
            .map(|bundle| {
                let source_daily: &[UsageDay] = match &self.provider_filter {
                    Some(filter) => bundle
                        .all
                        .daily_by_provider
                        .get(filter)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                    None => &bundle.all.daily,
                };
                build_heatmap_84(source_daily, bundle.all.refreshed_at_ms)
            })
            .unwrap_or_default();

        let weekday_hour: [[u64; 24]; 7] = self
            .bundle
            .as_ref()
            .map(|bundle| {
                let source_snap = if self.period == UsagePeriod::Today {
                    &bundle.week
                } else {
                    bundle.for_period(self.period)
                };
                match &self.provider_filter {
                    Some(filter) => source_snap
                        .weekday_hour_by_provider
                        .get(filter)
                        .copied()
                        .unwrap_or([[0; 24]; 7]),
                    None => source_snap.weekday_hour,
                }
            })
            .unwrap_or([[0; 24]; 7]);

        let is_loading = !has_snapshot && self.error.is_none();
        let is_empty = has_snapshot && all_rows.is_empty();
        let show_providers = !is_loading && !is_empty && provider_shares.len() > 1;
        let show_provider_name = show_providers && self.provider_filter.is_none();
        let is_wide = self.layout == LayoutMode::Wide;

        let hero = render_hero_bento(
            &rows,
            &daily,
            self.bundle.as_ref(),
            self.period,
            self.provider_filter.as_deref(),
        )
        .into_any_element();

        let error_banner = self
            .error
            .clone()
            .map(|err| render_error_banner(err, cx).into_any_element());
        let notice = (!warnings.is_empty())
            .then(|| render_notice_pill(&warnings, self.info_open, cx).into_any_element());
        let rates_banner = self
            .rates_message
            .clone()
            .map(|m| render_rates_banner(m).into_any_element());
        let info_drawer = self
            .info_open
            .then(|| render_info_drawer(&date, warnings).into_any_element());

        let main_content = if is_loading {
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(hero)
                .child(
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
                .children(notice)
                .children(rates_banner)
                .children(info_drawer)
                .into_any_element()
        } else if is_empty {
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(hero)
                .child(render_empty_state(self.period, cx))
                .children(notice)
                .children(rates_banner)
                .children(info_drawer)
                .into_any_element()
        } else {
            let telemetry = render_telemetry_strip(&rows).into_any_element();
            let chart = render_chart_card(
                ChartParams {
                    points: &chart_points,
                    truncated,
                    is_hourly,
                    metric: self.metric,
                    chart_style: self.chart_style,
                    stack_by: self.stack_by,
                    provider_filter: self.provider_filter.as_deref(),
                    provider_count: distinct_providers.len(),
                    hovered_idx: self.hovered_chart_idx,
                    pinned_idx: self.pinned_baseline_idx,
                    selected_range: self.selected_range,
                    cursor_norm_x,
                    cursor_norm_y,
                    reveal_t,
                    plot_bounds: self.plot_bounds.clone(),
                },
                cx,
            )
            .into_any_element();
            let heatmap_el = render_activity_heatmap(
                &heatmap,
                &weekday_hour,
                self.heatmap_tab,
                self.hovered_heatmap.as_ref(),
                cx,
            )
            .into_any_element();
            let token_mix = render_token_mix(&rows).into_any_element();
            let providers = show_providers.then(|| {
                render_provider_share(
                    &provider_shares,
                    &self.assets,
                    self.provider_filter.as_deref(),
                    cx,
                )
                .into_any_element()
            });
            let models = render_models_section(
                ModelsSectionParams {
                    rows: &rows,
                    assets: &self.assets,
                    show_provider_name,
                    sort: self.model_sort,
                    expanded: &self.expanded_models,
                    now_ms,
                },
                cx,
            )
            .into_any_element();

            if is_wide {
                div()
                    .flex()
                    .items_start()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .child(hero)
                            .child(telemetry)
                            .child(chart)
                            .child(heatmap_el),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .children(providers)
                            .child(token_mix)
                            .child(models)
                            .children(notice)
                            .children(rates_banner)
                            .children(info_drawer),
                    )
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .children(providers)
                    .child(hero)
                    .child(telemetry)
                    .child(chart)
                    .child(heatmap_el)
                    .child(token_mix)
                    .child(models)
                    .children(notice)
                    .children(rates_banner)
                    .children(info_drawer)
                    .into_any_element()
            }
        };

        let body = div()
            .id("usage-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(16.))
            .py(px(13.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .children(error_banner)
                    .child(main_content),
            )
            .into_any_element();

        let footer = render_footer(&updated, scanned_files, self.info_open, cx).into_any_element();
        (body, footer)
    }
}

fn render_error_banner(error: String, cx: &mut Context<Dashboard>) -> impl IntoElement + use<> {
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
        .child(chip_button("retry", "Retry", false, Action::Refresh, cx))
}

fn render_rates_banner(message: String) -> impl IntoElement {
    div()
        .bg(rgba(ACCENT_LIME_SOFT))
        .border_1()
        .border_color(rgba(ACCENT_LIME_BORDER))
        .rounded(px(8.))
        .px(px(10.))
        .py(px(7.))
        .text_size(px(11.))
        .text_color(rgb(ACCENT_LIME))
        .child(message)
}

pub fn toggle_chart_style(current: ChartStyle) -> ChartStyle {
    current.next()
}
