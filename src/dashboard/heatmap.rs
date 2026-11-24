use gpui::{Context, FontWeight, IntoElement, div, prelude::*, px, relative, rgb, rgba};

use super::{
    Action, Dashboard,
    format::{HeatmapTab, compact, money},
    series::Heatmap84,
    theme::{
        ACCENT_COST, ACCENT_LIME, BG_ELEVATED, BG_RECESSED, BG_SURFACE, BORDER_STRONG,
        BORDER_SUBTLE, FONT_MONO, TEXT_MUTED, TEXT_PRIMARY, TEXT_SECONDARY, with_alpha,
    },
};

/// Structured hover payload for the Activity card's live telemetry readout bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeatmapHover {
    pub title: String,
    pub primary_value: String,
    pub detail: String,
    pub intensity_color: u32,
}

const CAL_RAMP: [u32; 5] = [0x161619, 0x27272A, 0x52525B, 0xA1A1AA, 0xFAFAFA];
const HR_RAMP: [u32; 5] = [0x161619, 0x17253B, 0x1E3A5F, 0x3B82F6, 0x60A5FA];

fn tier_for_ratio(active: bool, ratio: f64) -> usize {
    if !active {
        0
    } else if ratio > 0.74 {
        4
    } else if ratio > 0.46 {
        3
    } else if ratio > 0.20 {
        2
    } else {
        1
    }
}

pub fn render_activity_heatmap(
    heatmap: &Heatmap84,
    weekday_hour: &[[u64; 24]; 7],
    tab: HeatmapTab,
    hovered: Option<&HeatmapHover>,
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<> {
    let cal_tab = heatmap_tab_pill(
        "heat-cal-tab",
        "12-Week",
        tab == HeatmapTab::Calendar,
        HeatmapTab::Calendar,
        cx,
    );
    let hr_tab = heatmap_tab_pill(
        "heat-hr-tab",
        "Peak Hours",
        tab == HeatmapTab::PeakHours,
        HeatmapTab::PeakHours,
        cx,
    );

    let summary_strip = match tab {
        HeatmapTab::Calendar => render_calendar_stats(heatmap),
        HeatmapTab::PeakHours => render_peak_hour_stats(weekday_hour),
    };

    let grid_body = match tab {
        HeatmapTab::Calendar => render_calendar_grid(heatmap, cx).into_any_element(),
        HeatmapTab::PeakHours => render_peak_hours_grid(weekday_hour, cx).into_any_element(),
    };

    let readout_bar = render_readout_bar(hovered, tab);

    div()
        .bg(rgb(BG_SURFACE))
        .border_1()
        .border_color(rgba(BORDER_SUBTLE))
        .rounded(px(10.))
        .p(px(12.))
        .flex()
        .flex_col()
        .gap(px(9.))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(6.))
                .child(
                    div()
                        .text_size(px(10.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(TEXT_SECONDARY))
                        .child("ACTIVITY"),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(2.))
                        .bg(rgb(BG_RECESSED))
                        .border_1()
                        .border_color(rgba(BORDER_SUBTLE))
                        .rounded(px(6.5))
                        .p(px(2.))
                        .child(cal_tab)
                        .child(hr_tab),
                ),
        )
        .child(summary_strip)
        .child(
            div()
                .p(px(9.))
                .rounded(px(8.))
                .bg(rgb(BG_RECESSED))
                .border_1()
                .border_color(rgba(BORDER_SUBTLE))
                .child(grid_body),
        )
        .child(readout_bar)
}

fn heatmap_tab_pill(
    id: &'static str,
    label: &'static str,
    selected: bool,
    target: HeatmapTab,
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<> {
    div()
        .id(id)
        .px(px(7.))
        .py(px(2.5))
        .rounded(px(5.))
        .cursor_pointer()
        .text_size(px(10.))
        .border_1()
        .border_color(rgba(if selected { BORDER_STRONG } else { 0x00000000 }))
        .bg(if selected {
            rgba((BG_ELEVATED << 8) | 0xFF)
        } else {
            rgba(0x00000000)
        })
        .text_color(rgb(if selected {
            TEXT_PRIMARY
        } else {
            TEXT_SECONDARY
        }))
        .when(selected, |d| d.font_weight(FontWeight::MEDIUM))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.act(Action::HeatmapTab(target), cx);
        }))
        .child(label)
}

fn stat_tile(label: &'static str, value: String, sub: String, value_color: u32) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .px(px(8.))
        .py(px(6.))
        .rounded(px(7.5))
        .bg(rgb(BG_RECESSED))
        .border_1()
        .border_color(rgba(BORDER_SUBTLE))
        .flex()
        .flex_col()
        .gap(px(1.5))
        .child(
            div()
                .font_family(FONT_MONO)
                .text_size(px(8.5))
                .text_color(rgb(TEXT_MUTED))
                .truncate()
                .child(label),
        )
        .child(
            div()
                .flex()
                .items_baseline()
                .justify_between()
                .gap(px(4.))
                .child(
                    div()
                        .font_family(FONT_MONO)
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(value_color))
                        .whitespace_nowrap()
                        .child(value),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(FONT_MONO)
                        .text_size(px(9.))
                        .text_color(rgb(TEXT_MUTED))
                        .child(sub),
                ),
        )
}

fn render_calendar_stats(heatmap: &Heatmap84) -> gpui::Div {
    let mut best_streak = 0usize;
    let mut cur = 0usize;
    let mut peak_cell = None;
    for cell in &heatmap.cells {
        if cell.requests > 0 {
            cur += 1;
            best_streak = best_streak.max(cur);
            if peak_cell.is_none_or(|(_, t)| cell.tokens > t) {
                peak_cell = Some((cell.pretty.as_str(), cell.tokens));
            }
        } else {
            cur = 0;
        }
    }
    let total_days = heatmap.cells.len().max(1);
    let consistency = ((heatmap.active_days as f64 / total_days as f64) * 100.0).round() as u32;
    let (peak_val, peak_sub) = match peak_cell {
        Some((date, tok)) if tok > 0 => (compact(tok), date.to_string()),
        _ => ("—".into(), "none".into()),
    };

    div()
        .flex()
        .gap(px(6.))
        .child(stat_tile(
            "STREAK",
            format!("{}d", heatmap.streak_days),
            format!("best {best_streak}d"),
            if heatmap.streak_days > 0 {
                TEXT_PRIMARY
            } else {
                TEXT_MUTED
            },
        ))
        .child(stat_tile(
            "CONSISTENCY",
            format!("{consistency}%"),
            format!("{}/{total_days}d", heatmap.active_days),
            TEXT_PRIMARY,
        ))
        .child(stat_tile("PEAK DAY", peak_val, peak_sub, TEXT_PRIMARY))
}

fn render_peak_hour_stats(grid: &[[u64; 24]; 7]) -> gpui::Div {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let mut hour_totals = [0u64; 24];
    let mut day_totals = [0u64; 7];
    let mut day_tok = 0u64;
    let mut night_tok = 0u64;
    for (dow, row) in grid.iter().enumerate() {
        for (h, &v) in row.iter().enumerate() {
            hour_totals[h] = hour_totals[h].saturating_add(v);
            day_totals[dow] = day_totals[dow].saturating_add(v);
            if (6..18).contains(&h) {
                day_tok = day_tok.saturating_add(v);
            } else {
                night_tok = night_tok.saturating_add(v);
            }
        }
    }
    let prime_hour = hour_totals
        .iter()
        .enumerate()
        .max_by_key(|&(_, v)| *v)
        .filter(|&(_, v)| *v > 0);
    let busiest_day = day_totals
        .iter()
        .enumerate()
        .max_by_key(|&(_, v)| *v)
        .filter(|&(_, v)| *v > 0);
    let total_all = day_tok.saturating_add(night_tok);
    let (rhythm_val, rhythm_sub) = if total_all > 0 {
        let day_pct = ((day_tok as f64 / total_all as f64) * 100.0).round() as u32;
        if day_pct >= 50 {
            (format!("{day_pct}%"), "daytime".into())
        } else {
            (format!("{}%", 100 - day_pct), "night owl".into())
        }
    } else {
        ("—".into(), "quiet".into())
    };

    div()
        .flex()
        .gap(px(6.))
        .child(stat_tile(
            "PRIME HOUR",
            prime_hour
                .map(|(h, _)| format!("{h:02}:00"))
                .unwrap_or_else(|| "—".into()),
            prime_hour
                .map(|(_, v)| compact(*v))
                .unwrap_or_else(|| "none".into()),
            TEXT_PRIMARY,
        ))
        .child(stat_tile(
            "TOP DAY",
            busiest_day
                .map(|(d, _)| DAYS[d].into())
                .unwrap_or_else(|| "—".into()),
            busiest_day
                .map(|(_, v)| compact(*v))
                .unwrap_or_else(|| "none".into()),
            TEXT_PRIMARY,
        ))
        .child(stat_tile("RHYTHM", rhythm_val, rhythm_sub, TEXT_PRIMARY))
}

fn render_readout_bar(hovered: Option<&HeatmapHover>, tab: HeatmapTab) -> gpui::Div {
    let ramp = match tab {
        HeatmapTab::Calendar => CAL_RAMP,
        HeatmapTab::PeakHours => HR_RAMP,
    };

    if let Some(h) = hovered {
        return div()
            .h(px(28.))
            .px(px(9.))
            .rounded(px(7.))
            .bg(rgb(BG_RECESSED))
            .border_1()
            .border_color(rgba(BORDER_STRONG))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .min_w_0()
                    .child(
                        div()
                            .size(px(7.))
                            .rounded(px(2.))
                            .bg(rgb(h.intensity_color))
                            .flex_none(),
                    )
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .text_size(px(10.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(TEXT_PRIMARY))
                            .whitespace_nowrap()
                            .child(h.title.clone()),
                    )
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .text_size(px(10.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(h.intensity_color))
                            .whitespace_nowrap()
                            .child(h.primary_value.clone()),
                    ),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(FONT_MONO)
                    .text_size(px(9.5))
                    .text_color(rgb(TEXT_SECONDARY))
                    .child(h.detail.clone()),
            );
    }

    div()
        .h(px(28.))
        .px(px(9.))
        .rounded(px(7.))
        .bg(rgb(BG_RECESSED))
        .border_1()
        .border_color(rgba(BORDER_SUBTLE))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .child(
            div()
                .font_family(FONT_MONO)
                .text_size(px(9.5))
                .text_color(rgb(TEXT_MUTED))
                .truncate()
                .child("Hover any cell for breakdown"),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(3.))
                .font_family(FONT_MONO)
                .text_size(px(9.))
                .text_color(rgb(TEXT_MUTED))
                .child("Less")
                .children(ramp.into_iter().map(|col| {
                    div()
                        .size(px(8.5))
                        .rounded(px(2.))
                        .border_1()
                        .border_color(rgba(0xFFFFFF12))
                        .bg(rgb(col))
                }))
                .child("More"),
        )
}

fn render_calendar_grid(
    heatmap: &Heatmap84,
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<> {
    let max_tok = heatmap.max_tokens.max(1);
    let total_slots = heatmap.leading_empty + heatmap.cells.len();
    let num_weeks = total_slots.div_ceil(7).max(12);
    let last_cell_idx = heatmap.cells.len().saturating_sub(1);

    let mut week_totals = vec![0u64; num_weeks];
    let mut month_labels: Vec<String> = vec![String::new(); num_weeks];
    let mut last_month = String::new();
    let mut weeks = Vec::with_capacity(num_weeks);

    for week in 0..num_weeks {
        let mut col_cells = Vec::with_capacity(7);
        for day in 0..7 {
            let slot = week * 7 + day;
            let cell = slot
                .checked_sub(heatmap.leading_empty)
                .and_then(|idx| heatmap.cells.get(idx).map(|c| (idx, c)));
            let Some((idx, cell)) = cell else {
                col_cells.push(div().h(px(12.5)).w_full().into_any_element());
                continue;
            };
            week_totals[week] = week_totals[week].saturating_add(cell.tokens);
            if month_labels[week].is_empty()
                && let Some(mon) = cell.pretty.split_whitespace().next()
                && mon != last_month
            {
                month_labels[week] = mon.to_string();
                last_month = mon.to_string();
            }

            let ratio = cell.tokens as f64 / max_tok as f64;
            let tier = tier_for_ratio(cell.requests > 0, ratio);
            let fill_col = CAL_RAMP[tier];
            let is_today = idx == last_cell_idx;

            let cost_str = cell.cost.map(money).unwrap_or_else(|| "Unpriced".into());
            let calls_word = if cell.requests == 1 { "call" } else { "calls" };
            let pct_peak = (ratio * 100.0).round() as u32;
            let hover_data = HeatmapHover {
                title: format!("{}, {}", cell.day_name, cell.pretty),
                primary_value: if cell.requests > 0 {
                    format!("{} tok", compact(cell.tokens))
                } else {
                    "Idle".into()
                },
                detail: if cell.requests > 0 {
                    format!(
                        "{cost_str} · {} {calls_word} · {pct_peak}% peak",
                        compact(cell.requests)
                    )
                } else {
                    "No recorded activity".into()
                },
                intensity_color: if cell.requests > 0 {
                    ACCENT_LIME
                } else {
                    TEXT_MUTED
                },
            };

            let border_col = if is_today {
                rgba(0xFFFFFFCC)
            } else {
                rgba(0xFFFFFF0A)
            };

            col_cells.push(
                div()
                    .id(("heat-cal", idx))
                    .h(px(12.5))
                    .w_full()
                    .rounded(px(3.))
                    .border_1()
                    .border_color(border_col)
                    .bg(rgb(fill_col))
                    .hover(|s| s.border_color(rgba(0xFFFFFFCC)))
                    .cursor_pointer()
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered {
                            this.hovered_heatmap = Some(hover_data.clone());
                            cx.notify();
                        } else if this.hovered_heatmap.as_ref() == Some(&hover_data) {
                            this.hovered_heatmap = None;
                            cx.notify();
                        }
                    }))
                    .into_any_element(),
            );
        }
        weeks.push(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(3.))
                .children(col_cells),
        );
    }

    let max_week = week_totals.iter().copied().max().unwrap_or(0).max(1);
    let week_bars: Vec<_> = week_totals
        .iter()
        .map(|&wt| {
            let h = if wt == 0 {
                2.0
            } else {
                ((wt as f32 / max_week as f32) * 14.0).clamp(3.0, 14.0)
            };
            let alpha = if wt == 0 { 0x18 } else { 0x99 };
            div().flex_1().h(px(14.)).flex().items_end().child(
                div().w_full().h(px(h)).rounded_t(px(2.)).bg(if wt == 0 {
                    rgba(0xFFFFFF12)
                } else {
                    rgba(with_alpha(ACCENT_LIME, alpha))
                }),
            )
        })
        .collect();

    div()
        .flex()
        .flex_col()
        .gap(px(5.))
        .child(
            div()
                .pl(px(28.))
                .flex()
                .gap(px(3.))
                .children(month_labels.into_iter().map(|lbl| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(FONT_MONO)
                        .text_size(px(8.5))
                        .text_color(rgb(TEXT_MUTED))
                        .whitespace_nowrap()
                        .child(lbl)
                })),
        )
        .child(
            div()
                .flex()
                .gap(px(6.))
                .child(
                    div()
                        .w(px(22.))
                        .h(px(105.))
                        .flex_none()
                        .flex()
                        .flex_col()
                        .justify_between()
                        .font_family(FONT_MONO)
                        .text_size(px(8.5))
                        .text_color(rgb(TEXT_MUTED))
                        .child("Sun")
                        .child("Wed")
                        .child("Sat"),
                )
                .child(div().flex_1().flex().gap(px(3.)).children(weeks)),
        )
        .child(
            div()
                .pt(px(2.))
                .flex()
                .items_end()
                .gap(px(6.))
                .child(
                    div()
                        .w(px(22.))
                        .flex_none()
                        .font_family(FONT_MONO)
                        .text_size(px(8.))
                        .text_color(rgb(TEXT_MUTED))
                        .child("Wk"),
                )
                .child(div().flex_1().flex().gap(px(3.)).children(week_bars)),
        )
}

fn render_peak_hours_grid(
    grid: &[[u64; 24]; 7],
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<> {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let max_cell = grid
        .iter()
        .flat_map(|row| row.iter())
        .copied()
        .max()
        .unwrap_or(0)
        .max(1);

    let mut hour_totals = [0u64; 24];
    let mut day_totals = [0u64; 7];
    for (dow, row) in grid.iter().enumerate() {
        for (h, &v) in row.iter().enumerate() {
            hour_totals[h] = hour_totals[h].saturating_add(v);
            day_totals[dow] = day_totals[dow].saturating_add(v);
        }
    }
    let max_hour = hour_totals.iter().copied().max().unwrap_or(0).max(1);
    let max_day = day_totals.iter().copied().max().unwrap_or(0).max(1);

    let hour_hist: Vec<_> = hour_totals
        .iter()
        .map(|&v| {
            let h = if v == 0 {
                2.0
            } else {
                ((v as f32 / max_hour as f32) * 18.0).clamp(3.0, 18.0)
            };
            div().flex_1().h(px(18.)).flex().items_end().child(
                div().w_full().h(px(h)).rounded_t(px(2.)).bg(if v == 0 {
                    rgba(0xFFFFFF10)
                } else if v == max_hour {
                    rgb(ACCENT_COST)
                } else {
                    rgba(with_alpha(ACCENT_COST, 0x80))
                }),
            )
        })
        .collect();

    let mut day_rows = Vec::with_capacity(7);
    for (dow, day_name) in DAYS.iter().enumerate() {
        let mut hour_cells = Vec::with_capacity(24);
        for (hour, &val) in grid[dow].iter().enumerate() {
            let ratio = val as f64 / max_cell as f64;
            let tier = tier_for_ratio(val > 0, ratio);
            let fill_col = HR_RAMP[tier];
            let pct_peak = (ratio * 100.0).round() as u32;
            let hover_data = HeatmapHover {
                title: format!("{day_name} · {hour:02}:00–{:02}:00", (hour + 1) % 24),
                primary_value: if val > 0 {
                    format!("{} tok", compact(val))
                } else {
                    "Idle".into()
                },
                detail: if val > 0 {
                    format!("{pct_peak}% of peak hour")
                } else {
                    "No activity in this hour".into()
                },
                intensity_color: if val > 0 { ACCENT_COST } else { TEXT_MUTED },
            };
            hour_cells.push(
                div()
                    .id(("heat-hr", dow * 24 + hour))
                    .flex_1()
                    .h(px(12.))
                    .rounded(px(2.5))
                    .border_1()
                    .border_color(rgba(0xFFFFFF0A))
                    .bg(rgb(fill_col))
                    .hover(|s| s.border_color(rgba(0xFFFFFFCC)))
                    .cursor_pointer()
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered {
                            this.hovered_heatmap = Some(hover_data.clone());
                            cx.notify();
                        } else if this.hovered_heatmap.as_ref() == Some(&hover_data) {
                            this.hovered_heatmap = None;
                            cx.notify();
                        }
                    })),
            );
        }

        let day_ratio = (day_totals[dow] as f32 / max_day as f32).clamp(0.0, 1.0);
        day_rows.push(
            div()
                .flex()
                .items_center()
                .gap(px(5.))
                .child(
                    div()
                        .w(px(22.))
                        .flex_none()
                        .font_family(FONT_MONO)
                        .text_size(px(8.5))
                        .text_color(rgb(TEXT_MUTED))
                        .child(*day_name),
                )
                .child(div().flex_1().flex().gap(px(2.)).children(hour_cells))
                .child(
                    div()
                        .w(px(24.))
                        .h(px(4.5))
                        .flex_none()
                        .rounded_full()
                        .bg(rgba(0xFFFFFF0E))
                        .overflow_hidden()
                        .when(day_ratio > 0.0, |bar| {
                            bar.child(
                                div()
                                    .h_full()
                                    .w(relative(day_ratio.max(0.08)))
                                    .rounded_full()
                                    .bg(rgba(with_alpha(ACCENT_COST, 0x99))),
                            )
                        }),
                ),
        );
    }

    div()
        .flex()
        .flex_col()
        .gap(px(3.5))
        .child(
            div()
                .pl(px(27.))
                .pr(px(29.))
                .pb(px(2.))
                .flex()
                .gap(px(2.))
                .children(hour_hist),
        )
        .children(day_rows)
        .child(
            div()
                .pl(px(27.))
                .pr(px(29.))
                .pt(px(2.))
                .flex()
                .justify_between()
                .font_family(FONT_MONO)
                .text_size(px(8.5))
                .text_color(rgb(TEXT_MUTED))
                .child("00h")
                .child("06h")
                .child("12h")
                .child("18h")
                .child("23h"),
        )
}
