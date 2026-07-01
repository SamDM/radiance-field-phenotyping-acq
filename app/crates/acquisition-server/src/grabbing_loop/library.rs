use std::path::Path;

use acquisition_common;
use acquisition_common::protocol as wire;
use time::OffsetDateTime;
use tracing::info;

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub(super) struct RecordingMetadata {
    pub ok: bool,
    #[serde(default)]
    pub rating: Option<u8>,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub elapsed_seconds: Option<f64>,
    #[serde(default)]
    pub groups_saved: Option<u32>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
}

pub(super) fn read_metadata(folder: &Path) -> Option<RecordingMetadata> {
    let path = folder.join("metadata.json");
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

pub(super) fn write_metadata(folder: &str, lr: &wire::PastRecording) {
    let meta = RecordingMetadata {
        ok: lr.ok,
        rating: lr.rating,
        comment: lr.comment.clone(),
        elapsed_seconds: Some(lr.elapsed.as_seconds_f64()),
        groups_saved: Some(lr.groups_saved),
        finished_at: lr.finished_at,
    };
    let path = Path::new(folder).join("metadata.json");
    match serde_json::to_string_pretty(&meta) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!(path = %path.display(), error = %e, "failed to write metadata.json");
            }
        }
        Err(e) => tracing::warn!(error = %e, "failed to serialize metadata"),
    }
}

pub(super) struct RecordingLibrary {
    recordings: Vec<wire::PastRecording>,
}

impl RecordingLibrary {
    pub fn new(output_path_template: &str) -> Self {
        let recordings = scan_all_recordings(output_path_template);
        info!(count = recordings.len(), "recording library loaded");
        RecordingLibrary { recordings }
    }

    pub fn recordings(&self) -> &[wire::PastRecording] {
        &self.recordings
    }

    pub fn push(&mut self, lr: wire::PastRecording) {
        self.recordings.insert(0, lr);
    }

    pub fn find_mut(&mut self, folder: &str) -> Option<&mut wire::PastRecording> {
        self.recordings.iter_mut().find(|r| r.folder == folder)
    }

    pub fn remove(&mut self, folder: &str) {
        self.recordings.retain(|r| r.folder != folder);
    }
}

pub(super) fn base_dir_from_template(template: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let expanded = template.replace("$HOME", &home);
    let base = match expanded.find('{') {
        Some(i) => &expanded[..i],
        None => &expanded,
    };
    base.trim_end_matches('/').to_string()
}

fn scan_all_recordings(output_path_template: &str) -> Vec<wire::PastRecording> {
    let base_dir = base_dir_from_template(output_path_template);
    let base = Path::new(&base_dir);
    if !base.is_dir() {
        return Vec::new();
    }

    let sorted_dirs = |dir: &Path| -> Vec<std::ffi::OsString> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().ok().is_some_and(|t| t.is_dir()))
            .map(|e| e.file_name())
            .collect();
        entries.sort();
        entries
    };

    let mut recordings = Vec::new();

    for date_dir in sorted_dirs(base) {
        let date_path = base.join(&date_dir);
        for time_dir in sorted_dirs(&date_path) {
            let folder = date_path.join(&time_dir);
            if let Some(meta) = read_metadata(&folder) {
                let elapsed_seconds = match meta.elapsed_seconds {
                    Some(s) => s,
                    None => continue,
                };
                let groups_saved = match meta.groups_saved {
                    Some(g) => g,
                    None => continue,
                };
                let finished_at = match meta.finished_at {
                    Some(t) => t,
                    None => continue,
                };
                let folder_str = folder.to_string_lossy().into_owned();
                let name = acquisition_common::recording_display_name(&folder_str, &base_dir);
                recordings.push(wire::PastRecording {
                    folder: folder_str,
                    name,
                    ok: meta.ok,
                    elapsed: time::Duration::seconds_f64(elapsed_seconds),
                    groups_saved,
                    finished_at: Some(finished_at),
                    rating: meta.rating,
                    comment: meta.comment,
                });
            }
        }
    }

    // Reverse so newest is first (dirs are sorted lexicographically ascending)
    recordings.reverse();
    recordings
}
