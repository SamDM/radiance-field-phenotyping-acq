//! Measuring where the scanout boundary sat within one exposure, and turning
//! that into a fraction of a full display refresh.
//!
//! The flicker strip is used rather than the faint reset band elsewhere on the
//! white field: it swings between full black and full white every refresh, so
//! under a short exposure it produces a clean two-plateau step rather than a
//! subtle dip, which is far easier to locate reliably by pixel analysis alone.
//!
//! Two measurements happen here and they use different coordinate systems on
//! purpose. *Where* the boundary is found is entirely in this camera's own
//! image, from the strip location [`crate::calibrate`] fixed once for the
//! whole recording — no cross-camera information enters that step. *What
//! fraction of a refresh* that position represents is computed purely from the
//! page's own `g` geometry (CSS pixels), independent of the camera. Combining
//! an image-space measurement with a page-space scale this way is what makes
//! the resulting fraction comparable between two cameras that photograph the
//! same phone from different distances and angles: each camera's own strip
//! pixels calibrate that camera's own scale.

use crate::calibrate::StripCalib;
use crate::luma::sample_bilinear;
use crate::qr::Geometry;
use image::GrayImage;

/// How far in from each end of the strip axis to start sampling, as a
/// multiple of the code size. The codes sit at the axis endpoints, so this
/// clears the code itself plus its quiet zone; `strip_phase_in_strip` returns
/// `None` if this would consume the whole strip, but on every image this was
/// tested against the codes are a small fraction of the top-to-bottom span.
const END_MARGIN: f32 = 0.7;
/// Number of points sampled along the strip.
const SAMPLES: usize = 300;
/// Minimum plateau-to-plateau contrast, in the 0-255 luma this crate uses,
/// before a changepoint is trusted as a real flicker transition rather than
/// noise on an exposure that happened not to straddle one. The strip is meant
/// to swing across the full black/white range; anything this washed out means
/// the exposure most likely landed entirely within one flicker state, which
/// happens whenever the trigger and a transition are more than one exposure
/// time apart — the common case at a short exposure.
const MIN_CONTRAST: f32 = 40.0;

/// Fraction along the strip's own (shortened) axis where the flicker boundary
/// sits, `0.0` at the top-ward end and `1.0` at the bottom-ward end. `None` if
/// no confident step was found.
///
/// Locates the changepoint by the same principle as Otsu's threshold, just
/// applied to *position* along the profile rather than to intensity: the
/// splitting index chosen is the one that minimizes the summed variance of the
/// two segments it creates. That is robust to which side is the brighter
/// plateau and to a fair amount of sensor noise, and unlike a derivative-based
/// edge finder it does not need a separate smoothing pass tuned to the
/// exposure.
fn strip_phase_in_strip(img: &GrayImage, calib: &StripCalib) -> Option<f64> {
    let (tx, ty) = calib.top;
    let (bx, by) = calib.bot;
    let (dx, dy) = (bx - tx, by - ty);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return None;
    }
    let (ux, uy) = (dx / len, dy / len);
    // Perpendicular unit vector, for averaging across the strip's width.
    let (px, py) = (-uy, ux);

    let margin = calib.size * END_MARGIN;
    if margin * 2.0 >= len {
        return None; // strip too short relative to the codes bracketing it
    }
    let start = (tx + ux * margin, ty + uy * margin);
    let span = len - margin * 2.0;
    let half_w = (calib.size * 0.35).max(1.0);

    let mut profile = [0f32; SAMPLES];
    for (i, slot) in profile.iter_mut().enumerate() {
        let t = i as f32 / (SAMPLES - 1) as f32;
        let (cx, cy) = (start.0 + ux * span * t, start.1 + uy * span * t);
        // Average a handful of points across the strip's width rather than a
        // single centreline sample, to average out sensor noise the same way
        // the calibration blur does for QR decoding.
        const W: i32 = 4;
        let mut acc = 0.0;
        for k in -W..=W {
            let f = k as f32 / W as f32 * half_w;
            acc += sample_bilinear(img, cx + px * f, cy + py * f);
        }
        *slot = acc / (2 * W + 1) as f32;
    }

    best_changepoint(&profile)
}

/// The position (as a `0..1` fraction of the profile) minimizing the summed
/// within-segment variance of the split it makes, computed in O(n) via
/// prefix sums of the profile and its square.
fn best_changepoint(profile: &[f32]) -> Option<f64> {
    let n = profile.len();
    let mut s1 = vec![0f64; n + 1];
    let mut s2 = vec![0f64; n + 1];
    for (i, &v) in profile.iter().enumerate() {
        s1[i + 1] = s1[i] + v as f64;
        s2[i + 1] = s2[i] + (v as f64) * (v as f64);
    }
    let var = |a: usize, b: usize| -> f64 {
        let cnt = (b - a) as f64;
        if cnt < 1.0 {
            return 0.0;
        }
        let mean = (s1[b] - s1[a]) / cnt;
        ((s2[b] - s2[a]) / cnt - mean * mean).max(0.0)
    };
    // A reasonable minimum segment length keeps the split from degenerating
    // to the very first or last sample on a monotonic-but-flat profile.
    let min_seg = (n / 20).max(3);
    let mut best_k = None;
    let mut best_cost = f64::INFINITY;
    for k in min_seg..(n - min_seg) {
        let cost = var(0, k) * k as f64 + var(k, n) * (n - k) as f64;
        if cost < best_cost {
            best_cost = cost;
            best_k = Some(k);
        }
    }
    let k = best_k?;
    let lo_mean = (s1[k] - s1[0]) / k as f64;
    let hi_mean = (s1[n] - s1[k]) / (n - k) as f64;
    if (hi_mean - lo_mean).abs() < MIN_CONTRAST as f64 {
        return None; // no real transition in this exposure
    }
    Some(k as f64 / n as f64)
}

/// Convert a strip-local fraction to a fraction of the whole panel's scan
/// (long) axis, using only the page's own `g` geometry — no image-space
/// information enters here, which is what keeps this step identical for
/// every camera regardless of framing.
///
/// Assumes the page is at (or very near) fullscreen, so the viewport's long
/// axis stands in for the panel's: `g` reports both, but the strip's own
/// extent (`inset`, `dark`) is defined relative to the *stage*, which tracks
/// the viewport, not the panel. If the page were shown windowed this would be
/// off by the viewport/panel ratio; `sync-check-clock.html`'s own tap-for-
/// fullscreen behaviour is what makes that a reasonable assumption here.
fn strip_frac_to_panel_frac(strip_frac: f64, g: &Geometry) -> f64 {
    let stage_long = g.viewport_long - 2.0 * g.edge;
    let strip_start = g.dark + g.inset; // from the stage edge to the strip's near end
    let strip_span = stage_long - 2.0 * strip_start;
    if stage_long <= 0.0 || strip_span <= 0.0 {
        return f64::NAN;
    }
    (strip_start + strip_frac * strip_span) / stage_long
}

/// Full pipeline: locate the boundary in `img` using `calib`, and express it
/// as a fraction `0..1` of one full display refresh. `None` if no confident
/// boundary was found or the calibration lacks what it needs.
pub fn measure(img: &GrayImage, calib: &crate::calibrate::CameraCalib) -> Option<f64> {
    let strip = calib.strip.as_ref()?;
    let g = calib.geometry.as_ref()?;
    let t = strip_phase_in_strip(img, strip)?;
    let frac = strip_frac_to_panel_frac(t, g);
    frac.is_finite().then_some(frac)
}

/// Signed difference `a - b` of two phases in `0..1`, wrapped to `(-0.5, 0.5]`.
///
/// Phase is only known modulo one refresh, so a naive subtraction would report
/// a near-one-period sync error as if it were the near-zero error on the other
/// side of the wrap. True sync error is presumed to be a small fraction of a
/// refresh, so the wrapped difference is the physically meaningful one.
pub fn wrapped_diff(a: f64, b: f64) -> f64 {
    let d = a - b;
    d - (d + 0.5).floor()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapped_diff_is_the_short_way_around() {
        assert!((wrapped_diff(0.51, 0.50) - 0.01).abs() < 1e-9);
        // 0.99 vs 0.01 are 0.02 apart across the wrap, not 0.98 apart directly.
        assert!((wrapped_diff(0.01, 0.99) - 0.02).abs() < 1e-9);
        assert!((wrapped_diff(0.99, 0.01) - (-0.02)).abs() < 1e-9);
        assert!((wrapped_diff(0.5, 0.5)).abs() < 1e-9);
    }

    #[test]
    fn changepoint_finds_a_clean_step() {
        let mut profile = vec![10.0f32; 60];
        profile[30..].fill(200.0);
        let t = best_changepoint(&profile).expect("clear step should be found");
        assert!((t - 0.5).abs() < 0.05, "got {t}");
    }

    #[test]
    fn changepoint_rejects_low_contrast() {
        // A step that never leaves the noise floor should not be reported —
        // this is the exposures-that-missed-a-transition case, which must
        // come back as "unmeasurable" rather than a fabricated position.
        let mut profile = vec![100.0f32; 60];
        profile[30..].fill(110.0);
        assert!(best_changepoint(&profile).is_none());
    }

    #[test]
    fn changepoint_ignores_polarity() {
        // White-to-black and black-to-white transitions must land at the same
        // position — the method must not have a directional bias.
        let mut hi_lo = vec![200.0f32; 60];
        hi_lo[40..].fill(10.0);
        let mut lo_hi = vec![10.0f32; 60];
        lo_hi[40..].fill(200.0);
        let a = best_changepoint(&hi_lo).unwrap();
        let b = best_changepoint(&lo_hi).unwrap();
        assert!((a - b).abs() < 1e-9, "{a} vs {b}");
    }

    #[test]
    fn strip_frac_to_panel_frac_is_linear_and_centred() {
        let g = Geometry {
            viewport_short: 400.0,
            viewport_long: 900.0,
            panel_short: 400.0,
            panel_long: 900.0,
            dpr: 2.0,
            inset: 10.0,
            dark: 60.0,
            hz: 60.0,
            edge: 20.0,
        };
        let a = strip_frac_to_panel_frac(0.0, &g);
        let b = strip_frac_to_panel_frac(1.0, &g);
        let mid = strip_frac_to_panel_frac(0.5, &g);
        assert!(a < mid && mid < b, "should be monotonic: {a} {mid} {b}");
        assert!((mid - (a + b) / 2.0).abs() < 1e-9, "midpoint should be linear");
        // Both ends should sit strictly inside the panel (the codes occupy the
        // space beyond them), and symmetric about the centre for a
        // symmetric layout.
        assert!(a > 0.0 && b < 1.0);
        assert!((a + b - 1.0).abs() < 1e-9);
    }
}
