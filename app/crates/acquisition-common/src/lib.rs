pub mod debayer;
pub mod downscale;
pub mod frame;
pub mod pixel;
pub mod protocol;

pub use frame::{AnyFrame, Frame, FrameMetadata};
pub use pixel::{AnyPixelType, Bayer, Color, Gray};

/// Returns the display name for a recording folder relative to the recordings base directory.
/// Falls back to the full folder path if it doesn't start with `base_dir`.
pub fn recording_display_name(folder: &str, base_dir: &str) -> String {
    std::path::Path::new(folder)
        .strip_prefix(base_dir)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| folder.to_owned())
}
