//! End-to-end tests for the watcher, with no cameras and no Pylon involvement.
//!
//! An integration test rather than a unit one: it drives the watcher entirely through
//! its public surface, exactly as the acquisition loop does, so nothing here can reach
//! into internals the loop could not. Gated by `required-features` in `Cargo.toml`.
//!
//! Everything else in this module is tested in isolation: the downscalers against
//! synthetic mosaics, the trigger against synthetic detections. These tests join the
//! pieces — a full-resolution RGGB mosaic goes in one end and an [`AutoAction`] comes out
//! the other, through the real downscale, the real channel handoff, the real detector and
//! the real state machine.
//!
//! That matters because the seams are where the plausible bugs live. An off-by-one in the
//! green-site index still produces a perfectly reasonable-looking image; only running an
//! actual detector over it reveals that the image is a checkerboard. Likewise, the
//! round-robin and in-flight interlocks only misbehave in combination.
//!
//! Since detection is asynchronous, these tests offer frames in a loop and poll until an
//! action arrives, bounded by an iteration cap rather than a wall-clock timeout.

use std::time::Duration;

use acquisition_common::{Bayer, Frame};
use arucotools::dictionaries::predefined;
use image::GrayImage;
use time::OffsetDateTime;

use acquisition_server::aruco::synth;
use acquisition_server::aruco::{
    AutoAction, AutoRecordCfg, AutoRecordWatcher, MarkerGate, MarkerSelector, PollCtx,
};

/// Small enough to keep the test fast, large enough to stay realistic.
const W: u32 = 1024;
const H: u32 = 768;
const FAMILY: &str = "DICT_5X5_250";
const ID: usize = 30;

fn frame_from_scene(scene: &GrayImage) -> Frame<Bayer> {
    Frame::new(
        synth::gray_to_rggb_mosaic(scene),
        scene.width(),
        scene.height(),
        0,
        OffsetDateTime::UNIX_EPOCH,
        "cam".into(),
    )
}

/// A frame showing the marker at `marker_px`, and one showing nothing.
fn frames(marker_px: u32) -> (Frame<Bayer>, Frame<Bayer>) {
    let present = frame_from_scene(
        &synth::scene_with_marker(W, H, &predefined::DICT_5X5_250, ID, marker_px).unwrap(),
    );
    let absent = frame_from_scene(&synth::empty_scene(W, H));
    (present, absent)
}

fn cfg(n_cameras: usize) -> AutoRecordCfg {
    AutoRecordCfg {
        markers: vec![MarkerSelector { family: FAMILY.into(), ids: vec![ID] }],
        // No dispatch floor: these tests want every offered frame examined, and the
        // interlocks under test are the in-flight flag and the round-robin, not the clock.
        interval: Duration::ZERO,
        downscale: 2,
        cooldown: Duration::ZERO,
        cameras: Some((0..n_cameras).collect()),
        ..Default::default()
    }
}

/// How long each pump round waits for the detector.
///
/// A bare `yield_now` is not enough: a round that finds a detection already in flight costs
/// essentially nothing, so the loop laps the detector thousands of times over before it
/// finishes even one frame, and the test concludes "no detection" while the detector is
/// still working. Sleeping makes each round worth something.
const ROUND: Duration = Duration::from_millis(1);

/// Feed `frame` on `cam` until an action arrives or `max_rounds` is exhausted.
///
/// Detection is off-thread, so a single offer/poll pair returns nothing: the poll happens
/// long before the detector has finished. Looping is what the acquisition loop does anyway.
fn pump(
    watcher: &mut AutoRecordWatcher,
    frame: &Frame<Bayer>,
    cam: usize,
    ctx: PollCtx,
    max_rounds: usize,
) -> Option<AutoAction> {
    for _ in 0..max_rounds {
        watcher.offer(frame, cam);
        if let Some(action) = watcher.poll(ctx) {
            return Some(action);
        }
        std::thread::sleep(ROUND);
    }
    None
}

fn idle() -> PollCtx {
    PollCtx { recording: false, auto_started: false, active: true }
}

fn auto_recording() -> PollCtx {
    PollCtx { recording: true, auto_started: true, active: true }
}

/// The headline path: a marker in front of a camera starts a recording, and taking it
/// away stops it.
#[test]
fn a_visible_marker_starts_a_recording_and_its_removal_stops_it() {
    let (present, absent) = frames(240);
    let mut w = AutoRecordWatcher::new(&cfg(1), 1).unwrap();
    assert!(w.is_enabled());

    let action = pump(&mut w, &present, 0, idle(), 200);
    assert!(
        matches!(&action, Some(AutoAction::Start { family, id, camera: 0 }) if family == FAMILY && *id == ID),
        "expected a start from the visible marker, got {action:?}"
    );

    let action = pump(&mut w, &absent, 0, auto_recording(), 200);
    assert!(
        matches!(action, Some(AutoAction::Stop)),
        "expected a stop once the marker left, got {action:?}"
    );
}

/// An empty scene must never trigger. A false start is the expensive failure: it fills the
/// disk with a recording nobody asked for.
#[test]
fn an_empty_scene_never_triggers() {
    let (_, absent) = frames(240);
    let mut w = AutoRecordWatcher::new(&cfg(1), 1).unwrap();
    assert_eq!(pump(&mut w, &absent, 0, idle(), 200), None);
}

/// A marker that is not in the configured id list must not trigger, even though it is a
/// perfectly valid marker of the configured family. The detector identifies it as itself,
/// and the marker set drops it.
#[test]
fn an_unconfigured_marker_of_the_same_family_does_not_trigger() {
    let frame = frame_from_scene(
        &synth::scene_with_marker(W, H, &predefined::DICT_5X5_250, ID + 1, 240).unwrap(),
    );
    let mut w = AutoRecordWatcher::new(&cfg(1), 1).unwrap();
    assert_eq!(pump(&mut w, &frame, 0, idle(), 200), None);
}

/// With nothing configured the watcher spawns no thread and does nothing at all.
#[test]
fn an_empty_configuration_is_inert() {
    let (present, _) = frames(240);
    let mut w = AutoRecordWatcher::new(&AutoRecordCfg::default(), 1).unwrap();
    assert!(!w.is_enabled());
    assert_eq!(pump(&mut w, &present, 0, idle(), 20), None);
}

/// Only the camera actually holding the marker should fire, and it should fire even when
/// other cameras are in the round-robin ahead of it.
#[test]
fn the_camera_that_sees_the_marker_is_the_one_that_fires() {
    let (present, absent) = frames(240);
    let mut w = AutoRecordWatcher::new(&cfg(3), 3).unwrap();

    // Camera 2 holds the marker; 0 and 1 see nothing. Note `poll` reports whichever
    // detection has finished, not the camera most recently offered — so what is asserted
    // is which camera the action *names*, not which offer preceded it.
    for _ in 0..600 {
        for (cam, frame) in [(0, &absent), (1, &absent), (2, &present)] {
            w.offer(frame, cam);
            if let Some(action) = w.poll(idle()) {
                assert!(
                    matches!(action, AutoAction::Start { camera: 2, .. }),
                    "a camera with no marker fired: {action:?}"
                );
                return;
            }
        }
        std::thread::sleep(ROUND);
    }
    panic!("camera 2 never fired despite holding the marker");
}

/// Detection must be inert during replay and auto-adjust, and must not have quietly
/// accumulated state that fires the moment it becomes active again.
#[test]
fn nothing_fires_while_inactive() {
    let (present, _) = frames(240);
    let inactive = PollCtx { recording: false, auto_started: false, active: false };
    let mut w = AutoRecordWatcher::new(&cfg(1), 1).unwrap();

    assert_eq!(pump(&mut w, &present, 0, inactive, 200), None);
    // The very next active poll must not fire off history gathered while inactive; it has
    // to re-confirm from scratch.
    assert_eq!(w.poll(idle()), None);
}

/// The sizing question the rig would normally answer. Detection is expected to hold up
/// well past the point where the marker is comfortably large, and to give out somewhere
/// below that — the test asserts the comfortable end, and reports the rest.
#[test]
fn detects_across_the_useful_range_of_marker_sizes() {
    for marker_px in [140u32, 210, 280, 420] {
        let (present, _) = frames(marker_px);
        let mut w = AutoRecordWatcher::new(&cfg(1), 1).unwrap();
        let action = pump(&mut w, &present, 0, idle(), 300);
        assert!(
            action.is_some(),
            "marker of {marker_px}px full-res ({}px after downscale) was not detected",
            marker_px / 4
        );
    }
}

/// `cfg`, with only markers inside `gate` counting.
fn gated_cfg(gate: MarkerGate) -> AutoRecordCfg {
    AutoRecordCfg { gate, ..cfg(1) }
}

/// The rack-pot case: a configured marker in plain view, but outside the region of
/// interest, must not start a recording. `synth` centres the marker, at (0.5, 0.5).
#[test]
fn a_marker_outside_the_roi_does_not_start_a_recording() {
    let (present, _) = frames(240);
    let off_centre = MarkerGate { roi: [0.0, 0.0, 0.4, 0.4], ..MarkerGate::OPEN };
    let mut w = AutoRecordWatcher::new(&gated_cfg(off_centre), 1).unwrap();
    assert_eq!(pump(&mut w, &present, 0, idle(), 200), None);

    let around_centre = MarkerGate { roi: [0.4, 0.4, 0.6, 0.6], ..MarkerGate::OPEN };
    let mut w = AutoRecordWatcher::new(&gated_cfg(around_centre), 1).unwrap();
    assert!(matches!(pump(&mut w, &present, 0, idle(), 200), Some(AutoAction::Start { .. })));
}

/// Nor may it keep one alive: a rack pot in view must not hold a recording open after the
/// gripped pot has gone.
#[test]
fn a_marker_outside_the_roi_does_not_sustain_a_recording() {
    let (present, _) = frames(240);
    let off_centre = MarkerGate { roi: [0.0, 0.0, 0.4, 0.4], ..MarkerGate::OPEN };
    let mut w = AutoRecordWatcher::new(&gated_cfg(off_centre), 1).unwrap();
    assert_eq!(pump(&mut w, &present, 0, auto_recording(), 200), Some(AutoAction::Stop));
}

/// The size floor is measured in full-resolution pixels, whatever the downscale.
#[test]
fn the_size_floor_is_in_full_resolution_pixels() {
    let side = synth::marker_side_px(&predefined::DICT_5X5_250, 240) as f64;
    let (present, _) = frames(240);

    let too_big = MarkerGate { min_side_px: side * 1.2, ..MarkerGate::OPEN };
    let mut w = AutoRecordWatcher::new(&gated_cfg(too_big), 1).unwrap();
    assert_eq!(pump(&mut w, &present, 0, idle(), 200), None);

    let below = MarkerGate { min_side_px: side * 0.8, ..MarkerGate::OPEN };
    let mut w = AutoRecordWatcher::new(&gated_cfg(below), 1).unwrap();
    assert!(matches!(pump(&mut w, &present, 0, idle(), 200), Some(AutoAction::Start { .. })));
}

/// Dropping the watcher must shut the detector thread down rather than leaking it or
/// hanging. A hang here would wedge server shutdown.
#[test]
fn dropping_the_watcher_stops_the_detector() {
    let (present, _) = frames(240);
    let mut w = AutoRecordWatcher::new(&cfg(1), 1).unwrap();
    w.offer(&present, 0);
    drop(w);
}
