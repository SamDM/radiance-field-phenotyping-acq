use std::path::{Path, PathBuf};

use time::macros::format_description;
use time::OffsetDateTime;

/// Expand the directory template's placeholders for a given recording start time.
/// Supported placeholders: `$HOME`, `{local_date}`, `{local_time}`, `{local_datetime}`.
pub(super) fn expand_dir(dir_template: &str, recording_start: OffsetDateTime) -> PathBuf {
    let date = recording_start
        .format(format_description!("[year]-[month]-[day]"))
        .expect("OffsetDateTime formatting cannot fail");
    let time = recording_start
        .format(format_description!("[hour]-[minute]-[second]"))
        .expect("OffsetDateTime formatting cannot fail");
    let datetime = format!("{date}T{time}");
    let home = std::env::var("HOME").unwrap_or_default();
    let s = dir_template
        .replace("$HOME", &home)
        .replace("{local_datetime}", &datetime)
        .replace("{local_date}", &date)
        .replace("{local_time}", &time);
    Path::new(&s).to_path_buf()
}

/// Build the fixed per-frame filename: `cam-{cam_idx}_frame-{frame_idx:05}.{ext}`.
/// This is the single place that defines the on-disk frame naming scheme; keep it
/// symmetric with `parse_frame_filename` below.
pub(super) fn frame_filename(cam_idx: usize, frame_idx: u32, ext: &str) -> String {
    format!("cam-{cam_idx}_frame-{frame_idx:05}.{ext}")
}

/// Parse a frame file stem (filename without extension) produced by `frame_filename`
/// back into `(cam_idx, frame_idx)`. Kept next to the generator so the two can't drift.
pub(super) fn parse_frame_filename(stem: &str) -> Option<(usize, u32)> {
    let (cam_part, frame_part) = stem.split_once('_')?;
    let cam_idx = cam_part.strip_prefix("cam-")?.parse::<usize>().ok()?;
    let frame_idx = frame_part.strip_prefix("frame-")?.parse::<u32>().ok()?;
    Some((cam_idx, frame_idx))
}

/// Extensions a recording frame can carry.
///
/// The recorder itself writes only `tiff`, `png` or `jpg` (see `FrameFormat::extension`);
/// the rest are accepted because the same files turn up hand-converted.
const FRAME_EXTENSIONS: [&str; 5] = ["png", "tiff", "tif", "jpg", "jpeg"];

/// Recognise a frame file by its full path, returning `(cam_idx, frame_idx)`.
///
/// The single definition of "this file is a frame", shared by replay and the library
/// scan. Both the name and the extension have to match: every frame is written with a
/// `.json` sidecar of the *same stem*, so a stem-only test counts each frame twice, and
/// an allowlist rather than a `!= "json"` test means an unrecognised file is never
/// mistaken for image data that replay would then fail to decode.
pub(super) fn parse_frame_path(path: &Path) -> Option<(usize, u32)> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    if !FRAME_EXTENSIONS.contains(&ext) {
        return None;
    }
    parse_frame_filename(path.file_stem().and_then(|s| s.to_str())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_filename_round_trips() {
        for (cam_idx, frame_idx) in [(0, 0), (3, 42), (11, 99999)] {
            let filename = frame_filename(cam_idx, frame_idx, "tiff");
            let stem = filename.strip_suffix(".tiff").unwrap();
            assert_eq!(parse_frame_filename(stem), Some((cam_idx, frame_idx)));
        }
    }

    #[test]
    fn the_json_sidecar_is_not_a_frame() {
        // Sidecar and frame share a stem, so only the extension tells them apart.
        assert_eq!(parse_frame_path(Path::new("d/cam-0_frame-00007.png")), Some((0, 7)));
        assert_eq!(parse_frame_path(Path::new("d/cam-0_frame-00007.json")), None);
    }

    #[test]
    fn parse_frame_path_requires_a_known_image_extension() {
        assert_eq!(parse_frame_path(Path::new("cam-1_frame-00002.tiff")), Some((1, 2)));
        assert_eq!(parse_frame_path(Path::new("cam-1_frame-00002.bmp")), None);
        assert_eq!(parse_frame_path(Path::new("cam-1_frame-00002")), None);
        assert_eq!(parse_frame_path(Path::new("metadata.json")), None);
    }

    #[test]
    fn every_recorded_format_is_recognised() {
        // Guards against a new FrameFormat variant whose files replay cannot find.
        for fmt in [crate::FrameFormat::Tiff, crate::FrameFormat::Png, crate::FrameFormat::Jpeg] {
            let name = frame_filename(0, 1, fmt.extension());
            assert_eq!(
                parse_frame_path(Path::new(&name)),
                Some((0, 1)),
                "{} files would be invisible", fmt.extension()
            );
        }
    }

    #[test]
    fn parse_frame_filename_rejects_garbage() {
        assert_eq!(parse_frame_filename("not-a-frame-file"), None);
        assert_eq!(parse_frame_filename("cam-x_frame-00001"), None);
        assert_eq!(parse_frame_filename("cam-0_frame-x"), None);
    }
}
