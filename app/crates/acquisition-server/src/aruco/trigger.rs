//! Decide when marker sightings should start or stop a recording.
//!
//! Pure: no I/O, no clock of its own, no `arucotools` types. Time arrives as an argument
//! so the whole state machine is testable at whatever pace a test wants.
//!
//! # Shape of the problem
//!
//! The intended behaviour is simple: **a recording runs for as long as any configured
//! marker is visible to any configured camera.** What complicates it is that we cannot
//! afford to look at every camera on every frame, so cameras are examined round-robin,
//! one per detection cycle. Round-robin is an approximation of "watch everything at
//! once", and the two rules below exist to keep the approximation honest.
//!
//! - **Starting is per camera.** A global "K of the last N detections" window would, with
//!   four cameras, degenerate to K=1 — the other three cameras keep flushing the window
//!   with misses that mean nothing. So K-of-N is counted against the ring of the camera
//!   that saw the marker.
//!
//! - **Stopping is per sweep.** Absence is a claim about *every* camera, and a single
//!   cycle only ever examines one of them. Counting raw misses would stop a recording
//!   while the marker sits in plain view of a camera that simply has not had its turn
//!   yet: with four cameras and the marker on camera 2, three of every four cycles are
//!   misses. So the miss budget is scaled by the number of cameras, and
//!   `stop_after` counts *complete round-robin passes* in which nothing was seen
//!   anywhere. Any hit on any camera resets it.
//!
//! Scaling the budget is what makes hand-off work without anybody tracking it. If camera
//! 1 sees the marker over frames Ta..Tc and camera 2 sees it over Tb..Td, camera 2's
//! sightings keep resetting the counter after camera 1 has lost it, and the recording
//! runs until Td — no camera "owns" the recording, and none needs to.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use super::markers::Hit;
use super::{AutoAction, PollCtx};

pub struct TriggerCfg {
    /// `K`: sightings required within the window to start.
    pub confirmations: u8,
    /// `N`: length of the per-camera sighting window.
    pub window: u8,
    /// Round-robin passes with no sighting on any camera before stopping.
    pub stop_after: u32,
    /// Debounce after any fire.
    pub cooldown: Duration,
    /// How many cameras the round-robin covers. Scales the miss budget so `stop_after`
    /// counts sweeps rather than cycles.
    pub n_cameras: usize,
}

pub struct TriggerState {
    cfg: TriggerCfg,
    /// Per-camera ring of recent observations: did any configured marker appear?
    rings: HashMap<usize, VecDeque<bool>>,
    /// Consecutive observations, across all cameras, in which nothing was seen.
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
        self.misses = 0;
    }

    /// Consecutive marker-free observations that amount to `stop_after` full sweeps.
    fn miss_budget(&self) -> u32 {
        self.cfg
            .stop_after
            .saturating_mul(self.cfg.n_cameras.max(1) as u32)
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
            self.observe_while_recording(seen, ctx, now)
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

        // `hits` is non-empty and the window is satisfied: start.
        let hit = &hits[0];
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
        seen: bool,
        ctx: PollCtx,
        now: Instant,
    ) -> Option<AutoAction> {
        // Any camera seeing any configured marker keeps the recording alive, whichever
        // camera started it. This is the whole of the keep-alive rule.
        if seen {
            self.misses = 0;
            return None;
        }

        // Saturating because a manually started recording never trips the branch below,
        // so this would otherwise climb for as long as the recording runs.
        self.misses = self.misses.saturating_add(1);
        if self.misses < self.miss_budget() {
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
        Some(AutoAction::Stop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Single camera: one sweep is one observation, so `stop_after` counts raw misses.
    fn cfg() -> TriggerCfg {
        cfg_n(1)
    }

    fn cfg_n(n_cameras: usize) -> TriggerCfg {
        TriggerCfg {
            confirmations: 2,
            window: 3,
            stop_after: 3,
            cooldown: Duration::from_secs(5),
            n_cameras,
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

    /// Drive a start on `cam` and assert it happened, so tests about stopping can begin
    /// from a recording without restating how starting works.
    fn start_on(s: &mut TriggerState, cam: usize, t: Instant) {
        assert_eq!(s.observe(cam, &hit(), idle(), t), None);
        assert!(
            matches!(s.observe(cam, &hit(), idle(), t), Some(AutoAction::Start { .. })),
            "fixture failed to start a recording"
        );
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
        let mut s = TriggerState::new(cfg_n(3));
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
        start_on(&mut s, 0, t);
        // Marker stays in view for a long time; nothing further should fire.
        for i in 0..20 {
            assert_eq!(s.observe(0, &hit(), recording(), later(t, i)), None);
        }
    }

    #[test]
    fn stops_after_exactly_n_misses_on_a_single_camera_rig() {
        let mut s = TriggerState::new(cfg());
        let t = Instant::now();
        start_on(&mut s, 0, t);

        assert_eq!(s.observe(0, &[], recording(), t), None);
        assert_eq!(s.observe(0, &[], recording(), t), None);
        assert_eq!(
            s.observe(0, &[], recording(), t),
            Some(AutoAction::Stop),
            "third consecutive miss should stop"
        );
    }

    #[test]
    fn a_reappearance_resets_the_miss_count() {
        let mut s = TriggerState::new(cfg());
        let t = Instant::now();
        start_on(&mut s, 0, t);

        s.observe(0, &[], recording(), t);
        s.observe(0, &[], recording(), t);
        s.observe(0, &hit(), recording(), t); // back in view
        s.observe(0, &[], recording(), t);
        s.observe(0, &[], recording(), t);
        assert_eq!(s.observe(0, &[], recording(), t), Some(AutoAction::Stop));
    }

    /// The miss budget counts complete round-robin passes, not cycles. With three cameras
    /// and the marker parked in front of one of them, two thirds of all cycles are misses
    /// — and none of them mean the marker is gone.
    #[test]
    fn a_marker_on_one_camera_keeps_a_three_camera_rig_recording() {
        let mut s = TriggerState::new(cfg_n(3));
        let t = Instant::now();
        start_on(&mut s, 0, t);

        for i in 0..30 {
            let t = later(t, i);
            assert_eq!(s.observe(0, &[], recording(), t), None);
            assert_eq!(s.observe(1, &[], recording(), t), None);
            // Only camera 2 can see it, and only every third cycle.
            assert_eq!(s.observe(2, &hit(), recording(), t), None);
        }
    }

    /// Hand-off: camera 1 sees the marker over Ta..Tc, camera 2 over Tb..Td. Nothing owns
    /// the recording, so it must run until the *last* camera loses sight of it.
    #[test]
    fn one_camera_hands_the_recording_off_to_another() {
        let mut s = TriggerState::new(cfg_n(2));
        let t = Instant::now();
        start_on(&mut s, 0, t);

        // Overlap: both cameras see it.
        for i in 0..5 {
            let t = later(t, i);
            assert_eq!(s.observe(0, &hit(), recording(), t), None);
            assert_eq!(s.observe(1, &hit(), recording(), t), None);
        }
        // Camera 0 loses it; camera 1 still has it. The recording must survive this
        // indefinitely — under the old camera lock it would have stopped here.
        for i in 5..25 {
            let t = later(t, i);
            assert_eq!(s.observe(0, &[], recording(), t), None);
            assert_eq!(s.observe(1, &hit(), recording(), t), None);
        }
        // Now camera 1 loses it too, and the budget finally runs out: stop_after=3 sweeps
        // over 2 cameras is 6 consecutive marker-free observations.
        let t = later(t, 25);
        for _ in 0..5 {
            assert_eq!(s.observe(0, &[], recording(), t), None);
        }
        assert_eq!(s.observe(1, &[], recording(), t), Some(AutoAction::Stop));
    }

    #[test]
    fn never_auto_stops_a_manually_started_recording() {
        let mut s = TriggerState::new(cfg());
        let t = Instant::now();
        let manual = PollCtx { recording: true, auto_started: false, active: true };
        // Simulate the human pressing record, then the marker being absent indefinitely.
        start_on(&mut s, 0, t);
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
        start_on(&mut s, 0, t);

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
        start_on(&mut s, 0, t);
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
