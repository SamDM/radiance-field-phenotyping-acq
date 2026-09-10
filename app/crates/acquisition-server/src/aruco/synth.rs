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
use image::{GenericImage, GrayImage, Luma};

/// Render a single marker, sized as close to `target_side_px` as the module grid allows.
///
/// The rendered side is `npix_per_bit * (pattern bits + 2 border bits)`, so the result is
/// usually a few pixels off the request; read `.width()` for what you actually got.
pub fn marker_image(
    family: &StaticMarkerFamily,
    id: usize,
    target_side_px: u32,
) -> Result<GrayImage, Box<dyn std::error::Error>> {
    let modules = family.nbits_side_pattern() + 2; // one border bit each side
    let npix_per_bit = ((target_side_px as usize + modules / 2) / modules).max(1);

    let board = MarkerBoardCfg::new(*family)
        // A 1x1 `Range` grid starting at `id` is just that one marker.
        .with_marker_ids((1, 1, id))
        .with_nbits_border(1)
        .with_phys_bit_size(1.0)
        .with_phys_sep_len(0.0)
        .build()?;

    let drawer = BoardDrawerCfg::new(board)
        .with_npix_per_bit(npix_per_bit)
        .build()?;

    Ok(drawer.draw_grid::<GrayImage>())
}

/// Paste `marker` onto a white `w`×`h` background, centred.
///
/// The white surround is not decoration: the detector needs a quiet zone around the
/// marker to find its outer contour at all.
pub fn scene_with_marker(w: u32, h: u32, marker: &GrayImage) -> GrayImage {
    let mut scene = GrayImage::from_pixel(w, h, Luma([255]));
    if marker.width() > w || marker.height() > h {
        return scene;
    }
    let x0 = (w - marker.width()) / 2;
    let y0 = (h - marker.height()) / 2;
    scene
        .copy_from(marker, x0, y0)
        .expect("marker fits inside the scene");
    scene
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
    fn renders_a_marker_near_the_requested_size() {
        let img = marker_image(&predefined::DICT_5X5_250, 3, 70).unwrap();
        // 5 pattern bits + 2 border bits = 7 modules, so 70 px is exactly 10 px/bit.
        assert_eq!(img.dimensions(), (70, 70));
    }

    #[test]
    fn mosaic_round_trips_through_a_green_reduction() {
        // The point of the whole synth module: a phase-correct reduction must recover the
        // scene, not a checkerboard.
        let marker = marker_image(&predefined::DICT_4X4_50, 7, 60).unwrap();
        let scene = scene_with_marker(240, 180, &marker);
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
