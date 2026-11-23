use gpui::{
    Bounds, IntoElement, Pixels, Point, canvas, div, fill, point, prelude::*, px, rgb, rgba, size,
};

use super::{
    chart_curves::{CurveStyle, paint_monotone_area_and_stroke},
    format::{ChartStyle, Metric, StackBreakdown, effective_stack_breakdown},
    series::ChartPoint,
    theme::{
        ACCENT_AMBER, ACCENT_CALLS, ACCENT_COST, ACCENT_LIME, BG_SURFACE, provider_color,
        provider_display, with_alpha,
    },
};

/// Splits an optional series into contiguous runs of known `(index, value)` points.
pub fn contiguous_known_runs(values: &[Option<f64>]) -> Vec<Vec<(usize, f64)>> {
    let mut runs = Vec::new();
    let mut current = Vec::new();
    for (i, val) in values.iter().enumerate() {
        if let Some(v) = val {
            current.push((i, *v));
        } else if !current.is_empty() {
            runs.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        runs.push(current);
    }
    runs
}

/// Renders a full-width GPU-accelerated Hero KPI sparkline ribbon using Fritsch-Carlson
/// monotone splines, DX11 Oklab vertical gradients, luminous glow, and an illuminated live beacon.
pub fn sparkline(values: Vec<Option<f64>>, color: u32) -> impl IntoElement {
    div().w_full().h(px(34.)).child(
        canvas(
            move |bounds, _, _| bounds,
            move |_, bounds, window, _| {
                let w = f32::from(bounds.size.width);
                let h = f32::from(bounds.size.height);
                if w <= 6.0 || h <= 4.0 || values.is_empty() {
                    return;
                }
                let origin = bounds.origin;
                let has_known = values.iter().any(Option::is_some);
                let max = values.iter().flatten().copied().fold(0.0_f64, f64::max);
                if !has_known || max <= 0.0 {
                    let y = origin.y + px(h - 4.0);
                    let line_color = if has_known { color } else { ACCENT_AMBER };
                    window.paint_quad(fill(
                        Bounds::new(point(origin.x + px(2.0), y), size(px(w - 4.0), px(1.5))),
                        rgba(with_alpha(line_color, 0x44)),
                    ));
                    return;
                }
                let min = values
                    .iter()
                    .flatten()
                    .copied()
                    .fold(max, f64::min)
                    .min(0.0);
                let span = (max - min).max(1e-9);
                let usable_w = (w - 10.0).max(1.0);
                let usable_h = (h - 10.0).max(1.0);
                let n = values.len();
                let x_for = |i: usize| -> Pixels {
                    let x = if n == 1 {
                        w * 0.5
                    } else {
                        4.0 + (i as f32 / (n - 1) as f32) * usable_w
                    };
                    origin.x + px(x)
                };
                let y_for = |v: f64| -> Pixels {
                    let norm = ((v - min) / span).clamp(0.0, 1.0) as f32;
                    origin.y + px((h - 4.5) - norm * usable_h)
                };

                let runs = contiguous_known_runs(&values);
                let mut last_known: Option<Point<Pixels>> = None;
                for run in &runs {
                    let pts: Vec<Point<Pixels>> = run
                        .iter()
                        .map(|&(i, v)| point(x_for(i), y_for(v)))
                        .collect();
                    if pts.len() >= 2 {
                        paint_monotone_area_and_stroke(
                            bounds,
                            &pts,
                            origin.y + px(h - 1.0),
                            CurveStyle {
                                color,
                                fill_alpha: 0x16,
                                stroke_width: 1.5,
                                dim_factor: 1.0,
                            },
                            window,
                        );
                    } else if let Some(pt) = pts.first() {
                        let mut dot = fill(
                            Bounds::new(
                                point(pt.x - px(2.0), pt.y - px(2.0)),
                                size(px(4.0), px(4.0)),
                            ),
                            rgb(color),
                        );
                        dot.corner_radii = px(2.0).into();
                        window.paint_quad(dot);
                    }
                    if let Some(&last) = pts.last() {
                        last_known = Some(last);
                    }
                }

                if let Some(last) = last_known {
                    for (r, bg) in [
                        (3.6_f32, rgba((BG_SURFACE << 8) | 0xFF)),
                        (2.4, rgba((color << 8) | 0xFF)),
                    ] {
                        let mut dot = fill(
                            Bounds::new(
                                point(last.x - px(r), last.y - px(r)),
                                size(px(r * 2.0), px(r * 2.0)),
                            ),
                            bg,
                        );
                        dot.corner_radii = px(r).into();
                        window.paint_quad(dot);
                    }
                }
            },
        )
        .size_full(),
    )
}

/// Returns `(bucket_index, label)` pairs so each X-axis tick aligns directly beneath its bar/point.
pub fn build_x_axis_labels(points: &[ChartPoint], is_hourly: bool) -> Vec<(usize, String)> {
    let n = points.len();
    if n == 0 {
        return Vec::new();
    }
    if is_hourly {
        return [
            (0usize, "00:00"),
            (6, "06:00"),
            (12, "12:00"),
            (18, "18:00"),
            (23, "23:00"),
        ]
        .into_iter()
        .filter(|&(idx, _)| idx < n)
        .map(|(idx, lbl)| (idx, lbl.to_string()))
        .collect();
    }
    if n <= 7 {
        return points
            .iter()
            .enumerate()
            .map(|(i, p)| (i, p.label.clone()))
            .collect();
    }
    let mut indices = vec![0, n / 4, n / 2, (3 * n) / 4, n - 1];
    indices.dedup();
    indices
        .into_iter()
        .map(|i| (i, points[i].label.clone()))
        .collect()
}

pub fn build_legend_items(
    points: &[ChartPoint],
    metric: Metric,
    style: ChartStyle,
    stack_by: StackBreakdown,
    provider_filter: Option<&str>,
    provider_count: usize,
) -> Vec<(String, u32)> {
    if metric != Metric::Tokens || points.is_empty() || style == ChartStyle::Lollipop {
        return Vec::new();
    }
    let by_provider = effective_stack_breakdown(stack_by, provider_filter, provider_count)
        == StackBreakdown::ByProvider;
    if by_provider {
        let mut seen = Vec::new();
        for pt in points {
            for (slug, tok) in &pt.by_provider {
                if *tok > 0 && !seen.contains(slug) {
                    seen.push(slug.clone());
                }
            }
        }
        return seen
            .into_iter()
            .take(5)
            .map(|slug| (provider_display(&slug), provider_color(&slug)))
            .collect();
    }
    if style != ChartStyle::StackedBar {
        return Vec::new();
    }
    vec![
        ("Fresh In".into(), ACCENT_LIME),
        ("Cached".into(), ACCENT_COST),
        ("Output".into(), ACCENT_CALLS),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contiguous_known_runs_splits_unknowns_and_keeps_measured_zeros() {
        let runs = contiguous_known_runs(&[
            Some(100.0),
            Some(0.0),
            None,
            Some(50.0),
            None,
            None,
            Some(10.0),
            Some(20.0),
        ]);
        assert_eq!(
            runs,
            vec![
                vec![(0, 100.0), (1, 0.0)],
                vec![(3, 50.0)],
                vec![(6, 10.0), (7, 20.0)],
            ]
        );
        assert!(contiguous_known_runs(&[None, None]).is_empty());
    }
}
