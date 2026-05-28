#[cxx::bridge(namespace = "rfp")]
mod ffi {
    unsafe extern "C++" {
        include!("pylon/cpp/pylon_bridge.h");

        fn pylon_init();
        fn pylon_terminate();

        type CameraArray;

        fn new_camera_array(n: usize) -> UniquePtr<CameraArray>;
        fn open(self: Pin<&mut CameraArray>);
        fn close(self: Pin<&mut CameraArray>);
        fn start_grabbing(self: Pin<&mut CameraArray>) -> Result<()>;
        fn stop_grabbing(self: Pin<&mut CameraArray>);
        fn is_grabbing(self: &CameraArray) -> bool;
        fn len(self: &CameraArray) -> usize;
        fn get_camera(self: &CameraArray, index: usize) -> UniquePtr<Camera>;
        fn retrieve_result(self: Pin<&mut CameraArray>, timeout_ms: u32) -> UniquePtr<GrabResult>;

        type Camera;

        fn set_feature(self: Pin<&mut Camera>, name: &str, value: &str) -> Result<()>;
        fn get_feature(self: &Camera, name: &str) -> Result<String>;
        fn execute_command(self: Pin<&mut Camera>, name: &str) -> Result<()>;
        fn load_config_file(self: Pin<&mut Camera>, path: &str);
        fn save_config_file(self: Pin<&mut Camera>, path: &str);
        fn device_info(self: &Camera) -> UniquePtr<DeviceInfo>;

        type DeviceInfo;

        fn serial_number(self: &DeviceInfo) -> Result<String>;
        fn ip_address(self: &DeviceInfo) -> Result<String>;
        fn subnet_mask(self: &DeviceInfo) -> Result<String>;
        fn friendly_name(self: &DeviceInfo) -> Result<String>;

        type GrabResult;

        fn succeeded(self: &GrabResult) -> bool;
        fn error_description(self: &GrabResult) -> String;
        fn width(self: &GrabResult) -> u32;
        fn height(self: &GrabResult) -> u32;
        fn camera_index(self: &GrabResult) -> usize;
        #[cxx_name = "image_data"]
        fn image_data<'a>(self: &'a GrabResult) -> &'a [u8];
        fn timestamp_ns(self: &GrabResult) -> u64;
        fn pixel_format(self: &GrabResult) -> String;
    }
}

pub struct PylonContext(());

impl Drop for PylonContext {
    fn drop(&mut self) {
        ffi::pylon_terminate();
    }
}

pub fn pylon_init() -> PylonContext {
    ffi::pylon_init();
    PylonContext(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Mono8,
    BayerRG8,
    BayerRG10,
    BayerRG12,
    Rgb8,
    Bgr8,
    YCbCr422_8,
    Other,
}

impl PixelFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mono8 => "Mono8",
            Self::BayerRG8 => "BayerRG8",
            Self::BayerRG10 => "BayerRG10",
            Self::BayerRG12 => "BayerRG12",
            Self::Rgb8 => "RGB8",
            Self::Bgr8 => "BGR8",
            Self::YCbCr422_8 => "YCbCr422_8",
            Self::Other => "Other",
        }
    }

    pub(crate) fn from_pylon_str(s: &str) -> Self {
        match s {
            "Mono8" => Self::Mono8,
            "BayerRG8" => Self::BayerRG8,
            "BayerRG10" => Self::BayerRG10,
            "BayerRG12" => Self::BayerRG12,
            "RGB8" => Self::Rgb8,
            "BGR8" => Self::Bgr8,
            "YCbCr422_8" => Self::YCbCr422_8,
            _ => Self::Other,
        }
    }
}

use cxx::UniquePtr;
use std::marker::PhantomData;
use time::{OffsetDateTime, UtcDateTime};
use tracing::trace;

pub struct DeviceInfo(UniquePtr<ffi::DeviceInfo>);

impl DeviceInfo {
    pub fn serial_number(&self) -> Result<String, cxx::Exception> {
        self.0.serial_number()
    }
    pub fn ip_address(&self) -> Result<String, cxx::Exception> {
        self.0.ip_address()
    }
    pub fn subnet_mask(&self) -> Result<String, cxx::Exception> {
        self.0.subnet_mask()
    }
    pub fn friendly_name(&self) -> Result<String, cxx::Exception> {
        self.0.friendly_name()
    }
}

pub struct Camera<'a>(UniquePtr<ffi::Camera>, PhantomData<&'a CameraArray>);

impl<'a> Camera<'a> {
    pub fn set_feature(&mut self, name: &str, value: &str) -> Result<(), cxx::Exception> {
        trace!(name, value, "setting feature");
        self.0.pin_mut().set_feature(name, value)
    }
    pub fn get_feature(&self, name: &str) -> Result<String, cxx::Exception> {
        self.0.get_feature(name)
    }
    pub fn execute_command(&mut self, name: &str) -> Result<(), cxx::Exception> {
        trace!(name, "running command");
        self.0.pin_mut().execute_command(name)
    }
    pub fn load_config_file(&mut self, path: &str) {
        trace!(path, "loading config");
        self.0.pin_mut().load_config_file(path);
    }
    pub fn save_config_file(&mut self, path: &str) {
        trace!(path, "saving config");
        self.0.pin_mut().save_config_file(path);
    }
    pub fn device_info(&self) -> DeviceInfo {
        DeviceInfo(self.0.device_info())
    }
}

pub struct CameraArray(UniquePtr<ffi::CameraArray>);

// TODO: pre-allocate `ffi::Camera` instances?
// I often create and destroy them for one-off operations.
// This may also cleanup the <'a> on each Camera instance,
// because then `get_camera` can return a reference tied to self.
impl CameraArray {
    pub fn new(n: usize) -> Self {
        Self(ffi::new_camera_array(n))
    }
    pub fn open(&mut self) {
        self.0.pin_mut().open();
    }
    pub fn close(&mut self) {
        self.0.pin_mut().close();
    }
    pub fn start_grabbing(&mut self) -> Result<(), cxx::Exception> {
        self.0.pin_mut().start_grabbing()
    }
    pub fn stop_grabbing(&mut self) {
        self.0.pin_mut().stop_grabbing();
    }
    pub fn is_grabbing(&self) -> bool {
        self.0.is_grabbing()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn get_camera(&self, index: usize) -> Camera<'_> {
        Camera(self.0.get_camera(index), PhantomData)
    }
    pub fn retrieve_result(&mut self, timeout_ms: u32) -> Option<Result<GrabResult, GrabError>> {
        let result = self.0.pin_mut().retrieve_result(timeout_ms);
        if result.is_null() {
            return None;
        }
        let camera_idx = result.camera_index();
        let name = self
            .get_camera(camera_idx)
            .device_info()
            .friendly_name()
            .unwrap_or_default();
        if result.succeeded() {
            Some(Ok(GrabResult(
                result,
                OffsetDateTime::now_local().expect("getting host time succeeds"),
                name,
            )))
        } else {
            Some(Err(GrabError {
                camera_index: camera_idx,
                camera_name: name,
                description: result.error_description(),
            }))
        }
    }
}

pub struct GrabResult(UniquePtr<ffi::GrabResult>, OffsetDateTime, String);

#[derive(Debug)]
pub struct GrabError {
    pub camera_index: usize,
    pub camera_name: String,
    pub description: String,
}

impl std::fmt::Display for GrabError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "camera {} ({}): {}", self.camera_index, self.camera_name, self.description)
    }
}

impl std::error::Error for GrabError {}

impl GrabResult {
    pub fn width(&self) -> u32 {
        self.0.width()
    }
    pub fn height(&self) -> u32 {
        self.0.height()
    }
    pub fn camera_index(&self) -> usize {
        self.0.camera_index()
    }
    pub fn camera_name(&self) -> &str {
        &self.2
    }
    pub fn image_data(&self) -> &[u8] {
        self.0.image_data()
    }
    pub fn pixel_format(&self) -> PixelFormat {
        PixelFormat::from_pylon_str(&self.0.pixel_format())
    }

    pub fn camera_timestamp_ns(&self) -> u64 {
        self.0.timestamp_ns()
    }
    pub fn camera_datetime_utc(&self) -> UtcDateTime {
        let ns = self.0.timestamp_ns() as i128;
        UtcDateTime::from_unix_timestamp_nanos(ns)
            .expect("camera timestamp out of representable range (corrupted clock?)")
    }
    pub fn camera_datetime_local(&self) -> OffsetDateTime {
        OffsetDateTime::from(self.camera_datetime_utc()).to_offset(self.1.offset())
    }
    pub fn host_datetime_utc(&self) -> UtcDateTime {
        UtcDateTime::from(self.1)
    }
    pub fn host_datetime_local(&self) -> OffsetDateTime {
        self.1
    }
}
