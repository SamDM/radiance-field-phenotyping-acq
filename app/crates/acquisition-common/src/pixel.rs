use std::path::Path;

use image::{ColorType, DynamicImage, GenericImageView, GrayImage, ImageBuffer, ImageResult, Pixel, RgbImage};

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AnyPixelType {
    Gray,
    Bayer,
    Color,
}

pub trait PixelType: sealed::Sealed + Default + serde::Serialize + for<'de> serde::Deserialize<'de> {
    type Image: Clone + GenericImageView<Pixel: Pixel<Subpixel = u8>>;
    fn pixel_type() -> AnyPixelType;
    fn from_dynamic(img: DynamicImage) -> Self::Image;
    fn into_dynamic(img: Self::Image) -> DynamicImage;
    // TODO: return option
    fn from_raw(bytes: Vec<u8>, w: u32, h: u32) -> Self::Image;
    fn into_raw(img: Self::Image) -> Vec<u8>;
    // TODO: do we need this?
    fn save_to(bytes: &[u8], w: u32, h: u32, path: &Path) -> ImageResult<()>;
    fn into_color(img: Self::Image, _w: u32, _h: u32) -> RgbImage {
        Self::into_dynamic(img).into_rgb8()
    }
}

mod sealed {
    pub trait Sealed {}
}

#[derive(Debug, Clone, Default)]
pub struct Gray;
#[derive(Debug, Clone, Default)]
pub struct Bayer;
#[derive(Debug, Clone, Default)]
pub struct Color;

impl sealed::Sealed for Gray {}
impl sealed::Sealed for Bayer {}
impl sealed::Sealed for Color {}

macro_rules! impl_pixel_serde {
    ($ty:ty, $variant:ident) => {
        impl serde::Serialize for $ty {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                AnyPixelType::$variant.serialize(s)
            }
        }
        impl<'de> serde::Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                match AnyPixelType::deserialize(d)? {
                    AnyPixelType::$variant => Ok(Self),
                    other => Err(serde::de::Error::custom(format!("expected {}, got {:?}", stringify!($variant), other))),
                }
            }
        }
    };
}
impl_pixel_serde!(Gray, Gray);
impl_pixel_serde!(Bayer, Bayer);
impl_pixel_serde!(Color, Color);

impl PixelType for Gray {
    type Image = GrayImage;
    fn pixel_type() -> AnyPixelType { AnyPixelType::Gray }
    fn from_dynamic(img: DynamicImage) -> GrayImage { img.into_luma8() }
    fn into_dynamic(img: GrayImage) -> DynamicImage { DynamicImage::ImageLuma8(img) }
    fn from_raw(bytes: Vec<u8>, w: u32, h: u32) -> GrayImage {
        ImageBuffer::from_raw(w, h, bytes).expect("buffer size mismatch")
    }
    fn into_raw(img: GrayImage) -> Vec<u8> { img.into_raw() }
    fn save_to(bytes: &[u8], w: u32, h: u32, path: &Path) -> ImageResult<()> {
        image::save_buffer(path, bytes, w, h, ColorType::L8)
    }
}

impl PixelType for Bayer {
    type Image = GrayImage;
    fn pixel_type() -> AnyPixelType { AnyPixelType::Bayer }
    fn from_dynamic(img: DynamicImage) -> GrayImage { img.into_luma8() }
    fn into_dynamic(img: GrayImage) -> DynamicImage { DynamicImage::ImageLuma8(img) }
    fn from_raw(bytes: Vec<u8>, w: u32, h: u32) -> GrayImage {
        ImageBuffer::from_raw(w, h, bytes).expect("buffer size mismatch")
    }
    fn into_raw(img: GrayImage) -> Vec<u8> { img.into_raw() }
    fn save_to(bytes: &[u8], w: u32, h: u32, path: &Path) -> ImageResult<()> {
        image::save_buffer(path, bytes, w, h, ColorType::L8)
    }
    fn into_color(img: GrayImage, w: u32, h: u32) -> RgbImage {
        crate::debayer::debayer(img.as_raw(), w, h)
    }
}

impl PixelType for Color {
    type Image = RgbImage;
    fn pixel_type() -> AnyPixelType { AnyPixelType::Color }
    fn from_dynamic(img: DynamicImage) -> RgbImage { img.into_rgb8() }
    fn into_dynamic(img: RgbImage) -> DynamicImage { DynamicImage::ImageRgb8(img) }
    fn from_raw(bytes: Vec<u8>, w: u32, h: u32) -> RgbImage {
        ImageBuffer::from_raw(w, h, bytes).expect("buffer size mismatch")
    }
    fn into_raw(img: RgbImage) -> Vec<u8> { img.into_raw() }
    fn save_to(bytes: &[u8], w: u32, h: u32, path: &Path) -> ImageResult<()> {
        image::save_buffer(path, bytes, w, h, ColorType::Rgb8)
    }
}
