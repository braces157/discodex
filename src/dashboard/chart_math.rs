use gpui::{Pixels, Point, point, px};

/// Analytical closed-form spring state (inspired by Detent UI's `springStep`).
///
/// Exact continuous-time integration avoids Euler instability at any frame delta,
/// settling cleanly to the target so `window.request_animation_frame()` stops at rest.
#[derive(Clone, Copy, Debug)]
pub struct SpringState {
    pub value: f32,
    pub velocity: f32,
    pub target: f32,
    pub initialized: bool,
}

impl Default for SpringState {
    fn default() -> Self {
        Self {
            value: 0.0,
            velocity: 0.0,
            target: 0.0,
            initialized: false,
        }
    }
}

impl SpringState {
    pub fn snap_to(&mut self, value: f32) {
        self.value = value;
        self.velocity = 0.0;
        self.target = value;
        self.initialized = true;
    }

    pub fn set_target(&mut self, target: f32) {
        if !self.initialized {
            self.snap_to(target);
        } else {
            self.target = target;
        }
    }

    pub fn reset(&mut self) {
        self.initialized = false;
        self.velocity = 0.0;
    }

    /// Advances the spring by `dt` seconds with angular frequency `omega` and damping `zeta`.
    /// Returns `true` if still animating (another frame should be requested).
    pub fn step(&mut self, dt: f32, omega: f32, zeta: f32) -> bool {
        if !self.initialized {
            return false;
        }
        let dt = dt.clamp(0.0, 0.05);
        let x0 = self.value - self.target;
        let v0 = self.velocity;
        if x0.abs() < 0.0008 && v0.abs() < 0.002 {
            self.value = self.target;
            self.velocity = 0.0;
            return false;
        }

        let (x, v) = if zeta >= 1.0 - 1e-4 {
            // Critically damped closed form
            let e = (-omega * dt).exp();
            let c1 = x0;
            let c2 = v0 + omega * x0;
            let x = (c1 + c2 * dt) * e;
            let v = (c2 - omega * (c1 + c2 * dt)) * e;
            (x, v)
        } else {
            // Underdamped closed form
            let wd = omega * (1.0 - zeta * zeta).sqrt();
            let e = (-zeta * omega * dt).exp();
            let cos_t = (wd * dt).cos();
            let sin_t = (wd * dt).sin();
            let a = x0;
            let b = (v0 + zeta * omega * x0) / wd;
            let x = e * (a * cos_t + b * sin_t);
            let v =
                e * ((-zeta * omega * a + wd * b) * cos_t - (wd * a + zeta * omega * b) * sin_t);
            (x, v)
        };

        self.value = self.target + x;
        self.velocity = v;
        if x.abs() < 0.0008 && v.abs() < 0.002 {
            self.value = self.target;
            self.velocity = 0.0;
            false
        } else {
            true
        }
    }
}

/// A cubic Bezier segment between `p0` and `p1` with control points `c0` and `c1`.
#[derive(Clone, Copy, Debug)]
pub struct CubicSegment {
    pub p0: Point<Pixels>,
    pub c0: Point<Pixels>,
    pub c1: Point<Pixels>,
    pub p1: Point<Pixels>,
}

/// Computes Fritsch-Carlson monotone cubic Hermite Bezier segments through `pts`.
///
/// Guarantees `C^1` smoothness while strictly preserving monotonicity: the curve never
/// overshoots local peaks or dips below zero valleys.
pub fn monotone_cubic_segments(pts: &[Point<Pixels>]) -> Vec<CubicSegment> {
    let n = pts.len();
    if n < 2 {
        return Vec::new();
    }
    if n == 2 {
        let p0 = pts[0];
        let p1 = pts[1];
        let dx = (f32::from(p1.x) - f32::from(p0.x)) / 3.0;
        return vec![CubicSegment {
            p0,
            c0: point(px(f32::from(p0.x) + dx), p0.y),
            c1: point(px(f32::from(p1.x) - dx), p1.y),
            p1,
        }];
    }

    let xs: Vec<f32> = pts.iter().map(|p| f32::from(p.x)).collect();
    let ys: Vec<f32> = pts.iter().map(|p| f32::from(p.y)).collect();

    let mut dx = vec![0.0_f32; n - 1];
    let mut slope = vec![0.0_f32; n - 1];
    for i in 0..(n - 1) {
        dx[i] = (xs[i + 1] - xs[i]).max(1e-4);
        slope[i] = (ys[i + 1] - ys[i]) / dx[i];
    }

    let mut tangents = vec![0.0_f32; n];
    tangents[0] = slope[0];
    tangents[n - 1] = slope[n - 2];
    for i in 1..(n - 1) {
        if slope[i - 1] * slope[i] <= 0.0 {
            tangents[i] = 0.0;
        } else {
            let w1 = 2.0 * dx[i] + dx[i - 1];
            let w2 = dx[i] + 2.0 * dx[i - 1];
            tangents[i] = (w1 + w2) / (w1 / slope[i - 1] + w2 / slope[i]);
        }
    }

    // Fritsch-Carlson overshoot clamp (alpha^2 + beta^2 <= 9)
    for i in 0..(n - 1) {
        if slope[i].abs() < 1e-6 {
            tangents[i] = 0.0;
            tangents[i + 1] = 0.0;
        } else {
            let alpha = tangents[i] / slope[i];
            let beta = tangents[i + 1] / slope[i];
            let mag_sq = alpha * alpha + beta * beta;
            if mag_sq > 9.0 {
                let tau = 3.0 / mag_sq.sqrt();
                tangents[i] = tau * alpha * slope[i];
                tangents[i + 1] = tau * beta * slope[i];
            }
        }
    }

    let mut out = Vec::with_capacity(n - 1);
    for i in 0..(n - 1) {
        let h = dx[i] / 3.0;
        let c0 = point(px(xs[i] + h), px(ys[i] + tangents[i] * h));
        let c1 = point(px(xs[i + 1] - h), px(ys[i + 1] - tangents[i + 1] * h));
        out.push(CubicSegment {
            p0: pts[i],
            c0,
            c1,
            p1: pts[i + 1],
        });
    }
    out
}

/// Evaluates the exact Y pixel coordinate along monotone cubic `segments` at `x_target`.
///
/// Uses binary search over `t` on the matching monotonic-X cubic segment.
pub fn sample_segments_y(segments: &[CubicSegment], x_target: f32) -> Option<f32> {
    if segments.is_empty() {
        return None;
    }
    let first_x = f32::from(segments[0].p0.x);
    let last_x = f32::from(segments[segments.len() - 1].p1.x);
    if x_target < first_x - 0.5 || x_target > last_x + 0.5 {
        return None;
    }
    let seg = segments
        .iter()
        .find(|s| x_target >= f32::from(s.p0.x) - 0.5 && x_target <= f32::from(s.p1.x) + 0.5)?;

    let x0 = f32::from(seg.p0.x);
    let x1 = f32::from(seg.c0.x);
    let x2 = f32::from(seg.c1.x);
    let x3 = f32::from(seg.p1.x);

    let mut lo = 0.0_f32;
    let mut hi = 1.0_f32;
    for _ in 0..14 {
        let mid = 0.5 * (lo + hi);
        let u = 1.0 - mid;
        let x = u * u * u * x0
            + 3.0 * u * u * mid * x1
            + 3.0 * u * mid * mid * x2
            + mid * mid * mid * x3;
        if x < x_target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let t = 0.5 * (lo + hi);
    let u = 1.0 - t;
    let y0 = f32::from(seg.p0.y);
    let y1 = f32::from(seg.c0.y);
    let y2 = f32::from(seg.c1.y);
    let y3 = f32::from(seg.p1.y);
    Some(u * u * u * y0 + 3.0 * u * u * t * y1 + 3.0 * u * t * t * y2 + t * t * t * y3)
}

/// Computes a clean, human-readable upper Y-axis ceiling (e.g. 1.2, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10).
pub fn compute_nice_scale(max_raw: f64) -> f64 {
    if !max_raw.is_finite() || max_raw <= 0.0 {
        return 1.0;
    }
    let padded = max_raw * 1.10;
    let exp = padded.log10().floor();
    let base = 10.0_f64.powf(exp);
    let frac = padded / base;
    let nice = if frac <= 1.0 {
        1.0
    } else if frac <= 1.2 {
        1.2
    } else if frac <= 1.5 {
        1.5
    } else if frac <= 2.0 {
        2.0
    } else if frac <= 2.5 {
        2.5
    } else if frac <= 3.0 {
        3.0
    } else if frac <= 4.0 {
        4.0
    } else if frac <= 5.0 {
        5.0
    } else if frac <= 6.0 {
        6.0
    } else if frac <= 8.0 {
        8.0
    } else {
        10.0
    };
    nice * base
}

/// Comparison summary between a target point and a reference value (pinned baseline or period avg).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValueDelta {
    pub diff: f64,
    pub pct: Option<f64>,
}

impl ValueDelta {
    pub fn between(current: f64, reference: f64) -> Self {
        let diff = current - reference;
        let pct = if reference.abs() > 1e-9 {
            Some((diff / reference) * 100.0)
        } else {
            None
        };
        Self { diff, pct }
    }

    pub fn is_positive(self) -> bool {
        self.diff > 1e-9
    }

    pub fn is_negative(self) -> bool {
        self.diff < -1e-9
    }
}

/// Formats a signed percentage delta (e.g. `+42.7%`, `-8.4%`, `0%`).
pub fn format_signed_pct(pct: f64) -> String {
    if pct.abs() < 0.05 {
        return "0%".into();
    }
    let sign = if pct > 0.0 { "+" } else { "-" };
    let abs = pct.abs();
    if abs >= 100.0 {
        format!("{sign}{:.0}%", abs)
    } else {
        format!("{sign}{:.1}%", abs)
    }
}

/// Finds the index of the highest known positive value in `values`.
pub fn peak_index(values: &[Option<f64>]) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (idx, val) in values.iter().enumerate() {
        if let Some(v) = *val
            && v > 0.0
            && best.is_none_or(|(_, max_v)| v > max_v)
        {
            best = Some((idx, v));
        }
    }
    best.map(|(idx, _)| idx)
}

/// Returns the last known value index in `values`.
pub fn last_known_index(values: &[Option<f64>]) -> Option<usize> {
    values.iter().rposition(Option::is_some)
}

/// Smooth `easeOutQuart` curve used for left-to-right chart reveal and staggered bar waves.
#[inline]
pub fn ease_out_quart(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let inv = 1.0 - t;
    1.0 - inv * inv * inv * inv
}

/// Computes per-bar staggered wave reveal progress in `[0.0, 1.0]`.
#[inline]
pub fn staggered_bar_reveal(global_t: f32, index: usize, count: usize) -> f32 {
    if global_t >= 1.0 || count <= 1 {
        return ease_out_quart(global_t);
    }
    let stagger_span = 0.38_f32;
    let start = (index as f32 / (count - 1) as f32) * stagger_span;
    let local = ((global_t - start) / (1.0 - stagger_span)).clamp(0.0, 1.0);
    ease_out_quart(local)
}

/// Aggregated statistics for a user-dragged range `[start..=end]` across the chart.
#[derive(Clone, Debug, PartialEq)]
pub struct RangeSummary {
    pub start: usize,
    pub end: usize,
    pub count: usize,
    pub known_count: usize,
    pub total: f64,
    pub avg: Option<f64>,
    pub delta: Option<ValueDelta>,
    pub cost_usd: Option<f64>,
    pub calls: u64,
}

impl RangeSummary {
    pub fn compute(
        values: &[Option<f64>],
        costs: &[Option<f64>],
        calls: &[u64],
        a: usize,
        b: usize,
    ) -> Option<Self> {
        if values.is_empty() {
            return None;
        }
        let max_idx = values.len() - 1;
        let start = a.min(b).min(max_idx);
        let end = a.max(b).min(max_idx);
        let mut total = 0.0_f64;
        let mut known_count = 0usize;
        let mut first_val = None;
        let mut last_val = None;
        let mut cost_sum = 0.0_f64;
        let mut any_cost = false;
        let mut call_sum = 0u64;

        for (idx, val) in values.iter().enumerate().take(end + 1).skip(start) {
            if let Some(v) = *val {
                total += v;
                known_count += 1;
                if first_val.is_none() {
                    first_val = Some(v);
                }
                last_val = Some(v);
            }
            if let Some(Some(c)) = costs.get(idx) {
                cost_sum += *c;
                any_cost = true;
            }
            if let Some(&req) = calls.get(idx) {
                call_sum = call_sum.saturating_add(req);
            }
        }
        let avg = (known_count > 0).then(|| total / known_count as f64);
        let delta = match (first_val, last_val) {
            (Some(s), Some(e)) if end > start => Some(ValueDelta::between(e, s)),
            _ => None,
        };
        Some(Self {
            start,
            end,
            count: end - start + 1,
            known_count,
            total,
            avg,
            delta,
            cost_usd: any_cost.then_some(cost_sum),
            calls: call_sum,
        })
    }
}

/// A single stacked bar slice positioned in top-down screen coordinates (`y` increases downward).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StackedSlice {
    /// Index into the caller's input `weights` slice (`0` = topmost segment).
    pub index: usize,
    /// Top Y coordinate in screen space.
    pub y: f32,
    /// Slice height in pixels (`> 0.0`).
    pub height: f32,
}

/// Computes exact, contiguous, baseline-flush vertical slices for a stacked bar spanning
/// `[base_y - total_h, base_y]`. Guarantees zero baseline overhang and avoids distorted
/// sub-pixel slivers on short bars.
pub fn layout_stacked_bar(base_y: f32, total_h: f32, weights: &[u64]) -> Vec<StackedSlice> {
    if total_h <= 0.0 || weights.is_empty() {
        return Vec::new();
    }
    let positive: Vec<(usize, u64)> = weights
        .iter()
        .copied()
        .enumerate()
        .filter(|&(_, w)| w > 0)
        .collect();
    if positive.is_empty() {
        return Vec::new();
    }
    let bar_top = base_y - total_h;
    if positive.len() == 1 || total_h < 8.0 {
        let dominant = positive
            .iter()
            .copied()
            .max_by_key(|&(_, w)| w)
            .map(|(i, _)| i)
            .unwrap_or(positive[0].0);
        return vec![StackedSlice {
            index: dominant,
            y: bar_top,
            height: total_h,
        }];
    }

    let total_w: u64 = positive.iter().map(|&(_, w)| w).sum::<u64>().max(1);
    let active: Vec<(usize, u64)> = if total_h < 14.0 {
        let filtered: Vec<(usize, u64)> = positive
            .iter()
            .copied()
            .filter(|&(_, w)| (w as f32 / total_w as f32) * total_h >= 2.0)
            .collect();
        if filtered.is_empty() {
            let dominant = positive
                .iter()
                .copied()
                .max_by_key(|&(_, w)| w)
                .unwrap_or(positive[0]);
            vec![dominant]
        } else {
            filtered
        }
    } else {
        positive
    };

    if active.len() == 1 {
        return vec![StackedSlice {
            index: active[0].0,
            y: bar_top,
            height: total_h,
        }];
    }

    let active_sum: u64 = active.iter().map(|&(_, w)| w).sum::<u64>().max(1);
    let min_h = if total_h >= 14.0 {
        3.0_f32.min(total_h / active.len() as f32)
    } else {
        2.0_f32.min(total_h / active.len() as f32)
    };

    let mut heights: Vec<f32> = active
        .iter()
        .map(|&(_, w)| ((w as f32 / active_sum as f32) * total_h).max(min_h))
        .collect();

    // Water-filling redistribution so boosted tiny segments borrow strictly from taller segments
    // and the column sum remains strictly equal to `total_h`.
    for _ in 0..3 {
        let sum_h: f32 = heights.iter().sum();
        let excess = sum_h - total_h;
        if excess <= 1e-4 {
            break;
        }
        let slack: f32 = heights.iter().map(|&h| (h - min_h).max(0.0)).sum();
        if slack <= 1e-5 {
            let scale = total_h / sum_h.max(1e-5);
            for h in &mut heights {
                *h *= scale;
            }
            break;
        }
        for h in &mut heights {
            let s = (*h - min_h).max(0.0);
            if s > 0.0 {
                *h = (*h - excess * (s / slack)).max(min_h);
            }
        }
    }

    let mut out = Vec::with_capacity(active.len());
    let mut cursor_y = bar_top;
    let last_idx = active.len() - 1;
    for (pos, &(seg_idx, _)) in active.iter().enumerate() {
        let h = if pos == last_idx {
            (base_y - cursor_y).max(0.0)
        } else {
            heights[pos].min((base_y - cursor_y).max(0.0))
        };
        if h > 0.0 {
            out.push(StackedSlice {
                index: seg_idx,
                y: cursor_y,
                height: h,
            });
            cursor_y += h;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotone_cubic_never_overshoots_local_extrema() {
        let pts = vec![
            point(px(0.0), px(100.0)),
            point(px(20.0), px(100.0)),
            point(px(40.0), px(20.0)),
            point(px(60.0), px(20.0)),
            point(px(80.0), px(100.0)),
        ];
        let segs = monotone_cubic_segments(&pts);
        assert_eq!(segs.len(), 4);
        for x in 0..=80 {
            let y = sample_segments_y(&segs, x as f32).expect("in range");
            assert!(
                (19.9..=100.1).contains(&y),
                "monotone spline overshot at x={x}: y={y}"
            );
        }
    }

    #[test]
    fn nice_scale_rounds_cleanly_above_peak() {
        assert_eq!(compute_nice_scale(0.0), 1.0);
        assert_eq!(compute_nice_scale(82.0), 100.0);
        assert_eq!(compute_nice_scale(130.0), 150.0);
        assert_eq!(compute_nice_scale(1_750_000.0), 2_000_000.0);
        assert!((compute_nice_scale(0.42) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn spring_converges_cleanly_to_target() {
        let mut s = SpringState::default();
        s.snap_to(0.0);
        s.set_target(100.0);
        let mut running = true;
        for _ in 0..120 {
            running = s.step(1.0 / 60.0, 22.0, 0.96);
            if !running {
                break;
            }
        }
        assert!(!running);
        assert!((s.value - 100.0).abs() < 1e-3);
    }

    #[test]
    fn value_delta_handles_zero_and_signed_formatting() {
        let up = ValueDelta::between(142.7, 100.0);
        assert!(up.is_positive());
        assert_eq!(format_signed_pct(up.pct.unwrap()), "+42.7%");

        let from_zero = ValueDelta::between(50.0, 0.0);
        assert!(from_zero.pct.is_none());
        assert_eq!(format_signed_pct(-125.4), "-125%");
        assert_eq!(format_signed_pct(0.02), "0%");
    }

    #[test]
    fn range_summary_and_wave_reveal_behave_cleanly() {
        let vals = [Some(10.0), Some(20.0), None, Some(30.0)];
        let costs = [Some(0.5), Some(1.0), None, Some(1.5)];
        let calls = [2, 4, 0, 6];
        let r = RangeSummary::compute(&vals, &costs, &calls, 3, 1).expect("range");
        assert_eq!((r.start, r.end, r.count, r.known_count), (1, 3, 3, 2));
        assert!((r.total - 50.0).abs() < 1e-9);
        assert!((r.avg.unwrap() - 25.0).abs() < 1e-9);
        assert_eq!(r.calls, 10);
        assert!((r.cost_usd.unwrap() - 2.5).abs() < 1e-9);
        assert!((r.delta.unwrap().pct.unwrap() - 50.0).abs() < 1e-9);

        assert_eq!(staggered_bar_reveal(0.0, 3, 7), 0.0);
        assert_eq!(staggered_bar_reveal(1.0, 6, 7), 1.0);
        assert!(staggered_bar_reveal(0.45, 0, 7) > staggered_bar_reveal(0.45, 6, 7));
    }

    #[test]
    fn stacked_bar_layout_never_overhangs_baseline_or_distorts_short_bars() {
        let base_y = 176.0_f32;
        for &total_h in &[3.0_f32, 6.0, 10.0, 14.0, 16.0, 28.0, 140.0] {
            for weights in [
                vec![1, 98, 1],
                vec![1, 94, 5],
                vec![10, 10, 10],
                vec![0, 50, 0],
                vec![2, 1, 97],
            ] {
                let slices = layout_stacked_bar(base_y, total_h, &weights);
                assert!(!slices.is_empty(), "expected slices for h={total_h}");
                assert!(
                    (slices[0].y - (base_y - total_h)).abs() < 1e-3,
                    "top misaligned for h={total_h}: got {} vs {}",
                    slices[0].y,
                    base_y - total_h
                );
                let last = slices.last().unwrap();
                assert!(
                    ((last.y + last.height) - base_y).abs() < 1e-3,
                    "bottom overhung baseline for h={total_h}: got {} vs {base_y}",
                    last.y + last.height
                );
                for win in slices.windows(2) {
                    assert!(
                        ((win[0].y + win[0].height) - win[1].y).abs() < 1e-3,
                        "slices not contiguous for h={total_h}"
                    );
                }
                if total_h >= 14.0 && slices.len() > 1 {
                    for s in &slices {
                        assert!(
                            s.height >= 2.99,
                            "tiny distorted slice h={} in total_h={total_h}",
                            s.height
                        );
                    }
                }
            }
        }
    }
}
