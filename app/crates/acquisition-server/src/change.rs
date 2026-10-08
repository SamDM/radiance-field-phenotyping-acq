//! Decide whether a frame group shows anything the recording does not already have.
//!
//! A machine that stalls or slows down keeps the cameras running, and every frame of the
//! stall is the same picture. Saving them costs disk and buys nothing, so a group is saved
//! only if some camera has changed enough since the last *saved* group. Comparing against
//! the last saved group, not the previous one, means slow drift still accumulates and gets
//! recorded once it adds up, while a true standstill saves nothing.
//!
//! # The metric
//!
//! Each frame is reduced to a ~147×147 thumbnail (green subsample at quad stride 3, then a
//! 4×4 box average), and the score is the share of thumbnail pixels whose difference from
//! the reference strays more than [`LEVEL`] grey levels from the *median* difference.
//! Subtracting the median cancels a global brightness shift — exposure creep, a light
//! flickering — that changes every pixel by the same amount without moving anything.
//!
//! Measured on the watering rig (2026-10-07): a pot held still scores exactly 0%, a turning
//! pot 1.4–7% between adjacent frames. Sensor noise at gain 10 has p99 |Δ| = 8, so
//! [`LEVEL`] sits at twice that.

use acquisition_common::downscale::bayer_rggb_green_subsample;
use image::GrayImage;

/// Quad stride of the green subsample.
const SUBSAMPLE: u32 = 3;
/// Side of the box average applied after the subsample.
const BOX: u32 = 4;
/// Grey levels a pixel's difference must stray from the median difference to count.
pub const LEVEL: i16 = 16;

/// Shrink a raw RGGB mosaic to the thumbnail the metric compares.
///
/// About 1 ms for the rig's 3536×3536 sensor: the subsample reads one byte per output
/// pixel, and the box average runs on the already small result.
pub fn thumbnail(data: &[u8], w: u32, h: u32) -> GrayImage {
    let green = bayer_rggb_green_subsample(data, w, h, SUBSAMPLE);
    let (gw, gh) = green.dimensions();
    let (tw, th) = ((gw / BOX).max(1), (gh / BOX).max(1));
    GrayImage::from_fn(tw, th, |tx, ty| {
        let mut sum = 0u32;
        let mut n = 0u32;
        for y in ty * BOX..((ty + 1) * BOX).min(gh) {
            for x in tx * BOX..((tx + 1) * BOX).min(gw) {
                sum += green.get_pixel(x, y).0[0] as u32;
                n += 1;
            }
        }
        image::Luma([(sum / n.max(1)) as u8])
    })
}

/// Share of pixels, in `[0, 1]`, whose difference strays more than [`LEVEL`] from the
/// median difference. Thumbnails of different sizes count as entirely changed.
pub fn changed_fraction(a: &GrayImage, b: &GrayImage) -> f64 {
    if a.dimensions() != b.dimensions() || a.as_raw().is_empty() {
        return 1.0;
    }
    let deltas = || a.as_raw().iter().zip(b.as_raw()).map(|(&p, &q)| p as i16 - q as i16);

    // The median through a histogram: differences span only -255..=255.
    let mut histogram = [0u32; 511];
    for d in deltas() {
        histogram[(d + 255) as usize] += 1;
    }
    let half = (a.as_raw().len() as u32).div_ceil(2);
    let mut seen = 0;
    let mut median = 0i16;
    for (i, &count) in histogram.iter().enumerate() {
        seen += count;
        if seen >= half {
            median = i as i16 - 255;
            break;
        }
    }

    let changed = deltas().filter(|d| (d - median).abs() > LEVEL).count();
    changed as f64 / a.as_raw().len() as f64
}

/// Remembers each camera's last saved thumbnail and judges new groups against it.
pub struct ChangeGate {
    /// Minimum [`changed_fraction`], on at least one camera, for a group to be saved.
    min_fraction: f64,
    last_saved: Vec<Option<GrayImage>>,
    /// Groups judged unchanged since the last reset.
    skipped: u32,
}

/// What [`ChangeGate::judge`] decided, and on what evidence.
pub struct Judgement {
    pub save: bool,
    /// The largest change across the group's cameras; `1.0` with no reference yet.
    pub max_fraction: f64,
    thumbnails: Vec<GrayImage>,
}

impl ChangeGate {
    pub fn new(min_fraction: f64) -> Self {
        ChangeGate { min_fraction, last_saved: Vec::new(), skipped: 0 }
    }

    /// Forget the references, so the next group is saved whatever it shows. Called when a
    /// recording starts: its first group has nothing to be compared against.
    pub fn reset(&mut self) {
        self.last_saved.clear();
        self.skipped = 0;
    }

    /// Groups skipped as unchanged since the last reset.
    pub fn skipped(&self) -> u32 {
        self.skipped
    }

    /// Judge one group, given each camera's raw mosaic as `(data, width, height)`.
    ///
    /// The group is saved if any camera changed enough: the cameras see one scene, and
    /// keeping the group whole is what keeps the stereo pairs together.
    pub fn judge<'a>(&self, mosaics: impl IntoIterator<Item = (&'a [u8], u32, u32)>) -> Judgement {
        let thumbnails: Vec<GrayImage> =
            mosaics.into_iter().map(|(data, w, h)| thumbnail(data, w, h)).collect();
        let max_fraction = thumbnails
            .iter()
            .enumerate()
            .map(|(cam, thumb)| match self.last_saved.get(cam) {
                Some(Some(reference)) => changed_fraction(thumb, reference),
                _ => 1.0,
            })
            .fold(0.0, f64::max);
        Judgement { save: max_fraction >= self.min_fraction, max_fraction, thumbnails }
    }

    /// Make a saved group the new reference.
    pub fn saved(&mut self, judgement: Judgement) {
        self.last_saved = judgement.thumbnails.into_iter().map(Some).collect();
    }

    /// Count a group that was judged unchanged and not saved.
    pub fn skip(&mut self) {
        self.skipped += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 240;
    const H: u32 = 240;

    /// A mosaic whose green sites carry `f(x, y)`, the others a constant.
    fn mosaic(f: impl Fn(u32, u32) -> u8) -> Vec<u8> {
        (0..H)
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .map(|(x, y)| if (x + y) % 2 == 1 { f(x, y) } else { 40 })
            .collect()
    }

    fn gradient(x: u32, _: u32) -> u8 {
        (x * 255 / W) as u8
    }

    #[test]
    fn the_thumbnail_is_the_subsample_shrunk_by_the_box() {
        let t = thumbnail(&mosaic(gradient), W, H);
        assert_eq!(t.dimensions(), (W / (2 * SUBSAMPLE) / BOX, H / (2 * SUBSAMPLE) / BOX));
    }

    #[test]
    fn identical_frames_have_not_changed() {
        let t = thumbnail(&mosaic(gradient), W, H);
        assert_eq!(changed_fraction(&t, &t), 0.0);
    }

    #[test]
    fn a_global_brightness_shift_is_not_change() {
        let a = thumbnail(&mosaic(gradient), W, H);
        let b = thumbnail(&mosaic(|x, y| gradient(x, y).saturating_add(30)), W, H);
        // Only pixels clipped at 255 differ from the median shift.
        assert!(changed_fraction(&a, &b) < 0.15, "{}", changed_fraction(&a, &b));
    }

    #[test]
    fn a_moved_object_is_change() {
        let block = |x0: u32| move |x: u32, y: u32| if (x0..x0 + 60).contains(&x) && y < 120 { 220 } else { 30 };
        let a = thumbnail(&mosaic(block(20)), W, H);
        let b = thumbnail(&mosaic(block(140)), W, H);
        assert!(changed_fraction(&a, &b) > 0.1);
    }

    #[test]
    fn the_first_group_is_always_saved_and_a_repeat_is_not() {
        let m = mosaic(gradient);
        let mut gate = ChangeGate::new(0.005);
        let first = gate.judge([(&m[..], W, H), (&m[..], W, H)]);
        assert!(first.save);
        gate.saved(first);
        assert!(!gate.judge([(&m[..], W, H), (&m[..], W, H)]).save);
    }

    #[test]
    fn one_changed_camera_saves_the_whole_group() {
        let still = mosaic(gradient);
        let moved = mosaic(|x, y| if x < 120 { 250 } else { gradient(x, y) });
        let mut gate = ChangeGate::new(0.005);
        let first = gate.judge([(&still[..], W, H), (&still[..], W, H)]);
        gate.saved(first);
        assert!(gate.judge([(&still[..], W, H), (&moved[..], W, H)]).save);
    }

    #[test]
    fn reset_forgets_the_reference() {
        let m = mosaic(gradient);
        let mut gate = ChangeGate::new(0.005);
        let first = gate.judge([(&m[..], W, H)]);
        gate.saved(first);
        gate.reset();
        assert!(gate.judge([(&m[..], W, H)]).save);
    }
}
