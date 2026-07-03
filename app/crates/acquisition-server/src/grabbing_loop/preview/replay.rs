use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use acquisition_common::AnyFrame;
use acquisition_common::protocol as wire;
use tokio::sync::broadcast;
use tracing::{info, warn};

use super::super::naming::parse_frame_filename;

pub(super) struct ReplayIterState {
    name: String,
    /// Frame groups sorted by frame_idx: (frame_idx, per-camera paths).
    frame_groups: Vec<(u32, Vec<PathBuf>)>,
    /// Camera timestamp (ns) of each group, used for playback timing.
    group_timestamps_ns: Vec<u64>,
    /// Camera timestamp of the first group — subtracted to get relative offsets.
    first_ts_ns: u64,
    /// Which group to emit next.
    current_idx: usize,
    /// When the current loop through the recording started.
    replay_started_at: Instant,
    paused: bool,
    /// Wall-clock time when the most recent pause began.
    paused_at: Option<Instant>,
}

impl ReplayIterState {
    /// Scan `path` (a recording directory resolved by the caller via the recording
    /// library — never a raw client-supplied string) for frame files and build
    /// playback state. Returns `Err` if no frames are found.
    pub(super) fn new(path: &Path, name: &str) -> Result<Self, String> {
        let dir = std::fs::read_dir(path)
            .map_err(|e| format!("cannot read folder '{}': {e}", path.display()))?;

        let mut entries: Vec<(usize, u32, PathBuf)> = Vec::new();
        for entry in dir.flatten() {
            let entry_path = entry.path();
            let ext = entry_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "png" && ext != "tiff" && ext != "tif" && ext != "jpg" && ext != "jpeg" {
                continue;
            }
            let stem = match entry_path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_owned(),
                None => continue,
            };
            if let Some((cam_idx, frame_idx)) = parse_frame_filename(&stem) {
                entries.push((cam_idx, frame_idx, entry_path));
            }
        }

        if entries.is_empty() {
            return Err(format!("no frames found in '{}'", path.display()));
        }

        let mut by_frame: std::collections::BTreeMap<u32, Vec<(usize, PathBuf)>> =
            std::collections::BTreeMap::new();
        for (cam_idx, frame_idx, path) in entries {
            by_frame.entry(frame_idx).or_default().push((cam_idx, path));
        }

        let mut frame_groups: Vec<(u32, Vec<PathBuf>)> = by_frame
            .into_iter()
            .map(|(frame_idx, mut cams)| {
                cams.sort_by_key(|(c, _)| *c);
                (frame_idx, cams.into_iter().map(|(_, p)| p).collect())
            })
            .collect();
        frame_groups.sort_by_key(|(idx, _)| *idx);

        let mut group_timestamps_ns: Vec<u64> = Vec::with_capacity(frame_groups.len());
        for (_, paths) in &frame_groups {
            let ts = read_timestamp_ns(&paths[0]).unwrap_or(0);
            group_timestamps_ns.push(ts);
        }

        let first_ts_ns = group_timestamps_ns[0];

        let last_idx = frame_groups.len() - 1;

        info!(
            path = %path.display(),
            total_groups = frame_groups.len(),
            "replay loaded"
        );

        Ok(ReplayIterState {
            name: name.to_owned(),
            frame_groups,
            group_timestamps_ns,
            first_ts_ns,
            current_idx: last_idx,
            replay_started_at: Instant::now(),
            paused: false,
            paused_at: None,
        })
    }

    pub(super) fn pause(&mut self) {
        if !self.paused {
            self.paused = true;
            self.paused_at = Some(Instant::now());
        }
    }

    pub(super) fn resume(&mut self) {
        if self.paused {
            if let Some(paused_at) = self.paused_at.take() {
                self.replay_started_at += paused_at.elapsed();
            }
            self.paused = false;
        }
    }

    pub(super) fn rewind(&mut self) {
        self.current_idx = 0;
        if !self.paused {
            self.replay_started_at = Instant::now();
        } else {
            // Stay paused at frame 0; adjust timing so resume works correctly.
            self.replay_started_at = Instant::now();
            self.paused_at = Some(Instant::now());
        }
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn is_paused(&self) -> bool {
        self.paused
    }

    pub(super) fn current_frame_paths(&self) -> &[PathBuf] {
        &self.frame_groups[self.current_idx].1
    }

    /// Auto-pauses if needed, steps forward one frame, returns paths for the new frame.
    pub(super) fn step_forward(&mut self) -> &[PathBuf] {
        self.pause();
        self.current_idx = (self.current_idx + 1) % self.frame_groups.len();
        self.sync_timing_to_current();
        &self.frame_groups[self.current_idx].1
    }

    /// Auto-pauses if needed, steps backward one frame, returns paths for the new frame.
    pub(super) fn step_backward(&mut self) -> &[PathBuf] {
        self.pause();
        self.current_idx = self.current_idx
            .checked_sub(1)
            .unwrap_or(self.frame_groups.len() - 1);
        self.sync_timing_to_current();
        &self.frame_groups[self.current_idx].1
    }

    pub(super) fn seek_to_frame(&mut self, frame_idx: u32) -> &[PathBuf] {
        self.current_idx = self.frame_groups
            .partition_point(|(idx, _)| *idx < frame_idx)
            .min(self.frame_groups.len() - 1);
        self.sync_timing_to_current_preserving_pause();
        &self.frame_groups[self.current_idx].1
    }

    /// Adjust replay_started_at so that resuming from current_idx plays at the right time.
    /// Used by step commands which also auto-pause.
    fn sync_timing_to_current(&mut self) {
        let offset_ns = self.group_timestamps_ns[self.current_idx].saturating_sub(self.first_ts_ns);
        self.replay_started_at = Instant::now() - Duration::from_nanos(offset_ns);
        // Reset paused_at so the pause duration doesn't drift.
        self.paused_at = Some(Instant::now());
    }

    /// Like sync_timing_to_current but preserves the current pause state.
    /// Used by seek, which must not change whether playback is running or paused.
    fn sync_timing_to_current_preserving_pause(&mut self) {
        let offset_ns = self.group_timestamps_ns[self.current_idx].saturating_sub(self.first_ts_ns);
        self.replay_started_at = Instant::now() - Duration::from_nanos(offset_ns);
        if self.paused {
            self.paused_at = Some(Instant::now());
        }
    }

    pub(super) fn wire_activity(&self) -> wire::Activity {
        let frame_idx = self.frame_groups[self.current_idx].0;
        let elapsed_ns = self.group_timestamps_ns[self.current_idx]
            .saturating_sub(self.first_ts_ns);
        let total_ns = self.group_timestamps_ns
            .last()
            .copied()
            .unwrap_or(self.first_ts_ns)
            .saturating_sub(self.first_ts_ns);
        wire::Activity::Replaying {
            name: self.name.clone(),
            frame_idx,
            total_frames: self.frame_groups.len() as u32,
            elapsed_ns,
            total_ns,
            paused: self.paused,
        }
    }
}

/// Load and emit one frame group (used by step commands).
pub(super) fn emit_frame(
    paths: &[PathBuf],
    zoom_requests: &HashMap<String, (f32, f32)>,
    preview_tx: &broadcast::Sender<Vec<wire::PreviewFrame>>,
) {
    let frames: Vec<AnyFrame> = paths
        .iter()
        .filter_map(|p| match AnyFrame::load(p) {
            Ok(f) => Some(f),
            Err(e) => {
                warn!(path = %p.display(), error = %e, "replay: failed to load frame");
                None
            }
        })
        .collect();
    if !frames.is_empty() {
        let previews = super::recordings::make_jpeg_previews(&frames, zoom_requests);
        let _ = preview_tx.send(previews);
    }
}

/// Emit the next replay frame if it is due based on original capture timing.
/// Called every loop iteration; returns quickly when no frame is due yet.
pub(super) fn iteration(
    state: &mut ReplayIterState,
    zoom_requests: &HashMap<String, (f32, f32)>,
    preview_tx: &broadcast::Sender<Vec<wire::PreviewFrame>>,
) {
    if state.paused || preview_tx.receiver_count() == 0 {
        return;
    }

    // current_idx is the last-emitted frame; check whether the next frame is due.
    let next_idx = (state.current_idx + 1) % state.frame_groups.len();
    let frame_due_ns =
        state.group_timestamps_ns[next_idx].saturating_sub(state.first_ts_ns);
    let elapsed_ns = state.replay_started_at.elapsed().as_nanos() as u64;

    if elapsed_ns < frame_due_ns {
        return;
    }

    let paths = &state.frame_groups[next_idx].1;
    let frames: Vec<AnyFrame> = paths
        .iter()
        .filter_map(|p| match AnyFrame::load(p) {
            Ok(f) => Some(f),
            Err(e) => {
                warn!(path = %p.display(), error = %e, "replay: failed to load frame");
                None
            }
        })
        .collect();

    if !frames.is_empty() {
        let previews = super::recordings::make_jpeg_previews(&frames, zoom_requests);
        let _ = preview_tx.send(previews);
    }

    state.current_idx = next_idx;
    if state.current_idx == 0 {
        // Wrapped around to the first frame — reset timing for the new loop.
        state.replay_started_at = Instant::now();
    }
}

#[derive(serde::Deserialize)]
struct TimestampPeek {
    camera_timestamp_ns: u64,
}

fn read_timestamp_ns(image_path: &PathBuf) -> Option<u64> {
    let json = std::fs::read_to_string(image_path.with_extension("json")).ok()?;
    let peek: TimestampPeek = serde_json::from_str(&json).ok()?;
    Some(peek.camera_timestamp_ns)
}
