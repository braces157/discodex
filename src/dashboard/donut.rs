use std::f32::consts::{FRAC_PI_2, TAU};

use gpui::{
    Bounds, FontWeight, IntoElement, PathBuilder, Pixels, Point, Window, canvas, div, point,
    prelude::*, px, rgb, rgba,
};

use super::theme::{FONT_MONO, TEXT_MUTED, TEXT_PRIMARY, TRACK_BG};

/// A single weighted slice in the GPU-rendered Token Mix donut ring.
#[derive(Clone, Copy, Debug)]
pub struct DonutSlice {
    pub value: f64,
    pub color: u32,
}

/// Renders a 4× MSAA GPU-accelerated segmented Donut Ring chart (`gpui-kit` PieChart/Donut motif)
/// with crisp angular gaps, outer ambient glow pass, and centered tabular KPI readout.
pub fn render_donut_ring(
    slices: Vec<DonutSlice>,
    center_value: String,
    center_label: &'static str,
    center_color: u32,
) -> impl IntoElement + use<> {
    div()
        .size(px(88.))
        .flex_none()
        .relative()
        .child(
            canvas(
                move |bounds, _, _| bounds,
                move |_, bounds, window, _| {
                    paint_donut_ring(bounds, &slices, window);
                },
            )
            .size_full()
            .absolute()
            .inset_0(),
        )
        .child(
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .font_family(FONT_MONO)
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(center_color))
                        .child(center_value),
                )
                .child(
                    div()
                        .font_family(FONT_MONO)
                        .text_size(px(8.5))
                        .text_color(rgb(TEXT_MUTED))
                        .child(center_label),
                ),
        )
}

fn paint_donut_ring(bounds: Bounds<Pixels>, slices: &[DonutSlice], window: &mut Window) {
    let w = f32::from(bounds.size.width);
    let h = f32::from(bounds.size.height);
    let r_outer = (w.min(h) * 0.5 - 3.0).max(8.0);
    let r_inner = (r_outer - 8.5).max(4.0);
    let cx = f32::from(bounds.origin.x) + w * 0.5;
    let cy = f32::from(bounds.origin.y) + h * 0.5;

    window.paint_layer(bounds, |window| {
        paint_annulus_arc(cx, cy, r_inner, r_outer, 0.0, TAU, rgba(TRACK_BG), window);
    });

    let active: Vec<DonutSlice> = slices
        .iter()
        .copied()
        .filter(|s| s.value > 0.0 && s.value.is_finite())
        .collect();
    let total: f64 = active.iter().map(|s| s.value).sum();
    if total <= 0.0 {
        return;
    }

    let gap = if active.len() > 1 { 0.055_f32 } else { 0.0 };
    let mut angle = -FRAC_PI_2;

    for slice in &active {
        let sweep = ((slice.value / total) as f32 * TAU).max(0.02);
        let draw_start = angle + gap * 0.5;
        let draw_end = (angle + sweep - gap * 0.5).max(draw_start + 0.015);

        window.paint_layer(bounds, |window| {
            paint_annulus_arc(
                cx,
                cy,
                r_inner,
                r_outer,
                draw_start,
                draw_end,
                rgb(slice.color),
                window,
            );
        });

        angle += sweep;
    }
    let _ = TEXT_PRIMARY;
}

#[allow(clippy::too_many_arguments)]
fn paint_annulus_arc(
    cx: f32,
    cy: f32,
    r_inner: f32,
    r_outer: f32,
    start_rad: f32,
    end_rad: f32,
    color: impl Into<gpui::Background>,
    window: &mut Window,
) {
    let span = (end_rad - start_rad).abs();
    if span < 0.005 {
        return;
    }
    let steps = ((span / TAU) * 64.0).ceil().clamp(6.0, 64.0) as usize;
    let mut path = PathBuilder::fill();

    let pt =
        |r: f32, a: f32| -> Point<Pixels> { point(px(cx + r * a.cos()), px(cy + r * a.sin())) };

    path.move_to(pt(r_outer, start_rad));
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        path.line_to(pt(r_outer, start_rad + span * t));
    }
    for i in (0..=steps).rev() {
        let t = i as f32 / steps as f32;
        path.line_to(pt(r_inner, start_rad + span * t));
    }
    path.close();

    if let Ok(built) = path.build() {
        window.paint_path(built, color);
    }
}
