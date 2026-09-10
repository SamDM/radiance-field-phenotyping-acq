use crate::cfg::sync_trigger::{self, RateControl};
use crate::pylon::{Camera, PixelFormat};

pub fn load_user_set(cam: &mut Camera<'_>, set_name: &str) -> Result<(), cxx::Exception> {
    cam.set_feature("UserSetSelector", set_name)?;
    cam.execute_command("UserSetLoad")
}

pub fn set_exposure_time(cam: &mut Camera<'_>, usec: u32) -> Result<(), cxx::Exception> {
    cam.set_feature("ExposureTime", &usec.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemosaicingMethod {
    NearestNeighbor,
    Unilinear,
}

impl DemosaicingMethod {
    fn as_str(self) -> &'static str {
        match self {
            Self::NearestNeighbor => "NearestNeighbor",
            Self::Unilinear => "Unilinear",
        }
    }
}

pub fn set_pixel_format(cam: &mut Camera<'_>, format: PixelFormat) -> Result<(), cxx::Exception> {
    cam.set_feature("PixelFormat", format.as_str())
}

/// Set the acquisition rate via whichever feature owns it in this `mode`, returning
/// the rate the camera will actually run at.
///
/// The mode is passed explicitly rather than inferred from camera state so that the
/// compiler forces every call site to handle both. Writing the periodic-signal period
/// while the camera is actually free-running would silently do nothing — and worse,
/// reading it back would hand the client a plausible number that controls nothing.
pub fn set_framerate(
    cam: &mut Camera<'_>,
    fps: f64,
    mode: RateControl,
) -> Result<f64, Box<dyn std::error::Error>> {
    match mode {
        RateControl::PeriodicSignal => sync_trigger::set_signal_period(cam, fps),
        RateControl::AcquisitionLimiter => {
            set_acquisition_framerate(cam, fps)?;
            Ok(fps)
        }
    }
}

/// Free-running acquisition paced by the `AcquisitionFrameRate` limiter.
/// Only correct under [`RateControl::AcquisitionLimiter`] — go through
/// [`set_framerate`] unless you specifically mean the limiter.
pub fn set_acquisition_framerate(
    cam: &mut Camera<'_>,
    fps: f64,
) -> Result<(), cxx::Exception> {
    cam.set_feature("AcquisitionFrameRateEnable", "true")?;
    cam.set_feature("AcquisitionFrameRate", &fps.to_string())
}

pub fn set_demosaicing_method(
    cam: &mut Camera<'_>,
    method: DemosaicingMethod,
) -> Result<(), cxx::Exception> {
    cam.set_feature("BslDemosaicingMode", "Manual")?;
    cam.set_feature("BslDemosaicingMethod", method.as_str())
}
