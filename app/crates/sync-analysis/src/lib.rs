//! Turning a recording made against `app/sync-check-clock.html` into a bound
//! on cross-camera synchronization error.
//!
//! See the binary in `src/main.rs` for the end-to-end pipeline and
//! `phase.rs`'s module docs for why the measurement is split into an
//! image-space part (per camera) and a page-space part (shared).

pub mod calibrate;
pub mod luma;
pub mod phase;
pub mod qr;
