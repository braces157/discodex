use gpui::{Context, FontWeight, IntoElement, canvas, div, prelude::*, px, rgb, rgba};

use super::{
    Action, Dashboard,
    format::{ChartStyle, Metric, StackBreakdown, compact, format_metric_value, money},
    series::ChartPoint,
    sparkline::{build_legend_items, build_x_axis_labels, paint_plot_canvas},
    theme::{
        ACCENT_AMBER, ACCENT_CALLS, ACCENT_COST, ACCENT_LIME, BG_ELEVATED, BG_HOVER, BG_RECESSED,
        BG_SURFACE, BORDER_STRONG, BORDER_SUBTLE, FONT_MONO, TEXT_MUTED, TEXT_PRIMARY,
        TEXT_SECONDARY, provider_color, with_alpha,
    },
};

const PLOT_HEIGHT: f32 = 124.0;
const Y_AXIS_WIDTH: f32 = 42.0;

pub struct ChartParams<'a> {
    pub points: &'a [ChartPoint],
    pub truncated: bool,
    pub is_hourly: bool,
    pub metric: Metric,
    pub chart_style: ChartStyle,
    pub stack_by: StackBreakdown,
    pub provider_filter: Option<&'a str>,
    pub provider_count: usize,
    pub hovered_idx: Option<usize>,
}

fn chart_pill(
    id: &'static str,
    label: &'static str,
    selected: bool,
    selected_color: u32,
    action: Action,
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<> {
    div()
        .id(id)
        .px(px(6.))
        .py(px(3.))
        .rounded(px(5.5))
        .cursor_pointer()
        .text_size(px(10.5))
        .whitespace_nowrap()
        .border_1()
        .border_color(rgba(if selected { BORDER_STRONG } else { 0x00000000 }))
        .bg(if selected {
            rgba((BG_ELEVATED << 8) | 0xFF)
        } else {
            rgba(0x00000000)
        })
        .text_color(rgb(if selected {
            selected_color
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
                selected_color
            } else {
                TEXT_PRIMARY
            }))
        })
        .on_click(cx.listener(move |this, _, _, cx| this.act(action.clone(), cx)))
        .child(label)
}

fn pill_group() -> gpui::Div {
    div()
        .flex()
        .gap(px(2.))
        .bg(rgb(BG_RECESSED))
        .border_1()
        .border_color(rgba(BORDER_SUBTLE))
        .rounded(px(7.))
        .p(px(2.))
}

pub fn render_chart_card(
    params: ChartParams<'_>,
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<> {
    let points = params.points;
    let values: Vec<Option<f64>> = points.iter().map(|p| p.value(params.metric)).collect();
    let max_raw = values.iter().flatten().copied().fold(0.0_f64, f64::max);
    let max_scale = if max_raw > 0.0 { max_raw * 1.12 } else { 1.0 };
    let total: f64 = values.iter().flatten().sum();
    let has_unknown = values.iter().any(Option::is_none);
    let avg_val = if !values.is_empty() && max_raw > 0.0 {
        total / values.len() as f64
    } else {
        0.0
    };
    let metric_color = params.metric.accent();
    let dim_color = params.metric.dim();

    let mut display_total = if values.is_empty() || values.iter().all(Option::is_none) {
        "—".into()
    } else {
        format_metric_value(params.metric, total)
    };
    if has_unknown && values.iter().any(Option::is_some) {
        display_total.push('*');
    }
    let hovered_pt = params
        .hovered_idx
        .and_then(|idx| points.get(idx).map(|pt| (pt, values[idx])));
    let readout_strip = (!points.is_empty())
        .then(|| render_readout_strip(hovered_pt, &params, max_raw, avg_val, metric_color));

    let show_provider_stack = params.metric == Metric::Tokens
        && params.chart_style == ChartStyle::StackedBar
        && params.provider_filter.is_none()
        && params.provider_count > 1;
    let controls_row = render_chart_controls(&params, show_provider_stack, cx);

    let canvas_values = values.clone();
    let canvas_hovered = params.hovered_idx;
    let canvas_style = params.chart_style;
    let x_labels = build_x_axis_labels(points, params.is_hourly);
    let legend_items = build_legend_items(
        points,
        params.metric,
        params.chart_style,
        params.stack_by,
        params.provider_filter,
        params.provider_count,
    );

    let plot_cols: Vec<_> = {
        let mut cols = Vec::with_capacity(points.len());
        for (index, pt) in points.iter().enumerate() {
            cols.push(render_plot_column(
                index,
                pt,
                values[index],
                max_scale,
                metric_color,
                dim_color,
                &params,
                cx,
            ));
        }
        cols
    };

    div()
        .bg(rgb(BG_SURFACE))
        .border_1()
        .border_color(rgba(BORDER_SUBTLE))
        .rounded(px(11.))
        .p(px(12.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .gap(px(6.))
                .child(controls_row)
                .child(
                    div()
                        .font_family(FONT_MONO)
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(metric_color))
                        .flex_none()
                        .whitespace_nowrap()
                        .child(display_total),
                ),
        )
        .children(readout_strip)
        .child(if points.is_empty() {
            div()
                .h(px(64.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.5))
                .text_color(rgb(TEXT_MUTED))
                .child("No timestamped usage in this period")
        } else {
            div()
                .h(px(PLOT_HEIGHT))
                .flex()
                .gap(px(6.))
                .child(
                    div()
                        .w(px(Y_AXIS_WIDTH))
                        .h_full()
                        .flex_none()
                        .flex()
                        .flex_col()
                        .justify_between()
                        .items_end()
                        .font_family(FONT_MONO)
                        .text_size(px(10.))
                        .text_color(rgb(TEXT_MUTED))
                        .child(format_metric_value(params.metric, max_scale))
                        .child(format_metric_value(params.metric, max_scale * 0.5))
                        .child("0"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .relative()
                        .child(
                            canvas(
                                move |bounds, _, _| bounds,
                                move |_, bounds, window, _| {
                                    paint_plot_canvas(
                                        bounds,
                                        window,
                                        &canvas_values,
                                        max_scale,
                                        avg_val,
                                        metric_color,
                                        canvas_style,
                                        canvas_hovered,
                                    );
                                },
                            )
                            .size_full()
                            .absolute()
                            .inset_0(),
                        )
                        .child(
                            div()
                                .relative()
                                .size_full()
                                .flex()
                                .items_end()
                                .children(plot_cols),
                        ),
                )
        })
        .when(!x_labels.is_empty(), |this| {
            this.child(
                div()
                    .pl(px(Y_AXIS_WIDTH + 6.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .font_family(FONT_MONO)
                    .text_size(px(10.))
                    .text_color(rgb(TEXT_MUTED))
                    .children(x_labels),
            )
        })
        .when(
            !legend_items.is_empty() || params.is_hourly || params.truncated,
            |this| {
                let suffix = if params.is_hourly {
                    Some("24h local timeline")
                } else if params.truncated {
                    Some("Latest 30 days · Full range in totals")
                } else {
                    None
                };
                this.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap(px(6.))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(px(10.))
                                .children(legend_items.into_iter().map(|(label, color)| {
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(4.))
                                        .text_size(px(10.5))
                                        .text_color(rgb(TEXT_SECONDARY))
                                        .child(div().size(px(7.)).rounded(px(2.)).bg(rgb(color)))
                                        .child(label)
                                })),
                        )
                        .when_some(suffix, |row, note| {
                            row.child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(rgb(TEXT_MUTED))
                                    .child(note),
                            )
                        }),
                )
            },
        )
        .when(has_unknown, |this| {
            this.child(
                div()
                    .text_size(px(10.5))
                    .text_color(rgb(ACCENT_AMBER))
                    .child("Some daily values are incomplete."),
            )
        })
}

#[allow(clippy::too_many_arguments)]
fn render_plot_column(
    index: usize,
    pt: &ChartPoint,
    value: Option<f64>,
    max_scale: f64,
    metric_color: u32,
    dim_color: u32,
    params: &ChartParams<'_>,
    cx: &mut Context<Dashboard>,
) -> gpui::AnyElement {
    let is_hovered = params.hovered_idx == Some(index);
    let dimmed = params.hovered_idx.is_some() && !is_hovered;
    let bar_slot = div()
        .id(("chart-col", index))
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .justify_end()
        .items_center()
        .cursor_pointer()
        .when(params.chart_style == ChartStyle::StackedBar, |slot| {
            slot.px(px(1.))
        })
        .when(
            is_hovered && params.chart_style == ChartStyle::StackedBar,
            |slot| slot.bg(rgba(0xFFFFFF0A)).rounded(px(3.)),
        )
        .on_hover(cx.listener(move |this, hovered, _, cx| {
            if *hovered {
                if this.hovered_chart_idx != Some(index) {
                    this.hovered_chart_idx = Some(index);
                    cx.notify();
                }
            } else if this.hovered_chart_idx == Some(index) {
                this.hovered_chart_idx = None;
                cx.notify();
            }
        }));

    if params.chart_style == ChartStyle::AreaLine {
        return bar_slot.into_any_element();
    }

    let bar_element = match value {
        Some(v) if v > 0.0 && max_scale > 0.0 => {
            let total_h =
                ((v / max_scale) * PLOT_HEIGHT as f64).clamp(3.0, PLOT_HEIGHT as f64) as f32;
            let segments: Vec<(u64, u32)> = if params.metric == Metric::Tokens {
                if params.stack_by == StackBreakdown::ByProvider && params.provider_filter.is_none()
                {
                    pt.by_provider
                        .iter()
                        .rev()
                        .filter(|(_, tok)| *tok > 0)
                        .map(|(slug, tok)| (*tok, provider_color(slug)))
                        .collect()
                } else {
                    vec![
                        (pt.output, ACCENT_CALLS),
                        (pt.cached_read, ACCENT_COST),
                        (pt.fresh_input, ACCENT_LIME),
                    ]
                    .into_iter()
                    .filter(|(tok, _)| *tok > 0)
                    .collect()
                }
            } else {
                Vec::new()
            };

            if segments.is_empty() {
                let col = if dimmed {
                    rgba(dim_color)
                } else {
                    rgb(metric_color)
                };
                div()
                    .w_full()
                    .max_w(px(24.))
                    .h(px(total_h))
                    .rounded_t(px(3.5))
                    .bg(col)
                    .into_any_element()
            } else {
                let sum: u64 = segments.iter().map(|(tok, _)| *tok).sum::<u64>().max(1);
                let last_idx = segments.len() - 1;
                div()
                    .w_full()
                    .max_w(px(24.))
                    .h(px(total_h))
                    .rounded_t(px(3.5))
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .children(segments.into_iter().enumerate().map(|(s_idx, (tok, col))| {
                        let color = if dimmed {
                            rgba(with_alpha(col, 0x4D))
                        } else {
                            rgb(col)
                        };
                        let seg = div().w_full().bg(color);
                        if s_idx == last_idx {
                            seg.flex_1().min_h(px(1.0))
                        } else {
                            seg.h(px(((tok as f32 / sum as f32) * total_h).max(1.0)))
                        }
                    }))
                    .into_any_element()
            }
        }
        Some(_) => stub_bar(2.0, 1.0, rgba(BORDER_SUBTLE)),
        None => stub_bar(3.0, 1.5, rgb(ACCENT_AMBER)),
    };

    bar_slot.child(bar_element).into_any_element()
}

fn stub_bar(height: f32, radius: f32, bg: gpui::Rgba) -> gpui::AnyElement {
    div()
        .w_full()
        .max_w(px(24.))
        .h(px(height))
        .rounded_t(px(radius))
        .bg(bg)
        .into_any_element()
}

fn render_readout_strip(
    hovered: Option<(&ChartPoint, Option<f64>)>,
    params: &ChartParams<'_>,
    max_raw: f64,
    avg_val: f64,
    metric_color: u32,
) -> gpui::Div {
    let (r1_left, r1_right, r2_left, r2_right, active) = if let Some((pt, val)) = hovered {
        let val_str = val
            .map(|v| format_metric_value(params.metric, v))
            .unwrap_or_else(|| "Unknown".into());
        let calls_word = if pt.requests == 1 { "call" } else { "calls" };
        let cache_pct = if pt.input > 0 {
            (pt.cached_read as f64 / pt.input as f64 * 100.0).round() as u64
        } else {
            0
        };
        let cost_str = pt.cost.map(money).unwrap_or_else(|| "Unpriced".into());
        (
            pt.hover_title.clone(),
            val_str,
            format!(
                "Fresh {} · Cache {} · Out {}",
                compact(pt.fresh_input),
                compact(pt.cached_read),
                compact(pt.output)
            ),
            format!(
                "{cost_str} · {} {calls_word} · {cache_pct}% cache",
                compact(pt.requests)
            ),
            true,
        )
    } else {
        let summary_right = if max_raw > 0.0 {
            format!(
                "Peak {} · Avg {}",
                format_metric_value(params.metric, max_raw),
                format_metric_value(params.metric, avg_val)
            )
        } else {
            "No activity".into()
        };
        (
            "Period summary".into(),
            summary_right,
            "Hover chart for exact breakdown".into(),
            if params.is_hourly { "24h local" } else { "" }.into(),
            false,
        )
    };
    let r1_color = if active { TEXT_PRIMARY } else { TEXT_MUTED };
    let val_color = if active { metric_color } else { TEXT_SECONDARY };
    let r2_color = if active { TEXT_SECONDARY } else { TEXT_MUTED };
    div()
        .h(px(42.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(6.))
        .bg(rgb(BG_RECESSED))
        .border_1()
        .border_color(rgba(BORDER_SUBTLE))
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(2.))
        .overflow_hidden()
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
                        .truncate()
                        .text_size(px(10.5))
                        .when(active, |d| d.font_weight(FontWeight::MEDIUM))
                        .text_color(rgb(r1_color))
                        .child(r1_left),
                )
                .child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .font_family(FONT_MONO)
                        .text_size(px(if active { 11. } else { 10.5 }))
                        .when(active, |d| d.font_weight(FontWeight::SEMIBOLD))
                        .text_color(rgb(val_color))
                        .child(r1_right),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .font_family(FONT_MONO)
                .text_size(px(10.))
                .text_color(rgb(r2_color))
                .child(div().flex_1().min_w_0().truncate().child(r2_left))
                .child(div().flex_none().whitespace_nowrap().child(r2_right)),
        )
}

fn render_chart_controls(
    p: &ChartParams<'_>,
    show_prov: bool,
    cx: &mut Context<Dashboard>,
) -> gpui::Div {
    use Action::{ChartStyle as Cs, Metric as M, StackBy as S};
    use ChartStyle::{AreaLine, StackedBar};
    use Metric::{Calls, Cost, Tokens};
    use StackBreakdown::{ByProvider, TokenMix};
    let mut g1 = pill_group();
    for (id, lbl, m, col) in [
        ("metric-tokens", "Tokens", Tokens, ACCENT_LIME),
        ("metric-cost", "Cost", Cost, ACCENT_COST),
        ("metric-calls", "Calls", Calls, ACCENT_CALLS),
    ] {
        g1 = g1.child(chart_pill(id, lbl, p.metric == m, col, M(m), cx));
    }
    let mut g2 = pill_group();
    for (id, lbl, st) in [
        ("style-bar", "Bar", StackedBar),
        ("style-area", "Area", AreaLine),
    ] {
        let on = p.chart_style == st;
        g2 = g2.child(chart_pill(id, lbl, on, TEXT_PRIMARY, Cs(st), cx));
    }
    let mut g3 = pill_group();
    if show_prov {
        for (id, lbl, sb) in [
            ("stack-mix", "Mix", TokenMix),
            ("stack-prov", "Providers", ByProvider),
        ] {
            let on = p.stack_by == sb;
            g3 = g3.child(chart_pill(id, lbl, on, ACCENT_LIME, S(sb), cx));
        }
    }
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .min_w_0()
        .child(g1)
        .child(g2)
        .when(show_prov, |row| row.child(g3))
}
