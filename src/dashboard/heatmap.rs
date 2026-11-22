use gpui::{Context, FontWeight, IntoElement, div, prelude::*, px, rgb, rgba};

use super::{
    Action, Dashboard,
    format::{HeatmapTab, compact, money},
    series::Heatmap84,
    theme::{
        ACCENT_COST, ACCENT_LIME, ACCENT_LIME_BORDER, ACCENT_LIME_SOFT, BG_ELEVATED, BG_RECESSED,
        BG_SURFACE, BORDER_STRONG, BORDER_SUBTLE, FONT_MONO, TEXT_MUTED, TEXT_PRIMARY,
        TEXT_SECONDARY, TRACK_BG, with_alpha,
    },
};

pub fn render_activity_heatmap(
    heatmap: &Heatmap84,
    weekday_hour: &[[u64; 24]; 7],
    tab: HeatmapTab,
    hovered_readout: Option<&str>,
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<> {
    let pill_tab = |id: &'static str, label: &'static str, selected: bool, target: HeatmapTab| {
        div()
            .id(id)
            .px(px(7.))
            .py(px(2.5))
            .rounded(px(5.))
            .cursor_pointer()
            .text_size(px(10.5))
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
    };

    let readout = hovered_readout.unwrap_or("Hover any cell for breakdown");

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
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(px(6.))
                .child(
                    div()
                        .flex()
                        .items_center()
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
                                .px(px(6.))
                                .py(px(1.5))
                                .rounded_full()
                                .bg(rgba(ACCENT_LIME_SOFT))
                                .border_1()
                                .border_color(rgba(ACCENT_LIME_BORDER))
                                .font_family(FONT_MONO)
                                .text_size(px(10.))
                                .text_color(rgb(ACCENT_LIME))
                                .child(format!(
                                    "{}d streak · {}d active",
                                    heatmap.streak_days, heatmap.active_days
                                )),
                        ),
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
                        .child(pill_tab(
                            "heat-cal-tab",
                            "12-Week",
                            tab == HeatmapTab::Calendar,
                            HeatmapTab::Calendar,
                        ))
                        .child(pill_tab(
                            "heat-hr-tab",
                            "Peak Hours",
                            tab == HeatmapTab::PeakHours,
                            HeatmapTab::PeakHours,
                        )),
                ),
        )
        .child(match tab {
            HeatmapTab::Calendar => render_calendar_grid(heatmap, cx).into_any_element(),
            HeatmapTab::PeakHours => render_peak_hours_grid(weekday_hour, cx).into_any_element(),
        })
        .child(
            div()
                .font_family(FONT_MONO)
                .text_size(px(10.5))
                .text_color(rgb(if hovered_readout.is_some() {
                    TEXT_PRIMARY
                } else {
                    TEXT_MUTED
                }))
                .truncate()
                .child(readout.to_string()),
        )
}

fn render_calendar_grid(
    heatmap: &Heatmap84,
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<> {
    let max_tok = heatmap.max_tokens.max(1);
    let total_slots = heatmap.leading_empty + heatmap.cells.len();
    let num_weeks = total_slots.div_ceil(7).max(12);
    let mut weeks = Vec::with_capacity(num_weeks);
    for week in 0..num_weeks {
        let mut col_cells = Vec::with_capacity(7);
        for day in 0..7 {
            let slot = week * 7 + day;
            let cell = slot
                .checked_sub(heatmap.leading_empty)
                .and_then(|idx| heatmap.cells.get(idx).map(|c| (idx, c)));
            let Some((idx, cell)) = cell else {
                col_cells.push(div().h(px(11.)).w_full().into_any_element());
                continue;
            };
            let ratio = cell.tokens as f64 / max_tok as f64;
            let color = if cell.requests == 0 {
                rgba(TRACK_BG)
            } else if ratio > 0.72 {
                rgb(ACCENT_LIME)
            } else if ratio > 0.45 {
                rgba(with_alpha(ACCENT_LIME, 0xAD))
            } else if ratio > 0.20 {
                rgba(with_alpha(ACCENT_LIME, 0x66))
            } else {
                rgba(with_alpha(ACCENT_LIME, 0x33))
            };
            let cost_str = cell.cost.map(money).unwrap_or_else(|| "Unpriced".into());
            let calls_word = if cell.requests == 1 { "call" } else { "calls" };
            let info = format!(
                "{}, {} · {} tok · {cost_str} · {} {calls_word}",
                cell.day_name,
                cell.pretty,
                compact(cell.tokens),
                compact(cell.requests)
            );
            col_cells.push(
                div()
                    .id(("heat-cal", idx))
                    .h(px(11.))
                    .w_full()
                    .rounded(px(2.5))
                    .border_1()
                    .border_color(rgba(0xFFFFFF0A))
                    .bg(color)
                    .cursor_pointer()
                    .on_hover(cx.listener(move |this, hovered, _, cx| {
                        if *hovered {
                            this.hovered_heatmap = Some(info.clone());
                            cx.notify();
                        } else if this.hovered_heatmap.as_deref() == Some(info.as_str()) {
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

    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .flex()
                .gap(px(6.))
                .child(
                    div()
                        .h(px(95.))
                        .flex()
                        .flex_col()
                        .justify_between()
                        .font_family(FONT_MONO)
                        .text_size(px(9.5))
                        .text_color(rgb(TEXT_MUTED))
                        .child("Sun")
                        .child("Wed")
                        .child("Sat"),
                )
                .child(div().flex_1().flex().gap(px(3.)).children(weeks)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .text_size(px(10.))
                .text_color(rgb(TEXT_MUTED))
                .child("12 weeks ago")
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(3.))
                        .child("Less")
                        .child(div().size(px(8.)).rounded(px(2.)).bg(rgba(TRACK_BG)))
                        .child(
                            div()
                                .size(px(8.))
                                .rounded(px(2.))
                                .bg(rgba(with_alpha(ACCENT_LIME, 0x33))),
                        )
                        .child(
                            div()
                                .size(px(8.))
                                .rounded(px(2.))
                                .bg(rgba(with_alpha(ACCENT_LIME, 0x66))),
                        )
                        .child(
                            div()
                                .size(px(8.))
                                .rounded(px(2.))
                                .bg(rgba(with_alpha(ACCENT_LIME, 0xAD))),
                        )
                        .child(div().size(px(8.)).rounded(px(2.)).bg(rgb(ACCENT_LIME)))
                        .child("More"),
                )
                .child("Today"),
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

    let mut day_rows = Vec::with_capacity(7);
    for (dow, day_name) in DAYS.iter().enumerate() {
        let mut hour_cells = Vec::with_capacity(24);
        for (hour, &val) in grid[dow].iter().enumerate() {
            let ratio = val as f64 / max_cell as f64;
            let color = if val == 0 {
                rgba(TRACK_BG)
            } else if ratio > 0.72 {
                rgb(ACCENT_COST)
            } else if ratio > 0.45 {
                rgba(with_alpha(ACCENT_COST, 0xA6))
            } else if ratio > 0.20 {
                rgba(with_alpha(ACCENT_COST, 0x61))
            } else {
                rgba(with_alpha(ACCENT_COST, 0x2E))
            };
            let info = format!("{day_name} {hour:02}:00 · {} tok", compact(val));
            hour_cells.push(
                div()
                    .id(("heat-hr", dow * 24 + hour))
                    .flex_1()
                    .h(px(11.))
                    .rounded(px(2.))
                    .bg(color)
                    .cursor_pointer()
                    .on_hover(cx.listener(move |this, hovered, _, cx| {
                        if *hovered {
                            this.hovered_heatmap = Some(info.clone());
                            cx.notify();
                        } else if this.hovered_heatmap.as_deref() == Some(info.as_str()) {
                            this.hovered_heatmap = None;
                            cx.notify();
                        }
                    })),
            );
        }
        day_rows.push(
            div()
                .flex()
                .items_center()
                .gap(px(5.))
                .child(
                    div()
                        .w(px(24.))
                        .flex_none()
                        .font_family(FONT_MONO)
                        .text_size(px(9.5))
                        .text_color(rgb(TEXT_MUTED))
                        .child(*day_name),
                )
                .child(div().flex_1().flex().gap(px(2.)).children(hour_cells)),
        );
    }

    div()
        .flex()
        .flex_col()
        .gap(px(3.))
        .children(day_rows)
        .child(
            div()
                .pl(px(29.))
                .pt(px(2.))
                .flex()
                .justify_between()
                .font_family(FONT_MONO)
                .text_size(px(9.5))
                .text_color(rgb(TEXT_MUTED))
                .child("00h")
                .child("06h")
                .child("12h")
                .child("18h")
                .child("23h"),
        )
}
