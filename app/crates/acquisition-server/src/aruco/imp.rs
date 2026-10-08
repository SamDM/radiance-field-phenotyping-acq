//! The watcher itself: dispatch policy on the acquisition thread, detection elsewhere.
//!
//! Lives entirely on the acquisition thread and owns only its own two channels to the
//! detector. Nothing crosses into the tokio side — in particular it never holds a clone of
//! the command sender, because dropping the last of those is how shutdown tells the
//! acquisition loop to exit, and an extra long-lived clone would hang Ctrl-C. Actions come
//! back through `poll` and are applied by the existing recording state machine instead.

use std::sync::mpsc::{channel, sync_channel, Receiver, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use acquisition_common::downscale::{bayer_rggb_box_average, bayer_rggb_green_subsample};
use acquisition_common::{Bayer, Frame};
use tracing::{debug, info, warn};

use super::markers::MarkerSet;
use super::trigger::{TriggerCfg, TriggerState};
use super::worker::{self, DetectJob, DetectOutcome, Selection};
use super::{AutoAction, AutoRecordCfg, DownscaleMode, MarkerCfgError, PollCtx};

/// How often to report dropped frames. Frequent enough to notice a misconfiguration,
/// rare enough not to spam a log that runs for days.
const DROP_REPORT_INTERVAL: Duration = Duration::from_secs(60);

pub struct AutoRecordWatcher {
    inner: Option<Inner>,
}

struct Inner {
    /// `Option` so `Drop` can close the channel before joining.
    job_tx: Option<SyncSender<DetectJob>>,
    out_rx: Receiver<DetectOutcome>,
    worker: Option<JoinHandle<()>>,

    /// True between dispatching a job and receiving its outcome. This, not the channel
    /// capacity, is what guarantees at most one detection is ever outstanding.
    in_flight: bool,
    cameras: Vec<usize>,
    next_cam: usize,
    last_dispatch: Instant,
    interval: Duration,
    downscale: u32,
    mode: DownscaleMode,

    trigger: TriggerState,

    dropped: u64,
    last_drop_report: Instant,
}

impl AutoRecordWatcher {
    /// Build a watcher, spawning the detector thread if any markers are configured.
    ///
    /// Returns an error only for a bad marker configuration, which is a startup failure —
    /// better to refuse to launch than to run a rig that silently never triggers.
    pub fn new(cfg: &AutoRecordCfg, n_cameras: usize) -> Result<Self, MarkerCfgError> {
        if !cfg.is_enabled() {
            return Ok(AutoRecordWatcher { inner: None });
        }

        let cameras = cfg.camera_list(n_cameras);
        if cameras.is_empty() {
            return Ok(AutoRecordWatcher { inner: None });
        }

        // Resolved only once there is something to resolve it for. Deferring it past the
        // bail above costs no error coverage: `Args::validate_aruco` runs the same resolution
        // during argument parsing, so a bad family name is rejected before this is
        // reached whether or not any camera is watched.
        let markers = MarkerSet::new(&cfg.markers)?;

        // The sweep width is how many cameras the round-robin actually visits, which
        // `--aruco-cameras` may have narrowed to fewer than the rig has. Captured here
        // because `cameras` is moved into `Inner` below.
        let n_watched = cameras.len();

        let params = worker::detector_params(cfg.thresh_win, cfg.min_perimeter_rate);

        // Overlapping dictionaries mean two configured names can denote one physical
        // marker. Triggering is unaffected, but detections of it can only be reported
        // under one name, so say which — otherwise the logs name a marker the operator
        // never configured.
        for alias in markers.aliases() {
            warn!(
                reported = %format!("{}:{}", alias.reported.family, alias.reported.id),
                also_configured_as = %format!("{}:{}", alias.also.family, alias.also.id),
                "two configured markers are the same pattern; reporting the first"
            );
        }

        // Report what the detector will actually look for: duplicates and overlapping
        // names collapse, so the count can differ from what was typed on the command line.
        // Taken now because `markers` moves to the worker below.
        let n_markers = markers.n_markers();
        let n_dictionaries = markers.families().len();

        // Capacity 1 with `try_send`: a busy worker means the frame is dropped, never
        // queued. A frame that waited in a queue cannot answer "is the marker there now?".
        let selection = Selection {
            markers,
            gate: cfg.gate,
            scale: 2.0 * cfg.downscale as f64,
        };

        let (job_tx, job_rx) = sync_channel::<DetectJob>(1);
        let (out_tx, out_rx) = channel::<DetectOutcome>();

        let worker = std::thread::Builder::new()
            .name("aruco-detect".into())
            .spawn(move || worker::run(job_rx, out_tx, selection, params))
            .expect("spawn aruco detection thread");

        info!(
            markers = n_markers,
            dictionaries = n_dictionaries,
            cameras = ?cameras,
            downscale = cfg.downscale,
            mode = ?cfg.downscale_mode,
            interval_ms = cfg.interval.as_millis() as u64,
            confirmations = cfg.confirmations,
            window = cfg.window,
            stop_after = cfg.stop_after,
            roi = ?cfg.gate.roi,
            min_side_px = cfg.gate.min_side_px,
            "aruco auto-record enabled"
        );

        Ok(AutoRecordWatcher {
            inner: Some(Inner {
                job_tx: Some(job_tx),
                out_rx,
                worker: Some(worker),
                in_flight: false,
                cameras,
                next_cam: 0,
                // Allow the first frame to dispatch immediately.
                last_dispatch: Instant::now() - cfg.interval,
                interval: cfg.interval,
                downscale: cfg.downscale,
                mode: cfg.downscale_mode,
                trigger: TriggerState::new(TriggerCfg {
                    confirmations: cfg.confirmations,
                    window: cfg.window,
                    stop_after: cfg.stop_after,
                    cooldown: cfg.cooldown,
                    n_cameras: n_watched,
                }),
                dropped: 0,
                last_drop_report: Instant::now(),
            }),
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    /// Offer one grabbed frame. Called for every frame on the acquisition thread, so the
    /// cheap rejections come first and the overwhelming majority of calls never touch a
    /// pixel.
    pub fn offer(&mut self, frame: &Frame<Bayer>, cam_idx: usize) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        inner.offer(frame, cam_idx);
    }

    /// Drain any completed detection and advance the trigger.
    pub fn poll(&mut self, ctx: PollCtx) -> Option<AutoAction> {
        let inner = self.inner.as_mut()?;
        inner.poll(ctx)
    }
}

impl Inner {
    fn offer(&mut self, frame: &Frame<Bayer>, cam_idx: usize) {
        if self.in_flight {
            self.dropped += 1;
            return;
        }
        if self.cameras.get(self.next_cam) != Some(&cam_idx) {
            return;
        }
        if self.last_dispatch.elapsed() < self.interval {
            return;
        }

        // Only now is it worth reading the mosaic.
        let gray = match self.mode {
            DownscaleMode::Green => {
                bayer_rggb_green_subsample(&frame.data, frame.width, frame.height, self.downscale)
            }
            DownscaleMode::Box => {
                bayer_rggb_box_average(&frame.data, frame.width, frame.height, self.downscale)
            }
        };

        let job = DetectJob { cam_idx, gray, dispatched_at: Instant::now() };
        match self.job_tx.as_ref().expect("sender live until drop").try_send(job) {
            Ok(()) => {
                self.in_flight = true;
                self.last_dispatch = Instant::now();
                self.next_cam = (self.next_cam + 1) % self.cameras.len();
            }
            // Full means the worker is still busy; disconnected means it is gone. Either
            // way the frame is simply not examined.
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.dropped += 1;
            }
        }
    }

    fn poll(&mut self, ctx: PollCtx) -> Option<AutoAction> {
        self.report_drops();

        let mut action = None;
        while let Ok(outcome) = self.out_rx.try_recv() {
            self.in_flight = false;
            if !ctx.active {
                // Replay or auto-adjust took over while this was in flight. Discard it and
                // forget the history: detection must never start a recording out from
                // under a replay session.
                self.trigger.reset();
                continue;
            }
            // Keep the first action of the batch; in practice there is at most one
            // outcome per poll, since only one detection is ever outstanding.
            let decided = self.trigger.observe(outcome.cam_idx, &outcome.hits, ctx, Instant::now());
            if action.is_none() {
                action = decided;
            }
        }
        action
    }

    fn report_drops(&mut self) {
        if self.dropped == 0 || self.last_drop_report.elapsed() < DROP_REPORT_INTERVAL {
            return;
        }
        // Only frames offered while a detection was still running are counted; the ones
        // the round-robin and the interval turn away are not. So a large number is the
        // drop-if-busy policy working as intended rather than a fault, and what is worth
        // watching is a change in it, which means detection has slowed down.
        debug!(
            dropped = self.dropped,
            window_secs = self.last_drop_report.elapsed().as_secs(),
            "aruco frames offered while the detector was busy"
        );
        self.dropped = 0;
        self.last_drop_report = Instant::now();
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Closing the channel ends the worker's receive loop.
        self.job_tx.take();
        if let Some(handle) = self.worker.take() {
            // Blocks for at most one detection, which is nothing against the existing
            // camera teardown.
            let _ = handle.join();
        }
    }
}
