use std::collections::BTreeSet;

use gpui::{
    Context, FontWeight, IntoElement, ObjectFit, div, img, prelude::*, px, relative, rgb, rgba,
};

use super::{
    Action, Dashboard,
    format::{
        ModelSort, compact, duration, format_relative_time, money, row_tokens, share_ratio,
        sort_model_rows, sum_tokens, unknown,
    },
    theme::{
        ACCENT_AMBER, ACCENT_CALLS, ACCENT_COST, ACCENT_LIME, ACCENT_REASONING, Assets,
        BG_ELEVATED, BG_HOVER, BG_RECESSED, BG_ROW_HOVER, BG_SURFACE, BORDER_STRONG, BORDER_SUBTLE,
        FONT_ICON, FONT_MONO, TEXT_MUTED, TEXT_PRIMARY, TEXT_SECONDARY, TRACK_BG, provider_color,
        provider_display,
    },
};
use crate::usage::UsageRow;

pub struct ModelsSectionParams<'a> {
    pub rows: &'a [UsageRow],
    pub assets: &'a Assets,
    pub show_provider_name: bool,
    pub sort: ModelSort,
    pub expanded: &'a BTreeSet<(String, String, String)>,
    pub now_ms: i64,
}

fn opt_tokens(value: Option<u64>) -> String {
    value.map(compact).unwrap_or_else(|| "—".into())
}

fn detail_cell(label: &'static str, value: String, color: u32) -> impl IntoElement {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .text_size(px(10.))
                .text_color(rgb(TEXT_MUTED))
                .truncate()
                .child(label),
        )
        .child(
            div()
                .font_family(FONT_MONO)
                .text_size(px(11.))
                .text_color(rgb(color))
                .truncate()
                .child(value),
        )
}

pub fn render_models_section(
    params: ModelsSectionParams<'_>,
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<> {
    let visible_rows = sort_model_rows(params.rows, params.sort);
    let total_visible_tokens: u64 = visible_rows.iter().map(|row| row_tokens(row)).sum();

    let sort_pill = |id: &'static str, label: &'static str, target: ModelSort| {
        let selected = params.sort == target;
        div()
            .id(id)
            .px(px(6.5))
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
                ACCENT_LIME
            } else {
                TEXT_SECONDARY
            }))
            .when(selected, |d| d.font_weight(FontWeight::MEDIUM))
            .hover(move |style| {
                let style = if selected {
                    style
                } else {
                    style.bg(rgba(BG_HOVER))
                };
                style.text_color(rgb(if selected { ACCENT_LIME } else { TEXT_PRIMARY }))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.act(Action::ModelSort(target), cx);
            }))
            .child(label)
    };

    let mut row_cards = Vec::with_capacity(visible_rows.len());
    for (index, row) in visible_rows.iter().enumerate() {
        let row_tok = row_tokens(row);
        let tokens = sum_tokens(std::slice::from_ref(*row));
        let share = share_ratio(row_tok, total_visible_tokens);
        let share_pct = (share * 100.0).round() as u32;
        let accent = provider_color(&row.provider);

        let effort = row.effort.trim();
        let show_effort = !effort.is_empty() && !effort.eq_ignore_ascii_case("unknown");

        let calls_label = if row.requests == 1 { "call" } else { "calls" };
        let cache_note = match (row.cached_input_tokens, row.input_tokens) {
            (Some(c), Some(i)) if i > 0 && c > 0 => {
                format!(" · {:.0}% cache", c as f64 / i as f64 * 100.0)
            }
            _ => String::new(),
        };
        let prefix = if params.show_provider_name {
            format!("{} · ", provider_display(&row.provider))
        } else {
            String::new()
        };
        let left_meta = format!(
            "{prefix}{} · {} {calls_label}{cache_note}",
            super::format::format_count(tokens),
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

        let icon = params.assets.provider_icon(&row.provider);
        let badge_letter = provider_display(&row.provider)
            .chars()
            .next()
            .unwrap_or('?')
            .to_uppercase()
            .to_string();

        let key = (row.provider.clone(), row.model.clone(), row.effort.clone());
        let is_expanded = params.expanded.contains(&key);
        let avg_per_call = row_tok
            .checked_div(row.requests)
            .map(compact)
            .unwrap_or_else(|| "—".into());
        let saved_label = row
            .cache_savings_usd
            .map(money)
            .unwrap_or_else(|| "—".into());
        let last_active = format_relative_time(row.last_seen_ms, params.now_ms);

        row_cards.push(
            div()
                .when(index > 0, |this| {
                    this.border_t_1().border_color(rgba(BORDER_SUBTLE))
                })
                .flex()
                .flex_col()
                .child(
                    div()
                        .id(("model-row", index))
                        .px(px(12.))
                        .py(px(9.5))
                        .cursor_pointer()
                        .hover(|style| style.bg(rgba(BG_ROW_HOVER)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.act(Action::ToggleModel(key.clone()), cx);
                        }))
                        .flex()
                        .flex_col()
                        .gap(px(5.5))
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
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .child(
                                            div()
                                                .font_family(FONT_MONO)
                                                .text_size(px(12.5))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(rgb(cost_color))
                                                .child(cost_label),
                                        )
                                        .child(
                                            div()
                                                .font_family(FONT_ICON)
                                                .text_size(px(9.5))
                                                .text_color(rgb(TEXT_MUTED))
                                                .child(if is_expanded {
                                                    "\u{E70E}"
                                                } else {
                                                    "\u{E70D}"
                                                }),
                                        ),
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
                        ),
                )
                .when(is_expanded, |card| {
                    card.child(
                        div()
                            .mx(px(11.))
                            .mb(px(10.))
                            .p(px(9.))
                            .rounded(px(8.))
                            .bg(rgb(BG_RECESSED))
                            .border_1()
                            .border_color(rgba(BORDER_SUBTLE))
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex()
                                    .gap(px(8.))
                                    .child(detail_cell(
                                        "Input",
                                        opt_tokens(row.input_tokens),
                                        TEXT_PRIMARY,
                                    ))
                                    .child(detail_cell(
                                        "Cache Read",
                                        opt_tokens(row.cached_input_tokens),
                                        ACCENT_COST,
                                    ))
                                    .child(detail_cell(
                                        "Cache Write",
                                        opt_tokens(row.cache_write_tokens),
                                        ACCENT_AMBER,
                                    )),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(px(8.))
                                    .child(detail_cell(
                                        "Output",
                                        opt_tokens(row.output_tokens),
                                        ACCENT_CALLS,
                                    ))
                                    .child(detail_cell(
                                        "Reasoning",
                                        opt_tokens(row.reasoning_tokens),
                                        ACCENT_REASONING,
                                    ))
                                    .child(detail_cell("Avg / Call", avg_per_call, TEXT_PRIMARY)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(px(8.))
                                    .child(detail_cell(
                                        "Sessions",
                                        row.sessions.to_string(),
                                        TEXT_PRIMARY,
                                    ))
                                    .child(detail_cell("Cache Saved", saved_label, ACCENT_COST))
                                    .child(detail_cell("Last Active", last_active, TEXT_SECONDARY)),
                            ),
                    )
                }),
        );
    }

    div()
        .flex()
        .flex_col()
        .gap(px(7.))
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
                    div()
                        .flex()
                        .gap(px(2.))
                        .bg(rgb(BG_RECESSED))
                        .border_1()
                        .border_color(rgba(BORDER_SUBTLE))
                        .rounded(px(7.))
                        .p(px(2.))
                        .child(sort_pill("sort-tok", "Tokens", ModelSort::Tokens))
                        .child(sort_pill("sort-cost", "Cost", ModelSort::Cost))
                        .child(sort_pill("sort-calls", "Calls", ModelSort::Calls))
                        .child(sort_pill("sort-cache", "Cache %", ModelSort::CacheHit)),
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
                .children(row_cards),
        )
}
