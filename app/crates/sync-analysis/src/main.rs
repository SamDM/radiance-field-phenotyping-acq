//! Scan a recording made against `app/sync-check-clock.html` and report a
//! bound on cross-camera synchronization error.
//!
//! Usage: `scan-recording <recording-dir>`, where the directory holds
//! `cam-0_frame-NNNNN.png`/`.json` and `cam-1_frame-NNNNN.png`/`.json` pairs,
//! as written by the acquisition server's recorder.
//!
//! # Method
//!
//! Each camera is calibrated once against every frame it has (see
//! `calibrate.rs`), fixing where its flicker strip sits in its own image —
//! this only needs *some* frames to decode, not every one, since camera and
//! phone are both static for the recording. Every frame is then measured
//! independently (`phase.rs`) against that fixed calibration, converting the
//! flicker boundary's position to a fraction of one display refresh using
//! only the page's own reported geometry.
//!
//! Frame index pairs cam-0 and cam-1 without needing anything decoded from
//! either photo: the recorder's own `camera_timestamp_ns` sidecar already
//! confirms same-index files are the same trigger (checked below, and
//! established against this project's synchronized-triggering work). For each
//! pair with a phase on both sides, the wrapped difference of the two phases,
//! scaled by the refresh period, is one estimate of the two cameras'
//! simultaneity error at that trigger. The spread of that estimate over the
//! whole recording is the bound this binary reports.
//!
//! This bound sits below the display's own refresh period by construction —
//! phase is only measured modulo one refresh — so it is meaningful exactly
//! when the underlying sync error is in fact much smaller than that period,
//! which is the regime PTP-locked triggering is expected to deliver.

use acquisition_common::{AnyFrame, Bayer, Frame};
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sync_analysis::calibrate::{self, CameraCalib};
use sync_analysis::luma::{blur1, frame_luma};
use sync_analysis::phase;

fn frames_for_camera(dir: &Path, cam: u32) -> BTreeMap<u32, PathBuf> {
    let prefix = format!("cam-{cam}_frame-");
    let mut out = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|e| e != "png") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(&prefix) else { continue };
        let Some(idx_str) = rest.strip_suffix(".png") else { continue };
        if let Ok(idx) = idx_str.parse::<u32>() {
            out.insert(idx, path);
        }
    }
    out
}

fn camera_timestamp_ns(path: &Path) -> Option<u64> {
    let f = AnyFrame::load(path).ok()?;
    Some(f.camera_timestamp_ns())
}

fn print_calib(cam: u32, c: &CameraCalib) {
    println!("cam {cam} calibration:");
    println!(
        "  corner decodes: id1={} id2={} id3={} id4={}",
        c.corner_counts[0], c.corner_counts[1], c.corner_counts[2], c.corner_counts[3]
    );
    match &c.strip {
        Some(s) => {
            println!(
                "  strip: side {} · top n={} bot n={}{}",
                s.side,
                s.top_n,
                s.bot_n,
                if s.top_n < THIN_SAMPLE_THRESHOLD || s.bot_n < THIN_SAMPLE_THRESHOLD {
                    "  (few samples — geometry is approximate)"
                } else {
                    ""
                }
            );
        }
        None => println!("  strip: none — need both ends of a corner pair (1&3 or 2&4) decoded at least once"),
    }
    match &c.geometry {
        Some(g) => println!(
            "  geometry: {:.2} Hz · viewport {:.0}x{:.0} · panel {:.0}x{:.0}",
            g.hz, g.viewport_short, g.viewport_long, g.panel_short, g.panel_long
        ),
        None => println!("  geometry: none — centre code (id 5) never decoded"),
    }
}

/// Below this many decodes, a strip endpoint is thin enough that its
/// position — and hence that camera's whole pixel-to-phase scale — should
/// be treated as approximate rather than precise.
const THIN_SAMPLE_THRESHOLD: usize = 10;

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");
    let dir = PathBuf::from(
        args.iter()
            .find(|a| !a.starts_with('-'))
            .ok_or("usage: scan-recording [-v|--verbose] <recording-dir>")?,
    );

    let cam0 = frames_for_camera(&dir, 0);
    let cam1 = frames_for_camera(&dir, 1);
    if cam0.is_empty() || cam1.is_empty() {
        return Err(format!(
            "found {} cam-0 and {} cam-1 frames in {dir:?} — need both cameras present",
            cam0.len(),
            cam1.len()
        )
        .into());
    }
    println!("{}: {} cam-0 frames, {} cam-1 frames", dir.display(), cam0.len(), cam1.len());

    println!("\ncalibrating (decoding every frame once)...");
    let paths0: Vec<PathBuf> = cam0.values().cloned().collect();
    let paths1: Vec<PathBuf> = cam1.values().cloned().collect();
    let (calib0, calib1) = rayon::join(|| calibrate::calibrate(&paths0), || calibrate::calibrate(&paths1));
    print_calib(0, &calib0);
    print_calib(1, &calib1);

    if calib0.strip.is_none() || calib0.geometry.is_none() {
        return Err("cam 0: could not calibrate a strip and/or read the page geometry — \
                     too few QR decodes across this recording to proceed"
            .into());
    }
    if calib1.strip.is_none() || calib1.geometry.is_none() {
        return Err("cam 1: could not calibrate a strip and/or read the page geometry — \
                     too few QR decodes across this recording to proceed"
            .into());
    }
    let (g0, g1) = (calib0.geometry.unwrap(), calib1.geometry.unwrap());
    if (g0.hz - g1.hz).abs() > 0.5 {
        println!(
            "\nwarning: cameras report different refresh rates ({:.2} vs {:.2} Hz) — \
             the phone's measured Hz can drift a little between recordings; using cam 0's.",
            g0.hz, g1.hz
        );
    }
    let hz = g0.hz;

    // Pair by frame index. The recorder writes matched indices from one
    // synchronized capture group, but that is a claim worth checking rather
    // than assuming — a large camera_timestamp_ns mismatch would mean the
    // pairing itself is wrong, which would invalidate everything downstream.
    let shared: Vec<u32> = cam0.keys().filter(|k| cam1.contains_key(k)).copied().collect();
    println!("\n{} frame indices present for both cameras", shared.len());

    println!("measuring phase per frame...");
    struct Row {
        idx: u32,
        ts_diff_ns: i64,
        phase0: Option<f64>,
        phase1: Option<f64>,
    }
    let mut rows: Vec<Row> = shared
        .par_iter()
        .map(|&idx| {
            let p0 = &cam0[&idx];
            let p1 = &cam1[&idx];
            let ts0 = camera_timestamp_ns(p0);
            let ts1 = camera_timestamp_ns(p1);
            let ts_diff_ns = match (ts0, ts1) {
                (Some(a), Some(b)) => a as i64 - b as i64,
                _ => i64::MAX,
            };
            let phase0 = measure_one(p0, &calib0);
            let phase1 = measure_one(p1, &calib1);
            Row { idx, ts_diff_ns, phase0, phase1 }
        })
        .collect();
    rows.sort_by_key(|r| r.idx);

    let bad_pairing = rows.iter().filter(|r| r.ts_diff_ns.unsigned_abs() > 1_000_000).count();
    if bad_pairing > 0 {
        println!(
            "warning: {bad_pairing} of {} pairs have camera_timestamp_ns differing by >1ms — \
             frame-index pairing may not reflect the same trigger for these",
            rows.len()
        );
    }

    let period_us = 1e6 / hz;

    if verbose {
        println!("\nper-frame (phase as % of one refresh, diff wrapped to the nearer refresh):");
        for r in &rows {
            let diff = match (r.phase0, r.phase1) {
                (Some(p0), Some(p1)) => format!("{:+8.1} us", phase::wrapped_diff(p0, p1) * period_us),
                _ => "     n/a".into(),
            };
            println!(
                "  frame {:>5}  cam0 {}  cam1 {}  diff {diff}",
                r.idx,
                r.phase0.map_or("  n/a ".into(), |p| format!("{:5.1}%", p * 100.0)),
                r.phase1.map_or("  n/a ".into(), |p| format!("{:5.1}%", p * 100.0)),
            );
        }
    }

    let mut diffs_us: Vec<f64> = Vec::new();
    let mut measured_both = 0;
    for r in &rows {
        if let (Some(p0), Some(p1)) = (r.phase0, r.phase1) {
            measured_both += 1;
            diffs_us.push(phase::wrapped_diff(p0, p1) * period_us);
        }
    }

    println!(
        "\nphase measured on both cameras for {measured_both}/{} pairs \
         (cam0 alone: {}, cam1 alone: {}, neither: {})",
        rows.len(),
        rows.iter().filter(|r| r.phase0.is_some() && r.phase1.is_none()).count(),
        rows.iter().filter(|r| r.phase0.is_none() && r.phase1.is_some()).count(),
        rows.iter().filter(|r| r.phase0.is_none() && r.phase1.is_none()).count(),
    );

    if diffs_us.len() < 5 {
        println!(
            "\ntoo few paired measurements ({}) to report a bound. This recording's exposure, \
             frame count, or lighting may not suit the method — see the module docs in \
             src/phase.rs for what a valid measurement requires.",
            diffs_us.len()
        );
        return Ok(());
    }

    diffs_us.sort_by(|a, b| a.total_cmp(b));
    let n = diffs_us.len() as f64;
    let mean = diffs_us.iter().sum::<f64>() / n;
    let variance = diffs_us.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / n;
    let sd = variance.sqrt();
    let median = percentile(&diffs_us, 0.5);
    let p5 = percentile(&diffs_us, 0.05);
    let p95 = percentile(&diffs_us, 0.95);

    // Coarse drift check: split the recording in half by frame index and
    // compare means. A large shift here would suggest the servo (or the
    // measurement itself) is not stationary over the recording, which the
    // single spread number below would hide.
    let mid = rows.len() / 2;
    let (first_half, second_half): (Vec<f64>, Vec<f64>) = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| Some((i, r.phase0?, r.phase1?)))
        .map(|(i, p0, p1)| (i, phase::wrapped_diff(p0, p1) * period_us))
        .fold((Vec::new(), Vec::new()), |(mut a, mut b), (i, d)| {
            if i < mid { a.push(d) } else { b.push(d) }
            (a, b)
        });
    let half_mean = |v: &[f64]| if v.is_empty() { f64::NAN } else { v.iter().sum::<f64>() / v.len() as f64 };

    println!("\n== cam0 - cam1 phase difference, in microseconds (wrapped to ±{:.0}) ==", period_us / 2.0);
    println!("  n            {}", diffs_us.len());
    println!("  mean         {mean:+.1} us");
    println!("  std dev      {sd:.1} us");
    println!("  median       {median:+.1} us");
    println!("  p5 .. p95    {p5:+.1} .. {p95:+.1} us");
    println!("  min .. max   {:+.1} .. {:+.1} us", diffs_us[0], diffs_us[diffs_us.len() - 1]);
    println!(
        "  first half vs second half mean: {:+.1} vs {:+.1} us (shift {:+.1} us)",
        half_mean(&first_half),
        half_mean(&second_half),
        half_mean(&second_half) - half_mean(&first_half)
    );

    // Per-camera phase drift rate, independent of the other camera: each
    // trigger sits at a slightly different point in the panel's refresh cycle
    // (the trigger period and the display's own oscillator are uncorrelated),
    // so phase is expected to ramp steadily across a recording even for a
    // single, perfectly calibrated camera. That ramp is not sync error. What
    // *would* indicate a problem is the two cameras' ramps disagreeing — the
    // simplest innocent cause of which is the strip's length being under-
    // determined, since a single bottom-corner decode sets that camera's whole
    // pixel-to-phase scale and any error in it appears as a multiplicative
    // rate error that compounds every frame instead of cancelling.
    let slope = |pts: &[(f64, f64)]| -> f64 {
        if pts.len() < 2 {
            return f64::NAN;
        }
        let n = pts.len() as f64;
        let (sx, sy) = pts.iter().fold((0.0, 0.0), |(sx, sy), (x, y)| (sx + x, sy + y));
        let (mx, my) = (sx / n, sy / n);
        let (mut num, mut den) = (0.0, 0.0);
        for &(x, y) in pts {
            num += (x - mx) * (y - my);
            den += (x - mx) * (x - mx);
        }
        if den == 0.0 { f64::NAN } else { num / den }
    };
    let pts0: Vec<(f64, f64)> = rows.iter().enumerate().filter_map(|(i, r)| Some((i as f64, r.phase0?))).collect();
    let pts1: Vec<(f64, f64)> = rows.iter().enumerate().filter_map(|(i, r)| Some((i as f64, r.phase1?))).collect();
    let slope0_us = slope(&pts0) * period_us;
    let slope1_us = slope(&pts1) * period_us;
    println!(
        "\nphase drift per frame: cam0 {slope0_us:+.1} us/frame · cam1 {slope1_us:+.1} us/frame \
         (difference {:+.1} us/frame)",
        slope0_us - slope1_us
    );
    if (slope0_us - slope1_us).abs() * rows.len() as f64 > sd.max(50.0) {
        // A strip anchored on very few corner decodes has an under-determined
        // length, and a scale error in that length shows up as exactly this
        // kind of compounding rate mismatch — the innocent, calibration-side
        // explanation. Once both strips rest on a solid number of decodes
        // (the reason this crate decouples calibration from measurement in
        // the first place, see calibrate.rs), that explanation is spent, and
        // a persistent rate mismatch is more likely camera-vs-camera relative
        // drift — the trigger and the display's own oscillator are
        // uncorrelated, so both cameras should track the *same* drift if they
        // are seeing the same instants; a difference between them is a
        // legitimate thing to keep watching across further recordings.
        let thin = |s: &calibrate::StripCalib| {
            s.top_n < THIN_SAMPLE_THRESHOLD || s.bot_n < THIN_SAMPLE_THRESHOLD
        };
        if calib0.strip.as_ref().is_some_and(thin) || calib1.strip.as_ref().is_some_and(thin) {
            println!(
                "  these disagree enough, accumulated over the recording, to plausibly account \
                 for the first/second-half shift above by itself — see the strip's top_n/bot_n \
                 counts printed in the calibration section. More frames (or better visibility of \
                 the corners that decoded only a few times) would tighten this rather than more \
                 analysis."
            );
        } else {
            println!(
                "  both strips rest on a solid number of decodes (see top_n/bot_n above), so a \
                 scale error in the calibration is not a likely explanation here. This mismatch \
                 is worth treating as a real signal — check whether it repeats in the same \
                 direction across other recordings rather than dismissing it as noise."
            );
        }
    }

    println!(
        "\nRead this as: the *mean* mixes in an unknown, fixed geometric offset from each \
         camera's mounting angle and cannot be interpreted as absolute simultaneity error. \
         The *spread* (std dev / p5..p95) is not subject to that offset and is the more \
         defensible bound on relative sync jitter — but only once a per-camera phase ramp \
         (normal, see above) has been ruled out as the dominant contributor to any \
         first/second-half shift or wide spread."
    );

    Ok(())
}

fn measure_one(path: &Path, calib: &CameraCalib) -> Option<f64> {
    let f = Frame::<Bayer>::load(path).ok()?;
    let img = blur1(&frame_luma(&f.data, f.width, f.height));
    phase::measure(&img, calib)
}
