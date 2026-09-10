//! Stand-in used when the `aruco-auto-record` feature is off.
//!
//! Every method is an inlined no-op on a zero-sized struct, so the acquisition loop's
//! call sites compile down to nothing at all. Keeping the shape identical to the real
//! watcher is what lets `recordings.rs` stay free of `#[cfg]`.

use acquisition_common::{Bayer, Frame};

use super::{AutoAction, AutoRecordCfg, MarkerCfgError, PollCtx};

pub struct AutoRecordWatcher;

impl AutoRecordWatcher {
    #[inline(always)]
    pub fn new(_cfg: &AutoRecordCfg, _n_cameras: usize) -> Result<Self, MarkerCfgError> {
        Ok(AutoRecordWatcher)
    }

    #[inline(always)]
    pub fn is_enabled(&self) -> bool {
        false
    }

    #[inline(always)]
    pub fn offer(&mut self, _frame: &Frame<Bayer>, _cam_idx: usize) {}

    #[inline(always)]
    pub fn poll(&mut self, _ctx: PollCtx) -> Option<AutoAction> {
        None
    }
}
