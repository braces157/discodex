use std::{cell::Cell, rc::Rc};

use chrono::{Local, Timelike};
use gpui::{
    Bounds, Context, FontWeight, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, canvas, div, prelude::*, px, rgb, rgba,
};

use super::{
    Action, Dashboard,
    chart_canvas::{
        AreaLayerData, BarColumnData, PLOT_PAD_BOTTOM, PLOT_PAD_TOP, PlotCanvasSpec,
        paint_plot_canvas, plot_local_y,
    },
    chart_math::{compute_nice_scale, last_known_index, staggered_bar_reveal},
    chart_readout::{render_crosshair_pill, render_editorial_header, render_y_axis_rail},
    format::{
        ChartStyle, Metric, StackBreakdown, compute_chart_avg, effective_stack_breakdown,
        format_metric_value,
    },
    series::ChartPoint,
    sparkline::{build_legend_items, build_x_axis_labels},
    theme::{
        ACCENT_AMBER, ACCENT_CALLS, ACCENT_COST, ACCENT_LIME, BG_ELEVATED, BG_HOVER, BG_RECESSED,
        BG_SURFACE, BORDER_STRONG, BORDER_SUBTLE, FONT_MONO, TEXT_MUTED, TEXT_PRIMARY,
        TEXT_SECONDARY, provider_color, with_alpha,
    },
};

pub const PLOT_HEIGHT: f32 = 188.0;
pub const Y_AXIS_WIDTH: f32 = 48.0;

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
    pub pinned_idx: Option<usize>,
    pub selected_range: Option<(usize, usize)>,
    pub cursor_norm_x: Option<f32>,
    pub cursor_norm_y: Option<f32>,
    pub reveal_t: f32,
    pub plot_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
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
        .px(px(5.))
        .py(px(2.5))
        .rounded(px(5.))
        .cursor_pointer()
        .text_size(px(9.5))
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

fn build_bar_columns(
    points: &[ChartPoint],
    values: &[Option<f64>],
    params: &ChartParams<'_>,
) -> Vec<BarColumnData> {
    let by_provider = effective_stack_breakdown(
        params.stack_by,
        params.provider_filter,
        params.provider_count,
    ) == StackBreakdown::ByProvider;

    points
        .iter()
        .enumerate()
        .map(|(i, pt)| {
            let segments: Vec<(u64, u32)> = if params.metric == Metric::Tokens {
                if by_provider {
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
            BarColumnData {
                value: values[i],
                segments,
            }
        })
        .collect()
}

fn build_area_layers(points: &[ChartPoint], params: &ChartParams<'_>) -> Vec<AreaLayerData> {
    if params.chart_style != ChartStyle::AreaLine
        || params.metric != Metric::Tokens
        || params.stack_by != StackBreakdown::ByProvider
        || params.provider_filter.is_some()
        || params.provider_count <= 1
    {
        return Vec::new();
    }
    let mut totals: Vec<(String, u64)> = Vec::new();
    for pt in points {
        for (slug, tok) in &pt.by_provider {
            if let Some((_, sum)) = totals.iter_mut().find(|(s, _)| s == slug) {
                *sum = sum.saturating_add(*tok);
            } else if *tok > 0 {
                totals.push((slug.clone(), *tok));
            }
        }
    }
    totals.sort_by_key(|b| std::cmp::Reverse(b.1));
    totals
        .into_iter()
        .take(4)
        .map(|(slug, _)| {
            let values = points
                .iter()
                .map(|pt| {
                    pt.value(Metric::Tokens).map(|_| {
                        pt.by_provider
                            .iter()
                            .find(|(s, _)| s == &slug)
                            .map(|(_, tok)| *tok as f64)
                            .unwrap_or(0.0)
                    })
                })
                .collect();
            AreaLayerData {
                color: provider_color(&slug),
                values,
            }
        })
        .collect()
}

fn index_from_x(bounds: Bounds<Pixels>, x: Pixels, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let w = f32::from(bounds.size.width);
    if w <= 1.0 {
        return None;
    }
    let frac = ((f32::from(x) - f32::from(bounds.origin.x)) / w).clamp(0.0, 0.9999);
    Some(((frac * count as f32).floor() as usize).min(count - 1))
}

pub fn render_chart_card(
    params: ChartParams<'_>,
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<> {
    let points = params.points;
    let point_count = points.len();
    let values: Vec<Option<f64>> = points.iter().map(|p| p.value(params.metric)).collect();
    let area_layers = build_area_layers(points, &params);
    // Keep one aggregate scale across plot curves, readouts, Y-axis rail, and cursor spring
    let max_raw = values.iter().flatten().copied().fold(0.0_f64, f64::max);
    let max_scale = compute_nice_scale(max_raw);
    let total: f64 = values.iter().flatten().sum();
    let has_unknown = values.iter().any(Option::is_none);
    let (avg_val, avg_partial) = compute_chart_avg(&values);
    let metric_color = params.metric.accent();

    let pinned_idx = params
        .pinned_idx
        .filter(|&idx| idx < point_count && values[idx].is_some());
    let hovered_idx = params.hovered_idx.filter(|&idx| idx < point_count);
    let selected_range = params
        .selected_range
        .filter(|&(a, b)| a < point_count && b < point_count && a != b);
    let latest_val = last_known_index(&values).and_then(|i| values[i]);

    let editorial_header = (!points.is_empty()).then(|| {
        render_editorial_header(
            points,
            &values,
            hovered_idx,
            pinned_idx,
            selected_range,
            &params,
            total,
            max_raw,
            avg_val,
            avg_partial,
            has_unknown,
            metric_color,
            cx,
        )
    });

    let show_provider_stack = params.metric == Metric::Tokens
        && (params.chart_style == ChartStyle::StackedBar
            || params.chart_style == ChartStyle::AreaLine)
        && params.provider_filter.is_none()
        && params.provider_count > 1;
    let controls_row = render_chart_controls(&params, show_provider_stack, cx);

    let x_labels = build_x_axis_labels(points, params.is_hourly);
    let legend_items = build_legend_items(
        points,
        params.metric,
        params.chart_style,
        params.stack_by,
        params.provider_filter,
        params.provider_count,
    );

    let live_bucket_idx = if params.is_hourly {
        let cur_hour = Local::now().hour() as usize;
        (cur_hour < point_count).then_some(cur_hour)
    } else if !params.truncated {
        point_count.checked_sub(1)
    } else {
        None
    };

    let bars = build_bar_columns(points, &values, &params);
    let canvas_spec = PlotCanvasSpec {
        values: values.clone(),
        bars,
        area_layers,
        max_scale,
        avg_val,
        metric_color,
        style: params.chart_style,
        hovered_idx,
        pinned_idx,
        selected_range,
        cursor_norm_x: params.cursor_norm_x,
        cursor_norm_y: params.cursor_norm_y,
        reveal_t: params.reveal_t,
        live_bucket_idx,
    };

    let active_bar_local_y = if params.chart_style == ChartStyle::StackedBar && max_scale > 0.0 {
        let active_idx = hovered_idx.or_else(|| last_known_index(&values));
        active_idx.and_then(|idx| {
            let v = values.get(idx).copied().flatten()?;
            if v > 0.0 {
                let usable_h = (PLOT_HEIGHT - PLOT_PAD_TOP - PLOT_PAD_BOTTOM).max(1.0);
                let wave = staggered_bar_reveal(params.reveal_t, idx, point_count).max(0.02);
                let bar_h = (((v * f64::from(wave)) / max_scale) * usable_h as f64)
                    .clamp(3.0, usable_h as f64) as f32;
                Some((PLOT_HEIGHT - PLOT_PAD_BOTTOM).round() - bar_h)
            } else {
                None
            }
        })
    } else {
        None
    };

    let y_axis_rail = render_y_axis_rail(
        params.metric,
        max_scale,
        hovered_idx.and_then(|i| values[i]),
        latest_val,
        pinned_idx.and_then(|i| values[i]),
        hovered_idx == pinned_idx,
        params.cursor_norm_y,
        active_bar_local_y,
        metric_color,
    );

    let avg_tag = avg_val
        .filter(|&a| a > 0.0 && max_scale > 0.0 && pinned_idx.is_none())
        .map(|avg| {
            let y =
                (plot_local_y(PLOT_HEIGHT, avg, max_scale) - 13.0).clamp(4.0, PLOT_HEIGHT - 22.0);
            div()
                .absolute()
                .left(px(6.))
                .top(px(y))
                .px(px(5.))
                .py(px(1.))
                .rounded(px(3.5))
                .bg(rgba((BG_RECESSED << 8) | 0xE6))
                .border_1()
                .border_color(rgba(with_alpha(metric_color, 0x44)))
                .font_family(FONT_MONO)
                .text_size(px(9.))
                .text_color(rgba(with_alpha(metric_color, 0xE8)))
                .child(format!("AVG {}", format_metric_value(params.metric, avg)))
        });

    let crosshair_pill = hovered_idx
        .filter(|_| selected_range.is_none())
        .map(|h_idx| {
            let norm_x = params
                .cursor_norm_x
                .unwrap_or_else(|| (h_idx as f32 + 0.5) / point_count.max(1) as f32);
            render_crosshair_pill(
                &points[h_idx],
                values[h_idx],
                params.metric,
                norm_x,
                metric_color,
            )
        });

    let bounds_paint = params.plot_bounds.clone();
    let bounds_move = params.plot_bounds.clone();
    let bounds_down = params.plot_bounds.clone();
    let bounds_up = params.plot_bounds.clone();
    let known_mask: Vec<bool> = values.iter().map(Option::is_some).collect();
    let known_up = known_mask.clone();

    div()
        .bg(rgb(BG_SURFACE))
        .border_1()
        .border_color(rgba(BORDER_SUBTLE))
        .rounded(px(10.))
        .overflow_hidden()
        .p(px(13.))
        .flex()
        .flex_col()
        .gap(px(9.))
        .child(controls_row)
        .children(editorial_header)
        .child(if points.is_empty() {
            div()
                .h(px(96.))
                .rounded(px(9.))
                .bg(rgb(BG_RECESSED))
                .border_1()
                .border_color(rgba(BORDER_SUBTLE))
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
                        .id("chart-plot-stage")
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .relative()
                        .cursor_pointer()
                        .rounded(px(9.))
                        .bg(rgb(BG_RECESSED))
                        .border_1()
                        .border_color(rgba(BORDER_SUBTLE))
                        .overflow_hidden()
                        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                            if !*hovered
                                && this.drag_anchor_idx.is_none()
                                && this.hovered_chart_idx.is_some()
                            {
                                this.hovered_chart_idx = None;
                                cx.notify();
                            }
                        }))
                        .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                            let Some(b) = bounds_move.get() else {
                                return;
                            };
                            let Some(idx) = index_from_x(b, event.position.x, point_count) else {
                                return;
                            };
                            let mut dirty = false;
                            if this.hovered_chart_idx != Some(idx) {
                                this.hovered_chart_idx = Some(idx);
                                dirty = true;
                            }
                            if let Some(anchor) = this.drag_anchor_idx {
                                let next_range = (anchor != idx).then_some((anchor, idx));
                                if this.selected_range != next_range {
                                    this.selected_range = next_range;
                                    dirty = true;
                                }
                            }
                            if dirty {
                                cx.notify();
                            }
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                let Some(b) = bounds_down.get() else {
                                    return;
                                };
                                if let Some(idx) = index_from_x(b, event.position.x, point_count) {
                                    this.drag_anchor_idx = Some(idx);
                                    this.hovered_chart_idx = Some(idx);
                                    cx.notify();
                                }
                            }),
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseUpEvent, _, cx| {
                                let Some(anchor) = this.drag_anchor_idx.take() else {
                                    return;
                                };
                                let end_idx = bounds_up
                                    .get()
                                    .and_then(|b| index_from_x(b, event.position.x, point_count))
                                    .unwrap_or(anchor);
                                if end_idx != anchor {
                                    this.selected_range = Some((anchor, end_idx));
                                    this.pinned_baseline_idx = None;
                                } else {
                                    // Single click: clear range if active, otherwise toggle Baseline pin
                                    if this.selected_range.take().is_none()
                                        && known_up.get(anchor).copied().unwrap_or(false)
                                    {
                                        let next = if this.pinned_baseline_idx == Some(anchor) {
                                            None
                                        } else {
                                            Some(anchor)
                                        };
                                        this.pinned_baseline_idx = next;
                                    }
                                }
                                cx.notify();
                            }),
                        )
                        .on_mouse_up_out(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                if this.drag_anchor_idx.take().is_some() {
                                    if this.selected_range.is_some() {
                                        this.pinned_baseline_idx = None;
                                    }
                                    this.hovered_chart_idx = None;
                                    cx.notify();
                                }
                            }),
                        )
                        .child(
                            canvas(
                                move |bounds, _, _| {
                                    bounds_paint.set(Some(bounds));
                                    bounds
                                },
                                move |_, bounds, window, _| {
                                    paint_plot_canvas(bounds, window, &canvas_spec);
                                },
                            )
                            .size_full()
                            .absolute()
                            .inset_0(),
                        )
                        .children(avg_tag)
                        .children(crosshair_pill),
                )
                .child(y_axis_rail)
        })
        .when(!x_labels.is_empty(), |this| {
            let edge_flush = point_count > 7;
            let slot_labels: Vec<Option<String>> = (0..point_count)
                .map(|i| {
                    x_labels
                        .iter()
                        .find(|(idx, _)| *idx == i)
                        .map(|(_, lbl)| lbl.clone())
                })
                .collect();
            this.child(
                div()
                    .pl(px(1.))
                    .pr(px(Y_AXIS_WIDTH + 7.0))
                    .h(px(15.))
                    .flex()
                    .items_center()
                    .children(slot_labels.into_iter().enumerate().map(|(i, maybe_lbl)| {
                        let is_active = hovered_idx == Some(i);
                        let align_start = edge_flush && i == 0;
                        let align_end = edge_flush && i + 1 == point_count;
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .when(align_start, |d| d.justify_start())
                            .when(align_end, |d| d.justify_end())
                            .when(!align_start && !align_end, |d| d.justify_center())
                            .when_some(maybe_lbl, |slot, lbl| {
                                slot.child(
                                    div()
                                        .font_family(FONT_MONO)
                                        .text_size(px(10.))
                                        .when(is_active, |t| {
                                            t.font_weight(FontWeight::SEMIBOLD)
                                                .text_color(rgb(TEXT_PRIMARY))
                                        })
                                        .when(!is_active, |t| t.text_color(rgb(TEXT_SECONDARY)))
                                        .whitespace_nowrap()
                                        .child(lbl),
                                )
                            })
                    })),
            )
        })
        .when(
            !legend_items.is_empty() || params.is_hourly || params.truncated,
            |this| {
                let suffix = if params.is_hourly {
                    Some("24h local · live buffer hatched")
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
                                        .text_size(px(10.))
                                        .text_color(rgb(TEXT_SECONDARY))
                                        .child(div().size(px(7.)).rounded(px(2.)).bg(rgb(color)))
                                        .child(label)
                                })),
                        )
                        .when_some(suffix, |row, note| {
                            row.child(
                                div()
                                    .text_size(px(9.5))
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
                    .text_size(px(10.))
                    .text_color(rgb(ACCENT_AMBER))
                    .child("Hatched amber markers indicate incomplete daily records."),
            )
        })
}

fn render_chart_controls(
    p: &ChartParams<'_>,
    show_prov: bool,
    cx: &mut Context<Dashboard>,
) -> gpui::Div {
    use Action::{ChartStyle as Cs, Metric as M, StackBy as S};
    use ChartStyle::{AreaLine, Lollipop, StackedBar};
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
        ("style-area", "Area", AreaLine),
        ("style-bar", "Bar", StackedBar),
        ("style-dots", "Dots", Lollipop),
    ] {
        let on = p.chart_style == st;
        g2 = g2.child(chart_pill(id, lbl, on, TEXT_PRIMARY, Cs(st), cx));
    }
    let mut g3 = pill_group();
    if show_prov {
        let mix_label = if p.chart_style == AreaLine {
            "Total"
        } else {
            "Mix"
        };
        for (id, lbl, sb) in [
            ("stack-mix", mix_label, TokenMix),
            ("stack-prov", "Providers", ByProvider),
        ] {
            let on = p.stack_by == sb;
            g3 = g3.child(chart_pill(id, lbl, on, ACCENT_LIME, S(sb), cx));
        }
    }
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap(px(4.))
        .child(div().flex().items_center().gap(px(3.)).child(g1).child(g2))
        .when(show_prov, |row| row.child(g3))
}
