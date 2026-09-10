use image::{ImageBuffer, Rgb, RgbImage};
use rayon::prelude::*;

#[derive(Debug)]
pub struct ShapeMismatch {
    pub input: (u32, u32),
    pub output: (u32, u32),
}

impl std::fmt::Display for ShapeMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "input shape {:?} does not match output shape {:?}",
            self.input, self.output
        )
    }
}

impl std::error::Error for ShapeMismatch {}

// TODO: go back to image::GrayImage here
pub fn debayer_with(src: &[u8], w: u32, h: u32, debayered: &mut RgbImage) -> Result<(), ShapeMismatch> {
    if (w, h) != debayered.dimensions() {
        return Err(ShapeMismatch {
            input: (w, h),
            output: debayered.dimensions(),
        });
    }
    let w = w as usize;
    let h = h as usize;
    debayered
        .as_mut()
        .par_chunks_mut(w * 3)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..w {
                let (r, g, b) = bilinear_rggb_raw(src, w, h, x, y);
                row[x * 3] = r;
                row[x * 3 + 1] = g;
                row[x * 3 + 2] = b;
            }
        });
    Ok(())
}

// TODO: go back to image::GrayImage here
pub fn debayer(src: &[u8], w: u32, h: u32) -> RgbImage {
    let mut out = ImageBuffer::new(w, h);
    debayer_with(src, w, h, &mut out).expect("same-shape output");
    out
}

/// Collapse each 2×2 RGGB quad to a single RGB pixel, producing a `w/2`×`h/2` image.
///
/// This is the only phase-correct way to shrink a Bayer mosaic. Resizing the mosaic
/// itself blends across color-filter sites, which destroys the RGGB phase and leaves a
/// buffer that is no longer debayerable at all. Building every output pixel from exactly
/// one complete quad sidesteps that: no interpolation, no false color, and the result is
/// a true RGB image that can then be resized like any other.
///
/// Cheaper than `debayer` by much more than the 4× pixel count suggests — there is no
/// neighbourhood gather and no edge clamping, just four sequential reads per output
/// pixel. Use it when the consumer wants a downscaled image anyway (previews, detection);
/// use `debayer` when full resolution is the point.
///
/// Odd widths and heights truncate: a trailing row or column has no complete quad.
pub fn debayer_half(src: &[u8], w: u32, h: u32) -> RgbImage {
    let wu = w as usize;
    let out_w = wu / 2;
    let out_h = h as usize / 2;
    if out_w == 0 || out_h == 0 {
        return ImageBuffer::new(0, 0);
    }
    let mut out = ImageBuffer::new(out_w as u32, out_h as u32);
    out.as_mut()
        .par_chunks_mut(out_w * 3)
        .enumerate()
        .for_each(|(oy, row)| {
            // RGGB: even source rows are `R G R G …`, odd rows are `G B G B …`.
            let top = &src[(2 * oy) * wu..][..wu];
            let bot = &src[(2 * oy + 1) * wu..][..wu];
            for ox in 0..out_w {
                let x = 2 * ox;
                row[ox * 3] = top[x];
                row[ox * 3 + 1] = avg2(top[x + 1], bot[x]);
                row[ox * 3 + 2] = bot[x + 1];
            }
        });
    out
}

/// Debayer only the `cw`×`ch` sub-region with top-left at `(x0, y0)` of a full
/// `w`×`h` RGGB image. Uses absolute source coordinates so the Bayer phase and
/// edge clamping match a full-frame `debayer`.
pub fn debayer_crop(src: &[u8], w: u32, h: u32, x0: u32, y0: u32, cw: u32, ch: u32) -> RgbImage {
    let wu = w as usize;
    let hu = h as usize;
    let mut out = ImageBuffer::new(cw, ch);
    for cy in 0..ch {
        for cx in 0..cw {
            let sx = (x0 + cx) as usize;
            let sy = (y0 + cy) as usize;
            let (r, g, b) = bilinear_rggb_raw(src, wu, hu, sx, sy);
            out.put_pixel(cx, cy, Rgb([r, g, b]));
        }
    }
    out
}

#[inline(always)]
fn clamp_get_raw(src: &[u8], w: usize, h: usize, x: i64, y: i64) -> u8 {
    let cx = x.clamp(0, w as i64 - 1) as usize;
    let cy = y.clamp(0, h as i64 - 1) as usize;
    src[cy * w + cx]
}

#[inline(always)]
fn avg2(a: u8, b: u8) -> u8 {
    ((a as u16 + b as u16) / 2) as u8
}

#[inline(always)]
fn avg4(a: u8, b: u8, c: u8, d: u8) -> u8 {
    ((a as u16 + b as u16 + c as u16 + d as u16) / 4) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `w`×`h` RGGB mosaic where every site carries its channel's constant value.
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

    #[test]
    fn half_recovers_flat_colour_exactly() {
        let (w, h) = (16u32, 12u32);
        let src = flat_mosaic(w, h, 10, 200, 30);
        let out = debayer_half(&src, w, h);
        assert_eq!(out.dimensions(), (w / 2, h / 2));
        for px in out.pixels() {
            assert_eq!(px.0, [10, 200, 30]);
        }
    }

    #[test]
    fn half_reads_the_correct_quad_sites() {
        // A mosaic whose every byte is its own linear index makes a wrong offset by even
        // one site immediately visible.
        let (w, h) = (8u32, 4u32);
        let src: Vec<u8> = (0..(w * h) as u8).collect();
        let out = debayer_half(&src, w, h);
        // Quad (1, 0) occupies source pixels (2,0),(3,0),(2,1),(3,1) = 2, 3, 10, 11.
        assert_eq!(out.get_pixel(1, 0).0, [2, (3 + 10) / 2, 11]);
        // Quad (0, 1) occupies (0,2),(1,2),(0,3),(1,3) = 16, 17, 24, 25.
        assert_eq!(out.get_pixel(0, 1).0, [16, (17 + 24) / 2, 25]);
    }

    #[test]
    fn half_tracks_the_full_debayer_on_real_content() {
        // The preview path swaps `debayer` for `debayer_half`, so the two must agree on
        // what the scene looks like. Compare per-channel means over a smooth gradient:
        // they sample the same mosaic differently, so demand closeness, not equality.
        let (w, h) = (64u32, 48u32);
        let src: Vec<u8> = (0..(w * h))
            .map(|i| {
                let (x, y) = (i % w, i / w);
                ((x * 3 + y * 2) % 256) as u8
            })
            .collect();

        let full = debayer(&src, w, h);
        let half = debayer_half(&src, w, h);

        for ch in 0..3 {
            let mean = |img: &RgbImage| -> f64 {
                img.pixels().map(|p| p.0[ch] as f64).sum::<f64>() / img.pixels().len() as f64
            };
            let (a, b) = (mean(&full), mean(&half));
            assert!(
                (a - b).abs() < 8.0,
                "channel {ch} means diverge: full {a:.2} vs half {b:.2}"
            );
        }
    }

    #[test]
    fn half_truncates_incomplete_quads() {
        // Odd dimensions have a trailing row/column with no complete quad.
        let (w, h) = (7u32, 5u32);
        let src = flat_mosaic(w, h, 1, 2, 3);
        assert_eq!(debayer_half(&src, w, h).dimensions(), (3, 2));
    }

    #[test]
    fn half_handles_images_too_small_for_a_quad() {
        assert_eq!(debayer_half(&[9], 1, 1).dimensions(), (0, 0));
        assert_eq!(debayer_half(&[9, 9], 2, 1).dimensions(), (0, 0));
    }

    #[test]
    fn crop_matches_full_debayer() {
        let (w, h) = (16u32, 12u32);
        let src: Vec<u8> = (0..(w * h)).map(|i| (i % 256) as u8).collect();
        let full = debayer(&src, w, h);

        // Interior region and an edge-touching region (exercises clamping).
        for (x0, y0, cw, ch) in [(3u32, 4u32, 5u32, 5u32), (0, 0, 4, 4), (12, 8, 4, 4)] {
            let crop = debayer_crop(&src, w, h, x0, y0, cw, ch);
            for cy in 0..ch {
                for cx in 0..cw {
                    assert_eq!(
                        crop.get_pixel(cx, cy),
                        full.get_pixel(x0 + cx, y0 + cy),
                        "mismatch at crop ({cx},{cy}) of region ({x0},{y0},{cw},{ch})"
                    );
                }
            }
        }
    }
}

fn bilinear_rggb_raw(src: &[u8], w: usize, h: usize, x: usize, y: usize) -> (u8, u8, u8) {
    let g = |dx: i64, dy: i64| clamp_get_raw(src, w, h, x as i64 + dx, y as i64 + dy);
    let p = g(0, 0);
    match (y % 2, x % 2) {
        (0, 0) => {
            let green = avg4(g(-1, 0), g(1, 0), g(0, -1), g(0, 1));
            let blue = avg4(g(-1, -1), g(1, -1), g(-1, 1), g(1, 1));
            (p, green, blue)
        }
        (0, 1) => {
            let red = avg2(g(-1, 0), g(1, 0));
            let blue = avg2(g(0, -1), g(0, 1));
            (red, p, blue)
        }
        (1, 0) => {
            let red = avg2(g(0, -1), g(0, 1));
            let blue = avg2(g(-1, 0), g(1, 0));
            (red, p, blue)
        }
        (1, 1) => {
            let green = avg4(g(-1, 0), g(1, 0), g(0, -1), g(0, 1));
            let red = avg4(g(-1, -1), g(1, -1), g(-1, 1), g(1, 1));
            (red, green, p)
        }
        _ => unreachable!(),
    }
}
