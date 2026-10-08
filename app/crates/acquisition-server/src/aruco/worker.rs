//! The detection thread.
//!
//! Owns the marker set, the gate and the detector parameters, all built once at startup — the
//! detector is stateless, so there is nothing to recreate per frame and nothing to
//! synchronise. Runs until its job channel closes, which is how the watcher's `Drop`
//! shuts it down.

use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;

use arucotools::detect::{detect_markers, CornerRefinementMethod, DetectorParams};
use image::GrayImage;
use tracing::debug;

use super::markers::{Hit, MarkerSet};
use super::MarkerGate;

pub struct DetectJob {
    pub cam_idx: usize,
    pub gray: GrayImage,
    pub dispatched_at: Instant,
}

pub struct DetectOutcome {
    pub cam_idx: usize,
    pub hits: Vec<Hit>,
}

/// Detector parameters tuned for "is a large marker on screen right now?".
///
/// Two deliberate departures from the defaults:
///
/// - The default sweeps adaptive-threshold window sizes `{3, 13, 23}`, running the full
///   threshold-and-contour pipeline three times per frame. One pinned window is the single
///   biggest speed lever available, and a marker large enough to read at all does not need
///   a multi-scale search to be found.
/// - `error_correction_rate` is dialled down from 0.6. A wide correction radius lets
///   marker-like texture — a QR code's finder patterns, a printed label — be corrected onto
///   a valid codeword and start a recording nobody asked for. Requiring a near-exact match
///   makes that far less likely, which matters more here than detecting a damaged tag: the
///   held pot shows several markers at once, so missing one costs nothing.
///
/// Corner refinement stays off: only IDs are wanted, never geometry.
pub fn detector_params(thresh_win: u32, min_perimeter_rate: f64) -> DetectorParams {
    DetectorParams {
        // One pass, not the detector's default three. See `DEFAULT_THRESH_WIN` for how
        // to put the sweep back if detection on the rig turns out to need it.
        adaptive_thresh_win_size_min: thresh_win,
        adaptive_thresh_win_size_max: thresh_win,
        min_marker_perimeter_rate: min_perimeter_rate,
        corner_refinement_method: CornerRefinementMethod::None,
        error_correction_rate: 0.2,
        ..Default::default()
    }
}

/// Which detections count, and how to measure them against the full-resolution frame.
pub struct Selection {
    pub markers: MarkerSet,
    pub gate: MarkerGate,
    /// Full-resolution pixels per detector-input pixel: `2 * downscale`, since both
    /// downscale modes reduce each `k`×`k` block of Bayer quads to one pixel.
    pub scale: f64,
}

/// Run every configured family over one image and collect the configured markers found
/// inside the gate.
pub fn detect(
    selection: &Selection,
    params: &DetectorParams,
    gray: &GrayImage,
    cam_idx: usize,
) -> Vec<Hit> {
    let (w, h) = (gray.width() as f64, gray.height() as f64);
    let mut hits = Vec::new();
    for found in detect_markers(gray, selection.markers.families(), params).markers {
        let Some(hit) = selection.markers.resolve(found.dictionary, found.marker.id) else {
            continue;
        };
        let c = found.marker.corners.map(|p| p.0);
        let centre = [
            c.iter().map(|p| p[0]).sum::<f64>() / 4.0,
            c.iter().map(|p| p[1]).sum::<f64>() / 4.0,
        ];
        let rel = [centre[0] / w, centre[1] / h];
        let side_px = (0..4)
            .map(|i| {
                let (a, b) = (c[i], c[(i + 1) % 4]);
                (a[0] - b[0]).hypot(a[1] - b[1])
            })
            .sum::<f64>()
            / 4.0
            * selection.scale;
        let admitted = selection.gate.admits(rel, side_px);

        // Every configured marker, admitted or not, so the ROI and size floor can be
        // tuned from the log of a real run.
        debug!(
            camera = cam_idx,
            marker = %format!("{}:{}", hit.family, hit.id),
            x = format_args!("{:.3}", rel[0]),
            y = format_args!("{:.3}", rel[1]),
            side_px = format_args!("{side_px:.0}"),
            confidence = format_args!("{:.2}", found.confidence),
            admitted,
            "aruco marker"
        );

        if admitted && !hits.contains(hit) {
            hits.push(hit.clone());
        }
    }
    hits
}

pub fn run(
    jobs: Receiver<DetectJob>,
    results: Sender<DetectOutcome>,
    selection: Selection,
    params: DetectorParams,
) {
    // Ends when the watcher drops its sender.
    for job in jobs {
        let queued_ms = job.dispatched_at.elapsed().as_secs_f64() * 1e3;
        let started = Instant::now();
        let hits = detect(&selection, &params, &job.gray, job.cam_idx);
        let detect_ms = started.elapsed().as_secs_f64() * 1e3;

        debug!(
            camera = job.cam_idx,
            width = job.gray.width(),
            height = job.gray.height(),
            n_hits = hits.len(),
            detect_ms = format_args!("{detect_ms:.1}"),
            queued_ms = format_args!("{queued_ms:.1}"),
            "aruco detection"
        );

        // A send error means the watcher is gone; nothing left to report to.
        if results
            .send(DetectOutcome { cam_idx: job.cam_idx, hits })
            .is_err()
        {
            break;
        }
    }
}
