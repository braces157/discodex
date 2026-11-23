use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};

use super::theme::{
    ACCENT_CALLS, ACCENT_COST, ACCENT_LIME, COMPACT_HEIGHT, COMPACT_WIDTH, WIDE_HEIGHT, WIDE_WIDTH,
    provider_color, provider_display,
};
use crate::usage::{UsageBundle, UsagePeriod, UsageRow, UsageSnapshot};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Metric {
    Tokens,
    Cost,
    Calls,
}

impl Metric {
    pub fn accent(self) -> u32 {
        match self {
            Self::Tokens => ACCENT_LIME,
            Self::Cost => ACCENT_COST,
            Self::Calls => ACCENT_CALLS,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayoutMode {
    Compact,
    Wide,
}

impl LayoutMode {
    pub fn dimensions(self) -> (f32, f32) {
        match self {
            Self::Compact => (COMPACT_WIDTH, COMPACT_HEIGHT),
            Self::Wide => (WIDE_WIDTH, WIDE_HEIGHT),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChartStyle {
    AreaLine,
    StackedBar,
    Lollipop,
}

impl ChartStyle {
    pub fn next(self) -> Self {
        match self {
            Self::AreaLine => Self::StackedBar,
            Self::StackedBar => Self::Lollipop,
            Self::Lollipop => Self::AreaLine,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StackBreakdown {
    TokenMix,
    ByProvider,
}

pub fn effective_stack_breakdown(
    stack_by: StackBreakdown,
    provider_filter: Option<&str>,
    provider_count: usize,
) -> StackBreakdown {
    if stack_by == StackBreakdown::ByProvider && provider_filter.is_none() && provider_count > 1 {
        StackBreakdown::ByProvider
    } else {
        StackBreakdown::TokenMix
    }
}

pub fn compute_chart_avg(values: &[Option<f64>]) -> (Option<f64>, bool) {
    let mut sum = 0.0_f64;
    let mut known = 0usize;
    let mut partial = false;
    for value in values {
        match value {
            Some(v) => {
                sum += *v;
                known += 1;
            }
            None => partial = true,
        }
    }
    if known == 0 {
        (None, partial)
    } else {
        (Some(sum / known as f64), partial)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HeatmapTab {
    Calendar,
    PeakHours,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModelSort {
    Tokens,
    Cost,
    Calls,
    CacheHit,
}

#[derive(Clone, Debug, Default)]
pub struct TokenMix {
    pub fresh_input: (Option<u64>, bool),
    pub cached_read: (Option<u64>, bool),
    pub cache_write: (Option<u64>, bool),
    pub output: (Option<u64>, bool),
    pub reasoning: (Option<u64>, bool),
    pub total: (Option<u64>, bool),
}

#[derive(Clone, Debug)]
pub struct ProviderShare {
    pub slug: String,
    pub display: String,
    pub color: u32,
    pub tokens: u64,
    pub share: f32,
}

pub fn unknown(value: &str) -> String {
    if value.trim().is_empty() {
        "Unknown".into()
    } else {
        value.into()
    }
}

pub fn share_ratio(value: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (value as f32 / total as f32).clamp(0.0, 1.0)
    }
}

pub fn row_tokens(row: &UsageRow) -> u64 {
    row.input_tokens
        .unwrap_or_default()
        .saturating_add(row.output_tokens.unwrap_or_default())
}

pub fn sort_model_rows(rows: &[UsageRow], sort: ModelSort) -> Vec<&UsageRow> {
    let mut visible: Vec<&UsageRow> = rows.iter().collect();
    visible.sort_by(|a, b| match sort {
        ModelSort::Tokens => row_tokens(b)
            .cmp(&row_tokens(a))
            .then_with(|| b.requests.cmp(&a.requests)),
        ModelSort::Cost => {
            let ca = a.estimated_cost_usd.unwrap_or(-1.0);
            let cb = b.estimated_cost_usd.unwrap_or(-1.0);
            cb.total_cmp(&ca)
                .then_with(|| row_tokens(b).cmp(&row_tokens(a)))
        }
        ModelSort::Calls => b
            .requests
            .cmp(&a.requests)
            .then_with(|| row_tokens(b).cmp(&row_tokens(a))),
        ModelSort::CacheHit => {
            let pct = |r: &UsageRow| match (r.cached_input_tokens, r.input_tokens) {
                (Some(c), Some(i)) if i > 0 => c as f64 / i as f64,
                _ => -1.0,
            };
            pct(b)
                .total_cmp(&pct(a))
                .then_with(|| row_tokens(b).cmp(&row_tokens(a)))
        }
    });
    visible
}

pub fn compute_token_mix(rows: &[UsageRow]) -> TokenMix {
    let cached_read = sum_field(rows, |r| {
        r.cached_input_tokens
            .map(|c| r.input_tokens.map_or(c, |i| c.min(i)))
    });
    let cache_write = sum_field(rows, |r| {
        r.cache_write_tokens.map(|w| {
            let max_w = match (r.input_tokens, r.cached_input_tokens) {
                (Some(i), Some(c)) => i.saturating_sub(c.min(i)),
                (Some(i), None) => i,
                _ => w,
            };
            w.min(max_w)
        })
    });
    let fresh_input = sum_field(rows, |r| {
        let input = r.input_tokens?;
        let cached = r.cached_input_tokens?.min(input);
        let written = match r.cache_write_tokens {
            Some(w) => w.min(input.saturating_sub(cached)),
            None if r.provider == "claude-code" => return None,
            None => 0,
        };
        Some(input.saturating_sub(cached).saturating_sub(written))
    });
    let output = sum_field(rows, |r| r.output_tokens);
    let reasoning = sum_field(rows, |r| {
        r.reasoning_tokens
            .map(|rs| r.output_tokens.map_or(rs, |o| rs.min(o)))
    });
    let total = sum_tokens(rows);

    TokenMix {
        fresh_input,
        cached_read,
        cache_write,
        output,
        reasoning,
        total,
    }
}

pub fn compute_provider_shares(all_rows: &[UsageRow]) -> Vec<ProviderShare> {
    let mut totals: BTreeMap<&str, u64> = BTreeMap::new();
    for row in all_rows {
        *totals.entry(row.provider.as_str()).or_default() += row_tokens(row);
    }
    let grand: u64 = totals.values().copied().fold(0, u64::saturating_add);
    let mut shares: Vec<ProviderShare> = totals
        .into_iter()
        .map(|(slug, tokens)| ProviderShare {
            slug: slug.to_owned(),
            display: provider_display(slug),
            color: provider_color(slug),
            tokens,
            share: share_ratio(tokens, grand),
        })
        .collect();
    shares.sort_by(|a, b| b.tokens.cmp(&a.tokens).then_with(|| a.slug.cmp(&b.slug)));
    shares
}

pub fn cache_hit_ratio(rows: &[UsageRow]) -> (Option<f64>, bool) {
    let mut cached_total = 0u64;
    let mut input_total = 0u64;
    let mut matched = 0usize;
    for row in rows {
        if let (Some(cached), Some(input)) = (row.cached_input_tokens, row.input_tokens) {
            cached_total = cached_total.saturating_add(cached);
            input_total = input_total.saturating_add(input);
            matched += 1;
        }
    }
    let partial = matched < rows.len();
    let ratio = if matched > 0 && input_total > 0 {
        Some(cached_total as f64 / input_total as f64 * 100.0)
    } else {
        None
    };
    (ratio, partial)
}

pub fn sum_field<T>(rows: &[T], field: impl Fn(&T) -> Option<u64>) -> (Option<u64>, bool) {
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

pub fn sum_tokens<T: std::borrow::Borrow<UsageRow>>(rows: &[T]) -> (Option<u64>, bool) {
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

pub fn sum_cost(rows: &[UsageRow]) -> (Option<f64>, bool) {
    let values: Vec<f64> = rows.iter().filter_map(|r| r.estimated_cost_usd).collect();
    (
        (!values.is_empty()).then(|| values.iter().sum()),
        values.len() < rows.len(),
    )
}

pub fn sum_cache_savings(rows: &[UsageRow]) -> (Option<f64>, bool) {
    let values: Vec<f64> = rows.iter().filter_map(|r| r.cache_savings_usd).collect();
    (
        (!values.is_empty()).then(|| values.iter().sum()),
        values.len() < rows.len(),
    )
}

pub fn period_delta_pct(
    bundle: &UsageBundle,
    period: UsagePeriod,
    provider_filter: Option<&str>,
) -> Option<(f64, &'static str)> {
    let filtered_tok = |snap: &UsageSnapshot| -> f64 {
        snap.rows
            .iter()
            .filter(|r| provider_filter.is_none_or(|p| r.provider == p))
            .map(row_tokens)
            .fold(0u64, u64::saturating_add) as f64
    };
    match period {
        UsagePeriod::Today => {
            let today = filtered_tok(&bundle.today);
            let week_avg = filtered_tok(&bundle.week) / 7.0;
            (week_avg > 0.0 && today > 0.0)
                .then_some((((today - week_avg) / week_avg) * 100.0, "vs 7d avg"))
        }
        UsagePeriod::Week => {
            let week_avg = filtered_tok(&bundle.week) / 7.0;
            let month_avg = filtered_tok(&bundle.month) / 30.0;
            (month_avg > 0.0 && week_avg > 0.0)
                .then_some((((week_avg - month_avg) / month_avg) * 100.0, "vs 30d avg"))
        }
        UsagePeriod::Month | UsagePeriod::All => None,
    }
}

pub fn format_count(value: (Option<u64>, bool)) -> String {
    value
        .0
        .map(|n| format!("{}{}", compact(n), if value.1 { "*" } else { "" }))
        .unwrap_or_else(|| "Unknown".into())
}

pub fn format_cost(value: (Option<f64>, bool)) -> String {
    value
        .0
        .map(|n| format!("{}{}", money(n), if value.1 { "*" } else { "" }))
        .unwrap_or_else(|| "Unpriced".into())
}

pub fn format_metric_value(metric: Metric, value: f64) -> String {
    match metric {
        Metric::Cost => money(value),
        _ => compact(value.round().max(0.0) as u64),
    }
}

pub fn compact(value: u64) -> String {
    if value >= 1_000_000_000 {
        format!("{:.2}B", value as f64 / 1_000_000_000.0)
    } else if value >= 1_000_000 {
        format!("{:.2}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}K", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

pub fn money(value: f64) -> String {
    if value > 0.0 && value < 0.01 {
        format!("${value:.4}")
    } else {
        format!("${value:.2}")
    }
}

pub fn duration(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds >= 3600 {
        format!("{}h {:02}m", seconds / 3600, seconds / 60 % 60)
    } else if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

pub fn format_relative_time(last_seen_ms: i64, now_ms: i64) -> String {
    if last_seen_ms <= 0 || now_ms < last_seen_ms {
        return "—".into();
    }
    let secs = (now_ms - last_seen_ms) / 1000;
    if secs < 60 {
        "Just now".into()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

pub fn pretty_date(date: &str) -> String {
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map(|d| format!("{} {}", d.format("%b"), d.day()))
        .unwrap_or_else(|_| date.get(5..).unwrap_or(date).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::theme::Assets;

    #[test]
    fn pretty_date_formats_iso_dates_and_falls_back_cleanly() {
        assert_eq!(pretty_date("2026-11-19"), "Nov 19");
        assert_eq!(pretty_date("2026-01-05"), "Jan 5");
        assert_eq!(pretty_date("bad-date"), "ate");
        assert_eq!(pretty_date("raw"), "raw");
    }

    #[test]
    fn provider_display_and_share_ratio_handle_edge_cases() {
        assert_eq!(provider_display("codex"), "Codex");
        assert_eq!(provider_display("claude-code"), "Claude Code");
        assert_eq!(provider_display("antigravity"), "Antigravity");
        assert_eq!(provider_display("deepseek-harness"), "DeepSeek");
        assert_eq!(provider_display("gemini-cli"), "Gemini CLI");
        assert_eq!(provider_display("qwen-code"), "Qwen Code");
        assert_eq!(provider_display("opencode"), "OpenCode");
        assert_eq!(provider_display("kilo-code"), "Kilo Code");
        assert_eq!(provider_display("factory-droid"), "Factory Droid");
        assert_eq!(provider_display("copilot-cli"), "Copilot CLI");
        assert_eq!(provider_display("junie"), "Junie");
        assert_eq!(provider_display("cline"), "Cline");
        assert_eq!(provider_display("roo-code"), "Roo Code");
        assert_eq!(provider_display("zed"), "Zed");
        assert_eq!(provider_display("crush"), "Crush");
        assert_eq!(provider_display("aider"), "Aider");
        assert_eq!(provider_display("  "), "Unknown");
        assert_eq!(provider_display("custom"), "custom");

        let assets = Assets::load();
        for slug in [
            "codex",
            "claude-code",
            "antigravity",
            "deepseek-harness",
            "gemini-cli",
            "qwen-code",
            "opencode",
            "kilo-code",
            "factory-droid",
            "copilot-cli",
            "junie",
            "cline",
            "roo-code",
            "zed",
            "crush",
            "aider",
        ] {
            assert!(
                assets.provider_icon(slug).is_some(),
                "missing icon for {slug}"
            );
        }

        assert_eq!(share_ratio(0, 0), 0.0);
        assert_eq!(share_ratio(50, 200), 0.25);
        assert_eq!(share_ratio(300, 200), 1.0);
    }

    #[test]
    fn cache_hit_ratio_sums_only_rows_with_both_values_present() {
        let rows = vec![
            UsageRow {
                input_tokens: Some(100),
                cached_input_tokens: None,
                ..Default::default()
            },
            UsageRow {
                input_tokens: None,
                cached_input_tokens: Some(500),
                ..Default::default()
            },
            UsageRow {
                input_tokens: Some(200),
                cached_input_tokens: Some(50),
                ..Default::default()
            },
        ];
        let (ratio, partial) = cache_hit_ratio(&rows);
        assert!(partial);
        assert!((ratio.unwrap() - 25.0).abs() < 1e-6);

        let disjoint = vec![
            UsageRow {
                input_tokens: Some(100),
                cached_input_tokens: None,
                ..Default::default()
            },
            UsageRow {
                input_tokens: None,
                cached_input_tokens: Some(100),
                ..Default::default()
            },
        ];
        let (ratio, partial) = cache_hit_ratio(&disjoint);
        assert!(partial);
        assert!(ratio.is_none());
    }

    #[test]
    fn token_mix_savings_and_model_sort_are_consistent() {
        let rows = vec![UsageRow {
            provider: "claude-code".into(),
            model: "claude-opus-4-6".into(),
            input_tokens: Some(1_000),
            cached_input_tokens: Some(700),
            cache_write_tokens: Some(100),
            output_tokens: Some(400),
            reasoning_tokens: Some(150),
            cache_savings_usd: Some(2.5),
            estimated_cost_usd: Some(0.5),
            requests: 4,
            ..Default::default()
        }];
        let mix = compute_token_mix(&rows);
        assert_eq!(mix.fresh_input, (Some(200), false));
        assert_eq!(mix.cached_read, (Some(700), false));
        assert_eq!(mix.cache_write, (Some(100), false));
        assert_eq!(mix.output, (Some(400), false));
        assert_eq!(mix.reasoning, (Some(150), false));
        assert_eq!(mix.total, (Some(1_400), false));

        // Unknown cached_input_tokens must not inflate fresh_input.
        let partial_rows = vec![UsageRow {
            provider: "codex".into(),
            input_tokens: Some(100),
            cached_input_tokens: None,
            output_tokens: Some(50),
            ..Default::default()
        }];
        let partial_mix = compute_token_mix(&partial_rows);
        assert_eq!(partial_mix.fresh_input, (None, true));
        assert_eq!(partial_mix.cached_read, (None, true));
        assert_eq!(partial_mix.output, (Some(50), false));
        assert_eq!(partial_mix.total, (Some(150), false));

        // Mixing complete and incomplete rows marks fresh_input partial without counting unknown input.
        let mixed = vec![rows[0].clone(), partial_rows[0].clone()];
        let mixed_mix = compute_token_mix(&mixed);
        assert_eq!(mixed_mix.fresh_input, (Some(200), true));
        assert_eq!(mixed_mix.total, (Some(1_550), false));

        // Chart average excludes unknown buckets while keeping measured zero buckets.
        assert_eq!(
            compute_chart_avg(&[Some(100.0), None, Some(100.0)]),
            (Some(100.0), true)
        );
        assert_eq!(
            compute_chart_avg(&[Some(100.0), Some(0.0)]),
            (Some(50.0), false)
        );
        assert_eq!(compute_chart_avg(&[None, None]), (None, true));
        assert_eq!(compute_chart_avg(&[]), (None, false));

        // Effective stack breakdown requires multiple providers and no provider filter.
        assert_eq!(
            effective_stack_breakdown(StackBreakdown::ByProvider, None, 2),
            StackBreakdown::ByProvider
        );
        assert_eq!(
            effective_stack_breakdown(StackBreakdown::ByProvider, None, 1),
            StackBreakdown::TokenMix
        );
        assert_eq!(
            effective_stack_breakdown(StackBreakdown::ByProvider, Some("codex"), 2),
            StackBreakdown::TokenMix
        );

        let (saved, partial_saved) = sum_cache_savings(&rows);
        assert!(!partial_saved);
        assert!((saved.unwrap() - 2.5).abs() < 1e-9);

        let rows2 = vec![
            rows[0].clone(),
            UsageRow {
                model: "b".into(),
                input_tokens: Some(500),
                cached_input_tokens: Some(100),
                estimated_cost_usd: Some(2.0),
                requests: 10,
                ..Default::default()
            },
        ];
        assert_eq!(
            sort_model_rows(&rows2, ModelSort::Tokens)[0].model,
            "claude-opus-4-6"
        );
        assert_eq!(sort_model_rows(&rows2, ModelSort::Cost)[0].model, "b");
        assert_eq!(sort_model_rows(&rows2, ModelSort::Calls)[0].model, "b");
        assert_eq!(
            sort_model_rows(&rows2, ModelSort::CacheHit)[0].model,
            "claude-opus-4-6"
        );
    }
}
