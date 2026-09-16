//! Synthetic marker scenes, for benchmarks and tests.
//!
//! The rig is not always available, so the sizing question — *is a marker of this many
//! pixels still detectable after downscaling by `k`?* — has to be answerable offline.
//! These helpers build a full-resolution RGGB mosaic containing a marker at a chosen
//! size, which is exactly what the acquisition thread would hand the watcher.
//!
//! The mosaic deliberately gives red, green and blue sites *different* responses. A
//! reduction that loses the Bayer phase then produces visible checkerboarding rather
//! than something subtly wrong, so phase bugs fail loudly instead of quietly costing
//! detections.

use arucotools::board::MarkerBoardCfg;
use arucotools::dictionaries::{HasMarkerFamily, StaticMarkerFamily};
use arucotools::draw::BoardDrawerCfg;
use image::{GrayImage, Luma};

/// The side length a marker request actually renders at.
///
/// A marker is a whole number of pixels per module, so the rendered side is
/// `npix_per_bit * (pattern bits + 2 border bits)` and usually lands a few pixels off the
/// request. Tests that care about the real size ask here rather than assuming.
pub fn marker_side_px(family: &StaticMarkerFamily, target_side_px: u32) -> u32 {
    let modules = family.nbits_side_pattern() + 2; // one border bit each side
    (npix_per_bit(family, target_side_px) * modules) as u32
}

fn npix_per_bit(family: &StaticMarkerFamily, target_side_px: u32) -> usize {
    let modules = family.nbits_side_pattern() + 2;
    ((target_side_px as usize + modules / 2) / modules).max(1)
}

/// Draw one marker, centred, on a white `w`×`h` background.
///
/// The white surround is not decoration: the detector needs a quiet zone around the
/// marker to find its outer contour at all.
///
/// A marker too large for the scene yields the bare background, so a mis-sized request
/// shows up as "detected nothing" rather than a panic.
pub fn scene_with_marker(
    w: u32,
    h: u32,
    family: &StaticMarkerFamily,
    id: usize,
    target_side_px: u32,
) -> Result<GrayImage, Box<dyn std::error::Error>> {
    let board = MarkerBoardCfg::new(*family)
        // A 1x1 `Range` grid starting at `id` is just that one marker.
        .with_marker_ids((1, 1, id))
        .with_nbits_border(1)
        .with_phys_bit_size(1.0)
        .with_phys_sep_len(0.0)
        .build()?;

    let drawer = BoardDrawerCfg::new(board)
        .with_npix_per_bit(npix_per_bit(family, target_side_px))
        .build()?;

    // `draw_grid_mut` paints onto an existing canvas at an offset, so the marker goes
    // straight into the scene — no intermediate image and no copy.
    let mut scene = empty_scene(w, h);
    let (mw, mh) = (drawer.npix_grid_width() as u32, drawer.npix_grid_height() as u32);
    if mw > w || mh > h {
        return Ok(scene);
    }
    drawer.draw_grid_mut(&mut scene, (((w - mw) / 2) as usize, ((h - mh) / 2) as usize));
    Ok(scene)
}

/// A `w`×`h` white scene with no marker in it.
pub fn empty_scene(w: u32, h: u32) -> GrayImage {
    GrayImage::from_pixel(w, h, Luma([255]))
}

/// Encode a grayscale scene as an RGGB Bayer mosaic of the same dimensions.
///
/// Green sites carry the scene value unchanged, so a phase-correct green reduction
/// recovers the scene exactly. Red and blue sites are attenuated to stand in for a real
/// colour-filter response under greenish illumination — which means any reduction that
/// strays off the green channel pulls in visibly darker samples.
pub fn gray_to_rggb_mosaic(scene: &GrayImage) -> Vec<u8> {
    let (w, h) = scene.dimensions();
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let v = scene.get_pixel(x, y).0[0] as u32;
            let sample = match (y % 2, x % 2) {
                (0, 0) => v * 50 / 100, // R site
                (1, 1) => v * 35 / 100, // B site
                _ => v,                 // G sites
            };
            out.push(sample as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use arucotools::dictionaries::predefined;

    #[test]
    fn renders_a_marker_at_the_expected_size() {
        // 5 pattern bits + 2 border bits = 7 modules, so 70 px is exactly 10 px/bit.
        assert_eq!(marker_side_px(&predefined::DICT_5X5_250, 70), 70);
    }

    #[test]
    fn the_marker_lands_centred_in_the_scene() {
        let scene = scene_with_marker(240, 180, &predefined::DICT_5X5_250, 3, 70).unwrap();
        assert_eq!(scene.dimensions(), (240, 180));
        // The marker is 70px centred in 240x180, so its border sits inside this box and
        // the scene corners stay white.
        assert!(scene.pixels().any(|p| p.0[0] == 0), "no marker was drawn");
        assert_eq!(scene.get_pixel(0, 0).0[0], 255, "background should be white");
        assert_eq!(scene.get_pixel(239, 179).0[0], 255, "background should be white");
    }

    #[test]
    fn a_marker_too_large_for_the_scene_leaves_it_blank() {
        let scene = scene_with_marker(40, 40, &predefined::DICT_5X5_250, 3, 700).unwrap();
        assert!(scene.pixels().all(|p| p.0[0] == 255));
    }

    #[test]
    fn mosaic_round_trips_through_a_green_reduction() {
        // The point of the whole synth module: a phase-correct reduction must recover the
        // scene, not a checkerboard.
        let scene = scene_with_marker(240, 180, &predefined::DICT_4X4_50, 7, 60).unwrap();
        let mosaic = gray_to_rggb_mosaic(&scene);

        let reduced =
            acquisition_common::downscale::bayer_rggb_green_subsample(&mosaic, 240, 180, 1);
        assert_eq!(reduced.dimensions(), (120, 90));
        // Green sites are untouched, so every reduced pixel is a genuine scene value:
        // pure black or pure white, never an attenuated red/blue sample.
        assert!(
            reduced.pixels().all(|p| p.0[0] == 0 || p.0[0] == 255),
            "reduction picked up attenuated non-green sites"
        );
    }
}
