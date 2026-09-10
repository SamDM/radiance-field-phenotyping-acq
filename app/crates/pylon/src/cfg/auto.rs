use std::time::{Duration, Instant};

use tracing::{debug, info};

use crate::cfg::config::set_framerate;
use crate::cfg::sync_trigger::{get_signal_period_us, realized_fps, RateControl};
use crate::pylon::{Camera, CameraArray};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
struct AutoError(String);

impl std::fmt::Display for AutoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AutoError {}

#[derive(Debug, Clone, Copy)]
pub enum AutoFunctionProfile {
    MinimizeGain,
    MinimizeExposureTime,
}

impl AutoFunctionProfile {
    fn as_feature_str(self) -> &'static str {
        match self {
            Self::MinimizeGain => "MinimizeGain",
            Self::MinimizeExposureTime => "MinimizeExposureTime",
        }
    }
}

/// Controls whether and how a single auto-tuning parameter is adjusted.
#[derive(Debug, Clone)]
pub enum AutoParam {
    /// Do not auto-tune this parameter.
    NoAdjust,
    /// Auto-tune using the given limits, or the camera's existing limit
    /// registers if `None`.
    Adjust(Option<[f64; 2]>),
}

#[derive(Debug, Clone)]
pub struct AutoAdjustConfig {
    pub timeout: Duration,
    /// Disable the acquisition framerate limiter during convergence so the
    /// camera runs at maximum throughput, then restore the original setting.
    pub boost_framerate: bool,
    pub profile: AutoFunctionProfile,
    pub target_brightness: f64,
    pub gain: AutoParam,
    pub exposure_us: AutoParam,
    pub auto_whitebalance: bool,
}

impl Default for AutoAdjustConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            boost_framerate: true,
            profile: AutoFunctionProfile::MinimizeExposureTime,
            target_brightness: 0.5,
            gain: AutoParam::NoAdjust,
            exposure_us: AutoParam::NoAdjust,
            auto_whitebalance: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AveragedSettings {
    pub exposure_time_us: Option<f64>,
    pub gain: Option<f64>,
    /// (red, green, blue) balance ratios.
    pub white_balance: Option<(f64, f64, f64)>,
}

const AUTO_FEATURES: &[&str] = &["ExposureAuto", "GainAuto", "BalanceWhiteAuto"];

fn parse_f64(value: String, context: &str) -> Result<f64> {
    value
        .parse::<f64>()
        .map_err(|e| AutoError(format!("{context} parse error: '{value}' ({e})")).into())
}

fn read_balance_ratio(cam: &mut Camera<'_>, channel: &str) -> Result<f64> {
    cam.set_feature("BalanceRatioSelector", channel)?;
    parse_f64(cam.get_feature("BalanceRatio")?, &format!("BalanceRatio({channel})"))
}

fn write_balance_ratio(cam: &mut Camera<'_>, channel: &str, value: f64) -> Result<()> {
    cam.set_feature("BalanceRatioSelector", channel)?;
    cam.set_feature("BalanceRatio", &format!("{value}"))?;
    Ok(())
}

pub fn sync_exposure(cameras: &mut CameraArray) -> Result<f64> {
    let n = cameras.len();
    let mut sum = 0.0f64;
    for i in 0..n {
        let mut cam = cameras.get_camera(i);
        let _ = cam.set_feature("ExposureAuto", "Off");
        let v = parse_f64(cam.get_feature("ExposureTime")?, "ExposureTime")?;
        debug!(camera = i, exposure_us = v, "exposure reading");
        sum += v;
    }
    let avg = sum / n as f64;
    for i in 0..n {
        cameras.get_camera(i).set_feature("ExposureTime", &format!("{avg}"))?;
    }
    Ok(avg)
}

pub fn sync_gain(cameras: &mut CameraArray) -> Result<f64> {
    let n = cameras.len();
    let mut sum = 0.0f64;
    for i in 0..n {
        let mut cam = cameras.get_camera(i);
        let _ = cam.set_feature("GainAuto", "Off");
        let v = parse_f64(cam.get_feature("Gain")?, "Gain")?;
        debug!(camera = i, gain = v, "gain reading");
        sum += v;
    }
    let avg = sum / n as f64;
    for i in 0..n {
        cameras.get_camera(i).set_feature("Gain", &format!("{avg}"))?;
    }
    Ok(avg)
}

pub fn sync_whitebalance(cameras: &mut CameraArray) -> Result<(f64, f64, f64)> {
    let n = cameras.len();
    let mut sum_r = 0.0f64;
    let mut sum_g = 0.0f64;
    let mut sum_b = 0.0f64;
    for i in 0..n {
        let mut cam = cameras.get_camera(i);
        let _ = cam.set_feature("BalanceWhiteAuto", "Off");
        let r = read_balance_ratio(&mut cam, "Red")?;
        let g = read_balance_ratio(&mut cam, "Green")?;
        let b = read_balance_ratio(&mut cam, "Blue")?;
        debug!(camera = i, red = r, green = g, blue = b, "white balance reading");
        sum_r += r;
        sum_g += g;
        sum_b += b;
    }
    let n_f = n as f64;
    let (avg_r, avg_g, avg_b) = (sum_r / n_f, sum_g / n_f, sum_b / n_f);
    for i in 0..n {
        let mut cam = cameras.get_camera(i);
        write_balance_ratio(&mut cam, "Red", avg_r)?;
        write_balance_ratio(&mut cam, "Green", avg_g)?;
        write_balance_ratio(&mut cam, "Blue", avg_b)?;
    }
    Ok((avg_r, avg_g, avg_b))
}

/// Read the acquisition rate off every camera, average it, and write the average back
/// so all cameras are byte-identical. Returns the realized rate.
///
/// Reads whichever feature owns the rate in this `mode` — writing the periodic-signal
/// period while the cameras are actually free-running (or vice versa) would report a
/// number that controls nothing.
pub fn sync_framerate(cameras: &mut CameraArray, mode: RateControl) -> Result<f64> {
    let n = cameras.len();
    let mut sum = 0.0f64;
    for i in 0..n {
        let cam = cameras.get_camera(i);
        let v = match mode {
            RateControl::PeriodicSignal => realized_fps(get_signal_period_us(&cam)?),
            RateControl::AcquisitionLimiter => {
                parse_f64(cam.get_feature("AcquisitionFrameRate")?, "AcquisitionFrameRate")?
            }
        };
        debug!(camera = i, framerate = v, ?mode, "framerate reading");
        sum += v;
    }
    let avg = sum / n as f64;
    // Round-trip through the mode's own quantization so the value we report is the one
    // the cameras end up with.
    let mut realized = avg;
    for i in 0..n {
        let mut cam = cameras.get_camera(i);
        realized = set_framerate(&mut cam, avg, mode)?;
    }
    Ok(realized)
}

/// Handle for an in-progress auto-adjust operation. The caller drives the
/// process by calling [`AutoAdjustHandle::tick`] after each frame retrieved
/// from the camera array. When tick returns [`AutoAdjustTick::Converged`],
/// call [`AutoAdjustHandle::finish`] to read, average, and apply the settings.
/// To cancel early, call [`AutoAdjustHandle::abort`].
pub struct AutoAdjustHandle {
    deadline: Instant,
    confirmed: Vec<[bool; AUTO_FEATURES.len()]>,
    saved_rate: SavedRate,
    tune_exposure: bool,
    tune_gain: bool,
    tune_whitebalance: bool,
}

/// Period used while boosting under [`RateControl::PeriodicSignal`]: far shorter than
/// the camera can service, so it delivers frames as fast as it can. Over-triggering is
/// safe — the camera skips whole periods and stays on the shared grid.
const BOOST_PERIOD_US: u32 = 10_000;

/// The acquisition-rate settings the framerate boost displaced, so [`finish`] can put
/// them back. Which variant applies is fixed by the [`RateControl`] in force; keeping
/// them in one enum makes it impossible to restore the wrong feature.
///
/// [`finish`]: AutoAdjustHandle::finish
enum SavedRate {
    /// Boost was not requested — nothing to restore.
    NotBoosted,
    /// Per-camera `BslPeriodicSignalPeriod`, in microseconds.
    Period(Vec<u32>),
    /// Per-camera `AcquisitionFrameRateEnable` / `AcquisitionFrameRate`, verbatim.
    Limiter { enable: Vec<String>, rate: Vec<String> },
}

pub enum AutoAdjustTick {
    Converging,
    Converged,
    TimedOut,
}

/// Start auto-adjust. Configures all cameras and returns a handle. The caller
/// must keep calling [`AutoAdjustHandle::tick`] once per frame until the
/// handle signals completion or timeout.
pub fn begin_auto_adjust(
    cameras: &mut CameraArray,
    cfg: &AutoAdjustConfig,
    mode: RateControl,
) -> Result<AutoAdjustHandle> {
    let n = cameras.len();
    info!(
        timeout = ?cfg.timeout,
        target_brightness = ?cfg.target_brightness,
        profile = ?cfg.profile,
        n_cameras = n,
        ?mode,
        "starting auto-adjust",
    );

    let saved_rate = if !cfg.boost_framerate {
        SavedRate::NotBoosted
    } else {
        match mode {
            RateControl::PeriodicSignal => {
                let mut periods = Vec::with_capacity(n);
                for i in 0..n {
                    periods.push(get_signal_period_us(&cameras.get_camera(i))?);
                }
                SavedRate::Period(periods)
            }
            RateControl::AcquisitionLimiter => {
                let mut enable = Vec::with_capacity(n);
                let mut rate = Vec::with_capacity(n);
                for i in 0..n {
                    let cam = cameras.get_camera(i);
                    enable.push(cam.get_feature("AcquisitionFrameRateEnable")?);
                    rate.push(cam.get_feature("AcquisitionFrameRate")?);
                }
                SavedRate::Limiter { enable, rate }
            }
        }
    };

    let tune_gain = matches!(cfg.gain, AutoParam::Adjust(_));
    let tune_exposure = matches!(cfg.exposure_us, AutoParam::Adjust(_));

    for i in 0..n {
        let mut cam = cameras.get_camera(i);
        cam.set_feature("AutoFunctionProfile", cfg.profile.as_feature_str())?;
        cam.set_feature("AutoTargetBrightness", &format!("{}", cfg.target_brightness))?;
        if let AutoParam::Adjust(limits) = &cfg.gain {
            if let Some([lo, hi]) = limits {
                cam.set_feature("AutoGainLowerLimit", &format!("{lo}"))?;
                cam.set_feature("AutoGainUpperLimit", &format!("{hi}"))?;
            }
            cam.set_feature("GainAuto", "Once")?;
        }
        if let AutoParam::Adjust(limits) = &cfg.exposure_us {
            if let Some([lo, hi]) = limits {
                cam.set_feature("AutoExposureTimeLowerLimit", &format!("{lo}"))?;
                cam.set_feature("AutoExposureTimeUpperLimit", &format!("{hi}"))?;
            }
            cam.set_feature("ExposureAuto", "Once")?;
        }
        if cfg.auto_whitebalance {
            cam.set_feature("BalanceWhiteAuto", "Once")?;
        }
        match saved_rate {
            SavedRate::NotBoosted => {}
            // The limiter is already off in this mode, so shrinking the period is what
            // actually raises the rate.
            SavedRate::Period(_) => {
                cam.set_feature("BslPeriodicSignalPeriod", &BOOST_PERIOD_US.to_string())?;
            }
            SavedRate::Limiter { .. } => {
                cam.set_feature("AcquisitionFrameRateEnable", "false")?;
            }
        }
    }

    Ok(AutoAdjustHandle {
        deadline: Instant::now() + cfg.timeout,
        confirmed: vec![[false; AUTO_FEATURES.len()]; n],
        saved_rate,
        tune_exposure,
        tune_gain,
        tune_whitebalance: cfg.auto_whitebalance,
    })
}

impl AutoAdjustHandle {
    /// Check convergence progress. Call this once per frame retrieved from the
    /// camera array — frame arrival is the natural pacing signal for the Pylon
    /// auto algorithms.
    pub fn tick(&mut self, cameras: &mut CameraArray) -> Result<AutoAdjustTick> {
        if Instant::now() >= self.deadline {
            return Ok(AutoAdjustTick::TimedOut);
        }

        let mut all_done = true;
        for (i, cam_confirmed) in self.confirmed.iter_mut().enumerate() {
            let cam = cameras.get_camera(i);
            for (j, feature) in AUTO_FEATURES.iter().enumerate() {
                if cam_confirmed[j] {
                    continue;
                }
                let value = cam.get_feature(feature)?;
                if value == "Off" {
                    cam_confirmed[j] = true;
                    debug!(camera = i, feature, "converged");
                } else {
                    all_done = false;
                }
            }
        }

        if all_done {
            Ok(AutoAdjustTick::Converged)
        } else {
            Ok(AutoAdjustTick::Converging)
        }
    }

    /// Read converged settings from all cameras, average them, and write the
    /// averages back so all cameras are byte-identical. Also restores framerate
    /// settings if boost was active.
    pub fn finish(self, cameras: &mut CameraArray) -> Result<AveragedSettings> {
        self.restore_framerate(cameras)?;
        self.sync_cameras(cameras)
    }

    pub fn tunes_exposure(&self) -> bool { self.tune_exposure }
    pub fn tunes_gain(&self) -> bool { self.tune_gain }
    pub fn tunes_whitebalance(&self) -> bool { self.tune_whitebalance }

    fn sync_cameras(&self, cameras: &mut CameraArray) -> Result<AveragedSettings> {
        let exposure_time_us = if self.tune_exposure { Some(sync_exposure(cameras)?) } else { None };
        let gain = if self.tune_gain { Some(sync_gain(cameras)?) } else { None };
        let white_balance = if self.tune_whitebalance { Some(sync_whitebalance(cameras)?) } else { None };
        Ok(AveragedSettings { exposure_time_us, gain, white_balance })
    }

    fn restore_framerate(&self, cameras: &mut CameraArray) -> Result<()> {
        match &self.saved_rate {
            SavedRate::NotBoosted => {}
            SavedRate::Period(periods) => {
                for (i, period_us) in periods.iter().enumerate() {
                    cameras
                        .get_camera(i)
                        .set_feature("BslPeriodicSignalPeriod", &period_us.to_string())?;
                }
            }
            SavedRate::Limiter { enable, rate } => {
                for i in 0..enable.len() {
                    let mut cam = cameras.get_camera(i);
                    cam.set_feature("AcquisitionFrameRateEnable", &enable[i])?;
                    cam.set_feature("AcquisitionFrameRate", &rate[i])?;
                }
            }
        }
        Ok(())
    }
}
