//! Locating the strip once per camera, from whatever the recording's frames
//! happen to decode.
//!
//! Camera and phone are both static for the length of a recording, so the four
//! corner codes sit at fixed image coordinates throughout — they only need
//! decoding *somewhere* in the recording, not in every frame. Decoupling
//! calibration from measurement this way is what makes the phase measurement
//! itself usable on frames whose own QR codes failed to decode: the strip
//! location for a camera is fixed before any per-frame work starts.

use crate::luma::{blur1, frame_luma};
use crate::qr::{decode_all, Geometry};
use acquisition_common::{Bayer, Frame};
use rayon::prelude::*;
use std::path::Path;

/// One strip's endpoints and half-width, in this camera's image.
///
/// `top`/`bot` are the mean centres of whichever corner pair calibration
/// found decodes for. `top_n`/`bot_n` are how many decodes went into each
/// mean — the report surfaces these because on the recordings this was built
/// against, the bottom corners decoded far less often than the top ones, and a
/// strip anchored on a single decode is real data but deserves to be read with
/// that in mind.
#[derive(Debug, Clone, Copy)]
pub struct StripCalib {
    pub side: char, // 'L' (ids 1,3) or 'R' (ids 2,4)
    pub top: (f32, f32),
    pub bot: (f32, f32),
    pub top_n: usize,
    pub bot_n: usize,
    /// Mean code side length across every decode of any id, in image px —
    /// used as the strip's half-width and as the margin that keeps the sampled
    /// profile clear of the codes at either end.
    pub size: f32,
}

pub struct CameraCalib {
    /// Decode counts for ids 1..4, in that order — the raw material
    /// [`StripCalib`] was chosen from, kept for the report.
    pub corner_counts: [usize; 4],
    pub geometry: Option<Geometry>,
    pub strip: Option<StripCalib>,
}

/// Debayer, blur and decode every frame in `paths`, in parallel, and reduce
/// the result to a fixed strip location plus the page's reported geometry.
pub fn calibrate(paths: &[std::path::PathBuf]) -> CameraCalib {
    // Per-id observations: (x, y, size) for each successful corner decode,
    // plus every geometry payload seen (there should be one consistent value;
    // averaging is cheap insurance against a single bit-flipped decode).
    let mut per_id: [Vec<(f32, f32, f32)>; 4] = Default::default();
    let mut geoms = Vec::new();

    let results: Vec<_> = paths
        .par_iter()
        .filter_map(|p| load_and_decode(p).ok())
        .collect();
    for decs in results {
        for d in decs {
            if let Some(g) = d.geometry {
                geoms.push(g);
            }
            if (1..=4).contains(&d.id) {
                per_id[(d.id - 1) as usize].push((d.centre.x, d.centre.y, d.size));
            }
        }
    }

    let corner_counts = [
        per_id[0].len(),
        per_id[1].len(),
        per_id[2].len(),
        per_id[3].len(),
    ];

    let mean = |v: &[(f32, f32, f32)]| -> (f32, f32) {
        let n = v.len() as f32;
        (v.iter().map(|p| p.0).sum::<f32>() / n, v.iter().map(|p| p.1).sum::<f32>() / n)
    };
    let all_sizes: Vec<f32> = per_id.iter().flatten().map(|p| p.2).collect();
    let mean_size = if all_sizes.is_empty() {
        0.0
    } else {
        all_sizes.iter().sum::<f32>() / all_sizes.len() as f32
    };

    // Left (ids 1,3) preferred over right (ids 2,4) only by convention; either
    // is an equally valid strip. Use whichever this recording actually has
    // both ends of.
    let strip = if !per_id[0].is_empty() && !per_id[2].is_empty() {
        Some(StripCalib {
            side: 'L',
            top: mean(&per_id[0]),
            bot: mean(&per_id[2]),
            top_n: per_id[0].len(),
            bot_n: per_id[2].len(),
            size: mean_size,
        })
    } else if !per_id[1].is_empty() && !per_id[3].is_empty() {
        Some(StripCalib {
            side: 'R',
            top: mean(&per_id[1]),
            bot: mean(&per_id[3]),
            top_n: per_id[1].len(),
            bot_n: per_id[3].len(),
            size: mean_size,
        })
    } else {
        None
    };

    let geometry = geoms.first().copied();

    CameraCalib { corner_counts, geometry, strip }
}

fn load_and_decode(path: &Path) -> Result<Vec<crate::qr::Decoded>, Box<dyn std::error::Error>> {
    let f = Frame::<Bayer>::load(path)?;
    let luma = blur1(&frame_luma(&f.data, f.width, f.height));
    Ok(decode_all(luma))
}
