//! Start and stop recordings automatically when configured ArUco markers come into view.
//!
//! Compiled only when the `aruco-auto-record` feature is on; without it
//! [`AutoRecordWatcher`] is a zero-sized stub whose methods inline away to nothing, so
//! the acquisition loop carries one unconditional call site either way and no `#[cfg]`
//! leaks into the hot path.
//!
//! # Why this is not inline
//!
//! `arucotools` is a faithful but unoptimised port of the OpenCV detector: single
//! threaded, stateless, and explicit in its README that speed is a secondary concern.
//! The acquisition loop is a *single* thread that also drives `retrieve_result` for
//! every camera, so a detector call on that thread costs frame rate directly. Detection
//! therefore runs on its own thread, fed by a one-slot channel that drops frames rather
//! than queueing them — a frame that has been waiting is worthless for deciding whether
//! a marker is on screen *now*.
//!
//! Three limiters keep the detector cheap regardless of camera count: never dispatch
//! while a detection is in flight, examine one camera per cycle, and enforce a wall-clock
//! floor between dispatches. See [`imp`] for how they compose.

use std::time::Duration;

mod cfg;
pub use cfg::{AutoRecordCfg, DownscaleMode, MarkerCfgError, MarkerSelector};

#[cfg(feature = "aruco-auto-record")]
pub mod synth;

#[cfg(feature = "aruco-auto-record")]
mod imp;
#[cfg(feature = "aruco-auto-record")]
mod markers;
#[cfg(feature = "aruco-auto-record")]
mod trigger;
#[cfg(feature = "aruco-auto-record")]
mod worker;

#[cfg(all(test, feature = "aruco-auto-record"))]
mod e2e_tests;

#[cfg(feature = "aruco-auto-record")]
pub use imp::AutoRecordWatcher;

#[cfg(not(feature = "aruco-auto-record"))]
mod stub;
#[cfg(not(feature = "aruco-auto-record"))]
pub use stub::AutoRecordWatcher;

/// Check that every configured selector names a real family and an in-range id.
///
/// Exists so a bad configuration can be rejected during argument validation, before any
/// socket is bound or camera opened. The watcher performs the same resolution when it is
/// built, but that happens on the acquisition thread — far too late for the operator to
/// notice that the rig will never trigger.
#[cfg(feature = "aruco-auto-record")]
pub fn validate_markers(selectors: &[MarkerSelector]) -> Result<(), MarkerCfgError> {
    markers::merge(selectors).map(|_| ())
}

/// What the watcher wants the recording state machine to do.
///
/// Declared unconditionally so the call sites in the acquisition loop type-check
/// identically with and without the feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoAction {
    /// A configured marker came into view on `camera`.
    Start {
        family: String,
        id: usize,
        camera: usize,
    },
    /// No configured marker has been seen on any camera for long enough.
    Stop,
}

impl AutoAction {
    /// Human-readable reason, for the log line and the notice sent to clients.
    pub fn reason(&self) -> String {
        match self {
            AutoAction::Start { family, id, camera } => {
                format!("marker {family}:{id} seen on camera {camera}")
            }
            AutoAction::Stop => "no configured marker visible on any camera".to_string(),
        }
    }
}

/// Context the acquisition loop supplies on every poll.
///
/// `active` is false during replay and auto-adjust: frames are still grabbed then (the
/// Pylon buffer has to be drained) but must not be examined, and detection must never
/// start a recording out from under a replay session.
#[derive(Debug, Clone, Copy)]
pub struct PollCtx {
    pub recording: bool,
    /// Whether the current recording was started by this watcher. A recording a human
    /// started is never auto-stopped.
    pub auto_started: bool,
    pub active: bool,
}

/// Default minimum gap between detector dispatches.
///
/// Measured by `examples/aruco_bench.rs`, a detection at the default downscale costs
/// ~5 ms — around twenty times cheaper than this feature was sized for. At the rig's 2 fps
/// each camera only produces a frame every 500 ms anyway, so a floor anywhere below that
/// stops being the binding constraint and lets frame arrival set the pace, which is the
/// cheapest way to cut start latency. The floor still earns its keep at high frame rates:
/// even at 5 dispatches per second this is under 3% of one core.
pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(200);
/// Default quad stride for the Bayer downscale; output is `w/(2k)` × `h/(2k)`.
///
/// On the rig's 3536×2748 sensor this gives the detector 589×458. See
/// [`DEFAULT_DOWNSCALE`]'s bench for how large the marker then has to be.
pub const DEFAULT_DOWNSCALE: u32 = 3;
/// Default `K` of the K-of-N start hysteresis.
pub const DEFAULT_CONFIRMATIONS: u8 = 2;
/// Default `N` of the K-of-N start hysteresis.
pub const DEFAULT_WINDOW: u8 = 3;
/// Default number of consecutive round-robin passes with no sighting before stopping.
pub const DEFAULT_STOP_AFTER: u32 = 3;
/// Default debounce after a fire.
pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(5);
/// Default single adaptive-threshold window size.
///
/// The detector's own default sweeps `{3, 13, 23}`, running the full threshold and
/// contour pipeline three times per frame. Pinning one window is the single biggest
/// speed lever available.
pub const DEFAULT_THRESH_WIN: u32 = 13;
/// Default contour size floor, as a fraction of the larger image dimension.
pub const DEFAULT_MIN_PERIMETER_RATE: f64 = 0.03;
