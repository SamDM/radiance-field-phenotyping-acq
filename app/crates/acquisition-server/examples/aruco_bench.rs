//! Measure what a detection actually costs, and at what marker size it stops working.
//!
//! `arucotools` publishes no timing data and states outright that speed is a secondary
//! concern, so every timing default in the auto-record feature is otherwise a guess. This
//! answers two questions:
//!
//! 1. **How long does one detection take?** That sets `--aruco-interval-ms`, and decides
//!    whether the round-robin or the interval is the binding constraint.
//! 2. **How small can the marker get before detection fails?** That is the real constraint
//!    on `--aruco-downscale`, and it is the question the rig would normally answer by
//!    someone holding a card up.
//!
//! Run with:
//!
//! ```text
//! cargo run --release --features aruco-auto-record --example aruco_bench
//! ```
//!
//! Release matters: a debug build measures rustc, not the detector.

use std::time::Instant;

use acquisition_common::downscale::{bayer_rggb_box_average, bayer_rggb_green_subsample};
use acquisition_server::aruco::synth;
use acquisition_server::aruco::{DEFAULT_MIN_PERIMETER_RATE, DEFAULT_THRESH_WIN};
use arucotools::detect::{detect_markers, DetectorParams};
use arucotools::dictionaries::predefined;
use image::GrayImage;

/// The rig's sensor: a2A3536-9gcBAS.
const SENSOR_W: u32 = 3536;
const SENSOR_H: u32 = 2748;

const FAMILY_NAME: &str = "DICT_5X5_250";
const MARKER_ID: usize = 3;

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

/// Time `runs` detections and return (p50, p95) in milliseconds.
fn time_detect(
    gray: &GrayImage,
    params: &DetectorParams,
    runs: usize,
) -> (f64, f64, usize) {
    let family = predefined::get_by_name(FAMILY_NAME).expect("known family");

    // One warm-up pass so allocator behaviour settles before measuring.
    let found = detect_markers(gray, &[family], params).markers.len();

    let mut times = Vec::with_capacity(runs);
    for _ in 0..runs {
        let t = Instant::now();
        let _ = detect_markers(gray, &[family], params);
        times.push(t.elapsed().as_secs_f64() * 1e3);
    }
    times.sort_by(f64::total_cmp);
    (percentile(&times, 0.5), percentile(&times, 0.95), found)
}

fn fast_params() -> DetectorParams {
    DetectorParams {
        adaptive_thresh_win_size_min: DEFAULT_THRESH_WIN,
        adaptive_thresh_win_size_max: DEFAULT_THRESH_WIN,
        min_marker_perimeter_rate: DEFAULT_MIN_PERIMETER_RATE,
        ..Default::default()
    }
}

/// Build the detector's actual input: a full-resolution mosaic reduced by `k`.
fn detector_input(marker_px: u32, k: u32, box_mode: bool) -> GrayImage {
    let scene = synth::scene_with_marker(SENSOR_W, SENSOR_H, &predefined::DICT_5X5_250, MARKER_ID, marker_px)
        .expect("render marker");
    let mosaic = synth::gray_to_rggb_mosaic(&scene);
    if box_mode {
        bayer_rggb_box_average(&mosaic, SENSOR_W, SENSOR_H, k)
    } else {
        bayer_rggb_green_subsample(&mosaic, SENSOR_W, SENSOR_H, k)
    }
}

fn main() {
    let runs: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);

    println!("sensor {SENSOR_W}x{SENSOR_H}, family {FAMILY_NAME}, marker id {MARKER_ID}");
    println!("{runs} runs per cell; times in ms\n");

    // ---- 1. Detection cost, and what the 3-pass default costs against one pass ----
    println!("== detection cost ==");
    println!(
        "{:>2}  {:>11}  {:>10}  {:>16}  {:>16}",
        "k", "input", "marker px", "1 pass (p50/p95)", "3 pass (p50/p95)"
    );

    // A marker large enough to be detected at every k, so the timings are comparable.
    let marker_px = 480;
    for k in [2u32, 3, 4] {
        let gray = detector_input(marker_px, k, false);
        let (f50, f95, found_fast) = time_detect(&gray, &fast_params(), runs);
        let (d50, d95, found_def) = time_detect(&gray, &DetectorParams::default(), runs);
        println!(
            "{k:>2}  {:>4}x{:<6}  {:>10}  {:>7.1}/{:<7.1}  {:>7.1}/{:<7.1}{}",
            gray.width(),
            gray.height(),
            marker_px / (2 * k),
            f50,
            f95,
            d50,
            d95,
            if found_fast == 0 || found_def == 0 { "   (NOT DETECTED)" } else { "" }
        );
    }

    // ---- 2. Downscale cost, which runs on the acquisition thread ----
    println!("\n== downscale cost (acquisition thread) ==");
    let scene =
        synth::scene_with_marker(SENSOR_W, SENSOR_H, &predefined::DICT_5X5_250, MARKER_ID, marker_px)
            .unwrap();
    let mosaic = synth::gray_to_rggb_mosaic(&scene);
    for k in [2u32, 3, 4] {
        for (name, boxed) in [("green", false), ("box", true)] {
            let mut times = Vec::with_capacity(runs);
            for _ in 0..runs {
                let t = Instant::now();
                let out = if boxed {
                    bayer_rggb_box_average(&mosaic, SENSOR_W, SENSOR_H, k)
                } else {
                    bayer_rggb_green_subsample(&mosaic, SENSOR_W, SENSOR_H, k)
                };
                times.push(t.elapsed().as_secs_f64() * 1e3);
                std::hint::black_box(out);
            }
            times.sort_by(f64::total_cmp);
            println!(
                "k={k}  {name:<6}  p50 {:>6.2}  p95 {:>6.2}",
                percentile(&times, 0.5),
                percentile(&times, 0.95)
            );
        }
    }

    // ---- 3. The sizing sweep: where does detection give out? ----
    // This is what the rig would otherwise tell us. Bracket generously rather than tuning
    // to one value, since real lighting and print quality will only make it harder.
    println!("\n== detection vs. marker size ==");
    println!("marker px in the downscaled image; '.' = missed, 'Y' = found\n");
    print!("{:>10}", "full-res");
    for k in [2u32, 3, 4] {
        print!("   k={k} (green/box)");
    }
    println!();

    for marker_px in [28u32, 42, 56, 70, 84, 98, 112, 140, 210, 280, 420] {
        print!("{marker_px:>10}");
        for k in [2u32, 3, 4] {
            let mut cells = String::new();
            for boxed in [false, true] {
                let gray = detector_input(marker_px, k, boxed);
                let family = predefined::get_by_name(FAMILY_NAME).unwrap();
                let found = detect_markers(&gray, &[family], &fast_params())
                    .markers
                    .iter()
                    .any(|m| m.marker.id == MARKER_ID);
                cells.push(if found { 'Y' } else { '.' });
            }
            let px_in_output = marker_px / (2 * k);
            print!("   {cells}  ({px_in_output:>3}px)  ");
        }
        println!();
    }

    println!(
        "\nMeasured on this machine (clean synthetic markers, 2026-09):\n\
         - Detection bottoms out around 10px in the downscaled image, well below the\n\
           ~30px the module-size rule of thumb predicts. But between 7 and 17px it is\n\
           erratic -- a size that works at one k misses at the next. Treat ~17px as the\n\
           lowest reliable size here, and aim for 30px+ on real hardware, where print\n\
           quality, motion blur and lighting all subtract margin this bench does not model.\n\
         - Below ~14px the green subsample often beats the box average: at extreme\n\
           downscale, averaging blurs adjacent modules into each other.\n\
         - One threshold pass costs about a third of the default three-pass sweep.\n\
         - The box average costs ~20x the green subsample, and it runs on the ACQUISITION\n\
           thread, whereas detection runs on the worker. Box mode therefore spends more on\n\
           the thread that matters than the entire detection does on the one that does not."
    );
}
