//! Decoding the fiducials `app/sync-check-clock.html` paints on screen.
//!
//! The four corner codes carry `{"frame":N,"id":K}`; the centre one carries
//! `{"t":<epoch ms>,"n":N,"id":5,"g":[...]}`. See the page's own help panel
//! for what each `g` entry means — [`Geometry`] mirrors it field for field.
//!
//! # Why rxing, not rqrr
//!
//! An earlier version of this module used `rqrr`. Measured against these
//! recordings it found only a fraction of the codes present in a typical
//! frame — sometimes none of a whole corner across an entire recording, on
//! images a phone's own camera app reads instantly off a monitor showing the
//! same PNG. Swapping to `rxing` (a Rust port of ZXing, the library lineage
//! most phone camera scanners build on) with `TryHarder` enabled found all
//! five codes on the large majority of frames tested, including every case
//! `rqrr` had failed on. The gap was the decoder, not the image.

use image::GrayImage;
use rxing::{BarcodeFormat, DecodeHints, ResultPoint};
use serde::Deserialize;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy)]
pub struct Point2 {
    pub x: f32,
    pub y: f32,
}

/// One decoded code: its payload and where it sat in the image.
///
/// `size` is the code's own side length in image pixels, estimated from the
/// diagonal of its detected quadrilateral rather than any one edge, so it
/// stays meaningful even when the code is seen at a slight angle.
#[derive(Debug, Clone)]
pub struct Decoded {
    pub id: u8,
    pub frame: Option<u64>,
    pub centre: Point2,
    pub size: f32,
    pub geometry: Option<Geometry>,
}

/// The `g` array from the centre code, in the panel's own frame — short axis
/// first, long axis second — so nothing here changes meaning when the phone is
/// turned. See `app/sync-check-clock.html`'s help panel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    pub viewport_short: f64,
    pub viewport_long: f64,
    pub panel_short: f64,
    pub panel_long: f64,
    pub dpr: f64,
    /// Inset from each stage edge to a corner code's dark pattern, CSS px.
    pub inset: f64,
    /// Side length of that dark pattern, CSS px.
    pub dark: f64,
    pub hz: f64,
    /// Stage inset from the viewport edge (`--edge`), CSS px.
    ///
    /// Carried in the payload rather than assumed as a constant on this side:
    /// a page edit that changes `--edge` would otherwise silently make every
    /// recording captured under the old value misread, with nothing to flag
    /// it. Older recordings (before this field existed) fall back to a fixed
    /// guess — see [`CentrePayload`]'s deserialization.
    pub edge: f64,
}

#[derive(Deserialize)]
struct CornerPayload {
    frame: u64,
    id: u8,
}

/// `--edge` in `app/sync-check-clock.html` before it was added to `g` —
/// used only as a fallback for recordings made before that change.
const LEGACY_EDGE_PX: f64 = 12.0;

#[derive(Deserialize)]
struct CentrePayload {
    #[allow(dead_code)]
    t: i64,
    n: u64,
    id: u8,
    g: Vec<f64>,
}

fn parse(text: &str) -> Option<(u8, Option<u64>, Option<Geometry>)> {
    if let Ok(c) = serde_json::from_str::<CentrePayload>(text) {
        let g = c.g;
        if g.len() < 8 {
            return None;
        }
        let edge = g.get(8).copied().unwrap_or(LEGACY_EDGE_PX);
        return Some((
            c.id,
            Some(c.n),
            Some(Geometry {
                viewport_short: g[0],
                viewport_long: g[1],
                panel_short: g[2],
                panel_long: g[3],
                dpr: g[4],
                inset: g[5],
                dark: g[6],
                hz: g[7],
                edge,
            }),
        ));
    }
    if let Ok(c) = serde_json::from_str::<CornerPayload>(text) {
        return Some((c.id, Some(c.frame), None));
    }
    None
}

/// Detect and decode every code in `img`. `img` should already be [`super::luma::blur1`]'d —
/// this function does not blur, so callers doing per-frame repeat work (like the
/// same image at two blur levels) are not forced to redo it.
pub fn decode_all(img: GrayImage) -> Vec<Decoded> {
    let (w, h) = (img.width(), img.height());
    let luma = img.into_raw();

    let mut formats = HashSet::new();
    formats.insert(BarcodeFormat::QR_CODE);
    let mut hints = DecodeHints { TryHarder: Some(true), PossibleFormats: Some(formats), ..Default::default() };

    // No barcodes found comes back as an `Err` from this API, not an empty
    // `Ok(vec![])` — that is a normal outcome here (most frames have plenty
    // of white field and no guarantee any code survived motion blur or noise
    // in this particular exposure), not something to propagate as an error.
    let Ok(results) = rxing::helpers::detect_multiple_in_luma_with_hints(luma, w, h, &mut hints) else {
        return Vec::new();
    };

    results
        .iter()
        .filter_map(|r| {
            let (id, frame, geometry) = parse(r.getText())?;
            let pts = r.getRXingResultPoints();
            if pts.is_empty() {
                return None;
            }
            let (mut sx, mut sy) = (0.0f32, 0.0f32);
            for p in pts {
                sx += p.getX();
                sy += p.getY();
            }
            // Side length from the largest pairwise distance among the
            // returned points rather than any one edge: robust to both a
            // slight in-plane rotation and to the point count varying (QR
            // normally comes back as the symbol's 4 corners, but nothing here
            // assumes exactly 4).
            let mut diag = 0.0f32;
            for (i, a) in pts.iter().enumerate() {
                for b in &pts[i + 1..] {
                    let (dx, dy) = (a.getX() - b.getX(), a.getY() - b.getY());
                    diag = diag.max((dx * dx + dy * dy).sqrt());
                }
            }
            Some(Decoded {
                id,
                frame,
                centre: Point2 { x: sx / pts.len() as f32, y: sy / pts.len() as f32 },
                size: diag / std::f32::consts::SQRT_2,
                geometry,
            })
        })
        .collect()
}
