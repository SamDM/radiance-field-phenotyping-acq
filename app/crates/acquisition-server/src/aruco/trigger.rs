//! Decide when marker sightings should start or stop a recording.
//!
//! Pure: no I/O, no clock of its own, no `arucotools` types. Time arrives as an argument
//! so the whole state machine is testable at whatever pace a test wants.
//!
//! # Shape of the problem
//!
//! Cameras are examined round-robin, one per detection cycle, so any given camera is only
//! looked at every `n_cameras` cycles. That has two consequences the rules below exist to
//! handle:
//!
//! - Hysteresis has to be **per camera**. A global "K of the last N detections" window
//!   would, with four cameras, degenerate to K=1 — the other three cameras keep flushing
//!   the window with misses that mean nothing.
//! - Once recording, watching every camera is wasted effort. The watcher locks onto the
//!   camera that fired, so "gone for N checks" counts checks of the camera that actually
//!   saw the marker, and stop latency stops scaling with camera count.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use super::markers::Hit;
use super::{AutoAction, PollCtx};

pub struct TriggerCfg {
    /// `K`: sightings required within the window to start.
    pub confirmations: u8,
    /// `N`: length of the per-camera sighting window.
    pub window: u8,
    /// Consecutive misses on the locked camera before stopping.
    pub stop_after: u32,
    /// Debounce after any fire.
    pub cooldown: Duration,
}

pub struct TriggerState {
    cfg: TriggerCfg,
    /// Per-camera ring of recent observations: did any configured marker appear?
    rings: HashMap<usize, VecDeque<bool>>,
    /// The camera whose sighting started the current recording.
    locked_cam: Option<usize>,
    misses: u32,
    /// Set after any recording-state change the watcher did not cause. Blocks a start
    /// until the marker has been seen to leave; see `observe`.
    require_absence: bool,
    cooldown_until: Option<Instant>,
    prev_recording: bool,
}

impl TriggerState {
    pub fn new(cfg: TriggerCfg) -> Self {
        TriggerState {
            cfg,
            rings: HashMap::new(),
            locked_cam: None,
            misses: 0,
            require_absence: false,
            cooldown_until: None,
            prev_recording: false,
        }
    }

    /// Forget all sighting history. Used when detection goes inert (replay, auto-adjust)
    /// and after every fire, so stale observations cannot drive a later decision.
    pub fn reset(&mut self) {
        self.rings.clear();
        self.locked_cam = None;
        self.misses = 0;
    }

    /// Feed one completed detection.
    ///
    /// `hits` is whatever the detector found on `cam_idx`; empty means the marker is not
    /// there. Returns an action only when one is actually due.
    pub fn observe(
        &mut self,
        cam_idx: usize,
        hits: &[Hit],
        ctx: PollCtx,
        now: Instant,
    ) -> Option<AutoAction> {
        // A recording that started or stopped without us — a client command, or the
        // existing frame-limit auto-stop — invalidates everything we think we know. Most
        // importantly it must not let a marker still lying on the table immediately start
        // the next recording, so demand that the marker be seen to leave first.
        if ctx.recording != self.prev_recording {
            self.prev_recording = ctx.recording;
            self.reset();
            self.require_absence = true;
            self.cooldown_until = Some(now + self.cfg.cooldown);
        }

        let seen = !hits.is_empty();

        let ring = self.rings.entry(cam_idx).or_default();
        ring.push_back(seen);
        while ring.len() > self.cfg.window as usize {
            ring.pop_front();
        }
        let sightings = ring.iter().filter(|&&s| s).count() as u8;

        if ctx.recording {
            self.observe_while_recording(cam_idx, seen, ctx, now)
        } else {
            self.observe_while_idle(cam_idx, hits, sightings, now)
        }
    }

    fn observe_while_idle(
        &mut self,
        cam_idx: usize,
        hits: &[Hit],
        sightings: u8,
        now: Instant,
    ) -> Option<AutoAction> {
        // An absence clears the latch: the marker has demonstrably left the frame, so a
        // fresh appearance is a genuine new event rather than the tail of an old one.
        //
        // The history goes with it. Otherwise sightings from *before* the marker left
        // still count toward K, and a single frame of it coming back would satisfy a
        // window mostly filled by the previous visit — which is exactly the restart the
        // latch exists to prevent.
        if hits.is_empty() {
            if self.require_absence {
                self.require_absence = false;
                self.rings.clear();
            }
            return None;
        }
        if self.require_absence {
            return None;
        }
        if self.cooldown_until.is_some_and(|t| now < t) {
            return None;
        }
        if sightings < self.cfg.confirmations {
            return None;
        }

        // `hits` is non-empty and the window is satisfied: commit to this camera.
        let hit = &hits[0];
        self.locked_cam = Some(cam_idx);
        self.misses = 0;
        self.prev_recording = true;
        self.cooldown_until = Some(now + self.cfg.cooldown);
        self.rings.clear();
        Some(AutoAction::Start {
            family: hit.family.to_string(),
            id: hit.id,
            camera: cam_idx,
        })
    }

    fn observe_while_recording(
        &mut self,
        cam_idx: usize,
        seen: bool,
        ctx: PollCtx,
        now: Instant,
    ) -> Option<AutoAction> {
        // Only the camera that started this recording gets a vote on ending it. Cameras
        // that never saw the marker would otherwise accumulate misses forever.
        if self.locked_cam != Some(cam_idx) {
            return None;
        }
        if seen {
            self.misses = 0;
            return None;
        }

        self.misses += 1;
        if self.misses < self.cfg.stop_after {
            return None;
        }
        // Whoever started this recording gets to end it. A human pressing record is not
        // asking for a marker to stop them.
        if !ctx.auto_started {
            return None;
        }

        self.prev_recording = false;
        self.cooldown_until = Some(now + self.cfg.cooldown);
        self.reset();
        Some(AutoAction::Stop { camera: cam_idx })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> TriggerCfg {
        TriggerCfg {
            confirmations: 2,
            window: 3,
            stop_after: 3,
            cooldown: Duration::from_secs(5),
        }
    }

    fn hit() -> Vec<Hit> {
        vec![Hit { family: "DICT_5X5_250", id: 3 }]
    }

    fn idle() -> PollCtx {
        PollCtx { recording: false, auto_started: false, active: true }
    }

    fn recording() -> PollCtx {
        PollCtx { recording: true, auto_started: true, active: true }
    }

    /// Advance well past any cooldown.
    fn later(t: Instant, secs: u64) -> Instant {
        t + Duration::from_secs(secs)
    }

    #[test]
    fn starts_only_after_k_sightings_on_one_camera() {
        let mut s = TriggerState::new(cfg());
        let t = Instant::now();
        assert_eq!(s.observe(0, &hit(), idle(), t), None, "one sighting is not enough");
        let action = s.observe(0, &hit(), idle(), t);
        assert!(
            matches!(action, Some(AutoAction::Start { camera: 0, id: 3, .. })),
            "second sighting should start: {action:?}"
        );
    }

    #[test]
    fn sightings_do_not_pool_across_cameras() {
        // The bug this guards: with a global window, one sighting each on two different
        // cameras would satisfy K=2 even though neither camera ever saw it twice.
        let mut s = TriggerState::new(cfg());
        let t = Instant::now();
        assert_eq!(s.observe(0, &hit(), idle(), t), None);
        assert_eq!(s.observe(1, &hit(), idle(), t), None);
        assert_eq!(s.observe(2, &hit(), idle(), t), None);
    }

    #[test]
    fn sightings_age_out_of_the_window() {
        let mut s = TriggerState::new(cfg());
        let t = Instant::now();
        // One sighting, then a full window of misses flushes it.
        assert_eq!(s.observe(0, &hit(), idle(), t), None);
        for _ in 0..3 {
            assert_eq!(s.observe(0, &[], idle(), t), None);
        }
        assert_eq!(s.observe(0, &hit(), idle(), t), None, "stale sighting should have aged out");
    }

    #[test]
    fn does_not_restart_while_already_recording() {
        let mut s = TriggerState::new(cfg());
        let t = Instant::now();
        s.observe(0, &hit(), idle(), t);
        s.observe(0, &hit(), idle(), t);
        // Marker stays in view for a long time; nothing further should fire.
        for i in 0..20 {
            assert_eq!(s.observe(0, &hit(), recording(), later(t, i)), None);
        }
    }

    #[test]
    fn stops_after_exactly_n_misses_on_the_locked_camera() {
        let mut s = TriggerState::new(cfg());
        let t = Instant::now();
        s.observe(0, &hit(), idle(), t);
        s.observe(0, &hit(), idle(), t);

        assert_eq!(s.observe(0, &[], recording(), t), None);
        assert_eq!(s.observe(0, &[], recording(), t), None);
        assert_eq!(
            s.observe(0, &[], recording(), t),
            Some(AutoAction::Stop { camera: 0 }),
            "third consecutive miss should stop"
        );
    }

    #[test]
    fn a_reappearance_resets_the_miss_count() {
        let mut s = TriggerState::new(cfg());
        let t = Instant::now();
        s.observe(0, &hit(), idle(), t);
        s.observe(0, &hit(), idle(), t);

        s.observe(0, &[], recording(), t);
        s.observe(0, &[], recording(), t);
        s.observe(0, &hit(), recording(), t); // back in view
        s.observe(0, &[], recording(), t);
        s.observe(0, &[], recording(), t);
        assert_eq!(s.observe(0, &[], recording(), t), Some(AutoAction::Stop { camera: 0 }));
    }

    #[test]
    fn misses_on_other_cameras_never_stop_a_recording() {
        // Cameras that never saw the marker would otherwise accumulate misses forever and
        // end every recording after three cycles.
        let mut s = TriggerState::new(cfg());
        let t = Instant::now();
        s.observe(0, &hit(), idle(), t);
        s.observe(0, &hit(), idle(), t);
        for i in 0..30 {
            assert_eq!(s.observe(1, &[], recording(), later(t, i)), None);
            assert_eq!(s.observe(2, &[], recording(), later(t, i)), None);
        }
    }

    #[test]
    fn never_auto_stops_a_manually_started_recording() {
        let mut s = TriggerState::new(cfg());
        let t = Instant::now();
        let manual = PollCtx { recording: true, auto_started: false, active: true };
        // Simulate the human pressing record, then the marker being absent indefinitely.
        s.observe(0, &hit(), idle(), t);
        s.observe(0, &hit(), idle(), t);
        for i in 0..30 {
            assert_eq!(s.observe(0, &[], manual, later(t, i)), None);
        }
    }

    /// The interaction most likely to bite in the field: the existing frame-limit auto-stop
    /// ends the recording while the start marker is still sitting in front of the camera.
    /// Without the absence latch that immediately starts another recording, and the rig
    /// records forever.
    #[test]
    fn an_external_stop_does_not_immediately_restart() {
        let mut s = TriggerState::new(cfg());
        let t = Instant::now();
        s.observe(0, &hit(), idle(), t);
        s.observe(0, &hit(), idle(), t);

        // Frame limit hits: the loop stops recording on its own.
        let t = later(t, 60);
        for i in 0..20 {
            assert_eq!(
                s.observe(0, &hit(), idle(), later(t, i)),
                None,
                "marker still in view must not restart after an external stop"
            );
        }

        // Only once the marker is taken away and shown again does it re-arm.
        let t = later(t, 100);
        assert_eq!(s.observe(0, &[], idle(), t), None);
        assert_eq!(s.observe(0, &hit(), idle(), t), None);
        assert!(matches!(
            s.observe(0, &hit(), idle(), t),
            Some(AutoAction::Start { camera: 0, .. })
        ));
    }

    #[test]
    fn cooldown_blocks_an_immediate_restart_after_an_auto_stop() {
        let mut s = TriggerState::new(cfg());
        let t = Instant::now();
        s.observe(0, &hit(), idle(), t);
        s.observe(0, &hit(), idle(), t);
        for _ in 0..3 {
            s.observe(0, &[], recording(), t);
        }
        // Marker comes straight back within the cooldown window. Sightings still
        // accumulate — the marker really is there — but nothing may fire yet.
        assert_eq!(s.observe(0, &hit(), idle(), t), None);
        assert_eq!(s.observe(0, &hit(), idle(), t), None);

        // Once the cooldown expires the accumulated sightings are allowed to count, so the
        // very next observation fires.
        let t = later(t, 10);
        assert!(matches!(
            s.observe(0, &hit(), idle(), t),
            Some(AutoAction::Start { .. })
        ));
    }

    #[test]
    fn reset_clears_sighting_history() {
        let mut s = TriggerState::new(cfg());
        let t = Instant::now();
        s.observe(0, &hit(), idle(), t);
        s.reset();
        assert_eq!(s.observe(0, &hit(), idle(), t), None, "history should be gone after reset");
    }

    #[test]
    fn reports_the_family_and_id_that_fired() {
        let mut s = TriggerState::new(cfg());
        let t = Instant::now();
        let hits = vec![Hit { family: "DICT_6X6_250", id: 42 }];
        s.observe(1, &hits, idle(), t);
        assert_eq!(
            s.observe(1, &hits, idle(), t),
            Some(AutoAction::Start {
                family: "DICT_6X6_250".into(),
                id: 42,
                camera: 1
            })
        );
    }
}
