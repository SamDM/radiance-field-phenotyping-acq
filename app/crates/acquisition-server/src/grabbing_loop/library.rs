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
    /// Absent in recordings written before this field existed; the scan then falls back
    /// to counting the cameras that left frames behind.
    #[serde(default)]
    pub n_cameras: Option<usize>,
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
        n_cameras: Some(lr.n_cameras),
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
    pub n_cameras: usize,
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
    /// Which entry `recheck_next` looks at next.
    recheck_cursor: usize,
}

impl RecordingLibrary {
    pub fn new(output_dir_template: &str) -> Self {
        let recordings = scan_all_recordings(output_dir_template);
        info!(count = recordings.len(), "recording library loaded");
        RecordingLibrary { recordings, recheck_cursor: 0 }
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

    /// Re-check the next recording against the disk, advancing the sweep by one.
    ///
    /// The library is built once at startup, but recordings can disappear underneath it
    /// while the server runs — an offload job moving finished recordings to a network
    /// drive is the expected case. Without re-checking, the client goes on listing
    /// recordings whose frames are gone and offering replays that cannot load.
    ///
    /// One recording per call rather than a full re-scan, because this runs on the
    /// acquisition thread: a sweep of a day's recordings is a latency spike on the same
    /// thread that drives `retrieve_result`, and that thread has no slack. One `readdir`
    /// per call is not worth measuring, and the whole library is still revalidated in as
    /// many calls as it has entries.
    ///
    /// Verdicts are not written back to `metadata.json`. The startup scan recomputes
    /// them anyway, and writing to disk here would put the very latency this avoids
    /// straight back on the thread.
    pub fn recheck_next(&mut self) {
        if self.recordings.is_empty() {
            return;
        }
        let idx = self.recheck_cursor % self.recordings.len();
        let entry = &self.recordings[idx];
        let inventory = inventory_frames(&entry.path);

        if inventory.n_frames == 0 {
            info!(name = %entry.name, "recording frames are gone, removing from library");
            self.recordings.remove(idx);
            // Leave the cursor where it is: everything after the removed entry has
            // shifted down by one, so this slot now holds the recording that would
            // otherwise be skipped.
            self.recheck_cursor = idx;
            return;
        }
        self.recheck_cursor = idx + 1;

        // `ok` only ever goes false: a recording that failed verification when it was
        // made is not redeemed by a later look, and one whose frames went missing has
        // already lost the frames.
        if entry.ok {
            if let Some(problem) =
                describe_discrepancy(&inventory, entry.groups_saved, entry.n_cameras)
            {
                info!(name = %entry.name, problem = %problem, "recording no longer matches its metadata");
                self.recordings[idx].ok = false;
            }
        }
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

/// What a recording folder actually holds, as opposed to what its metadata claims.
///
/// `metadata.json` is written once, when the recording finishes, and nothing rewrites it
/// if the frames later go away. Anything that removes frames behind the server's back —
/// an offload job moving them to a network drive, a half-finished transfer, a manual
/// `rm` — therefore leaves metadata describing frames that are not there. Counting what
/// is on disk is what keeps the library honest about it.
#[derive(Debug, Default, PartialEq, Eq)]
struct FrameInventory {
    /// Files recognised as frames by [`super::naming::parse_frame_path`] — the same
    /// predicate replay uses, so the two cannot disagree about what is in a folder.
    n_frames: usize,
    /// How many distinct cameras contributed.
    n_cameras: usize,
    /// Highest frame index seen, plus one.
    n_groups: u32,
}

/// Describe how a folder's contents fail to match the recording it should hold, or
/// `None` if they match.
///
/// The single definition of "this recording is intact on disk", used both when a
/// recording finishes and when the library is scanned at startup. Those two had
/// separate implementations once and the difference was invisible: the finish-time
/// check sampled only the first and last frame of each camera, so a recording with a
/// hole in the middle passed. Keeping one function means the verdict cannot depend on
/// which side of a restart you ask from.
fn describe_discrepancy(inv: &FrameInventory, groups_saved: u32, n_cameras: usize) -> Option<String> {
    let expected = groups_saved as usize * n_cameras;
    if inv.n_cameras != n_cameras {
        return Some(format!(
            "expected frames from {n_cameras} cameras, found {}",
            inv.n_cameras
        ));
    }
    if inv.n_groups != groups_saved {
        return Some(format!(
            "expected {groups_saved} frame groups, found {}",
            inv.n_groups
        ));
    }
    if inv.n_frames != expected {
        return Some(format!(
            "expected {expected} frames ({groups_saved} groups x {n_cameras} cameras), found {}",
            inv.n_frames
        ));
    }
    None
}

/// [`describe_discrepancy`] against a folder read from disk.
pub(super) fn frame_discrepancy(dir: &Path, groups_saved: u32, n_cameras: usize) -> Option<String> {
    describe_discrepancy(&inventory_frames(dir), groups_saved, n_cameras)
}

fn inventory_frames(dir: &Path) -> FrameInventory {
    let mut cameras = std::collections::BTreeSet::new();
    let mut n_frames = 0usize;
    let mut max_idx: Option<u32> = None;

    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Some((cam_idx, frame_idx)) = super::naming::parse_frame_path(&entry.path()) else {
            continue;
        };
        cameras.insert(cam_idx);
        n_frames += 1;
        max_idx = Some(max_idx.map_or(frame_idx, |m: u32| m.max(frame_idx)));
    }

    FrameInventory {
        n_frames,
        n_cameras: cameras.len(),
        n_groups: max_idx.map_or(0, |m| m + 1),
    }
}

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
        // A folder whose frames have gone is not a recording any more, whatever its
        // metadata still says. Dropping it here is what keeps offloaded recordings out
        // of the client's list instead of offering replays that cannot load.
        let inventory = inventory_frames(dir);
        if inventory.n_frames == 0 {
            info!(path = %dir.display(), "recording has no frames on disk, omitting from library");
            return;
        }

        // Frames can go missing after the fact — most plausibly an offload interrupted
        // part-way. `ok` already means "frames verified present", so downgrading it here
        // reports the same thing it always did, just later.
        let n_cameras = meta.n_cameras.unwrap_or(inventory.n_cameras);
        let discrepancy = describe_discrepancy(&inventory, groups_saved, n_cameras);
        if let Some(ref problem) = discrepancy {
            info!(
                path = %dir.display(),
                problem = %problem,
                "recording no longer matches its metadata, marking not-ok"
            );
        }

        let folder_str = dir.to_string_lossy().into_owned();
        let name = acquisition_common::recording_display_name(&folder_str, base_dir);
        out.push(RecordingEntry {
            path: dir.to_path_buf(),
            name,
            ok: meta.ok && discrepancy.is_none(),
            elapsed: time::Duration::seconds_f64(elapsed_seconds),
            groups_saved,
            n_cameras,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a recording folder: `groups` groups across `cameras` cameras, minus any
    /// frames in `missing` (as `(cam_idx, frame_idx)`).
    fn recording_dir(
        base: &Path,
        name: &str,
        groups: u32,
        cameras: usize,
        missing: &[(usize, u32)],
    ) -> PathBuf {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for cam in 0..cameras {
            for g in 0..groups {
                if missing.contains(&(cam, g)) {
                    continue;
                }
                let stem = format!("cam-{cam}_frame-{g:05}");
                std::fs::write(dir.join(format!("{stem}.png")), b"x").unwrap();
                // The per-frame sidecar the recorder writes alongside every frame.
                std::fs::write(dir.join(format!("{stem}.json")), b"{}").unwrap();
            }
        }
        std::fs::write(
            dir.join("metadata.json"),
            format!(
                r#"{{"ok":true,"elapsed_seconds":1.0,"groups_saved":{groups},
                     "n_cameras":{cameras},"finished_at":"2026-09-16T12:00:00Z"}}"#
            ),
        )
        .unwrap();
        dir
    }

    #[test]
    fn sidecar_json_files_are_not_counted_as_frames() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = recording_dir(tmp.path(), "r", 3, 2, &[]);
        assert_eq!(
            inventory_frames(&dir),
            FrameInventory { n_frames: 6, n_cameras: 2, n_groups: 3 }
        );
    }

    #[test]
    fn an_intact_recording_is_listed_and_stays_ok() {
        let tmp = tempfile::tempdir().unwrap();
        recording_dir(tmp.path(), "r", 3, 2, &[]);
        let mut out = Vec::new();
        walk_for_recordings(tmp.path(), &tmp.path().to_string_lossy(), 8, &mut out);
        assert_eq!(out.len(), 1);
        assert!(out[0].ok);
        assert_eq!(out[0].groups_saved, 3);
    }

    /// The offload case: frames moved to the network drive, metadata left behind.
    #[test]
    fn a_recording_whose_frames_are_gone_is_omitted() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = recording_dir(tmp.path(), "r", 3, 2, &[]);
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            if e.file_name() != "metadata.json" {
                std::fs::remove_file(e.path()).unwrap();
            }
        }
        let mut out = Vec::new();
        walk_for_recordings(tmp.path(), &tmp.path().to_string_lossy(), 8, &mut out);
        assert!(out.is_empty(), "a frameless folder must not be listed");
    }

    /// An interrupted transfer: some frames moved, the rest still here. It is still a
    /// recording, but not an intact one, so it is listed and flagged.
    #[test]
    fn a_partially_offloaded_recording_is_listed_but_not_ok() {
        let tmp = tempfile::tempdir().unwrap();
        recording_dir(tmp.path(), "r", 4, 2, &[(0, 1), (1, 1)]);
        let mut out = Vec::new();
        walk_for_recordings(tmp.path(), &tmp.path().to_string_lossy(), 8, &mut out);
        assert_eq!(out.len(), 1);
        assert!(!out[0].ok, "missing frames must clear the verified flag");
    }

    /// Losing the last group shortens the run rather than punching a hole in it, so the
    /// frame count alone still adds up — only the group count reveals it.
    #[test]
    fn a_truncated_recording_is_caught_by_the_group_count() {
        let tmp = tempfile::tempdir().unwrap();
        recording_dir(tmp.path(), "r", 4, 2, &[(0, 3), (1, 3)]);
        let mut out = Vec::new();
        walk_for_recordings(tmp.path(), &tmp.path().to_string_lossy(), 8, &mut out);
        assert_eq!(out.len(), 1);
        assert!(!out[0].ok);
    }

    /// A hole in the middle is the case the old finish-time check missed: it sampled
    /// only the first and last frame of each camera, so this passed as verified.
    #[test]
    fn a_hole_in_the_middle_is_caught() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = recording_dir(tmp.path(), "r", 5, 2, &[(0, 2)]);
        assert!(frame_discrepancy(&dir, 5, 2).is_some());
    }

    /// The scan and the finish-time path must reach the same verdict, or a recording
    /// would change status across a restart.
    #[test]
    fn the_scan_and_the_finish_time_check_agree() {
        let tmp = tempfile::tempdir().unwrap();
        for (name, groups, cams, missing) in [
            ("intact", 4u32, 2usize, &[][..]),
            ("hole", 4, 2, &[(0, 2)][..]),
            ("truncated", 4, 2, &[(0, 3), (1, 3)][..]),
            ("one_camera_gone", 4, 2, &[(1, 0), (1, 1), (1, 2), (1, 3)][..]),
        ] {
            let dir = recording_dir(tmp.path(), name, groups, cams, missing);
            let finish_says = frame_discrepancy(&dir, groups, cams).is_none();

            let mut out = Vec::new();
            walk_for_recordings(&dir, &tmp.path().to_string_lossy(), 8, &mut out);
            let scan_says = out.first().map(|r| r.ok).unwrap_or(false);

            assert_eq!(finish_says, scan_says, "{name}: finish and scan disagree");
        }
    }

    /// Strip the frames from a recording folder, leaving its metadata — what an offload
    /// job leaves behind.
    fn strip_frames(dir: &Path) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            if e.file_name() != "metadata.json" {
                std::fs::remove_file(e.path()).unwrap();
            }
        }
    }

    fn names(lib: &RecordingLibrary) -> Vec<String> {
        lib.wire_recordings().into_iter().map(|r| r.name).collect()
    }

    #[test]
    fn rechecking_drops_a_recording_whose_frames_went_away() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = recording_dir(tmp.path(), "a", 2, 2, &[]);
        let mut lib = RecordingLibrary::new(&tmp.path().to_string_lossy());
        assert_eq!(names(&lib).len(), 1);

        strip_frames(&dir);
        lib.recheck_next();
        assert!(names(&lib).is_empty(), "offloaded recording should be gone");
    }

    /// The cursor must not step past the entry that shifts down into a removed slot, or
    /// the sweep silently skips a recording every time it removes one.
    #[test]
    fn a_sweep_visits_every_recording_even_across_removals() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["a", "b", "c", "d"] {
            recording_dir(tmp.path(), name, 2, 2, &[]);
        }
        let mut lib = RecordingLibrary::new(&tmp.path().to_string_lossy());
        assert_eq!(names(&lib), vec!["d", "c", "b", "a"], "newest first");

        // Empty the two in the middle, so a skip would leave one of them behind.
        strip_frames(&tmp.path().join("b"));
        strip_frames(&tmp.path().join("c"));

        for _ in 0..4 {
            lib.recheck_next();
        }
        assert_eq!(names(&lib), vec!["d", "a"]);
    }

    #[test]
    fn rechecking_clears_ok_when_frames_go_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = recording_dir(tmp.path(), "a", 4, 2, &[]);
        let mut lib = RecordingLibrary::new(&tmp.path().to_string_lossy());
        assert!(lib.wire_recordings()[0].ok);

        std::fs::remove_file(dir.join("cam-0_frame-00002.png")).unwrap();
        lib.recheck_next();
        let rec = &lib.wire_recordings()[0];
        assert!(!rec.ok, "a recording missing frames must not still read as verified");
        assert_eq!(rec.name, "a", "it stays listed, it is just not intact");
    }

    #[test]
    fn rechecking_an_empty_library_does_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut lib = RecordingLibrary::new(&tmp.path().to_string_lossy());
        lib.recheck_next();
        assert!(names(&lib).is_empty());
    }

    #[test]
    fn rechecking_leaves_an_intact_recording_alone() {
        let tmp = tempfile::tempdir().unwrap();
        recording_dir(tmp.path(), "a", 3, 2, &[]);
        let mut lib = RecordingLibrary::new(&tmp.path().to_string_lossy());
        for _ in 0..5 {
            lib.recheck_next();
        }
        assert_eq!(names(&lib), vec!["a"]);
        assert!(lib.wire_recordings()[0].ok);
    }

    #[test]
    fn a_folder_with_no_metadata_is_not_a_recording() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("stray");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cam-0_frame-00000.png"), b"x").unwrap();
        let mut out = Vec::new();
        walk_for_recordings(tmp.path(), &tmp.path().to_string_lossy(), 8, &mut out);
        assert!(out.is_empty());
    }
}
