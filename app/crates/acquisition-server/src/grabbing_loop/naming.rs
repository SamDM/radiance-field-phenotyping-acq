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
    fn parse_frame_filename_rejects_garbage() {
        assert_eq!(parse_frame_filename("not-a-frame-file"), None);
        assert_eq!(parse_frame_filename("cam-x_frame-00001"), None);
        assert_eq!(parse_frame_filename("cam-0_frame-x"), None);
    }
}
