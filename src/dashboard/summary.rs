use gpui::{FontWeight, IntoElement, div, prelude::*, px, relative, rgb, rgba};

use super::{
    format::{
        cache_hit_ratio, compact, compute_token_mix, duration, format_cost, format_count, money,
        period_delta_pct, sum_cache_savings, sum_cost, sum_field, sum_tokens,
    },
    series::build_sparkline_series,
    sparkline::sparkline,
    theme::{
        ACCENT_AMBER, ACCENT_CALLS, ACCENT_COST, ACCENT_LIME, ACCENT_LIME_BORDER, ACCENT_LIME_SOFT,
        ACCENT_REASONING, BG_SURFACE, BORDER_SUBTLE, FONT_MONO, TEXT_MUTED, TEXT_PRIMARY,
        TEXT_SECONDARY, TRACK_BG, with_alpha,
    },
};
use crate::usage::{UsageBundle, UsageDay, UsagePeriod, UsageRow};

pub fn render_hero_bento(
    rows: &[UsageRow],
    daily: &[UsageDay],
    bundle: Option<&UsageBundle>,
    period: UsagePeriod,
    provider_filter: Option<&str>,
) -> impl IntoElement + use<> {
    let known = bundle.is_some();
    let empty = known && rows.is_empty();

    let tokens = sum_tokens(rows);
    let input = sum_field(rows, |r| r.input_tokens);
    let output = sum_field(rows, |r| r.output_tokens);
    let total_calls = rows
        .iter()
        .map(|r| r.requests)
        .fold(0u64, u64::saturating_add);
    let sessions: usize = rows.iter().map(|r| r.sessions).sum();

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
    let per_call_text = if !known {
        "Checking catalog rates".into()
    } else if empty {
        "No billable calls".into()
    } else if cost.0.is_none() {
        "No catalog rates matched".into()
    } else if let Some(total_usd) = cost.0
        && total_calls > 0
    {
        format!("{} / call", money(total_usd / total_calls as f64))
    } else if cost.1 {
        "Partial · unpriced".into()
    } else {
        "Base-rate estimate".into()
    };

    let active_days = daily.iter().filter(|d| d.requests > 0).count();
    let cost_footer = if !known || empty {
        None
    } else if let Some(total_usd) = cost.0
        && active_days > 1
    {
        Some(format!(
            "~{} / active day",
            money(total_usd / active_days as f64)
        ))
    } else if cost.1 {
        Some("Partial · some models unpriced".into())
    } else {
        Some("Base-rate approximation".into())
    };

    let delta = (!empty)
        .then(|| bundle.and_then(|b| period_delta_pct(b, period, provider_filter)))
        .flatten();
    let (spark_tok, spark_cost) = build_sparkline_series(bundle, daily, period, provider_filter);

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
                .p(px(12.))
                .flex()
                .flex_col()
                .justify_between()
                .child(kpi_header("TOKENS", ACCENT_LIME))
                .child(
                    div()
                        .mt(px(5.))
                        .font_family(FONT_MONO)
                        .text_size(px(21.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .whitespace_nowrap()
                        .truncate()
                        .text_color(rgb(if !known || empty {
                            TEXT_MUTED
                        } else {
                            TEXT_PRIMARY
                        }))
                        .child(token_value),
                )
                .child(
                    div()
                        .mt(px(5.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(6.))
                        .child(delta_or_sessions(delta, sessions))
                        .when(!empty && !spark_tok.is_empty(), |row| {
                            row.child(sparkline(spark_tok, ACCENT_LIME))
                        }),
                )
                .child(
                    div()
                        .mt(px(7.))
                        .font_family(FONT_MONO)
                        .text_size(px(10.5))
                        .text_color(rgb(TEXT_SECONDARY))
                        .truncate()
                        .child(token_sub),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .bg(rgb(BG_SURFACE))
                .border_1()
                .border_color(rgba(BORDER_SUBTLE))
                .rounded(px(11.))
                .p(px(12.))
                .flex()
                .flex_col()
                .justify_between()
                .child(kpi_header("EST. API COST", ACCENT_COST))
                .child(
                    div()
                        .mt(px(5.))
                        .font_family(FONT_MONO)
                        .text_size(px(21.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .whitespace_nowrap()
                        .truncate()
                        .text_color(rgb(cost_color))
                        .child(cost_value),
                )
                .child(
                    div()
                        .mt(px(5.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(6.))
                        .child(
                            div()
                                .min_w_0()
                                .font_family(FONT_MONO)
                                .text_size(px(10.5))
                                .text_color(rgb(TEXT_SECONDARY))
                                .whitespace_nowrap()
                                .truncate()
                                .child(per_call_text),
                        )
                        .when(!empty && !spark_cost.is_empty(), |row| {
                            row.child(sparkline(spark_cost, ACCENT_COST))
                        }),
                )
                .when_some(cost_footer, |this, footer| {
                    this.child(
                        div()
                            .mt(px(7.))
                            .font_family(FONT_MONO)
                            .text_size(px(10.5))
                            .text_color(rgb(TEXT_SECONDARY))
                            .truncate()
                            .child(footer),
                    )
                }),
        )
}

fn kpi_header(label: &'static str, dot_color: u32) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(px(10.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT_SECONDARY))
                .child(label),
        )
        .child(div().size(px(5.)).rounded_full().bg(rgb(dot_color)))
}

fn delta_or_sessions(delta: Option<(f64, &'static str)>, sessions: usize) -> impl IntoElement {
    if let Some((pct, label)) = delta {
        let positive = pct >= 0.0;
        let (bg, border, fg) = if positive {
            (ACCENT_LIME_SOFT, ACCENT_LIME_BORDER, ACCENT_LIME)
        } else {
            (
                with_alpha(ACCENT_CALLS, 0x1A),
                with_alpha(ACCENT_CALLS, 0x40),
                ACCENT_CALLS,
            )
        };
        div()
            .min_w_0()
            .px(px(5.))
            .py(px(1.5))
            .rounded(px(4.5))
            .bg(rgba(bg))
            .border_1()
            .border_color(rgba(border))
            .font_family(FONT_MONO)
            .text_size(px(10.))
            .whitespace_nowrap()
            .truncate()
            .text_color(rgb(fg))
            .child(format!(
                "{}{}% {label}",
                if positive { "+" } else { "" },
                pct.round() as i64
            ))
            .into_any_element()
    } else {
        let text = if sessions == 0 {
            "—".into()
        } else if sessions == 1 {
            "1 session".into()
        } else {
            format!("{sessions} sessions")
        };
        div()
            .min_w_0()
            .font_family(FONT_MONO)
            .text_size(px(10.5))
            .whitespace_nowrap()
            .truncate()
            .text_color(rgb(TEXT_MUTED))
            .child(text)
            .into_any_element()
    }
}

pub fn render_telemetry_strip(rows: &[UsageRow]) -> impl IntoElement + use<> {
    let calls = compact(rows.iter().map(|r| r.requests).fold(0, u64::saturating_add));
    let (cache_pct, partial_cache) = cache_hit_ratio(rows);
    let (cache_text, cache_color) = match cache_pct {
        Some(pct) => (
            format!("{pct:.0}%{}", if partial_cache { "*" } else { "" }),
            if pct >= 50.0 {
                ACCENT_LIME
            } else {
                TEXT_PRIMARY
            },
        ),
        None => ("—".into(), TEXT_MUTED),
    };

    let (saved_opt, partial_saved) = sum_cache_savings(rows);
    let (saved_text, saved_color) = match saved_opt {
        Some(usd) => (
            format!("{}{}", money(usd), if partial_saved { "*" } else { "" }),
            ACCENT_COST,
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
            .px(px(4.))
            .child(
                div()
                    .text_size(px(10.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(TEXT_MUTED))
                    .truncate()
                    .child(label),
            )
            .child(
                div()
                    .mt(px(3.))
                    .font_family(FONT_MONO)
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(color))
                    .truncate()
                    .child(value),
            )
    };
    let divider = || div().w(px(1.)).h(px(24.)).bg(rgba(BORDER_SUBTLE));

    div()
        .bg(rgb(BG_SURFACE))
        .border_1()
        .border_color(rgba(BORDER_SUBTLE))
        .rounded(px(10.))
        .px(px(8.))
        .py(px(9.))
        .flex()
        .items_center()
        .child(cell("CALLS", calls, TEXT_PRIMARY))
        .child(divider())
        .child(cell("CACHE HIT", cache_text, cache_color))
        .child(divider())
        .child(cell("CACHE SAVED", saved_text, saved_color))
        .child(divider())
        .child(cell("TASK TIME", task_time, time_color))
}

pub fn render_token_mix(rows: &[UsageRow]) -> impl IntoElement + use<> {
    let mix = compute_token_mix(rows);
    let calls = rows
        .iter()
        .map(|r| r.requests)
        .fold(0u64, u64::saturating_add);
    let avg_per_call = mix
        .total
        .0
        .and_then(|total| total.checked_div(calls))
        .map(|avg| format!("{}{}", compact(avg), if mix.total.1 { "*" } else { "" }))
        .unwrap_or_else(|| "—".into());
    let bar_items = [
        (mix.fresh_input.0, ACCENT_LIME),
        (mix.cached_read.0, ACCENT_COST),
        (mix.cache_write.0, ACCENT_AMBER),
        (mix.output.0, ACCENT_CALLS),
    ];
    let known_bar_sum: u64 = bar_items
        .iter()
        .filter_map(|(v, _)| *v)
        .fold(0u64, u64::saturating_add);
    let denom = mix.total.0.unwrap_or(0).max(known_bar_sum).max(1) as f32;
    let legend_rows = [
        ("Fresh In", mix.fresh_input, ACCENT_LIME, false),
        ("Cache Read", mix.cached_read, ACCENT_COST, false),
        ("Cache Write", mix.cache_write, ACCENT_AMBER, false),
        ("Output", mix.output, ACCENT_CALLS, false),
        ("Reasoning", mix.reasoning, ACCENT_REASONING, true),
    ];

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
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(10.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(TEXT_SECONDARY))
                        .child("TOKEN MIX"),
                )
                .child(
                    div()
                        .font_family(FONT_MONO)
                        .text_size(px(10.5))
                        .text_color(rgb(TEXT_SECONDARY))
                        .child(format!("{avg_per_call} tok / call")),
                ),
        )
        .child(
            div()
                .h(px(7.))
                .w_full()
                .rounded_full()
                .bg(rgba(TRACK_BG))
                .overflow_hidden()
                .flex()
                .gap(px(1.5))
                .children(bar_items.into_iter().filter_map(|(val, col)| {
                    let v = val.filter(|v| *v > 0)?;
                    Some(
                        div()
                            .h_full()
                            .w(relative((v as f32 / denom).clamp(0.01, 1.0)))
                            .bg(rgb(col)),
                    )
                })),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_x(px(12.))
                .gap_y(px(5.))
                .children(legend_rows.into_iter().map(|(label, val, col, of_output)| {
                    let (value_text, value_color) = match val.0 {
                        Some(v) => {
                            let star = if val.1 { "*" } else { "" };
                            let denom_opt = if of_output { mix.output.0 } else { mix.total.0 };
                            let text = if let Some(d) = denom_opt
                                && d > 0
                            {
                                let pct = ((v as f64 / d as f64) * 100.0).round() as u64;
                                let suffix = if of_output { "% out" } else { "%" };
                                format!("{}{star} ({pct}{suffix})", compact(v))
                            } else {
                                format!("{}{star}", compact(v))
                            };
                            (text, TEXT_PRIMARY)
                        }
                        None => ("—".into(), TEXT_MUTED),
                    };
                    div()
                        .w(px(176.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(6.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(5.))
                                .min_w_0()
                                .child(div().size(px(7.)).rounded_full().bg(rgb(col)))
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(rgb(TEXT_SECONDARY))
                                        .truncate()
                                        .child(label),
                                ),
                        )
                        .child(
                            div()
                                .font_family(FONT_MONO)
                                .text_size(px(11.))
                                .text_color(rgb(value_color))
                                .flex_shrink_0()
                                .child(value_text),
                        )
                })),
        )
}
