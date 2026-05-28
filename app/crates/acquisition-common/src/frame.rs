use std::marker::PhantomData;
use std::path::Path;

use image::{GenericImageView, ImageBuffer};
use time::{OffsetDateTime, UtcDateTime};

use crate::pixel::{AnyPixelType, Bayer, Color, Gray, PixelType};

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(bound(serialize = "", deserialize = ""))]
pub struct Frame<P: PixelType> {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub camera_timestamp_ns: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub host_timestamp: OffsetDateTime,
    pub camera_name: String,
    #[serde(skip)]
    _phantom: PhantomData<P>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(bound(serialize = "", deserialize = ""))]
pub struct FrameMetadata<P: PixelType> {
    pub pixel_type: P,
    pub camera_timestamp_ns: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub host_timestamp: OffsetDateTime,
    pub camera_name: String,
    pub width: u32,
    pub height: u32,
}

impl<P: PixelType> FrameMetadata<P> {
    pub fn new(
        camera_timestamp_ns: u64,
        host_timestamp: OffsetDateTime,
        camera_name: String,
        width: u32,
        height: u32,
    ) -> Self {
        FrameMetadata { pixel_type: P::default(), camera_timestamp_ns, host_timestamp, camera_name, width, height }
    }
}

impl From<Frame<Gray>> for AnyFrame {
    fn from(f: Frame<Gray>) -> Self { AnyFrame::Gray(f) }
}
impl From<Frame<Bayer>> for AnyFrame {
    fn from(f: Frame<Bayer>) -> Self { AnyFrame::Bayer(f) }
}
impl From<Frame<Color>> for AnyFrame {
    fn from(f: Frame<Color>) -> Self { AnyFrame::Color(f) }
}

impl<P: PixelType> Frame<P> {
    // --- Construction ---

    pub fn new(
        data: Vec<u8>,
        width: u32,
        height: u32,
        camera_timestamp_ns: u64,
        host_timestamp: OffsetDateTime,
        camera_name: String,
    ) -> Self {
        Frame { data, width, height, camera_timestamp_ns, host_timestamp, camera_name, _phantom: PhantomData }
    }

    pub fn from_parts(image: P::Image, metadata: FrameMetadata<P>) -> Self {
        Frame::new(
            P::into_raw(image),
            metadata.width, metadata.height,
            metadata.camera_timestamp_ns, metadata.host_timestamp, metadata.camera_name,
        )
    }

    // --- Decomposition / conversion ---

    pub fn into_parts(self) -> (P::Image, FrameMetadata<P>) {
        let meta = FrameMetadata::new(
            self.camera_timestamp_ns, self.host_timestamp, self.camera_name, self.width, self.height,
        );
        let image = P::from_raw(self.data, meta.width, meta.height);
        (image, meta)
    }

    pub fn into_data(self) -> P::Image {
        P::from_raw(self.data, self.width, self.height)
    }

    pub fn into_color(self) -> Frame<Color> {
        let (image, meta) = self.into_parts();
        let rgb = P::into_color(image, meta.width, meta.height);
        Frame::new(
            rgb.into_raw(),
            meta.width, meta.height,
            meta.camera_timestamp_ns, meta.host_timestamp, meta.camera_name,
        )
    }

    pub fn into_any(self) -> AnyFrame where AnyFrame: From<Frame<P>> {
        self.into()
    }

    // --- Persistence ---

    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let img = image::open(path)?;
        let json = std::fs::read_to_string(path.with_extension("json"))?;
        let meta: FrameMetadata<P> = serde_json::from_str(&json)?;
        Ok(Frame::from_parts(P::from_dynamic(img), meta))
    }

    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        P::save_to(&self.data, self.width, self.height, path)?;
        std::fs::write(path.with_extension("json"), serde_json::to_string_pretty(&self.metadata())?)?;
        Ok(())
    }

    // --- Accessors ---

    pub fn image(&self) -> ImageBuffer<<P::Image as GenericImageView>::Pixel, &[u8]> {
        ImageBuffer::from_raw(self.width, self.height, self.data.as_slice())
            .expect("stored bytes match declared dimensions")
    }

    pub fn metadata(&self) -> FrameMetadata<P> {
        FrameMetadata::new(
            self.camera_timestamp_ns, self.host_timestamp, self.camera_name.clone(), self.width, self.height,
        )
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn camera_name(&self) -> &str {
        &self.camera_name
    }

    pub fn camera_timestamp_ns(&self) -> u64 {
        self.camera_timestamp_ns
    }

    pub fn camera_datetime_utc(&self) -> UtcDateTime {
        let ns = self.camera_timestamp_ns as i128;
        UtcDateTime::from_unix_timestamp_nanos(ns)
            .expect("camera timestamp out of representable range (corrupted clock?)")
    }

    pub fn camera_datetime_local(&self) -> OffsetDateTime {
        OffsetDateTime::from(self.camera_datetime_utc()).to_offset(self.host_timestamp.offset())
    }

    pub fn host_datetime_utc(&self) -> UtcDateTime {
        UtcDateTime::from(self.host_timestamp)
    }

    pub fn host_datetime_local(&self) -> OffsetDateTime {
        self.host_timestamp
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub enum AnyFrame {
    Gray(Frame<Gray>),
    Bayer(Frame<Bayer>),
    Color(Frame<Color>),
}

macro_rules! map_any_frame {
    ($any:expr, $f:pat => $action:expr) => {
        match $any {
            AnyFrame::Gray($f) => $action,
            AnyFrame::Bayer($f) => $action,
            AnyFrame::Color($f) => $action,
        }
    };
}

impl AnyFrame {
    // --- Conversion ---

    pub fn into_color(self) -> Frame<Color> {
        map_any_frame!(self, f => f.into_color())
    }

    // --- Persistence ---

    pub fn load(path: &Path) -> Result<AnyFrame, Box<dyn std::error::Error>> {
        // Helper struct that matches the `Frame::pixel_type` field of the full json object,
        // other `Frame` fields are silently ignored by serde.
        #[derive(serde::Deserialize)]
        struct PixelTypePeek { pixel_type: AnyPixelType }

        // Use the helper struct to only extract the pixel type, to do correct dispatch.
        let json = std::fs::read_to_string(path.with_extension("json"))?;
        let peek: PixelTypePeek = serde_json::from_str(&json)?;
        match peek.pixel_type {
            AnyPixelType::Gray => Ok(Frame::<Gray>::load(path)?.into_any()),
            AnyPixelType::Bayer => Ok(Frame::<Bayer>::load(path)?.into_any()),
            AnyPixelType::Color => Ok(Frame::<Color>::load(path)?.into_any()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        map_any_frame!(self, f => f.save(path))
    }

    // --- Accessors ---

    pub fn width(&self) -> u32 {
        map_any_frame!(self, f => f.width())
    }

    pub fn height(&self) -> u32 {
        map_any_frame!(self, f => f.height())
    }

    pub fn camera_name(&self) -> &str {
        map_any_frame!(self, f => f.camera_name())
    }

    pub fn camera_timestamp_ns(&self) -> u64 {
        map_any_frame!(self, f => f.camera_timestamp_ns())
    }

    pub fn camera_datetime_utc(&self) -> UtcDateTime {
        map_any_frame!(self, f => f.camera_datetime_utc())
    }

    pub fn camera_datetime_local(&self) -> OffsetDateTime {
        map_any_frame!(self, f => f.camera_datetime_local())
    }

    pub fn host_datetime_utc(&self) -> UtcDateTime {
        map_any_frame!(self, f => f.host_datetime_utc())
    }

    pub fn host_datetime_local(&self) -> OffsetDateTime {
        map_any_frame!(self, f => f.host_datetime_local())
    }
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::*;

    fn gray_frame() -> AnyFrame {
        let (w, h) = (4, 4);
        AnyFrame::Gray(Frame::new(
            vec![64u8; w * h],
            w as u32, h as u32,
            1_000_000_000,
            OffsetDateTime::UNIX_EPOCH,
            "cam_gray".into(),
        ))
    }

    fn bayer_frame() -> AnyFrame {
        let (w, h) = (4, 4);
        AnyFrame::Bayer(Frame::new(
            (0u8..16).collect(),
            w as u32, h as u32,
            2_000_000_000,
            OffsetDateTime::UNIX_EPOCH,
            "cam_bayer".into(),
        ))
    }

    fn color_frame() -> AnyFrame {
        let (w, h) = (4, 4);
        AnyFrame::Color(Frame::new(
            vec![128u8; w * h * 3],
            w as u32, h as u32,
            3_000_000_000,
            OffsetDateTime::UNIX_EPOCH,
            "cam_color".into(),
        ))
    }

    fn assert_round_trip(original: AnyFrame) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frame.png");

        original.save(&path).unwrap();
        let loaded = AnyFrame::load(&path).unwrap();

        assert_eq!(original.width(), loaded.width());
        assert_eq!(original.height(), loaded.height());
        assert_eq!(original.camera_name(), loaded.camera_name());
        assert_eq!(original.camera_timestamp_ns(), loaded.camera_timestamp_ns());
        assert_eq!(original.host_datetime_utc(), loaded.host_datetime_utc());

        // Pixel data round-trips losslessly through PNG
        let orig_data = map_any_frame!(&original, f => f.data.clone());
        let load_data = map_any_frame!(&loaded, f => f.data.clone());
        assert_eq!(orig_data, load_data);
    }

    #[test]
    fn round_trip_gray() {
        let frame = gray_frame();
        assert!(matches!(frame, AnyFrame::Gray(_)));
        assert_round_trip(frame);
    }

    #[test]
    fn round_trip_bayer() {
        let frame = bayer_frame();
        assert!(matches!(frame, AnyFrame::Bayer(_)));
        assert_round_trip(frame);
    }

    #[test]
    fn round_trip_color() {
        let frame = color_frame();
        assert!(matches!(frame, AnyFrame::Color(_)));
        assert_round_trip(frame);
    }
}
