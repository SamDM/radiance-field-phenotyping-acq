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
