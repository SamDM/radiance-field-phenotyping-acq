use std::path::{Path, PathBuf};

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

pub(super) fn write_metadata(folder: &Path, lr: &RecordingEntry) {
    let meta = RecordingMetadata {
        ok: lr.ok,
        rating: lr.rating,
        comment: lr.comment.clone(),
        elapsed_seconds: Some(lr.elapsed.as_seconds_f64()),
        groups_saved: Some(lr.groups_saved),
        finished_at: lr.finished_at,
    };
    let path = folder.join("metadata.json");
    match serde_json::to_string_pretty(&meta) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!(path = %path.display(), error = %e, "failed to write metadata.json");
            }
        }
        Err(e) => tracing::warn!(error = %e, "failed to serialize metadata"),
    }
}

/// A recording as known to the server, with its real on-disk path. The path is only
/// ever populated by the server's own directory scan or its own `finish_recording`
/// logic — it is never reconstructed from a client-supplied string. Client commands
/// address recordings by `name` (see `RecordingLibrary::find_by_name`), and the
/// resolved `path` here is the single point where that name becomes a filesystem path.
pub(super) struct RecordingEntry {
    pub path: PathBuf,
    pub name: String,
    pub ok: bool,
    pub elapsed: time::Duration,
    pub groups_saved: u32,
    pub finished_at: Option<OffsetDateTime>,
    pub rating: Option<u8>,
    pub comment: Option<String>,
}

impl RecordingEntry {
    fn to_wire(&self) -> wire::PastRecording {
        wire::PastRecording {
            name: self.name.clone(),
            ok: self.ok,
            elapsed: self.elapsed,
            groups_saved: self.groups_saved,
            finished_at: self.finished_at,
            rating: self.rating,
            comment: self.comment.clone(),
        }
    }
}

pub(super) struct RecordingLibrary {
    recordings: Vec<RecordingEntry>,
}

impl RecordingLibrary {
    pub fn new(output_dir_template: &str) -> Self {
        let recordings = scan_all_recordings(output_dir_template);
        info!(count = recordings.len(), "recording library loaded");
        RecordingLibrary { recordings }
    }

    pub fn wire_recordings(&self) -> Vec<wire::PastRecording> {
        self.recordings.iter().map(RecordingEntry::to_wire).collect()
    }

    pub fn push(&mut self, lr: RecordingEntry) {
        self.recordings.insert(0, lr);
    }

    /// The single, centralized, trusted resolution point: client commands identify a
    /// recording by `name`; this looks it up against recordings the server itself
    /// discovered (via scan or its own recording lifecycle), never against a
    /// client-supplied path.
    pub fn find_by_name(&self, name: &str) -> Option<&RecordingEntry> {
        self.recordings.iter().find(|r| r.name == name)
    }

    pub fn find_by_name_mut(&mut self, name: &str) -> Option<&mut RecordingEntry> {
        self.recordings.iter_mut().find(|r| r.name == name)
    }

    pub fn remove(&mut self, name: &str) {
        self.recordings.retain(|r| r.name != name);
    }
}

pub(super) fn base_dir_from_template(dir_template: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let expanded = dir_template.replace("$HOME", &home);
    let base = match expanded.find('{') {
        Some(i) => &expanded[..i],
        None => &expanded,
    };
    base.trim_end_matches('/').to_string()
}

/// Recursively walk `base_dir` for recording folders (any directory containing a
/// `metadata.json`), so the scan stays correct regardless of how many directory
/// levels `--output-dir` produces. A recording folder is treated as a leaf — the
/// walk does not descend into it. Recursion depth is capped defensively.
const MAX_SCAN_DEPTH: u32 = 8;

fn scan_all_recordings(output_dir_template: &str) -> Vec<RecordingEntry> {
    let base_dir = base_dir_from_template(output_dir_template);
    let base = Path::new(&base_dir);
    if !base.is_dir() {
        return Vec::new();
    }

    let mut recordings = Vec::new();
    walk_for_recordings(base, &base_dir, MAX_SCAN_DEPTH, &mut recordings);

    // Newest first: sort by folder name (date/time-derived names sort lexicographically).
    recordings.sort_by(|a, b| b.name.cmp(&a.name));
    recordings
}

fn walk_for_recordings(dir: &Path, base_dir: &str, depth_remaining: u32, out: &mut Vec<RecordingEntry>) {
    if depth_remaining == 0 {
        return;
    }

    if let Some(meta) = read_metadata(dir) {
        let (Some(elapsed_seconds), Some(groups_saved), Some(finished_at)) =
            (meta.elapsed_seconds, meta.groups_saved, meta.finished_at)
        else {
            return;
        };
        let folder_str = dir.to_string_lossy().into_owned();
        let name = acquisition_common::recording_display_name(&folder_str, base_dir);
        out.push(RecordingEntry {
            path: dir.to_path_buf(),
            name,
            ok: meta.ok,
            elapsed: time::Duration::seconds_f64(elapsed_seconds),
            groups_saved,
            finished_at: Some(finished_at),
            rating: meta.rating,
            comment: meta.comment,
        });
        return;
    }

    let mut subdirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().ok().is_some_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    subdirs.sort();

    for subdir in subdirs {
        walk_for_recordings(&subdir, base_dir, depth_remaining - 1, out);
    }
}
