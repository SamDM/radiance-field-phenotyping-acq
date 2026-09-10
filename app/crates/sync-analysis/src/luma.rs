//! Converting a raw Bayer frame to the grayscale image the QR decoder and the
//! phase measurement both work on.

use acquisition_common::debayer;
use image::GrayImage;

/// Debayer, then collapse to luma with rec.601-ish integer weights.
///
/// Full-resolution rather than a half-res average: decode coverage on these
/// recordings was measured to be substantially better at full resolution (the
/// QR modules are only a few px wide at half-res, which is marginal for any
/// decoder), even though the raw sensor read is noisy enough that a debayered
/// full-res image also benefits from [`blur1`].
pub fn frame_luma(bayer: &[u8], w: u32, h: u32) -> GrayImage {
    let rgb = debayer::debayer(bayer, w, h);
    let mut g = GrayImage::new(w, h);
    for (i, p) in rgb.pixels().enumerate() {
        g.as_mut()[i] = ((p.0[0] as u32 * 2 + p.0[1] as u32 * 5 + p.0[2] as u32) / 8) as u8;
    }
    g
}

/// Light blur before QR detection.
///
/// Still measurably worth it with `rxing` (see `qr.rs`'s module docs for why
/// that replaced the original decoder): a spot check across the same
/// recordings found meaningfully more codes decoded per frame with this blur
/// applied than without it. Sigma 1.0 is sharp enough to keep the finder
/// patterns' edges crisp while soft enough to average out the sensor's
/// per-pixel read noise, which at a short exposure is comparable in scale to
/// a QR module.
pub fn blur1(img: &GrayImage) -> GrayImage {
    image::imageops::blur(img, 1.0)
}

/// Bilinear sample at a fractional pixel coordinate. Clamps to the image
/// bounds rather than panicking, since the calibrated strip axis is an
/// extrapolation from a handful of QR detections and can legitimately end up
/// a pixel or two outside the frame.
pub fn sample_bilinear(img: &GrayImage, x: f32, y: f32) -> f32 {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let x = x.clamp(0.0, w - 1.001);
    let y = y.clamp(0.0, h - 1.001);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (x0, y0) = (x0 as u32, y0 as u32);
    let px = |dx: u32, dy: u32| img.get_pixel(x0 + dx, y0 + dy).0[0] as f32;
    let top = px(0, 0) * (1.0 - fx) + px(1, 0) * fx;
    let bot = px(0, 1) * (1.0 - fx) + px(1, 1) * fx;
    top * (1.0 - fy) + bot * fy
}
