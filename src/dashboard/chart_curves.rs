use gpui::{
    Bounds, Corners, PathBuilder, Pixels, Point, Window, fill, pattern_slash, point, px, rgb, rgba,
    size,
};

use super::{
    chart_canvas::{PlotCanvasSpec, paint_dashed_hline},
    chart_math::{
        CubicSegment, ease_out_quart, last_known_index, monotone_cubic_segments, peak_index,
        sample_segments_y,
    },
    sparkline::contiguous_known_runs,
    theme::{ACCENT_AMBER, ACCENT_ERR, ACCENT_POS, BG_RECESSED, with_alpha},
};

/// Visual styling for a clean, flat shadcn/Apple monotone curve.
#[derive(Clone, Copy, Debug)]
pub struct CurveStyle {
    pub color: u32,
    pub fill_alpha: u8,
    pub stroke_width: f32,
    pub dim_factor: f32,
}

/// Paints a crisp 4× MSAA monotone cubic curve with flat translucent fill and clean solid stroke.
/// Each overlapping path is isolated in its own `paint_layer` for artifact-free DX11 compositing.
pub fn paint_monotone_area_and_stroke(
    clip_bounds: Bounds<Pixels>,
    pts: &[Point<Pixels>],
    base_y: Pixels,
    style: CurveStyle,
    window: &mut Window,
) {
    let segs = monotone_cubic_segments(pts);
    if segs.is_empty() {
        return;
    }
    let scale_a = |a: u8| -> u8 { ((f32::from(a) * style.dim_factor).clamp(0.0, 255.0)) as u8 };

    // Pass 1: Clean flat translucent area tint (zero gradients)
    if style.fill_alpha > 0 {
        let mut area = PathBuilder::fill();
        area.move_to(point(pts[0].x, base_y));
        area.line_to(pts[0]);
        for seg in &segs {
            area.cubic_bezier_to(seg.p1, seg.c0, seg.c1);
        }
        if let Some(last) = pts.last() {
            area.line_to(point(last.x, base_y));
        }
        area.close();
        if let Ok(path) = area.build() {
            window.paint_layer(clip_bounds, |window| {
                window.paint_path(
                    path,
                    rgba(with_alpha(style.color, scale_a(style.fill_alpha))),
                );
            });
        }
    }

    // Pass 2: Razor-sharp 4x MSAA solid stroke (zero neon glow)
    window.paint_layer(clip_bounds, |window| {
        paint_cubic_stroke(
            &segs,
            style.stroke_width,
            rgba(with_alpha(style.color, scale_a(0xFF))),
            window,
        );
    });
}

fn paint_cubic_stroke(
    segs: &[CubicSegment],
    width: f32,
    color: impl Into<gpui::Background>,
    window: &mut Window,
) {
    let mut line = PathBuilder::stroke(px(width));
    line.move_to(segs[0].p0);
    for seg in segs {
        line.cubic_bezier_to(seg.p1, seg.c0, seg.c1);
    }
    if let Ok(path) = line.build() {
        window.paint_path(path, color);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn paint_area_series(
    origin: Point<Pixels>,
    w: f32,
    h: f32,
    base_y: Pixels,
    slot_w: f32,
    spec: &PlotCanvasSpec,
    pinned_val: Option<f64>,
    x_for: &impl Fn(usize) -> Pixels,
    y_for: &impl Fn(f64) -> Pixels,
    window: &mut Window,
) {
    let reveal_w = (w * ease_out_quart(spec.reveal_t)).max(1.0);
    let clip_bounds = Bounds::new(origin, size(px(reveal_w), px(h)));

    window.paint_layer(clip_bounds, |window| {
        if !spec.area_layers.is_empty() && pinned_val.is_none() {
            paint_multi_series_area(origin, reveal_w, h, base_y, spec, x_for, y_for, window);
        } else {
            paint_single_area_series(
                origin, w, reveal_w, h, base_y, slot_w, spec, pinned_val, x_for, y_for, window,
            );
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn paint_multi_series_area(
    origin: Point<Pixels>,
    w: f32,
    h: f32,
    base_y: Pixels,
    spec: &PlotCanvasSpec,
    x_for: &impl Fn(usize) -> Pixels,
    y_for: &impl Fn(f64) -> Pixels,
    window: &mut Window,
) {
    let paint_layers = |clip: Bounds<Pixels>, dim_factor: f32, window: &mut Window| {
        for layer in &spec.area_layers {
            let runs = contiguous_known_runs(&layer.values);
            for run in &runs {
                if run.len() >= 2 {
                    let pts: Vec<Point<Pixels>> = run
                        .iter()
                        .map(|&(i, v)| point(x_for(i), y_for(v)))
                        .collect();
                    paint_monotone_area_and_stroke(
                        clip,
                        &pts,
                        base_y,
                        CurveStyle {
                            color: layer.color,
                            fill_alpha: 0x16,
                            stroke_width: 1.75,
                            dim_factor,
                        },
                        window,
                    );
                }
            }
        }
    };

    let full_clip = Bounds::new(origin, size(px(w), px(h)));
    if let Some((a, b)) = spec.selected_range {
        let lo = a.min(b);
        let hi = a.max(b);
        let x_lo = f32::from(x_for(lo) - origin.x).clamp(0.0, w);
        let x_hi = f32::from(x_for(hi) - origin.x).clamp(x_lo, w);
        paint_layers(full_clip, 0.28, window);
        let range_clip = Bounds::new(
            point(origin.x + px(x_lo), origin.y),
            size(px((x_hi - x_lo).max(1.0)), px(h)),
        );
        paint_layers(range_clip, 1.0, window);
    } else if let Some(h_idx) = spec.hovered_idx {
        let split_x = spec
            .cursor_norm_x
            .map(|nx| (nx.clamp(0.0, 1.0) * w).clamp(1.0, w))
            .unwrap_or_else(|| f32::from(x_for(h_idx) - origin.x).clamp(1.0, w));
        let left_clip = Bounds::new(origin, size(px(split_x), px(h)));
        let right_clip = Bounds::new(
            point(origin.x + px(split_x), origin.y),
            size(px((w - split_x).max(0.0)), px(h)),
        );
        paint_layers(left_clip, 1.0, window);
        paint_layers(right_clip, 0.28, window);

        let x = x_for(h_idx);
        for layer in &spec.area_layers {
            if let Some(Some(v)) = layer.values.get(h_idx) {
                let y = y_for(*v);
                for (r, bg) in [
                    (4.4_f32, rgba((BG_RECESSED << 8) | 0xFF)),
                    (2.8, rgba((layer.color << 8) | 0xFF)),
                ] {
                    let mut dot = fill(
                        Bounds::new(point(x - px(r), y - px(r)), size(px(r * 2.0), px(r * 2.0))),
                        bg,
                    );
                    dot.corner_radii = px(r).into();
                    window.paint_quad(dot);
                }
            }
        }
    } else {
        paint_layers(full_clip, 1.0, window);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_single_area_series(
    origin: Point<Pixels>,
    full_w: f32,
    reveal_w: f32,
    h: f32,
    base_y: Pixels,
    slot_w: f32,
    spec: &PlotCanvasSpec,
    pinned_val: Option<f64>,
    x_for: &impl Fn(usize) -> Pixels,
    y_for: &impl Fn(f64) -> Pixels,
    window: &mut Window,
) {
    let n = spec.values.len();
    let runs = contiguous_known_runs(&spec.values);
    let mut isolated_known = vec![false; n];
    let pin_split = spec
        .pinned_idx
        .zip(pinned_val)
        .map(|(idx, val)| (x_for(idx), y_for(val)));

    let paint_runs = |clip: Bounds<Pixels>, dim_factor: f32, window: &mut Window| {
        for run in &runs {
            if run.len() >= 2 {
                let pts: Vec<Point<Pixels>> = run
                    .iter()
                    .map(|&(i, v)| point(x_for(i), y_for(v)))
                    .collect();
                paint_monotone_area_and_stroke(
                    clip,
                    &pts,
                    base_y,
                    CurveStyle {
                        color: spec.metric_color,
                        fill_alpha: 0x22,
                        stroke_width: 2.0,
                        dim_factor,
                    },
                    window,
                );
            }
        }
    };

    for run in &runs {
        if run.len() == 1
            && let Some(&(idx, _)) = run.first()
        {
            isolated_known[idx] = true;
        }
    }

    let full_clip = Bounds::new(origin, size(px(reveal_w), px(h)));
    if let Some((pin_x, pin_y)) = pin_split {
        for run in &runs {
            if run.len() >= 2 {
                let pts: Vec<Point<Pixels>> = run
                    .iter()
                    .map(|&(i, v)| point(x_for(i), y_for(v)))
                    .collect();
                paint_baseline_split_curve(full_clip, &pts, pin_x, pin_y, base_y, window);
            }
        }
    } else if let Some((a, b)) = spec.selected_range {
        let lo = a.min(b);
        let hi = a.max(b);
        let x_lo = f32::from(x_for(lo) - origin.x).clamp(0.0, reveal_w);
        let x_hi = f32::from(x_for(hi) - origin.x).clamp(x_lo, reveal_w);
        paint_runs(full_clip, 0.25, window);
        let range_clip = Bounds::new(
            point(origin.x + px(x_lo), origin.y),
            size(px((x_hi - x_lo).max(1.0)), px(h)),
        );
        paint_runs(range_clip, 1.0, window);
    } else if let Some(h_idx) = spec.hovered_idx {
        // Liveline signature: crisp 100% left of cursor, smoothly dimmed right of cursor!
        let split_x = spec
            .cursor_norm_x
            .map(|nx| (nx.clamp(0.0, 1.0) * full_w).clamp(1.0, reveal_w))
            .unwrap_or_else(|| f32::from(x_for(h_idx) - origin.x).clamp(1.0, reveal_w));
        let left_clip = Bounds::new(origin, size(px(split_x), px(h)));
        let right_clip = Bounds::new(
            point(origin.x + px(split_x), origin.y),
            size(px((reveal_w - split_x).max(0.0)), px(h)),
        );
        paint_runs(left_clip, 1.0, window);
        paint_runs(right_clip, 0.26, window);
    } else {
        paint_runs(full_clip, 1.0, window);
    }

    let peak_idx = peak_index(&spec.values);
    let last_idx = last_known_index(&spec.values);
    for (i, val) in spec.values.iter().enumerate() {
        let x = x_for(i);
        let is_hovered = spec.hovered_idx == Some(i);
        let is_pinned = spec.pinned_idx == Some(i);
        match *val {
            Some(v) => {
                if is_hovered || is_pinned {
                    continue;
                }
                let y = y_for(v);
                let dot_color = if let Some(pv) = pinned_val {
                    if spec.pinned_idx.is_some_and(|pi| i < pi) {
                        0x71717A
                    } else if v >= pv {
                        ACCENT_POS
                    } else {
                        ACCENT_ERR
                    }
                } else {
                    spec.metric_color
                };

                // Clean solid live endpoint dot + dashed trace to right Y-axis when idle
                if last_idx == Some(i)
                    && spec.hovered_idx.is_none()
                    && spec.selected_range.is_none()
                {
                    paint_dashed_hline(
                        x + px(5.0),
                        origin.x + px(full_w),
                        y,
                        3.0,
                        3.0,
                        rgba(with_alpha(dot_color, 0x44)),
                        window,
                    );
                    for (r, bg) in [
                        (4.5_f32, rgba((BG_RECESSED << 8) | 0xFF)),
                        (3.0, rgba((dot_color << 8) | 0xFF)),
                    ] {
                        let mut ring = fill(
                            Bounds::new(
                                point(x - px(r), y - px(r)),
                                size(px(r * 2.0), px(r * 2.0)),
                            ),
                            bg,
                        );
                        ring.corner_radii = px(r).into();
                        window.paint_quad(ring);
                    }
                } else if peak_idx == Some(i) && spec.hovered_idx.is_none() && pinned_val.is_none()
                {
                    for (r, bg) in [
                        (3.6_f32, rgba((BG_RECESSED << 8) | 0xFF)),
                        (2.3, rgba((dot_color << 8) | 0xFF)),
                    ] {
                        let mut ring = fill(
                            Bounds::new(
                                point(x - px(r), y - px(r)),
                                size(px(r * 2.0), px(r * 2.0)),
                            ),
                            bg,
                        );
                        ring.corner_radii = px(r).into();
                        window.paint_quad(ring);
                    }
                } else if isolated_known[i] {
                    let mut dot = fill(
                        Bounds::new(point(x - px(2.6), y - px(2.6)), size(px(5.2), px(5.2))),
                        rgb(dot_color),
                    );
                    dot.corner_radii = px(2.6).into();
                    window.paint_quad(dot);
                }
            }
            None => {
                let stub_w = (slot_w * 0.50).clamp(5.0, 14.0);
                let stub_h = 8.0_f32;
                let mut stub = fill(
                    Bounds::new(
                        point(x - px(stub_w * 0.5), base_y - px(stub_h)),
                        size(px(stub_w), px(stub_h)),
                    ),
                    pattern_slash(rgba(with_alpha(ACCENT_AMBER, 0x66)).into(), 1.2, 3.6),
                );
                stub.corner_radii = Corners {
                    top_left: px(2.0),
                    top_right: px(2.0),
                    bottom_left: px(0.0),
                    bottom_right: px(0.0),
                };
                window.paint_quad(stub);
                window.paint_quad(fill(
                    Bounds::new(
                        point(x - px(stub_w * 0.5), base_y - px(stub_h)),
                        size(px(stub_w), px(1.5)),
                    ),
                    rgb(ACCENT_AMBER),
                ));
            }
        }
    }
}

/// Detent-inspired Baseline Split curve with layer-isolated DX11 paths.
fn paint_baseline_split_curve(
    clip_bounds: Bounds<Pixels>,
    pts: &[Point<Pixels>],
    pin_x: Pixels,
    pin_y: Pixels,
    base_y: Pixels,
    window: &mut Window,
) {
    let segs = monotone_cubic_segments(pts);
    if segs.is_empty() {
        return;
    }
    let min_x = f32::from(pts[0].x);
    let max_x = f32::from(pts[pts.len() - 1].x);
    let px_f = f32::from(pin_x);
    let py_f = f32::from(pin_y);

    let mut samples: Vec<(f32, f32)> = Vec::new();
    let mut x = min_x;
    while x < max_x {
        if let Some(y) = sample_segments_y(&segs, x) {
            samples.push((x, y));
        }
        x += 2.5;
    }
    if let Some(y) = sample_segments_y(&segs, max_x) {
        samples.push((max_x, y));
    }
    if px_f > min_x + 0.5
        && px_f < max_x - 0.5
        && let Some(y) = sample_segments_y(&segs, px_f)
    {
        samples.push((px_f, y));
        samples.sort_by(|a, b| a.0.total_cmp(&b.0));
    }

    let pre: Vec<(f32, f32)> = samples
        .iter()
        .copied()
        .filter(|(sx, _)| *sx <= px_f + 0.25)
        .collect();
    if pre.len() >= 2 {
        let mut area = PathBuilder::fill();
        area.move_to(point(px(pre[0].0), base_y));
        for &(sx, sy) in &pre {
            area.line_to(point(px(sx), px(sy)));
        }
        area.line_to(point(px(pre[pre.len() - 1].0), base_y));
        area.close();
        if let Ok(path) = area.build() {
            window.paint_layer(clip_bounds, |window| {
                window.paint_path(path, rgba(0xFFFFFF0A));
            });
        }
        window.paint_layer(clip_bounds, |window| {
            paint_polyline_stroke(&pre, 1.5, rgba(0xFFFFFF36), window);
        });
    }

    let post: Vec<(f32, f32)> = samples
        .iter()
        .copied()
        .filter(|(sx, _)| *sx >= px_f - 0.25)
        .collect();
    if post.len() < 2 {
        return;
    }

    let mut run: Vec<(f32, f32)> = vec![post[0]];
    let mut is_above = post[0].1 <= py_f;

    let flush_run = |run: &[(f32, f32)], above: bool, window: &mut Window| {
        if run.len() < 2 {
            return;
        }
        let col = if above { ACCENT_POS } else { ACCENT_ERR };
        let build_poly = || {
            let mut area = PathBuilder::fill();
            area.move_to(point(px(run[0].0), pin_y));
            for &(sx, sy) in run {
                area.line_to(point(px(sx), px(sy)));
            }
            area.line_to(point(px(run[run.len() - 1].0), pin_y));
            area.close();
            area.build()
        };
        if let Ok(path) = build_poly() {
            window.paint_layer(clip_bounds, |window| {
                window.paint_path(path, rgba(with_alpha(col, 0x1F)));
            });
        }
        if !above && let Ok(path) = build_poly() {
            window.paint_layer(clip_bounds, |window| {
                window.paint_path(
                    path,
                    pattern_slash(rgba(with_alpha(col, 0x36)).into(), 1.1, 4.6),
                );
            });
        }
        window.paint_layer(clip_bounds, |window| {
            paint_polyline_stroke(run, 1.85, rgb(col), window);
        });
    };

    for pair in post.windows(2) {
        let (x0, y0) = pair[0];
        let (x1, y1) = pair[1];
        let next_above = y1 <= py_f;
        if next_above != is_above && (y1 - y0).abs() > 1e-4 {
            let t = ((py_f - y0) / (y1 - y0)).clamp(0.0, 1.0);
            let cx = x0 + (x1 - x0) * t;
            run.push((cx, py_f));
            flush_run(&run, is_above, window);
            run.clear();
            run.push((cx, py_f));
            run.push((x1, y1));
            is_above = next_above;
        } else {
            run.push((x1, y1));
        }
    }
    flush_run(&run, is_above, window);
}

fn paint_polyline_stroke(
    pts: &[(f32, f32)],
    width: f32,
    color: impl Into<gpui::Background>,
    window: &mut Window,
) {
    if pts.len() < 2 {
        return;
    }
    let mut line = PathBuilder::stroke(px(width));
    line.move_to(point(px(pts[0].0), px(pts[0].1)));
    for &(sx, sy) in &pts[1..] {
        line.line_to(point(px(sx), px(sy)));
    }
    if let Ok(path) = line.build() {
        window.paint_path(path, color);
    }
}
