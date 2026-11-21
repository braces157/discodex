use gpui::{
    Bounds, IntoElement, PathBuilder, Pixels, Point, Window, canvas, div, fill, point, prelude::*,
    px, rgb, rgba, size,
};

use super::{
    format::{ChartStyle, Metric, StackBreakdown},
    series::ChartPoint,
    theme::{
        ACCENT_CALLS, ACCENT_COST, ACCENT_LIME, BORDER_SUBTLE, GRID_LINE, provider_color,
        provider_display, with_alpha,
    },
};

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
pub fn sparkline(values: Vec<f64>, color: u32) -> impl IntoElement {
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
                let max = values.iter().copied().fold(0.0_f64, f64::max);
                if max <= 0.0 {
                    let y = origin.y + px(h - 3.0);
                    window.paint_quad(fill(
                        Bounds::new(point(origin.x + px(2.0), y), size(px(w - 4.0), px(1.5))),
                        rgba(with_alpha(color, 0x44)),
                    ));
                    return;
                }
                let min = values.iter().copied().fold(max, f64::min).min(0.0);
                let span = (max - min).max(1e-9);
                let usable_w = (w - 6.0).max(1.0);
                let usable_h = (h - 6.0).max(1.0);
                let n = values.len();
                let pts: Vec<Point<Pixels>> = values
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let x = if n == 1 {
                            w * 0.5
                        } else {
                            3.0 + (i as f32 / (n - 1) as f32) * usable_w
                        };
                        let norm = ((*v - min) / span).clamp(0.0, 1.0) as f32;
                        let y = (h - 3.0) - norm * usable_h;
                        point(origin.x + px(x), origin.y + px(y))
                    })
                    .collect();

                paint_smooth_curve(&pts, origin.y + px(h - 1.0), color, 0x38, 1.6, window);

                if let Some(last) = pts.last() {
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
    avg_val: f64,
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

    if avg_val > 0.0 && max_scale > 0.0 {
        let norm = (avg_val / max_scale).clamp(0.0, 1.0) as f32;
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
    let pts: Vec<Point<Pixels>> = values
        .iter()
        .enumerate()
        .map(|(i, val)| {
            let norm = (val.unwrap_or(0.0) / max_scale).clamp(0.0, 1.0) as f32;
            let x = (i as f32 + 0.5) * slot_w;
            let y = (h - 3.0) - norm * (h - 8.0);
            point(origin.x + px(x), origin.y + px(y))
        })
        .collect();

    paint_smooth_curve(
        &pts,
        origin.y + px(h - 1.0),
        metric_color,
        0x3A,
        2.0,
        window,
    );

    for (i, pt) in pts.iter().enumerate() {
        let is_hovered = hovered_idx == Some(i);
        if is_hovered {
            window.paint_quad(fill(
                Bounds::new(point(pt.x - px(0.5), origin.y), size(px(1.0), px(h))),
                rgba(0xFFFFFF36),
            ));
            let mut halo = fill(
                Bounds::new(
                    point(pt.x - px(5.0), pt.y - px(5.0)),
                    size(px(10.0), px(10.0)),
                ),
                rgba(with_alpha(metric_color, 0x40)),
            );
            halo.corner_radii = px(5.0).into();
            window.paint_quad(halo);
        }
        if values[i].is_some_and(|v| v > 0.0) || is_hovered {
            let r = if is_hovered {
                3.5
            } else if n <= 12 {
                2.3
            } else {
                1.5
            };
            let mut dot = fill(
                Bounds::new(
                    point(pt.x - px(r), pt.y - px(r)),
                    size(px(r * 2.0), px(r * 2.0)),
                ),
                rgb(metric_color),
            );
            dot.corner_radii = px(r).into();
            window.paint_quad(dot);
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
    if stack_by == StackBreakdown::ByProvider && provider_filter.is_none() && provider_count > 1 {
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
