//! The detection thread.
//!
//! Owns the merged families and detector parameters, both built once at startup — the
//! detector is stateless, so there is nothing to recreate per frame and nothing to
//! synchronise. Runs until its job channel closes, which is how the watcher's `Drop`
//! shuts it down.

use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;

use arucotools::detect::{detect_markers, CornerRefinementMethod, DetectorParams};
use image::GrayImage;
use tracing::debug;

use super::markers::{Hit, MergedFamily};

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
/// - `error_correction_rate` is dialled right down. Merged families carry only the
///   configured codewords, so a *different* marker from the same family is no longer in the
///   code list — with a generous correction radius it could be pulled onto a configured
///   code and start a recording nobody asked for. Requiring a near-exact match makes a
///   false trigger far less likely, which matters more here than detecting a damaged tag.
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

/// Run every configured family over one image and collect the configured markers found.
pub fn detect(families: &[MergedFamily], params: &DetectorParams, gray: &GrayImage) -> Vec<Hit> {
    let mut hits = Vec::new();
    for merged in families {
        for marker in detect_markers(gray, merged.family(), params) {
            if let Some(hit) = merged.resolve(marker.id) {
                if !hits.contains(hit) {
                    hits.push(hit.clone());
                }
            }
        }
    }
    hits
}

pub fn run(
    jobs: Receiver<DetectJob>,
    results: Sender<DetectOutcome>,
    families: Vec<MergedFamily>,
    params: DetectorParams,
) {
    // Ends when the watcher drops its sender.
    for job in jobs {
        let queued_ms = job.dispatched_at.elapsed().as_secs_f64() * 1e3;
        let started = Instant::now();
        let hits = detect(&families, &params, &job.gray);
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
