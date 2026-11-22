use gpui::{
    Bounds, IntoElement, PathBuilder, Pixels, Point, Window, canvas, div, fill, point, prelude::*,
    px, rgb, rgba, size,
};

use super::{
    format::{ChartStyle, Metric, StackBreakdown, effective_stack_breakdown},
    series::ChartPoint,
    theme::{
        ACCENT_AMBER, ACCENT_CALLS, ACCENT_COST, ACCENT_LIME, BORDER_SUBTLE, GRID_LINE,
        provider_color, provider_display, with_alpha,
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

/// Paints a smooth cubic-bezier area fill and stroke line across `pts`.
pub fn paint_smooth_curve(
    pts: &[Point<Pixels>],
    base_y: Pixels,
    color: u32,
    fill_alpha: u8,
    stroke_width: f32,
    window: &mut Window,
) {
    if pts.len() < 2 {
        return;
    }
    let mut area = PathBuilder::fill();
    area.move_to(point(pts[0].x, base_y));
    area.line_to(pts[0]);
    for pair in pts.windows(2) {
        let mid_x = px((f32::from(pair[0].x) + f32::from(pair[1].x)) * 0.5);
        area.cubic_bezier_to(pair[1], point(mid_x, pair[0].y), point(mid_x, pair[1].y));
    }
    if let Some(last) = pts.last() {
        area.line_to(point(last.x, base_y));
    }
    area.close();
    if let Ok(path) = area.build() {
        window.paint_path(path, rgba(with_alpha(color, fill_alpha)));
    }

    let mut line = PathBuilder::stroke(px(stroke_width));
    line.move_to(pts[0]);
    for pair in pts.windows(2) {
        let mid_x = px((f32::from(pair[0].x) + f32::from(pair[1].x)) * 0.5);
        line.cubic_bezier_to(pair[1], point(mid_x, pair[0].y), point(mid_x, pair[1].y));
    }
    if let Ok(path) = line.build() {
        window.paint_path(path, rgb(color));
    }
}

/// Renders a GPU-accelerated inline sparkline using GPUI `canvas` and `PathBuilder`.
pub fn sparkline(values: Vec<Option<f64>>, color: u32) -> impl IntoElement {
    div().w(px(66.)).h(px(22.)).flex_none().child(
        canvas(
            move |bounds, _, _| bounds,
            move |_, bounds, window, _| {
                let w = f32::from(bounds.size.width);
                let h = f32::from(bounds.size.height);
                if w <= 4.0 || h <= 4.0 || values.is_empty() {
                    return;
                }
                let origin = bounds.origin;
                let has_known = values.iter().any(Option::is_some);
                let max = values.iter().flatten().copied().fold(0.0_f64, f64::max);
                if !has_known || max <= 0.0 {
                    let y = origin.y + px(h - 3.0);
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
                let usable_w = (w - 6.0).max(1.0);
                let usable_h = (h - 6.0).max(1.0);
                let n = values.len();
                let x_for = |i: usize| -> Pixels {
                    let x = if n == 1 {
                        w * 0.5
                    } else {
                        3.0 + (i as f32 / (n - 1) as f32) * usable_w
                    };
                    origin.x + px(x)
                };
                let y_for = |v: f64| -> Pixels {
                    let norm = ((v - min) / span).clamp(0.0, 1.0) as f32;
                    origin.y + px((h - 3.0) - norm * usable_h)
                };

                let runs = contiguous_known_runs(&values);
                let mut last_known: Option<Point<Pixels>> = None;
                for run in &runs {
                    let pts: Vec<Point<Pixels>> = run
                        .iter()
                        .map(|&(i, v)| point(x_for(i), y_for(v)))
                        .collect();
                    if pts.len() >= 2 {
                        paint_smooth_curve(&pts, origin.y + px(h - 1.0), color, 0x38, 1.6, window);
                    } else if let Some(pt) = pts.first() {
                        let mut dot = fill(
                            Bounds::new(
                                point(pt.x - px(1.75), pt.y - px(1.75)),
                                size(px(3.5), px(3.5)),
                            ),
                            rgb(color),
                        );
                        dot.corner_radii = px(1.75).into();
                        window.paint_quad(dot);
                    }
                    if let Some(&last) = pts.last() {
                        last_known = Some(last);
                    }
                }

                for (i, val) in values.iter().enumerate() {
                    if val.is_none() {
                        let x = x_for(i);
                        window.paint_quad(fill(
                            Bounds::new(
                                point(x - px(1.5), origin.y + px(h - 3.0)),
                                size(px(3.0), px(2.0)),
                            ),
                            rgb(ACCENT_AMBER),
                        ));
                    }
                }

                if let Some(last) = last_known {
                    let mut dot = fill(
                        Bounds::new(
                            point(last.x - px(2.25), last.y - px(2.25)),
                            size(px(4.5), px(4.5)),
                        ),
                        rgb(color),
                    );
                    dot.corner_radii = px(2.25).into();
                    window.paint_quad(dot);
                }
            },
        )
        .size_full(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn paint_plot_canvas(
    bounds: Bounds<Pixels>,
    window: &mut Window,
    values: &[Option<f64>],
    max_scale: f64,
    avg_val: Option<f64>,
    metric_color: u32,
    style: ChartStyle,
    hovered_idx: Option<usize>,
) {
    let w = f32::from(bounds.size.width);
    let h = f32::from(bounds.size.height);
    if w <= 2.0 || h <= 2.0 {
        return;
    }
    let origin = bounds.origin;

    for (frac, color) in [(0.0_f32, GRID_LINE), (0.5, GRID_LINE), (1.0, BORDER_SUBTLE)] {
        let y = origin.y + px((h - 1.0) * frac);
        window.paint_quad(fill(
            Bounds::new(point(origin.x, y), size(px(w), px(1.0))),
            rgba(color),
        ));
    }

    if let Some(avg) = avg_val
        && avg > 0.0
        && max_scale > 0.0
    {
        let norm = (avg / max_scale).clamp(0.0, 1.0) as f32;
        let y = origin.y + px((h - 2.0) * (1.0 - norm));
        let mut x = 0.0_f32;
        let dash_color = rgba(with_alpha(metric_color, 0x55));
        while x < w {
            let seg = 4.0_f32.min(w - x);
            window.paint_quad(fill(
                Bounds::new(point(origin.x + px(x), y), size(px(seg), px(1.0))),
                dash_color,
            ));
            x += 7.0;
        }
    }

    if style != ChartStyle::AreaLine || values.is_empty() {
        return;
    }

    let n = values.len();
    let slot_w = w / n as f32;
    let x_for = |i: usize| -> Pixels { origin.x + px((i as f32 + 0.5) * slot_w) };
    let y_for = |v: f64| -> Pixels {
        let norm = (v / max_scale).clamp(0.0, 1.0) as f32;
        origin.y + px((h - 3.0) - norm * (h - 8.0))
    };

    let runs = contiguous_known_runs(values);
    let mut isolated_known = vec![false; n];
    for run in &runs {
        if run.len() >= 2 {
            let pts: Vec<Point<Pixels>> = run
                .iter()
                .map(|&(i, v)| point(x_for(i), y_for(v)))
                .collect();
            paint_smooth_curve(
                &pts,
                origin.y + px(h - 1.0),
                metric_color,
                0x3A,
                2.0,
                window,
            );
        } else if let Some(&(idx, _)) = run.first() {
            isolated_known[idx] = true;
        }
    }

    for (i, val) in values.iter().enumerate() {
        let is_hovered = hovered_idx == Some(i);
        let x = x_for(i);
        match *val {
            Some(v) => {
                let y = y_for(v);
                if is_hovered {
                    window.paint_quad(fill(
                        Bounds::new(point(x - px(0.5), origin.y), size(px(1.0), px(h))),
                        rgba(0xFFFFFF36),
                    ));
                    let mut halo = fill(
                        Bounds::new(point(x - px(5.0), y - px(5.0)), size(px(10.0), px(10.0))),
                        rgba(with_alpha(metric_color, 0x40)),
                    );
                    halo.corner_radii = px(5.0).into();
                    window.paint_quad(halo);
                }
                if v > 0.0 || is_hovered || isolated_known[i] {
                    let r = if is_hovered {
                        3.5
                    } else if n <= 12 || isolated_known[i] {
                        2.3
                    } else {
                        1.5
                    };
                    let mut dot = fill(
                        Bounds::new(point(x - px(r), y - px(r)), size(px(r * 2.0), px(r * 2.0))),
                        rgb(metric_color),
                    );
                    dot.corner_radii = px(r).into();
                    window.paint_quad(dot);
                }
            }
            None => {
                let stub_w = (slot_w * 0.5).clamp(4.0, 14.0);
                let mut stub = fill(
                    Bounds::new(
                        point(x - px(stub_w * 0.5), origin.y + px(h - 4.0)),
                        size(px(stub_w), px(3.0)),
                    ),
                    rgb(ACCENT_AMBER),
                );
                stub.corner_radii = px(1.5).into();
                window.paint_quad(stub);

                if is_hovered {
                    window.paint_quad(fill(
                        Bounds::new(point(x - px(0.5), origin.y), size(px(1.0), px(h))),
                        rgba(with_alpha(ACCENT_AMBER, 0x55)),
                    ));
                    let y = origin.y + px(h - 3.0);
                    let mut halo = fill(
                        Bounds::new(point(x - px(5.0), y - px(5.0)), size(px(10.0), px(10.0))),
                        rgba(with_alpha(ACCENT_AMBER, 0x40)),
                    );
                    halo.corner_radii = px(5.0).into();
                    window.paint_quad(halo);
                }
            }
        }
    }
}

pub fn build_x_axis_labels(points: &[ChartPoint], is_hourly: bool) -> Vec<String> {
    if points.is_empty() {
        return Vec::new();
    }
    if is_hourly {
        return ["00:00", "06:00", "12:00", "18:00", "23:00"]
            .into_iter()
            .map(String::from)
            .collect();
    }
    if points.len() == 1 {
        return vec![points[0].label.clone()];
    }
    let mut labels = vec![points[0].label.clone()];
    if points.len() >= 7 {
        labels.push(points[points.len() / 2].label.clone());
    }
    if let Some(last) = points.last() {
        labels.push(last.label.clone());
    }
    labels
}

pub fn build_legend_items(
    points: &[ChartPoint],
    metric: Metric,
    style: ChartStyle,
    stack_by: StackBreakdown,
    provider_filter: Option<&str>,
    provider_count: usize,
) -> Vec<(String, u32)> {
    if style != ChartStyle::StackedBar || metric != Metric::Tokens || points.is_empty() {
        return Vec::new();
    }
    if effective_stack_breakdown(stack_by, provider_filter, provider_count)
        == StackBreakdown::ByProvider
    {
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
