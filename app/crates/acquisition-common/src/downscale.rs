//! Reduce a raw RGGB Bayer mosaic straight to a small grayscale image.
//!
//! Marker detection wants luminance and nothing else, so it never needs a debayer at
//! all — and a debayer is by far the most expensive thing the acquisition thread could
//! do per frame. These functions skip it: they read the mosaic's green sites directly.
//!
//! **Staying on one colour-filter channel is load-bearing, not an optimisation.** A
//! stride that ignores the Bayer phase lands on red sites in some output pixels and blue
//! sites in others. On foliage, where green vastly outruns red and blue, that manufactures
//! a high-amplitude checkerboard that has nothing to do with the scene; adaptive
//! thresholding then finds contours everywhere and detection fails outright. Locking to
//! one channel is what makes the output a picture rather than an artefact, and green is
//! both the brightest channel and the one sampled twice per quad.
//!
//! In RGGB, even rows read `R G R G …` and odd rows read `G B G B …`, so quad `(qx, qy)`
//! has its green sites at `(2qx+1, 2qy)` and `(2qx, 2qy+1)`.

use image::GrayImage;

/// Take one green site per 2×2 quad at a quad stride of `k`, producing a
/// `w/(2k)`×`h/(2k)` grayscale image.
///
/// One byte read per output pixel. It aliases — all but one of every `4k²` bytes is
/// discarded, so hard edges land wherever the stride falls — which is affordable because
/// a detectable marker is many output pixels across regardless.
/// [`bayer_rggb_box_average`] is the antialiasing alternative.
///
/// Returns an empty image for inputs too small to hold a single quad.
pub fn bayer_rggb_green_subsample(data: &[u8], w: u32, h: u32, k: u32) -> GrayImage {
    if w < 2 || h < 2 {
        return GrayImage::new(0, 0);
    }
    let k = k.max(1) as usize;
    let wu = w as usize;
    // `.max(1)` keeps a `k` larger than the frame from producing a zero-sized image;
    // it degrades to a single sample rather than failing.
    let out_w = (wu / (2 * k)).max(1);
    let out_h = (h as usize / (2 * k)).max(1);

    let mut out = Vec::with_capacity(out_w * out_h);
    for oy in 0..out_h {
        // Hoisting the row keeps the inner loop to a single indexed read.
        let row = &data[(2 * k * oy) * wu..][..wu];
        for ox in 0..out_w {
            out.push(row[2 * k * ox + 1]);
        }
    }
    GrayImage::from_raw(out_w as u32, out_h as u32, out).expect("buffer matches dimensions")
}

/// Average both green sites of every quad across each `k`×`k` block of quads, producing a
/// `w/(2k)`×`h/(2k)` grayscale image.
///
/// Reads every green site, so it costs meaningfully more than
/// [`bayer_rggb_green_subsample`], but antialiases rather than point-sampling. Worth
/// trying if detection misses markers that are visibly present, or in noisy low light.
///
/// Partial blocks at the right and bottom edges average the quads that are there rather
/// than reading past the frame.
///
/// Returns an empty image for inputs too small to hold a single quad.
pub fn bayer_rggb_box_average(data: &[u8], w: u32, h: u32, k: u32) -> GrayImage {
    if w < 2 || h < 2 {
        return GrayImage::new(0, 0);
    }
    let k = k.max(1) as usize;
    let wu = w as usize;
    let (quads_w, quads_h) = (wu / 2, h as usize / 2);
    let out_w = (quads_w / k).max(1);
    let out_h = (quads_h / k).max(1);

    let mut out = Vec::with_capacity(out_w * out_h);
    for oy in 0..out_h {
        for ox in 0..out_w {
            let mut sum = 0u32;
            let mut n = 0u32;
            for qy in (oy * k)..((oy + 1) * k).min(quads_h) {
                let top = &data[(2 * qy) * wu..][..wu];
                let bot = &data[(2 * qy + 1) * wu..][..wu];
                for qx in (ox * k)..((ox + 1) * k).min(quads_w) {
                    sum += top[2 * qx + 1] as u32 + bot[2 * qx] as u32;
                    n += 2;
                }
            }
            out.push((sum / n.max(1)) as u8);
        }
    }
    GrayImage::from_raw(out_w as u32, out_h as u32, out).expect("buffer matches dimensions")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `w`×`h` RGGB mosaic where every site carries its own channel's constant value.
    fn flat_mosaic(w: u32, h: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
        (0..h)
            .flat_map(|y| {
                (0..w).map(move |x| match (y % 2, x % 2) {
                    (0, 0) => r,
                    (0, 1) => g,
                    (1, 0) => g,
                    _ => b,
                })
            })
            .collect()
    }

    /// The test that matters most: a mosaic where the three channels are far apart, so any
    /// slip in the Bayer phase shows up as a red or blue value in the output rather than
    /// as a subtle shift. This is what catches an off-by-one in the green-site index.
    #[test]
    fn both_modes_stay_on_the_green_channel() {
        let (w, h) = (64u32, 48u32);
        let src = flat_mosaic(w, h, 10, 200, 30);
        for k in 1..=4 {
            for (name, img) in [
                ("subsample", bayer_rggb_green_subsample(&src, w, h, k)),
                ("box", bayer_rggb_box_average(&src, w, h, k)),
            ] {
                assert!(
                    img.pixels().all(|p| p.0[0] == 200),
                    "{name} at k={k} sampled off the green channel"
                );
            }
        }
    }

    #[test]
    fn output_dimensions_are_input_over_2k() {
        let (w, h) = (3536u32, 2748u32);
        let src = vec![0u8; (w * h) as usize];
        for (k, expected) in [(2u32, (884u32, 687u32)), (3, (589, 458)), (4, (442, 343))] {
            assert_eq!(bayer_rggb_green_subsample(&src, w, h, k).dimensions(), expected);
            assert_eq!(bayer_rggb_box_average(&src, w, h, k).dimensions(), expected);
        }
    }

    #[test]
    fn subsample_reads_the_expected_sites() {
        // Every byte is its own linear index, so a wrong offset is immediately visible.
        let (w, h) = (16u32, 8u32);
        let src: Vec<u8> = (0..(w * h) as u8).collect();

        // k=1: output pixel (ox, oy) is the G1 site of quad (ox, oy) = (2ox+1, 2oy).
        let out = bayer_rggb_green_subsample(&src, w, h, 1);
        assert_eq!(out.get_pixel(0, 0).0[0], 1);
        assert_eq!(out.get_pixel(3, 0).0[0], 7);
        assert_eq!(out.get_pixel(0, 2).0[0], (4 * w) as u8 + 1);

        // k=2 skips every other quad in both axes.
        let out = bayer_rggb_green_subsample(&src, w, h, 2);
        assert_eq!(out.get_pixel(1, 0).0[0], 5);
        assert_eq!(out.get_pixel(0, 1).0[0], (4 * w) as u8 + 1);
    }

    #[test]
    fn box_average_averages_its_block() {
        // Green sites carry a horizontal ramp; red and blue are constant and must not
        // leak into the average.
        let (w, h) = (8u32, 4u32);
        let src: Vec<u8> = (0..h)
            .flat_map(|y| {
                (0..w).map(move |x| match (y % 2, x % 2) {
                    (0, 0) | (1, 1) => 0,      // R and B sites
                    _ => (x as u8 + 1) * 10,   // G sites
                })
            })
            .collect();

        let out = bayer_rggb_box_average(&src, w, h, 2);
        assert_eq!(out.dimensions(), (2, 1));
        // Block (0,0) covers quads (0,0),(1,0),(0,1),(1,1): G1 at x=1,3 → 20, 40;
        // G2 at x=0,2 → 10, 30. Two quad-rows, so each value appears twice.
        assert_eq!(out.get_pixel(0, 0).0[0], ((20 + 10 + 40 + 30) * 2 / 8) as u8);
    }

    #[test]
    fn box_average_beats_subsampling_on_a_hard_edge() {
        // A vertical black/white edge that does not sit on a stride boundary. Subsampling
        // snaps it to the nearest sampled quad; averaging renders a gradient. This is the
        // whole reason the box mode exists as a fallback.
        let (w, h, k) = (64u32, 8u32, 4u32);
        let edge = 27u32; // deliberately not a multiple of 2k
        let src: Vec<u8> = (0..h)
            .flat_map(|_y| (0..w).map(move |x| if x < edge { 0u8 } else { 255 }))
            .collect();

        let sub = bayer_rggb_green_subsample(&src, w, h, k);
        let avg = bayer_rggb_box_average(&src, w, h, k);

        // Subsampling produces only the two extremes; averaging produces at least one
        // intermediate value where the edge crosses a block.
        assert!(sub.pixels().all(|p| p.0[0] == 0 || p.0[0] == 255));
        assert!(
            avg.pixels().any(|p| p.0[0] > 0 && p.0[0] < 255),
            "box average should render a partially covered block as an intermediate value"
        );
    }

    #[test]
    fn handles_k_larger_than_the_frame() {
        let src = flat_mosaic(8, 6, 1, 2, 3);
        for f in [bayer_rggb_green_subsample, bayer_rggb_box_average] {
            let out = f(&src, 8, 6, 99);
            assert_eq!(out.dimensions(), (1, 1));
            assert_eq!(out.get_pixel(0, 0).0[0], 2, "should still read a green site");
        }
    }

    #[test]
    fn handles_frames_too_small_for_a_quad() {
        for f in [bayer_rggb_green_subsample, bayer_rggb_box_average] {
            assert_eq!(f(&[7], 1, 1, 1).dimensions(), (0, 0));
            assert_eq!(f(&[7, 7], 2, 1, 1).dimensions(), (0, 0));
        }
    }

    #[test]
    fn odd_dimensions_truncate_without_panicking() {
        let (w, h) = (35u32, 27u32);
        let src = flat_mosaic(w, h, 10, 200, 30);
        for k in 1..=3 {
            for f in [bayer_rggb_green_subsample, bayer_rggb_box_average] {
                let out = f(&src, w, h, k);
                assert!(out.pixels().all(|p| p.0[0] == 200));
            }
        }
    }
}
