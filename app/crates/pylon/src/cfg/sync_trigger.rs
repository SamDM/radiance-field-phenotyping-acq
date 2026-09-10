//! PTP-synchronized triggering via the camera's Periodic Signal feature.
//!
//! `PeriodicSignal1` is an internal camera signal that fires on a grid derived from
//! the camera's own clock — no I/O cabling required. When PTP is enabled that clock is
//! disciplined to the grand master, so cameras configured with the same period and a
//! zero delay expose on the *same absolute grid*: measured agreement between two
//! `a2A3536-9gcBAS` units is ~2 µs peak-to-peak over 180 frames, with zero missed
//! triggers.
//!
//! # Rate ownership
//!
//! Triggering and PTP are independent axes. [`RateControl`] records which camera
//! feature owns the acquisition rate; PTP only decides whether the resulting captures
//! line up in absolute time. Losing PTP lock costs synchronization, never rate
//! control — the periodic signal keeps running off the camera's undisciplined
//! `DeviceClock` at exactly the configured period.
//!
//! # Why the framerate limiter must be off
//!
//! `AcquisitionFrameRate` is a *limiter*, not a clock: it enforces a minimum spacing
//! between accepted frame starts. Because the period is quantized to whole
//! microseconds (see [`period_us_for`]), a nominal 3 fps trigger actually fires at
//! 3.000003 fps — marginally faster than a 3.0 fps limiter, so the limiter rejects
//! triggers that arrive a fraction of a microsecond "early". Measured: ~25 % of
//! triggers swallowed, and the two cameras swallowed *different* ones, which is worse
//! than no synchronization at all. With the limiter disabled the drop rate is zero.

use crate::pylon::Camera;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Smallest period the camera accepts, in microseconds.
const MIN_PERIOD_US: u32 = 1;
/// Largest period the camera accepts, in microseconds (1 s → a 1 fps floor).
const MAX_PERIOD_US: u32 = 1_000_000;

/// Which camera feature paces acquisition.
///
/// A camera can only be moved between these two while it is not grabbing, since
/// `TriggerMode` and `TriggerSource` are read-only once acquisition has started. The
/// two are not interchangeable at the point of setting a frame rate either — each
/// mode ignores the other's feature — so callers must carry the choice alongside the
/// camera rather than re-derive it per call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateControl {
    /// `BslPeriodicSignalPeriod` paces acquisition and the `AcquisitionFrameRate`
    /// limiter is off. Frame starts land on the periodic signal grid, which is shared
    /// between cameras whenever PTP is locked.
    PeriodicSignal,
    /// `AcquisitionFrameRate` and `AcquisitionFrameRateEnable` pace acquisition and no
    /// trigger is armed, so each camera free-runs on its own clock.
    AcquisitionLimiter,
}

/// Clock the camera derives `PeriodicSignal1` from. Read-only on the camera: it
/// follows `PtpEnable` automatically.
///
/// Note this flips to [`PtpClock`](Self::PtpClock) the instant PTP is *enabled*, tens
/// of seconds before the servo actually locks, so it is not on its own evidence that
/// captures are synchronized — check `PtpStatus`/`PtpServoStatus` as well.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodicSignalSource {
    /// Camera clock is PTP-disciplined; the trigger grid is shared across cameras.
    PtpClock,
    /// Camera clock is free-running; the trigger grid is local to this camera.
    DeviceClock,
}

impl PeriodicSignalSource {
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "PtpClock" => Ok(Self::PtpClock),
            "DeviceClock" => Ok(Self::DeviceClock),
            other => Err(SyncTriggerError(format!(
                "unknown BslPeriodicSignalSource: {other}"
            ))
            .into()),
        }
    }
}

#[derive(Debug)]
struct SyncTriggerError(String);

impl std::fmt::Display for SyncTriggerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SyncTriggerError {}

/// Converts a requested frame rate to a signal period in whole microseconds.
///
/// The camera quantizes `BslPeriodicSignalPeriod` to 1 µs and caps it at 1 s, so not
/// every rate is exactly representable and rates below 1 fps are clamped. Cameras
/// given the same `fps` therefore receive the *identical* integer period and stay
/// phase-locked regardless of the rounding.
pub fn period_us_for(fps: f64) -> u32 {
    if !fps.is_finite() || fps <= 0.0 {
        return MAX_PERIOD_US;
    }
    let period = (1e6 / fps).round();
    // `as` saturates at u32::MAX for out-of-range floats, which the clamp then handles.
    (period as u32).clamp(MIN_PERIOD_US, MAX_PERIOD_US)
}

/// The frame rate a given period actually produces.
///
/// Differs from the requested rate whenever that rate does not divide 1 s into whole
/// microseconds, so prefer this over the request when reporting what the camera is
/// doing.
pub fn realized_fps(period_us: u32) -> f64 {
    1e6 / period_us as f64
}

/// Configure `cam` to expose on the PTP-synchronized periodic signal grid, returning
/// the realized frame rate.
///
/// Call before `CameraArray::start_grabbing`: `TriggerMode` and `TriggerSource` are
/// read-only while grabbing. The rate can still be changed afterwards via
/// [`set_signal_period`], which stays writable.
///
/// PTP need not be enabled or locked yet — the periodic signal runs off the device
/// clock until it is, and picks up the disciplined clock automatically.
pub fn enable_sync_trigger(cam: &mut Camera<'_>, fps: f64) -> Result<f64> {
    // The limiter would swallow triggers (see module docs); disable it before the
    // trigger starts firing.
    cam.set_feature("AcquisitionFrameRateEnable", "false")?;
    let realized = set_signal_period(cam, fps)?;
    cam.set_feature("BslPeriodicSignalDelay", "0")?;
    cam.set_feature("TriggerSelector", "FrameStart")?;
    cam.set_feature("TriggerMode", "On")?;
    cam.set_feature("TriggerSource", "PeriodicSignal1")?;
    Ok(realized)
}

/// Disarm the frame-start trigger, so acquisition free-runs again.
///
/// Does **not** touch the `AcquisitionFrameRate` limiter, which
/// [`enable_sync_trigger`] left disabled — nothing paces acquisition until the rate is
/// set separately under [`RateControl::AcquisitionLimiter`].
///
/// Safe to call on a camera that [`enable_sync_trigger`] only partly configured: it
/// depends on none of the features that call sets.
pub fn disable_sync_trigger(cam: &mut Camera<'_>) -> Result<()> {
    cam.set_feature("TriggerSelector", "FrameStart")?;
    cam.set_feature("TriggerMode", "Off")?;
    Ok(())
}

/// Set the periodic signal period from a frame rate, returning the realized rate.
/// Writable while grabbing, unlike the trigger nodes themselves.
pub fn set_signal_period(cam: &mut Camera<'_>, fps: f64) -> Result<f64> {
    let period_us = period_us_for(fps);
    cam.set_feature("BslPeriodicSignalPeriod", &period_us.to_string())?;
    Ok(realized_fps(period_us))
}

/// Read back the configured period, in whole microseconds.
pub fn get_signal_period_us(cam: &Camera<'_>) -> Result<u32> {
    let raw = cam.get_feature("BslPeriodicSignalPeriod")?;
    // The camera formats this as a float ("333333.00"); the value is always integral.
    let period = raw.parse::<f64>().map_err(|e| {
        SyncTriggerError(format!(
            "BslPeriodicSignalPeriod parse error: '{raw}' ({e})"
        ))
    })?;
    Ok((period.round() as u32).clamp(MIN_PERIOD_US, MAX_PERIOD_US))
}

/// Which clock the camera is currently deriving `PeriodicSignal1` from.
pub fn signal_source(cam: &Camera<'_>) -> Result<PeriodicSignalSource> {
    PeriodicSignalSource::from_str(&cam.get_feature("BslPeriodicSignalSource")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typical_rates_round_to_whole_microseconds() {
        assert_eq!(period_us_for(3.0), 333_333);
        assert_eq!(period_us_for(2.0), 500_000);
        assert_eq!(period_us_for(10.0), 100_000);
    }

    /// 3 fps is not exactly representable: 1e6/3 rounds to 333333 µs, which runs
    /// marginally fast. Both cameras get the same integer, so they stay in phase.
    #[test]
    fn realized_rate_reflects_quantization() {
        let realized = realized_fps(period_us_for(3.0));
        assert!((realized - 3.000003).abs() < 1e-6, "got {realized}");
        // Exactly representable rates come back unchanged.
        assert_eq!(realized_fps(period_us_for(2.0)), 2.0);
    }

    #[test]
    fn rates_below_one_fps_clamp_to_the_one_second_period() {
        assert_eq!(period_us_for(0.5), MAX_PERIOD_US);
        assert_eq!(period_us_for(0.2), MAX_PERIOD_US);
        assert_eq!(realized_fps(period_us_for(0.5)), 1.0);
    }

    #[test]
    fn absurd_rates_clamp_instead_of_overflowing() {
        // Representable but faster than the 1 µs floor.
        assert_eq!(period_us_for(1e9), MIN_PERIOD_US);
        // Nonsensical requests fall back to the slowest period rather than the
        // fastest — an unusable rate is safer than hammering the camera.
        assert_eq!(period_us_for(0.0), MAX_PERIOD_US);
        assert_eq!(period_us_for(-1.0), MAX_PERIOD_US);
        assert_eq!(period_us_for(f64::NAN), MAX_PERIOD_US);
        assert_eq!(period_us_for(f64::INFINITY), MAX_PERIOD_US);
    }
}
