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
