use gpui::{
    Bounds, Corners, Pixels, Point, Window, fill, pattern_slash, point, px, rgb, rgba, size,
};

use super::{
    chart_curves::paint_area_series,
    chart_math::{layout_stacked_bar, peak_index, staggered_bar_reveal},
    format::ChartStyle,
    theme::{
        ACCENT_AMBER, ACCENT_ERR, ACCENT_POS, BG_RECESSED, BORDER_STRONG, BORDER_SUBTLE, GRID_LINE,
        TEXT_PRIMARY, with_alpha,
    },
};

pub const PLOT_PAD_TOP: f32 = 16.0;
pub const PLOT_PAD_BOTTOM: f32 = 12.0;

/// Precomputed per-bar stacked segments `(value_weight, rgb_hex)` top-to-bottom.
#[derive(Clone, Debug)]
pub struct BarColumnData {
    pub value: Option<f64>,
    pub segments: Vec<(u64, u32)>,
}

/// Optional multi-series overlay curve for `Area` mode (`Mix` or `Providers`).
#[derive(Clone, Debug)]
pub struct AreaLayerData {
    pub color: u32,
    pub values: Vec<Option<f64>>,
}

/// Complete render specification passed into the DX11 GPUI chart canvas.
#[derive(Clone)]
pub struct PlotCanvasSpec {
    pub values: Vec<Option<f64>>,
    pub bars: Vec<BarColumnData>,
    pub area_layers: Vec<AreaLayerData>,
    pub max_scale: f64,
    pub avg_val: Option<f64>,
    pub metric_color: u32,
    pub style: ChartStyle,
    pub hovered_idx: Option<usize>,
    pub pinned_idx: Option<usize>,
    pub selected_range: Option<(usize, usize)>,
    pub cursor_norm_x: Option<f32>,
    pub cursor_norm_y: Option<f32>,
    /// Entrance / mode-switch reveal progress in `[0.0, 1.0]`.
    pub reveal_t: f32,
    /// Marks the final bucket as live/in-progress (EvilCharts hatched buffer motif).
    pub live_last_bucket: bool,
}

/// Shared Y coordinate inside a plot of logical height `h` for value `v` on `[0, max_scale]`.
#[inline]
pub fn plot_local_y(h: f32, v: f64, max_scale: f64) -> f32 {
    let usable_h = (h - PLOT_PAD_TOP - PLOT_PAD_BOTTOM).max(1.0);
    let norm = if max_scale > 0.0 {
        (v / max_scale).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    PLOT_PAD_TOP + (1.0 - norm) * usable_h
}

/// Main DX11 GPU plot painter supporting Area/Spline, Multi-Series Area, Wave Stacked Bars,
/// Lollipop/Dots, Drag-to-Select Range Bands, Click-to-Pin Baseline Splits, and Spring Crosshairs.
pub fn paint_plot_canvas(bounds: Bounds<Pixels>, window: &mut Window, spec: &PlotCanvasSpec) {
    let w = f32::from(bounds.size.width);
    let h = f32::from(bounds.size.height);
    if w <= 6.0 || h <= 6.0 {
        return;
    }
    let origin = bounds.origin;
    let base_y = origin.y + px(h - PLOT_PAD_BOTTOM);
    let usable_h = (h - PLOT_PAD_TOP - PLOT_PAD_BOTTOM).max(1.0);

    paint_backdrop_grid(origin, w, h, usable_h, window);

    let pinned_val = spec
        .pinned_idx
        .and_then(|idx| spec.values.get(idx).copied().flatten());
    let y_for = |v: f64| -> Pixels { origin.y + px(plot_local_y(h, v, spec.max_scale)) };

    if let Some(avg) = spec.avg_val
        && avg > 0.0
        && spec.max_scale > 0.0
    {
        let avg_y = y_for(avg);
        let dash_alpha = if pinned_val.is_some() { 0x22 } else { 0x4E };
        paint_dashed_hline(
            origin.x,
            origin.x + px(w),
            avg_y,
            4.0,
            4.0,
            rgba(with_alpha(spec.metric_color, dash_alpha)),
            window,
        );
    }

    if spec.values.is_empty() {
        return;
    }

    let n = spec.values.len();
    let slot_w = w / n as f32;
    let x_for = |i: usize| -> Pixels { origin.x + px((i as f32 + 0.5) * slot_w) };

    // Drag-to-select range band backdrop (behind series geometry)
    if let Some((a, b)) = spec.selected_range {
        let lo = a.min(b).min(n - 1);
        let hi = a.max(b).min(n - 1);
        let x_lo = x_for(lo) - px(slot_w * 0.45);
        let x_hi = x_for(hi) + px(slot_w * 0.45);
        let band_w = (x_hi - x_lo).max(px(6.0));
        window.paint_quad(fill(
            Bounds::new(point(x_lo, origin.y), size(band_w, px(h))),
            rgba(with_alpha(spec.metric_color, 0x16)),
        ));
        for rail_x in [x_lo, x_lo + band_w - px(1.5)] {
            window.paint_quad(fill(
                Bounds::new(point(rail_x, origin.y), size(px(1.5), px(h))),
                rgba(with_alpha(spec.metric_color, 0x99)),
            ));
        }
    }

    match spec.style {
        ChartStyle::AreaLine => {
            paint_area_series(
                origin, w, h, base_y, slot_w, spec, pinned_val, &x_for, &y_for, window,
            );
        }
        ChartStyle::StackedBar => {
            paint_bar_series(origin, h, base_y, usable_h, slot_w, spec, &x_for, window);
        }
        ChartStyle::Lollipop => {
            paint_lollipop_series(base_y, usable_h, spec, pinned_val, &x_for, &y_for, window);
        }
    }

    // Pinned Baseline anchor rule + hollow machined ring
    let mut pin_y_px: Option<Pixels> = None;
    if let (Some(pin_idx), Some(pin_v)) = (spec.pinned_idx, pinned_val) {
        let px_x = x_for(pin_idx);
        let px_y = y_for(pin_v);
        pin_y_px = Some(px_y);

        paint_dashed_hline(
            origin.x,
            origin.x + px(w),
            px_y,
            5.0,
            3.5,
            rgba(0xFFFFFF54),
            window,
        );
        window.paint_quad(fill(
            Bounds::new(
                point(px_x - px(0.5), px_y + px(5.0)),
                size(px(1.0), (base_y - px_y - px(5.0)).max(px(0.0))),
            ),
            rgba(0xFFFFFF38),
        ));
        paint_anchor_ring(px_x, px_y, TEXT_PRIMARY, window);
    }

    if let Some(h_idx) = spec.hovered_idx
        && h_idx < n
        && spec.selected_range.is_none()
    {
        paint_active_cursor(
            origin, w, h, base_y, usable_h, slot_w, h_idx, pinned_val, pin_y_px, spec, &x_for,
            &y_for, window,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_active_cursor(
    origin: Point<Pixels>,
    w: f32,
    h: f32,
    base_y: Pixels,
    usable_h: f32,
    slot_w: f32,
    h_idx: usize,
    pinned_val: Option<f64>,
    pin_y_px: Option<Pixels>,
    spec: &PlotCanvasSpec,
    x_for: &impl Fn(usize) -> Pixels,
    y_for: &impl Fn(f64) -> Pixels,
    window: &mut Window,
) {
    let target_x = x_for(h_idx);
    let cur_x = spec
        .cursor_norm_x
        .map(|nx| origin.x + px(nx.clamp(0.0, 1.0) * w))
        .unwrap_or(target_x);

    match spec.values[h_idx] {
        Some(v) => {
            let target_y =
                if spec.style == ChartStyle::StackedBar && v > 0.0 && spec.max_scale > 0.0 {
                    let bar_h =
                        ((v / spec.max_scale) * usable_h as f64).clamp(3.0, usable_h as f64) as f32;
                    base_y - px(bar_h)
                } else {
                    y_for(v)
                };
            let cur_y = if spec.style == ChartStyle::StackedBar {
                target_y
            } else {
                spec.cursor_norm_y
                    .map(|ny| origin.y + px(PLOT_PAD_TOP + (1.0 - ny.clamp(0.0, 1.0)) * usable_h))
                    .unwrap_or(target_y)
            };

            let active_color = if let Some(pv) = pinned_val {
                if Some(h_idx) == spec.pinned_idx {
                    TEXT_PRIMARY
                } else if v >= pv {
                    ACCENT_POS
                } else {
                    ACCENT_ERR
                }
            } else {
                spec.metric_color
            };

            if spec.style != ChartStyle::StackedBar {
                window.paint_quad(fill(
                    Bounds::new(point(cur_x - px(0.5), origin.y), size(px(1.0), px(h))),
                    rgba(BORDER_STRONG),
                ));
            }

            // Dashed horizontal value trace connecting active point -> right Y-axis pill
            let trace_start = (cur_x + px(5.0)).min(origin.x + px(w));
            paint_dashed_hline(
                trace_start,
                origin.x + px(w),
                cur_y,
                3.0,
                3.0,
                rgba(with_alpha(active_color, 0x66)),
                window,
            );

            // Detent Caliper I-Beam Bracket vs Pinned Baseline or Period Avg
            let ref_y = if Some(h_idx) != spec.pinned_idx {
                pin_y_px.or_else(|| {
                    spec.avg_val
                        .filter(|a| *a > 0.0 && spec.max_scale > 0.0)
                        .map(y_for)
                })
            } else {
                None
            };
            if let Some(ry) = ref_y {
                let delta_px = f32::from(ry) - f32::from(cur_y);
                if delta_px.abs() >= 7.0 {
                    let caliper_color = if delta_px > 0.0 {
                        ACCENT_POS
                    } else {
                        ACCENT_ERR
                    };
                    let caliper_x = if spec.style == ChartStyle::StackedBar {
                        let offset = (slot_w * 0.38).clamp(5.0, 16.0);
                        if f32::from(cur_x - origin.x) + offset < w - 4.0 {
                            cur_x + px(offset)
                        } else {
                            cur_x - px(offset)
                        }
                    } else {
                        cur_x
                    };
                    paint_caliper_bracket(caliper_x, cur_y, ry, caliper_color, window);
                }
            }

            if spec.style == ChartStyle::StackedBar {
                paint_bar_top_beacon(cur_x, cur_y, active_color, window);
            } else {
                paint_glowing_reticle(cur_x, cur_y, active_color, window);
            }
        }
        None => {
            window.paint_quad(fill(
                Bounds::new(point(cur_x - px(0.5), origin.y), size(px(1.0), px(h))),
                rgba(with_alpha(ACCENT_AMBER, 0x55)),
            ));
            paint_glowing_reticle(cur_x, base_y - px(4.0), ACCENT_AMBER, window);
        }
    }
}

fn paint_backdrop_grid(origin: Point<Pixels>, w: f32, _h: f32, usable_h: f32, window: &mut Window) {
    // Crisp shadcn/Vercel horizontal gridlines aligned 1:1 with Y-axis ticks + solid baseline
    for (frac, color, dashed) in [
        (0.0_f32, GRID_LINE, true),
        (0.25_f32, 0x1D1D22FF, true),
        (0.5_f32, GRID_LINE, true),
        (0.75_f32, 0x1D1D22FF, true),
        (1.0_f32, BORDER_STRONG, false),
    ] {
        let y = origin.y + px((PLOT_PAD_TOP + usable_h * frac).round());
        if dashed {
            paint_dashed_hline(origin.x, origin.x + px(w), y, 3.0, 4.0, rgba(color), window);
        } else {
            window.paint_quad(fill(
                Bounds::new(point(origin.x, y), size(px(w), px(1.0))),
                rgba(color),
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_bar_series(
    origin: Point<Pixels>,
    h: f32,
    base_y: Pixels,
    usable_h: f32,
    slot_w: f32,
    spec: &PlotCanvasSpec,
    x_for: &impl Fn(usize) -> Pixels,
    window: &mut Window,
) {
    let _ = h;
    let n = spec.bars.len();
    let bar_w = (slot_w * 0.62).clamp(6.0, 30.0);
    let slot_bg_w = (slot_w - 2.0).max(bar_w + 4.0);
    let base_y_f = f32::from(base_y).round();

    for (i, col) in spec.bars.iter().enumerate() {
        let cx = x_for(i);
        let bar_x = px((f32::from(cx) - bar_w * 0.5).round());
        let is_hovered = spec.hovered_idx == Some(i) && spec.selected_range.is_none();
        let is_pinned = spec.pinned_idx == Some(i);
        let in_range = spec
            .selected_range
            .is_some_and(|(a, b)| i >= a.min(b) && i <= a.max(b));
        let dimmed = if spec.selected_range.is_some() {
            !in_range
        } else {
            spec.hovered_idx.is_some() && !is_hovered && !is_pinned
        };
        let wave = staggered_bar_reveal(spec.reveal_t, i, n).max(0.02);
        let is_live_buffer = spec.live_last_bucket && i + 1 == n;

        if is_hovered {
            let mut spot = fill(
                Bounds::new(
                    point(cx - px(slot_bg_w * 0.5), origin.y + px(PLOT_PAD_TOP - 4.0)),
                    size(
                        px(slot_bg_w),
                        px(base_y_f - f32::from(origin.y) - (PLOT_PAD_TOP - 4.0)),
                    ),
                ),
                rgba(0xFFFFFF09),
            );
            spot.corner_radii = Corners {
                top_left: px(4.0),
                top_right: px(4.0),
                bottom_left: px(0.0),
                bottom_right: px(0.0),
            };
            window.paint_quad(spot);
        }

        match col.value {
            Some(v) if v > 0.0 && spec.max_scale > 0.0 => {
                let total_h = (((v * f64::from(wave)) / spec.max_scale) * usable_h as f64)
                    .clamp(3.0, usable_h as f64) as f32;
                let bar_top = base_y_f - total_h;

                if col.segments.is_empty() {
                    paint_flat_bar_segment(
                        bar_x,
                        bar_top,
                        bar_w,
                        total_h,
                        spec.metric_color,
                        true,
                        dimmed,
                        window,
                    );
                } else {
                    let weights: Vec<u64> = col.segments.iter().map(|(w, _)| *w).collect();
                    let slices = layout_stacked_bar(base_y_f, total_h, &weights);
                    for (pos, slice) in slices.iter().enumerate() {
                        let (_, seg_col) = col.segments[slice.index];
                        paint_flat_bar_segment(
                            bar_x,
                            slice.y,
                            bar_w,
                            slice.height,
                            seg_col,
                            pos == 0,
                            dimmed,
                            window,
                        );
                        // Crisp 1px matte seam between adjacent slices (never shifts baseline)
                        if pos > 0 {
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(bar_x, px(slice.y - 0.5)),
                                    size(px(bar_w), px(1.0)),
                                ),
                                rgba((BG_RECESSED << 8) | 0xD8),
                            ));
                        }
                    }
                }

                // Subtle diagonal hatch over live in-progress bucket, flush with baseline
                if is_live_buffer && !dimmed && total_h >= 6.0 {
                    let hatch_r = 3.0_f32.min(total_h * 0.8).min(bar_w * 0.4);
                    let mut hatch = fill(
                        Bounds::new(point(bar_x, px(bar_top)), size(px(bar_w), px(total_h))),
                        pattern_slash(rgba(0x07090C44).into(), 1.2, 4.0),
                    );
                    hatch.corner_radii = Corners {
                        top_left: px(hatch_r),
                        top_right: px(hatch_r),
                        bottom_left: px(0.0),
                        bottom_right: px(0.0),
                    };
                    window.paint_quad(hatch);
                }
            }
            Some(_) => {
                // Zero-usage tick seated flush on the baseline
                window.paint_quad(fill(
                    Bounds::new(point(bar_x, px(base_y_f - 2.0)), size(px(bar_w), px(2.0))),
                    rgba(BORDER_SUBTLE),
                ));
            }
            None => {
                let stub_h = 12.0_f32;
                let mut stub = fill(
                    Bounds::new(
                        point(bar_x, px(base_y_f - stub_h)),
                        size(px(bar_w), px(stub_h)),
                    ),
                    pattern_slash(
                        rgba(with_alpha(ACCENT_AMBER, if dimmed { 0x34 } else { 0x76 })).into(),
                        1.2,
                        3.6,
                    ),
                );
                stub.corner_radii = Corners {
                    top_left: px(2.5),
                    top_right: px(2.5),
                    bottom_left: px(0.0),
                    bottom_right: px(0.0),
                };
                window.paint_quad(stub);
                window.paint_quad(fill(
                    Bounds::new(
                        point(bar_x, px(base_y_f - stub_h)),
                        size(px(bar_w), px(1.6)),
                    ),
                    rgba(with_alpha(ACCENT_AMBER, if dimmed { 0x60 } else { 0xFF })),
                ));
            }
        }
    }
}

/// Detent Evals / Lollipop Dot Plot mode: gradient stems rising to luminous glowing beacons.
fn paint_lollipop_series(
    base_y: Pixels,
    usable_h: f32,
    spec: &PlotCanvasSpec,
    pinned_val: Option<f64>,
    x_for: &impl Fn(usize) -> Pixels,
    y_for: &impl Fn(f64) -> Pixels,
    window: &mut Window,
) {
    let n = spec.values.len();
    let peak_idx = peak_index(&spec.values);
    let anchor_y = pinned_val.map(y_for).unwrap_or(base_y);

    for (i, val) in spec.values.iter().enumerate() {
        let cx = x_for(i);
        let is_hovered = spec.hovered_idx == Some(i) && spec.selected_range.is_none();
        let is_pinned = spec.pinned_idx == Some(i);
        let in_range = spec
            .selected_range
            .is_some_and(|(a, b)| i >= a.min(b) && i <= a.max(b));
        let dimmed = if spec.selected_range.is_some() {
            !in_range
        } else {
            spec.hovered_idx.is_some() && !is_hovered && !is_pinned
        };
        let wave = staggered_bar_reveal(spec.reveal_t, i, n).max(0.02);

        if let Some(v) = *val {
            let dot_col = if let Some(pv) = pinned_val {
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
            let target_y = y_for(v);
            let cur_y = anchor_y + px((f32::from(target_y) - f32::from(anchor_y)) * wave);
            let top = cur_y.min(anchor_y);
            let bot = cur_y.max(anchor_y);
            let stem_h = (bot - top).max(px(1.0));
            let stem_w = if is_hovered || is_pinned || in_range {
                2.0
            } else {
                1.5
            };
            let stem_col = if dimmed {
                rgba(0x27272A88)
            } else if is_hovered || is_pinned || in_range {
                rgba(with_alpha(dot_col, 0xCC))
            } else {
                rgba(BORDER_STRONG)
            };

            window.paint_quad(fill(
                Bounds::new(point(cx - px(stem_w * 0.5), top), size(px(stem_w), stem_h)),
                stem_col,
            ));

            let outer_r = if is_hovered || in_range {
                4.8
            } else if peak_idx == Some(i) {
                4.2
            } else {
                3.6
            };
            let core_a = if dimmed { 0x4D } else { 0xFF };
            for (r, bg) in [
                (outer_r + 1.5, rgba((BG_RECESSED << 8) | 0xFF)),
                (outer_r, rgba(with_alpha(dot_col, core_a))),
                (outer_r * 0.42, rgba((BG_RECESSED << 8) | 0xFF)),
            ] {
                let mut ring = fill(
                    Bounds::new(
                        point(cx - px(r), cur_y - px(r)),
                        size(px(r * 2.0), px(r * 2.0)),
                    ),
                    bg,
                );
                ring.corner_radii = px(r).into();
                window.paint_quad(ring);
            }
        } else {
            let _ = usable_h;
            let mut dot = fill(
                Bounds::new(
                    point(cx - px(3.0), base_y - px(6.0)),
                    size(px(6.0), px(6.0)),
                ),
                rgb(ACCENT_AMBER),
            );
            dot.corner_radii = px(3.0).into();
            window.paint_quad(dot);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_flat_bar_segment(
    x: Pixels,
    y: f32,
    w: f32,
    h: f32,
    color: u32,
    is_top: bool,
    dimmed: bool,
    window: &mut Window,
) {
    let alpha = if dimmed { 0x45 } else { 0xF5 };
    let mut quad = fill(
        Bounds::new(point(x, px(y)), size(px(w), px(h))),
        rgba(with_alpha(color, alpha)),
    );
    quad.corner_radii = if is_top {
        // Strictly bounded radius (r <= h * 0.8 and 0 bottom) prevents GPUI SDF hat artifacts
        let r = 3.0_f32.min(h * 0.8).min(w * 0.4);
        Corners {
            top_left: px(r),
            top_right: px(r),
            bottom_left: px(0.0),
            bottom_right: px(0.0),
        }
    } else {
        Corners::default()
    };
    window.paint_quad(quad);
}

fn paint_caliper_bracket(x: Pixels, y_a: Pixels, y_b: Pixels, color: u32, window: &mut Window) {
    let top = y_a.min(y_b);
    let bot = y_a.max(y_b);
    window.paint_quad(fill(
        Bounds::new(
            point(x - px(0.75), top),
            size(px(1.5), (bot - top).max(px(1.0))),
        ),
        rgba(with_alpha(color, 0xCC)),
    ));
    for cap_y in [top, bot] {
        let mut cap = fill(
            Bounds::new(point(x - px(4.0), cap_y - px(0.75)), size(px(8.0), px(1.5))),
            rgb(color),
        );
        cap.corner_radii = px(0.75).into();
        window.paint_quad(cap);
    }
}

fn paint_anchor_ring(x: Pixels, y: Pixels, color: u32, window: &mut Window) {
    for (r, bg) in [
        (4.5_f32, rgba((color << 8) | 0xFF)),
        (2.4, rgba((BG_RECESSED << 8) | 0xFF)),
    ] {
        let mut ring = fill(
            Bounds::new(point(x - px(r), y - px(r)), size(px(r * 2.0), px(r * 2.0))),
            bg,
        );
        ring.corner_radii = px(r).into();
        window.paint_quad(ring);
    }
}

fn paint_glowing_reticle(x: Pixels, y: Pixels, color: u32, window: &mut Window) {
    // Crisp shadcn/Apple solid active point with recessed ring cutout (zero glow)
    for (r, bg) in [
        (5.2_f32, rgba((BG_RECESSED << 8) | 0xFF)),
        (3.4, rgba((color << 8) | 0xFF)),
    ] {
        let mut ring = fill(
            Bounds::new(point(x - px(r), y - px(r)), size(px(r * 2.0), px(r * 2.0))),
            bg,
        );
        ring.corner_radii = px(r).into();
        window.paint_quad(ring);
    }
}

fn paint_bar_top_beacon(x: Pixels, y: Pixels, color: u32, window: &mut Window) {
    for (r, bg) in [
        (3.5_f32, rgba((BG_RECESSED << 8) | 0xFF)),
        (2.2, rgba((color << 8) | 0xFF)),
    ] {
        let mut dot = fill(
            Bounds::new(point(x - px(r), y - px(r)), size(px(r * 2.0), px(r * 2.0))),
            bg,
        );
        dot.corner_radii = px(r).into();
        window.paint_quad(dot);
    }
}

pub fn paint_dashed_hline(
    x_start: Pixels,
    x_end: Pixels,
    y: Pixels,
    dash: f32,
    gap: f32,
    color: gpui::Rgba,
    window: &mut Window,
) {
    let end = f32::from(x_end);
    let mut x = f32::from(x_start);
    let step = dash + gap;
    while x < end {
        let seg = dash.min(end - x);
        if seg > 0.2 {
            window.paint_quad(fill(
                Bounds::new(point(px(x), y), size(px(seg), px(1.0))),
                color,
            ));
        }
        x += step;
    }
}
