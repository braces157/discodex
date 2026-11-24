use std::collections::BTreeMap;

use chrono::{Datelike, Days, Local, NaiveDate, TimeZone};

use super::format::{Metric, pretty_date};
use crate::usage::{UsageBundle, UsageDay, UsageHour, UsagePeriod, UsageSnapshot};

#[derive(Clone, Debug)]
pub struct ChartPoint {
    pub label: String,
    pub hover_title: String,
    pub input: u64,
    pub cached_read: u64,
    pub fresh_input: u64,
    pub output: u64,
    pub cost: Option<f64>,
    pub requests: u64,
    pub by_provider: Vec<(String, u64)>,
    pub is_incomplete: bool,
}

impl ChartPoint {
    pub fn value(&self, metric: Metric) -> Option<f64> {
        match metric {
            Metric::Tokens => (!self.is_incomplete).then_some((self.input + self.output) as f64),
            Metric::Cost => self.cost,
            Metric::Calls => Some(self.requests as f64),
        }
    }
}

#[derive(Clone, Debug)]
pub struct HeatmapCell {
    pub pretty: String,
    pub day_name: String,
    pub tokens: u64,
    pub cost: Option<f64>,
    pub requests: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Heatmap84 {
    pub leading_empty: usize,
    pub cells: Vec<HeatmapCell>,
    pub max_tokens: u64,
    pub streak_days: usize,
    pub active_days: usize,
}

#[cfg(test)]
fn day_value(day: &UsageDay, metric: Metric) -> Option<f64> {
    match metric {
        Metric::Tokens => match (day.input_tokens, day.output_tokens) {
            (Some(input), Some(output)) => Some(input.saturating_add(output) as f64),
            _ => None,
        },
        Metric::Cost => day.estimated_cost_usd,
        Metric::Calls => Some(day.requests as f64),
    }
}

pub fn chart_days(
    daily: &[UsageDay],
    period: UsagePeriod,
    refreshed_at_ms: i64,
) -> (Vec<UsageDay>, bool) {
    let Some(now) = Local.timestamp_millis_opt(refreshed_at_ms).single() else {
        return (Vec::new(), false);
    };
    let parsed: BTreeMap<_, _> = daily
        .iter()
        .filter_map(|day| {
            NaiveDate::parse_from_str(&day.date, "%Y-%m-%d")
                .ok()
                .map(|date| (date, day))
        })
        .collect();
    if parsed.is_empty() {
        return (Vec::new(), false);
    }
    let end = if period == UsagePeriod::All {
        parsed
            .last_key_value()
            .map(|(date, _)| *date)
            .unwrap_or(now.date_naive())
    } else {
        now.date_naive()
    };
    let start = period
        .since_ms(refreshed_at_ms)
        .and_then(|time| Local.timestamp_millis_opt(time).single())
        .map(|time| time.date_naive())
        .or_else(|| parsed.first_key_value().map(|(date, _)| *date))
        .unwrap_or(end);
    let truncated = end.signed_duration_since(start).num_days() >= 30;
    let mut date = if truncated {
        end.checked_sub_days(Days::new(29)).unwrap_or(start)
    } else {
        start
    };
    let mut days = Vec::new();
    while date <= end && days.len() < 30 {
        days.push(
            parsed
                .get(&date)
                .map(|day| (*day).clone())
                .unwrap_or_else(|| UsageDay {
                    date: date.format("%Y-%m-%d").to_string(),
                    input_tokens: Some(0),
                    output_tokens: Some(0),
                    cached_input_tokens: Some(0),
                    estimated_cost_usd: Some(0.0),
                    requests: 0,
                }),
        );
        let Some(next) = date.succ_opt() else {
            break;
        };
        date = next;
    }
    (days, truncated)
}

pub fn build_chart_series(
    snapshot: &UsageSnapshot,
    period: UsagePeriod,
    provider_filter: Option<&str>,
    provider_order: &[String],
) -> (Vec<ChartPoint>, bool, bool) {
    if period == UsagePeriod::Today {
        return build_hourly_series(snapshot, provider_filter, provider_order);
    }
    let daily = match provider_filter {
        Some(slug) => snapshot
            .daily_by_provider
            .get(slug)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        None => &snapshot.daily,
    };
    let (visible, truncated) = chart_days(daily, period, snapshot.refreshed_at_ms);
    let mut prov_day_tok: BTreeMap<(&str, &str), u64> = BTreeMap::new();
    for slug in provider_order {
        if let Some(days) = snapshot.daily_by_provider.get(slug) {
            for day in days {
                let tok = day
                    .input_tokens
                    .unwrap_or_default()
                    .saturating_add(day.output_tokens.unwrap_or_default());
                prov_day_tok.insert((slug.as_str(), day.date.as_str()), tok);
            }
        }
    }
    let points = visible
        .into_iter()
        .map(|day| {
            let incomplete = day.input_tokens.is_none() || day.output_tokens.is_none();
            let input = day.input_tokens.unwrap_or_default();
            let cached = day.cached_input_tokens.unwrap_or_default().min(input);
            let fresh = input.saturating_sub(cached);
            let output = day.output_tokens.unwrap_or_default();
            let by_provider = provider_order
                .iter()
                .map(|slug| {
                    (
                        slug.clone(),
                        prov_day_tok
                            .get(&(slug.as_str(), day.date.as_str()))
                            .copied()
                            .unwrap_or_default(),
                    )
                })
                .collect();
            let label = pretty_date(&day.date);
            let hover_title = NaiveDate::parse_from_str(&day.date, "%Y-%m-%d")
                .map(|d| format!("{}, {}", d.format("%a"), pretty_date(&day.date)))
                .unwrap_or_else(|_| label.clone());
            ChartPoint {
                label,
                hover_title,
                input,
                cached_read: cached,
                fresh_input: fresh,
                output,
                cost: day.estimated_cost_usd,
                requests: day.requests,
                by_provider,
                is_incomplete: incomplete,
            }
        })
        .collect();
    (points, truncated, false)
}

fn build_hourly_series(
    snapshot: &UsageSnapshot,
    provider_filter: Option<&str>,
    provider_order: &[String],
) -> (Vec<ChartPoint>, bool, bool) {
    let active_hours: &[UsageHour] = match provider_filter {
        Some(slug) => snapshot
            .hourly_by_provider
            .get(slug)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        None => &snapshot.hourly,
    };
    if active_hours.is_empty() {
        return (Vec::new(), false, true);
    }
    let mut by_hour: BTreeMap<u8, &UsageHour> = BTreeMap::new();
    for h in active_hours {
        by_hour.insert(h.hour, h);
    }
    let mut prov_hour_tok: BTreeMap<(&str, u8), u64> = BTreeMap::new();
    for slug in provider_order {
        if let Some(list) = snapshot.hourly_by_provider.get(slug) {
            for item in list {
                let tok = item
                    .input_tokens
                    .unwrap_or_default()
                    .saturating_add(item.output_tokens.unwrap_or_default());
                prov_hour_tok.insert((slug.as_str(), item.hour), tok);
            }
        }
    }
    let points = (0..24u8)
        .map(|h| {
            let hover_title = format!("Today · {h:02}:00–{:02}:00", (h + 1) % 24);
            let label = format!("{h:02}:00");
            let Some(entry) = by_hour.get(&h) else {
                return ChartPoint {
                    label,
                    hover_title,
                    input: 0,
                    cached_read: 0,
                    fresh_input: 0,
                    output: 0,
                    cost: Some(0.0),
                    requests: 0,
                    by_provider: Vec::new(),
                    is_incomplete: false,
                };
            };
            let incomplete = entry.input_tokens.is_none() || entry.output_tokens.is_none();
            let input = entry.input_tokens.unwrap_or_default();
            let cached = entry.cached_input_tokens.unwrap_or_default().min(input);
            let fresh = input.saturating_sub(cached);
            let output = entry.output_tokens.unwrap_or_default();
            let by_provider = provider_order
                .iter()
                .map(|slug| {
                    (
                        slug.clone(),
                        prov_hour_tok
                            .get(&(slug.as_str(), h))
                            .copied()
                            .unwrap_or_default(),
                    )
                })
                .collect();
            ChartPoint {
                label,
                hover_title,
                input,
                cached_read: cached,
                fresh_input: fresh,
                output,
                cost: entry.estimated_cost_usd,
                requests: entry.requests,
                by_provider,
                is_incomplete: incomplete,
            }
        })
        .collect();
    (points, false, true)
}

pub fn build_sparkline_series(
    bundle: Option<&UsageBundle>,
    daily: &[UsageDay],
    period: UsagePeriod,
    provider_filter: Option<&str>,
) -> (Vec<Option<f64>>, Vec<Option<f64>>) {
    if period == UsagePeriod::Today
        && let Some(bundle) = bundle
    {
        let hours = match provider_filter {
            Some(slug) => bundle
                .today
                .hourly_by_provider
                .get(slug)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            None => &bundle.today.hourly,
        };
        if !hours.is_empty() {
            let mut by_h = [Some(0.0_f64); 24];
            let mut cost_h = [Some(0.0_f64); 24];
            let mut seen_h = [false; 24];
            for h in hours {
                let idx = usize::from(h.hour).min(23);
                let tok = match (h.input_tokens, h.output_tokens) {
                    (Some(i), Some(o)) => Some(i.saturating_add(o) as f64),
                    _ => None,
                };
                if !seen_h[idx] {
                    by_h[idx] = tok;
                    cost_h[idx] = h.estimated_cost_usd;
                    seen_h[idx] = true;
                } else {
                    by_h[idx] = match (by_h[idx], tok) {
                        (Some(a), Some(b)) => Some(a + b),
                        _ => None,
                    };
                    cost_h[idx] = match (cost_h[idx], h.estimated_cost_usd) {
                        (Some(a), Some(b)) => Some(a + b),
                        _ => None,
                    };
                }
            }
            return (by_h.to_vec(), cost_h.to_vec());
        }
    }

    let Some(refreshed_at_ms) = bundle.map(|b| b.for_period(period).refreshed_at_ms) else {
        return (Vec::new(), Vec::new());
    };
    let (filled, _) = chart_days(daily, period, refreshed_at_ms);
    if filled.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let take_n = if period == UsagePeriod::Week { 7 } else { 14 };
    let slice_start = filled.len().saturating_sub(take_n);
    let mut tok = Vec::with_capacity(filled.len() - slice_start);
    let mut cost = Vec::with_capacity(filled.len() - slice_start);
    for d in &filled[slice_start..] {
        tok.push(match (d.input_tokens, d.output_tokens) {
            (Some(i), Some(o)) => Some(i.saturating_add(o) as f64),
            _ => None,
        });
        cost.push(d.estimated_cost_usd);
    }
    (tok, cost)
}

pub fn build_heatmap_84(all_daily: &[UsageDay], refreshed_at_ms: i64) -> Heatmap84 {
    let end = Local
        .timestamp_millis_opt(refreshed_at_ms)
        .single()
        .unwrap_or_else(Local::now)
        .date_naive();
    let start = end.checked_sub_days(Days::new(83)).unwrap_or(end);
    let leading_empty = start.weekday().num_days_from_sunday() as usize;
    let parsed: BTreeMap<NaiveDate, &UsageDay> = all_daily
        .iter()
        .filter_map(|d| {
            NaiveDate::parse_from_str(&d.date, "%Y-%m-%d")
                .ok()
                .map(|date| (date, d))
        })
        .collect();

    let mut cells = Vec::with_capacity(84);
    let mut max_tokens = 0u64;
    let mut active_days = 0usize;

    for back in (0..84u64).rev() {
        let date = end.checked_sub_days(Days::new(back)).unwrap_or(end);
        let iso = date.format("%Y-%m-%d").to_string();
        let pretty = pretty_date(&iso);
        let day_name = date.format("%a").to_string();
        if let Some(day) = parsed.get(&date) {
            let tokens = day
                .input_tokens
                .unwrap_or_default()
                .saturating_add(day.output_tokens.unwrap_or_default());
            max_tokens = max_tokens.max(tokens);
            if day.requests > 0 {
                active_days += 1;
            }
            cells.push(HeatmapCell {
                pretty,
                day_name,
                tokens,
                cost: day.estimated_cost_usd,
                requests: day.requests,
            });
        } else {
            cells.push(HeatmapCell {
                pretty,
                day_name,
                tokens: 0,
                cost: Some(0.0),
                requests: 0,
            });
        }
    }

    let mut streak_days = 0usize;
    let start_idx = if cells.last().is_some_and(|c| c.requests > 0) {
        cells.len()
    } else {
        cells.len().saturating_sub(1)
    };
    for cell in cells[..start_idx].iter().rev() {
        if cell.requests > 0 {
            streak_days += 1;
        } else {
            break;
        }
    }

    Heatmap84 {
        leading_empty,
        cells,
        max_tokens,
        streak_days,
        active_days,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_preserves_calendar_gaps_and_unknown_recorded_values() {
        let at = Local
            .with_ymd_and_hms(2026, 10, 23, 12, 0, 0)
            .single()
            .unwrap()
            .timestamp_millis();
        let records = vec![
            UsageDay {
                date: "2026-10-19".into(),
                input_tokens: Some(100),
                requests: 1,
                ..Default::default()
            },
            UsageDay {
                date: "2026-10-23".into(),
                input_tokens: None,
                requests: 1,
                ..Default::default()
            },
        ];
        let (days, truncated) = chart_days(&records, UsagePeriod::Week, at);
        assert!(!truncated);
        assert_eq!(days.len(), 7);
        assert_eq!(days[2].date, "2026-10-19");
        assert_eq!(days[3].date, "2026-10-20");
        assert_eq!(days[3].input_tokens, Some(0));
        assert_eq!(days[3].estimated_cost_usd, Some(0.0));
        assert_eq!(days[6].input_tokens, None);
        assert_eq!(days.iter().map(|day| day.requests).sum::<u64>(), 2);
        assert!(chart_days(&[], UsagePeriod::Week, at).0.is_empty());
        assert!(chart_days(&[], UsagePeriod::All, at).0.is_empty());
    }

    #[test]
    fn heatmap_and_today_hourly_series_are_consistent() {
        let at = Local
            .with_ymd_and_hms(2026, 11, 21, 10, 0, 0)
            .single()
            .unwrap()
            .timestamp_millis();
        let all_daily = vec![
            UsageDay {
                date: "2026-11-19".into(),
                input_tokens: Some(1_000),
                output_tokens: Some(200),
                requests: 2,
                ..Default::default()
            },
            UsageDay {
                date: "2026-11-20".into(),
                input_tokens: Some(2_000),
                output_tokens: Some(500),
                requests: 3,
                ..Default::default()
            },
        ];
        let hm = build_heatmap_84(&all_daily, at);
        assert_eq!(hm.cells.len(), 84);
        assert_eq!(hm.active_days, 2);
        assert_eq!(hm.streak_days, 2);
        assert_eq!(hm.max_tokens, 2_500);
        assert_eq!(hm.leading_empty, 0);
        assert_eq!(hm.cells[0].day_name, "Sun");
        assert_eq!(hm.cells[83].day_name, "Sat");

        let wed_at = Local
            .with_ymd_and_hms(2026, 11, 18, 10, 0, 0)
            .single()
            .unwrap()
            .timestamp_millis();
        let hm_wed = build_heatmap_84(&all_daily, wed_at);
        assert_eq!(hm_wed.leading_empty, 4);
        assert_eq!(hm_wed.cells[0].day_name, "Thu");
        assert_eq!((hm_wed.leading_empty) % 7, 4);
        assert_eq!(hm_wed.cells[83].day_name, "Wed");
        assert_eq!((hm_wed.leading_empty + 83) % 7, 3);

        let sparse_daily = vec![
            UsageDay {
                date: "2026-11-15".into(),
                input_tokens: Some(100),
                output_tokens: Some(0),
                estimated_cost_usd: Some(0.5),
                requests: 1,
                ..Default::default()
            },
            UsageDay {
                date: "2026-11-21".into(),
                input_tokens: None,
                output_tokens: Some(50),
                estimated_cost_usd: None,
                requests: 1,
                ..Default::default()
            },
        ];
        let mut bundle = UsageBundle::default();
        bundle.week.refreshed_at_ms = at;
        let (tok_spark, cost_spark) =
            build_sparkline_series(Some(&bundle), &sparse_daily, UsagePeriod::Week, None);
        assert_eq!(
            tok_spark,
            vec![
                Some(100.0),
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(0.0),
                None,
            ]
        );
        assert_eq!(
            cost_spark,
            vec![
                Some(0.5),
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(0.0),
                None,
            ]
        );

        let snap = UsageSnapshot {
            hourly: vec![UsageHour {
                hour: 9,
                input_tokens: Some(500),
                cached_input_tokens: Some(300),
                output_tokens: Some(100),
                estimated_cost_usd: Some(0.12),
                requests: 2,
            }],
            refreshed_at_ms: at,
            ..Default::default()
        };
        let (pts, truncated, is_hourly) = build_chart_series(&snap, UsagePeriod::Today, None, &[]);
        assert!(!truncated);
        assert!(is_hourly);
        assert_eq!(pts.len(), 24);
        assert_eq!(pts[9].fresh_input, 200);
        assert_eq!(pts[9].cached_read, 300);
        assert_eq!(pts[9].output, 100);
        assert_eq!(pts[0].requests, 0);
        assert_eq!(day_value(&all_daily[0], Metric::Tokens), Some(1_200.0));
    }
}
