use gpui::{Context, FontWeight, div, prelude::*, px, rgb, rgba};

use super::{
    Action, Dashboard,
    chart_canvas::{PLOT_PAD_BOTTOM, PLOT_PAD_TOP, plot_local_y},
    chart_math::{RangeSummary, ValueDelta, format_signed_pct, last_known_index},
    charts::{ChartParams, PLOT_HEIGHT, Y_AXIS_WIDTH},
    format::{Metric, compact, format_metric_value, money},
    series::ChartPoint,
    theme::{
        ACCENT_AMBER, ACCENT_ERR, ACCENT_POS, BG_ELEVATED, BG_HOVER, BG_RECESSED, BG_TOOLTIP,
        BORDER_STRONG, FONT_MONO, TEXT_MUTED, TEXT_PRIMARY, TEXT_SECONDARY, with_alpha,
    },
};

/// Right-side Y-axis rail with Liveline-style gliding hover pill, live latest badge, and pin tag.
#[allow(clippy::too_many_arguments)]
pub fn render_y_axis_rail(
    metric: Metric,
    max_scale: f64,
    hovered_val: Option<f64>,
    latest_val: Option<f64>,
    pinned_val: Option<f64>,
    same_as_pin: bool,
    cursor_norm_y: Option<f32>,
    metric_color: u32,
) -> gpui::Div {
    let active_pill = hovered_val
        .map(|v| (v, true))
        .or_else(|| latest_val.map(|v| (v, false)))
        .map(|(v, is_hover)| {
            let local_y = if is_hover {
                cursor_norm_y
                    .map(|ny| {
                        PLOT_PAD_TOP
                            + (1.0 - ny.clamp(0.0, 1.0))
                                * (PLOT_HEIGHT - PLOT_PAD_TOP - PLOT_PAD_BOTTOM)
                    })
                    .unwrap_or_else(|| plot_local_y(PLOT_HEIGHT, v, max_scale))
            } else {
                plot_local_y(PLOT_HEIGHT, v, max_scale)
            };
            let pill_top = (local_y - 9.0).clamp(0.0, PLOT_HEIGHT - 18.0);
            let pill_bg = if let Some(pv) = pinned_val {
                if same_as_pin && is_hover {
                    TEXT_PRIMARY
                } else if v >= pv {
                    ACCENT_POS
                } else {
                    ACCENT_ERR
                }
            } else {
                metric_color
            };
            div()
                .absolute()
                .left_0()
                .top(px(pill_top))
                .px(px(5.))
                .h(px(17.))
                .rounded(px(4.5))
                .bg(if is_hover {
                    rgb(pill_bg)
                } else {
                    rgba(with_alpha(pill_bg, 0x26))
                })
                .when(!is_hover, |d| {
                    d.border_1().border_color(rgba(with_alpha(pill_bg, 0x66)))
                })
                .flex()
                .items_center()
                .font_family(FONT_MONO)
                .text_size(px(9.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(if is_hover { BG_RECESSED } else { pill_bg }))
                .whitespace_nowrap()
                .child(format_metric_value(metric, v))
        });

    let pin_pill = pinned_val.and_then(|pv| {
        let py = (plot_local_y(PLOT_HEIGHT, pv, max_scale) - 8.0).clamp(0.0, PLOT_HEIGHT - 16.0);
        if let Some(hv) = hovered_val.or(latest_val) {
            let hy =
                (plot_local_y(PLOT_HEIGHT, hv, max_scale) - 9.0).clamp(0.0, PLOT_HEIGHT - 18.0);
            if (hy - py).abs() < 18.0 {
                return None;
            }
        }
        Some(
            div()
                .absolute()
                .left_0()
                .top(px(py))
                .px(px(4.5))
                .h(px(16.))
                .rounded(px(4.))
                .bg(rgb(BG_ELEVATED))
                .border_1()
                .border_color(rgba(BORDER_STRONG))
                .flex()
                .items_center()
                .font_family(FONT_MONO)
                .text_size(px(9.))
                .text_color(rgb(TEXT_PRIMARY))
                .whitespace_nowrap()
                .child(format_metric_value(metric, pv)),
        )
    });

    // Exact pixel alignment with the plot's horizontal gridlines (100%, 50%, 0%)
    let ticks: [(f64, String, u32); 3] = [
        (
            max_scale,
            format_metric_value(metric, max_scale),
            TEXT_SECONDARY,
        ),
        (
            max_scale * 0.5,
            format_metric_value(metric, max_scale * 0.5),
            TEXT_SECONDARY,
        ),
        (0.0, "0".into(), TEXT_MUTED),
    ];

    div()
        .w(px(Y_AXIS_WIDTH))
        .h_full()
        .flex_none()
        .relative()
        .children(ticks.into_iter().map(|(val, label, col)| {
            let tick_top =
                (plot_local_y(PLOT_HEIGHT, val, max_scale) - 6.5).clamp(0.0, PLOT_HEIGHT - 13.0);
            div()
                .absolute()
                .left(px(3.))
                .top(px(tick_top))
                .font_family(FONT_MONO)
                .text_size(px(10.))
                .text_color(rgb(col))
                .whitespace_nowrap()
                .child(label)
        }))
        .children(pin_pill)
        .children(active_pill)
}

/// Editorial Detent-style hero readout positioned prominently above the plot.
#[allow(clippy::too_many_arguments)]
pub fn render_editorial_header(
    points: &[ChartPoint],
    values: &[Option<f64>],
    hovered_idx: Option<usize>,
    pinned_idx: Option<usize>,
    selected_range: Option<(usize, usize)>,
    params: &ChartParams<'_>,
    total: f64,
    max_raw: f64,
    avg_val: Option<f64>,
    avg_partial: bool,
    has_unknown: bool,
    metric_color: u32,
    cx: &mut Context<Dashboard>,
) -> gpui::Div {
    let mut delta_badge: Option<(String, u32)> = None;
    let mut clear_action: Option<(&'static str, Action)> = None;

    let (eyebrow, hero_num, hero_color, sub_left, sub_right) = if let Some((a, b)) = selected_range
    {
        let costs: Vec<Option<f64>> = points.iter().map(|p| p.cost).collect();
        let calls: Vec<u64> = points.iter().map(|p| p.requests).collect();
        let summary = RangeSummary::compute(values, &costs, &calls, a, b);
        clear_action = Some(("× Clear range", Action::ClearChartSelection));
        if let Some(r) = summary {
            let unit = if params.is_hourly { "h" } else { "d" };
            let eyebrow = format!(
                "SELECTED RANGE · {} → {} ({}{})",
                points[r.start].label, points[r.end].label, r.count, unit
            );
            if let Some(d) = r.delta
                && let Some(pct) = d.pct
            {
                let col = if d.is_positive() {
                    ACCENT_POS
                } else if d.is_negative() {
                    ACCENT_ERR
                } else {
                    TEXT_SECONDARY
                };
                delta_badge = Some((format_signed_pct(pct), col));
            }
            let avg_str = r
                .avg
                .map(|v| format!("Avg {}", format_metric_value(params.metric, v)))
                .unwrap_or_else(|| "Avg —".into());
            let cost_str = r.cost_usd.map(money).unwrap_or_else(|| "Unpriced".into());
            let calls_word = if r.calls == 1 { "call" } else { "calls" };
            (
                eyebrow,
                format_metric_value(params.metric, r.total),
                metric_color,
                format!("{avg_str} · {cost_str}"),
                format!("{} {calls_word}", compact(r.calls)),
            )
        } else {
            (
                "SELECTED RANGE".into(),
                "—".into(),
                TEXT_MUTED,
                "".into(),
                "".into(),
            )
        }
    } else if let Some(pin_i) = pinned_idx {
        let pin_pt = &points[pin_i];
        let pin_val = values[pin_i].unwrap_or(0.0);
        let target_i = hovered_idx
            .or_else(|| last_known_index(values))
            .unwrap_or(pin_i);
        let target_pt = &points[target_i];
        let target_val = values[target_i];
        clear_action = Some(("× Unpin", Action::PinBaseline(None)));

        let eyebrow = if target_i == pin_i {
            format!("BASELINE PINNED · {}", pin_pt.hover_title.to_uppercase())
        } else {
            format!(
                "SINCE {} → {}",
                pin_pt.label.to_uppercase(),
                target_pt.label.to_uppercase()
            )
        };
        let (hero_str, hero_col) = match target_val {
            Some(tv) if target_i != pin_i => {
                let d = ValueDelta::between(tv, pin_val);
                let col = if d.is_positive() {
                    ACCENT_POS
                } else if d.is_negative() {
                    ACCENT_ERR
                } else {
                    TEXT_SECONDARY
                };
                if let Some(pct) = d.pct {
                    delta_badge = Some((format_signed_pct(pct), col));
                }
                let sign = if d.diff > 0.0 {
                    "+"
                } else if d.diff < 0.0 {
                    "-"
                } else {
                    ""
                };
                (
                    format!("{sign}{}", format_metric_value(params.metric, d.diff.abs())),
                    col,
                )
            }
            Some(tv) => (format_metric_value(params.metric, tv), TEXT_PRIMARY),
            None => ("Unknown".into(), ACCENT_AMBER),
        };
        let sub_left = if target_i == pin_i {
            "Move across the plot to measure delta vs baseline".into()
        } else {
            format!(
                "{} → {}",
                format_metric_value(params.metric, pin_val),
                target_val
                    .map(|v| format_metric_value(params.metric, v))
                    .unwrap_or_else(|| "Unknown".into())
            )
        };
        let calls_word = if target_pt.requests == 1 {
            "call"
        } else {
            "calls"
        };
        (
            eyebrow,
            hero_str,
            hero_col,
            sub_left,
            format!("{} {calls_word}", compact(target_pt.requests)),
        )
    } else if let Some(h_idx) = hovered_idx {
        let pt = &points[h_idx];
        let val = values[h_idx];
        let hero_str = val
            .map(|v| format_metric_value(params.metric, v))
            .unwrap_or_else(|| "Unknown".into());
        if let (Some(v), Some(avg)) = (val, avg_val)
            && avg > 0.0
        {
            let d = ValueDelta::between(v, avg);
            if let Some(pct) = d.pct {
                let col = if d.is_positive() {
                    ACCENT_POS
                } else if d.is_negative() {
                    ACCENT_ERR
                } else {
                    TEXT_SECONDARY
                };
                delta_badge = Some((format!("{} vs avg", format_signed_pct(pct)), col));
            }
        }
        let cache_pct = if pt.input > 0 {
            (pt.cached_read as f64 / pt.input as f64 * 100.0).round() as u64
        } else {
            0
        };
        let cost_str = pt.cost.map(money).unwrap_or_else(|| "Unpriced".into());
        (
            pt.hover_title.to_uppercase(),
            hero_str,
            TEXT_PRIMARY,
            format!(
                "Fresh {} · Cache {} · Out {}",
                compact(pt.fresh_input),
                compact(pt.cached_read),
                compact(pt.output)
            ),
            format!(
                "{cost_str} · {} calls · {cache_pct}% cached",
                compact(pt.requests)
            ),
        )
    } else {
        let mut total_str = if values.is_empty() || values.iter().all(Option::is_none) {
            "—".into()
        } else {
            format_metric_value(params.metric, total)
        };
        if has_unknown && values.iter().any(Option::is_some) {
            total_str.push('*');
        }
        let summary_left = match avg_val {
            Some(avg) if max_raw > 0.0 => format!(
                "Peak {} · Avg {}{}",
                format_metric_value(params.metric, max_raw),
                format_metric_value(params.metric, avg),
                if avg_partial { "*" } else { "" }
            ),
            None if has_unknown => "Average unavailable".into(),
            _ => "No activity in period".into(),
        };
        let metric_title = match params.metric {
            Metric::Tokens => "TOKENS",
            Metric::Cost => "EST. COST",
            Metric::Calls => "API CALLS",
        };
        let scope = if params.is_hourly {
            format!("{metric_title} · LAST 24 HOURS")
        } else {
            format!("TOTAL {metric_title}")
        };
        (
            scope,
            total_str,
            TEXT_PRIMARY,
            summary_left,
            "Hover to inspect · Drag to compare".into(),
        )
    };

    div()
        .flex()
        .flex_col()
        .gap(px(3.))
        .min_w_0()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(6.))
                .min_w_0()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(FONT_MONO)
                        .text_size(px(9.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(TEXT_MUTED))
                        .child(eyebrow),
                )
                .when_some(clear_action, |row, (label, act)| {
                    row.child(
                        div()
                            .id("clear-chart-mode")
                            .flex_none()
                            .px(px(6.))
                            .py(px(1.5))
                            .rounded(px(4.5))
                            .cursor_pointer()
                            .bg(rgb(BG_ELEVATED))
                            .border_1()
                            .border_color(rgba(BORDER_STRONG))
                            .font_family(FONT_MONO)
                            .text_size(px(9.5))
                            .text_color(rgb(TEXT_PRIMARY))
                            .hover(|s| s.bg(rgba(BG_HOVER)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.act(act.clone(), cx);
                            }))
                            .child(label),
                    )
                }),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.))
                        .flex_none()
                        .child(
                            div()
                                .font_family(FONT_MONO)
                                .text_size(px(22.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(hero_color))
                                .whitespace_nowrap()
                                .child(hero_num),
                        )
                        .when_some(delta_badge, |row, (text, col)| {
                            row.child(
                                div()
                                    .px(px(6.))
                                    .py(px(2.))
                                    .rounded(px(5.))
                                    .bg(rgba(with_alpha(col, 0x22)))
                                    .border_1()
                                    .border_color(rgba(with_alpha(col, 0x4E)))
                                    .font_family(FONT_MONO)
                                    .text_size(px(9.5))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(col))
                                    .whitespace_nowrap()
                                    .child(text),
                            )
                        }),
                )
                .child(
                    div().flex_1().min_w_0().flex().justify_end().child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(FONT_MONO)
                            .text_size(px(9.5))
                            .text_color(rgb(TEXT_SECONDARY))
                            .child(sub_right),
                    ),
                ),
        )
        .child(
            div()
                .min_w_0()
                .font_family(FONT_MONO)
                .text_size(px(10.))
                .text_color(rgb(TEXT_MUTED))
                .truncate()
                .child(sub_left),
        )
}

/// Minimal Liveline-style inline crosshair tag pinned quietly near the top of the plot.
/// Never covers the curve or collides with the AVG marker.
pub fn render_crosshair_pill(
    pt: &ChartPoint,
    value: Option<f64>,
    metric: Metric,
    cursor_norm_x: f32,
    _metric_color: u32,
) -> gpui::Div {
    let val_str = value
        .map(|v| format_metric_value(metric, v))
        .unwrap_or_else(|| "Unknown".into());
    let place_on_left = cursor_norm_x > 0.55;
    div()
        .absolute()
        .top(px(7.))
        .when(place_on_left, |d| d.left(px(8.)))
        .when(!place_on_left, |d| d.right(px(8.)))
        .px(px(8.))
        .py(px(3.))
        .rounded(px(5.5))
        .bg(rgba((BG_TOOLTIP << 8) | 0xEE))
        .border_1()
        .border_color(rgba(BORDER_STRONG))
        .flex()
        .items_center()
        .gap(px(6.))
        .child(
            div()
                .font_family(FONT_MONO)
                .text_size(px(10.))
                .text_color(rgb(TEXT_SECONDARY))
                .whitespace_nowrap()
                .child(pt.label.clone()),
        )
        .child(
            div()
                .font_family(FONT_MONO)
                .text_size(px(10.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT_PRIMARY))
                .whitespace_nowrap()
                .child(val_str),
        )
}
