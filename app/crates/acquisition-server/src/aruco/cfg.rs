//! Configuration for the auto-record watcher.
//!
//! Deliberately free of `arucotools` types so it exists with or without the feature, and
//! so the CLI layer never has to know about dictionaries. Family names are resolved to
//! real marker families inside the detector, at construction.

use std::time::Duration;

/// Which Bayer-to-grayscale reduction to feed the detector.
///
/// `Green` is the default for two measured reasons (see `examples/aruco_bench.rs`). It is
/// ~20× cheaper — 0.3 ms against 6 ms on the rig's sensor — and that cost lands on the
/// *acquisition* thread, while detection itself runs on the worker; box mode therefore
/// spends more on the thread that matters than the whole detection does on the one that
/// does not. And below roughly 14 px of marker it also detects *better*, because at
/// extreme downscale averaging blurs adjacent modules into each other.
///
/// `Box` remains worth trying for noisy low-light scenes, where averaging suppresses shot
/// noise, or if aliasing turns out to cost detections on real imagery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum DownscaleMode {
    /// One green site per quad. One byte read per output pixel; aliases hard edges.
    #[default]
    Green,
    /// Average every green site in each block. Antialiases, at ~`(w*h)/2` reads.
    Box,
}

/// One `family: id, id, …` group from `--aruco-markers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerSelector {
    /// A predefined family name, e.g. `DICT_5X5_250`.
    pub family: String,
    /// Marker IDs within that family that should trigger a recording.
    pub ids: Vec<usize>,
}

/// Everything the watcher needs. An empty `markers` disables it entirely — no detector
/// thread is spawned.
#[derive(Debug, Clone)]
pub struct AutoRecordCfg {
    pub markers: Vec<MarkerSelector>,
    /// Cameras to watch, in round-robin order. `None` watches every camera.
    pub cameras: Option<Vec<usize>>,
    pub interval: Duration,
    /// Quad stride for the Bayer downscale; detector input is `w/(2k)` × `h/(2k)`.
    pub downscale: u32,
    pub downscale_mode: DownscaleMode,
    /// `K` of the K-of-N start hysteresis, counted per camera.
    pub confirmations: u8,
    /// `N` of the K-of-N start hysteresis.
    pub window: u8,
    /// Consecutive round-robin passes with no sighting on any camera before stopping.
    pub stop_after: u32,
    pub cooldown: Duration,
    /// Single adaptive-threshold window size, pinned to avoid the detector's 3-pass sweep.
    pub thresh_win: u32,
    pub min_perimeter_rate: f64,
}

impl Default for AutoRecordCfg {
    fn default() -> Self {
        AutoRecordCfg {
            markers: Vec::new(),
            cameras: None,
            interval: super::DEFAULT_INTERVAL,
            downscale: super::DEFAULT_DOWNSCALE,
            downscale_mode: DownscaleMode::Green,
            confirmations: super::DEFAULT_CONFIRMATIONS,
            window: super::DEFAULT_WINDOW,
            stop_after: super::DEFAULT_STOP_AFTER,
            cooldown: super::DEFAULT_COOLDOWN,
            thresh_win: super::DEFAULT_THRESH_WIN,
            min_perimeter_rate: super::DEFAULT_MIN_PERIMETER_RATE,
        }
    }
}

impl AutoRecordCfg {
    /// Whether anything is configured to trigger on.
    pub fn is_enabled(&self) -> bool {
        self.markers.iter().any(|m| !m.ids.is_empty())
    }

    /// Camera indices to round-robin over, given how many cameras exist.
    pub fn camera_list(&self, n_cameras: usize) -> Vec<usize> {
        match &self.cameras {
            Some(cams) => cams.iter().copied().filter(|&c| c < n_cameras).collect(),
            None => (0..n_cameras).collect(),
        }
    }
}

/// Everything wrong a `--aruco-markers` value can be.
#[derive(Debug)]
pub enum MarkerCfgError {
    UnknownFamily {
        name: String,
        known: Vec<&'static str>,
    },
    IdOutOfRange {
        family: &'static str,
        id: usize,
        n_codes: usize,
    },
}

impl std::fmt::Display for MarkerCfgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MarkerCfgError::UnknownFamily { name, known } => {
                write!(f, "unknown marker family `{name}`; known families: {}", known.join(", "))
            }
            MarkerCfgError::IdOutOfRange { family, id, n_codes } => write!(
                f,
                "marker id {id} is out of range for {family}, which has {n_codes} markers (0..{})",
                n_codes - 1
            ),
        }
    }
}

impl std::error::Error for MarkerCfgError {}
