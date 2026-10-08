use acquisition_server::aruco;
use clap::Parser;
use tracing_subscriber::fmt::format::FmtSpan;

mod grabbing_loop;
mod plumbing;
mod utils;

const DEFAULT_FRAME_COUNT: u32 = 100;
const DEFAULT_FRAMERATE: f64 = 2.0;
const DEFAULT_GROUPER_BUFFER_LEN: usize = 10;
const DEFAULT_PTP_POLL_INTERVAL_SECS: u64 = 1;
const DEFAULT_EVENT_BROADCAST_CAPACITY: usize = 32;
const DEFAULT_PREVIEW_BROADCAST_CAPACITY: usize = 2;
const DEFAULT_HEARTBEAT_INTERVAL_SECS: u64 = 5;
const DEFAULT_SEND_TIMEOUT_SECS: u64 = 2;
const DEFAULT_CONTROL_ADDR: &str = "127.0.0.1:8081";
const DEFAULT_PREVIEW_ADDR: &str = "127.0.0.1:8082";
const DEFAULT_AUTO_ADJUST_DURATION_SECS: u64 = 30;
const DEFAULT_AUTO_ADJUST_MINIMIZE_GAIN: bool = false;
const DEFAULT_AUTO_ADJUST_NO_BOOST_FRAMERATE: bool = false;
const DEFAULT_FRAME_FORMAT: FrameFormat = FrameFormat::Tiff;
const DEFAULT_ZOOM_SIZE: u32 = 101;
const MIN_ZOOM_SIZE: u32 = 3;
const MAX_ZOOM_SIZE: u32 = 999;

/// Validates and normalizes `--zoom-size`: must be an integer in
/// `[MIN_ZOOM_SIZE, MAX_ZOOM_SIZE]`; even values are rounded up by one so the crop
/// is always symmetric around the zoomed pixel.
fn parse_zoom_size(s: &str) -> Result<u32, String> {
    let value: u32 = s
        .parse()
        .map_err(|_| format!("`{s}` is not a valid non-negative integer"))?;
    if !(MIN_ZOOM_SIZE..=MAX_ZOOM_SIZE).contains(&value) {
        return Err(format!(
            "zoom size must be between {MIN_ZOOM_SIZE} and {MAX_ZOOM_SIZE} (got {value})"
        ));
    }
    Ok(value + (value % 2 == 0) as u32)
}

/// On-disk container format for recorded frames.
///
/// `Tiff` and `Png` are lossless and save the raw Bayer mosaic straight from the
/// sensor. `Jpeg` is lossy and debayers to RGB first — lossy compression must never
/// run directly on a Bayer mosaic (its checkerboard-like color-filter pattern isn't
/// real image content, and block-based/chroma-subsampled codecs would bake mosaic
/// artifacts into the debayered result). Given that recorded frames feed 3D
/// reconstruction, `Jpeg` uses "SfM-safe" turbojpeg settings: high quality and no
/// chroma subsampling (4:4:4).
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum FrameFormat {
    /// Uncompressed raw Bayer — fastest to write, largest on disk.
    Tiff,
    /// Losslessly compressed raw Bayer via the `png` crate.
    Png,
    /// Lossy, debayered RGB via turbojpeg (high quality, 4:4:4 chroma).
    Jpeg,
}

impl FrameFormat {
    pub fn extension(self) -> &'static str {
        match self {
            FrameFormat::Tiff => "tiff",
            FrameFormat::Png => "png",
            FrameFormat::Jpeg => "jpg",
        }
    }

    pub fn is_lossy(self) -> bool {
        matches!(self, FrameFormat::Jpeg)
    }
}

/// Multi-camera acquisition server: grabs synchronized frames and records them to disk,
/// driven over a control WebSocket.
#[derive(Parser)]
pub struct Args {
    /// Per-camera frame limit for a single recording. A recording auto-stops
    /// once every camera has saved this many frames.
    #[arg(long, default_value_t = DEFAULT_FRAME_COUNT)]
    pub frames: u32,

    /// Output directory template for recordings. Each recording gets its own directory
    /// expanded from this template; frame files are named internally as
    /// `cam-{cam_idx}_frame-{frame_idx:05d}.{ext}` (extension depends on
    /// `--frame-format`) within that directory. Supported placeholders:
    ///   {local_date}      — date of recording start, e.g. 2026-04-21
    ///   {local_time}      — time of recording start, e.g. 14-30-00
    ///   {local_datetime}  — combined, e.g. 2026-04-21T14-30-00
    #[arg(
        long,
        default_value = "$HOME/phenominator/{local_date}/{local_time}"
    )]
    pub output_dir: String,

    /// On-disk format for recorded frames: `tiff` (uncompressed raw Bayer, fastest),
    /// `png` (losslessly compressed raw Bayer via the `png` crate), or `jpeg` (lossy,
    /// debayered RGB via turbojpeg — smallest files, but not bit-exact).
    #[arg(long, value_enum, default_value_t = DEFAULT_FRAME_FORMAT)]
    pub frame_format: FrameFormat,

    /// Timeout for auto-adjust convergence in seconds.
    #[arg(long, default_value_t = DEFAULT_AUTO_ADJUST_DURATION_SECS)]
    pub auto_adjust_duration_secs: u64,

    /// Use MinimizeGain profile for auto-adjust. Default is MinimizeExposureTime.
    #[arg(long, default_value_t = DEFAULT_AUTO_ADJUST_MINIMIZE_GAIN)]
    pub auto_adjust_minimize_gain: bool,

    /// Disable the framerate boost during auto-adjust convergence.
    /// By default the framerate limiter is disabled during convergence for faster results.
    #[arg(long, default_value_t = DEFAULT_AUTO_ADJUST_NO_BOOST_FRAMERATE)]
    pub auto_adjust_no_boost_framerate: bool,

    /// Acquisition framerate in frames per second
    #[arg(long, default_value_t = DEFAULT_FRAMERATE)]
    pub framerate: f64,

    /// Number of cameras expected to be connected
    #[arg(long)]
    pub n_cameras: usize,

    /// Free-run the cameras instead of triggering them off the PTP-synchronized
    /// periodic signal. Falls back to pacing acquisition with the
    /// `AcquisitionFrameRate` limiter, so captures are not simultaneous across
    /// cameras. The server also falls back to this on its own if a camera does not
    /// support the periodic signal; this flag forces it, which is the only way to
    /// exercise that path on hardware that does support it.
    #[arg(long, default_value_t = false)]
    pub no_sync_trigger: bool,

    /// Do not enable PTP: skip spawning `ptp4l` and switch `PtpEnable` off on every
    /// camera. Camera clocks free-run, so captures are not simultaneous even when the
    /// periodic-signal trigger is active — it still paces both cameras at the
    /// requested rate, just off each camera's own clock.
    #[arg(long, default_value_t = false)]
    pub no_ptp: bool,

    /// Frame grouper patience (i.e. per camera stream buffer length)
    #[arg(long, default_value_t = DEFAULT_GROUPER_BUFFER_LEN)]
    pub grouper_buffer_len: usize,

    /// PTP status poll interval in seconds
    #[arg(long, default_value_t = DEFAULT_PTP_POLL_INTERVAL_SECS)]
    pub ptp_poll_interval_secs: u64,

    /// Broadcast channel capacity for server → client events (recording state, PTP status, errors)
    #[arg(long, default_value_t = DEFAULT_EVENT_BROADCAST_CAPACITY)]
    pub event_broadcast_capacity: usize,

    /// Broadcast channel capacity for preview frames
    #[arg(long, default_value_t = DEFAULT_PREVIEW_BROADCAST_CAPACITY)]
    pub preview_broadcast_capacity: usize,

    /// Interval (seconds) between WebSocket heartbeat pings to each client. A
    /// client that fails to answer with a pong for one full interval is
    /// considered disconnected. This is what lets the server notice clients that
    /// vanish without a clean close (e.g. behind an SSH/port-forward tunnel).
    #[arg(long, default_value_t = DEFAULT_HEARTBEAT_INTERVAL_SECS)]
    pub heartbeat_interval_secs: u64,

    /// Per-message send timeout (seconds). A send that blocks longer than this
    /// (wedged socket / frozen peer that stops draining) is treated as a
    /// disconnect rather than blocking the connection handler indefinitely.
    #[arg(long, default_value_t = DEFAULT_SEND_TIMEOUT_SECS)]
    pub send_timeout_secs: u64,

    /// Bind address for the control WebSocket (client → server commands).
    #[arg(long, default_value = DEFAULT_CONTROL_ADDR)]
    pub control_addr: std::net::SocketAddr,

    /// Bind address for the preview WebSocket (server → client frame metadata).
    #[arg(long, default_value = DEFAULT_PREVIEW_ADDR)]
    pub preview_addr: std::net::SocketAddr,

    /// Side length (pixels) of the zoomed preview crop shown around the crosshair.
    /// Must be between 3 and 999; even values are rounded up by one to keep the
    /// crop symmetric around the zoomed pixel.
    #[arg(long, default_value_t = DEFAULT_ZOOM_SIZE, value_parser = parse_zoom_size)]
    pub zoom_size: u32,

    #[cfg(feature = "aruco-auto-record")]
    #[command(flatten)]
    pub aruco: AutoRecordArgs,
}

// Marker-driven recording control. Present only under the `aruco-auto-record` feature,
// and inert unless `--aruco-markers` is given at least once.
//
// Note these carry no doc comment: clap takes a flattened struct's doc comment as the
// whole command's `about`, which would replace the server's own description.
#[cfg(feature = "aruco-auto-record")]
#[derive(clap::Args)]
#[command(next_help_heading = "ArUco auto-record")]
pub struct AutoRecordArgs {
    /// Markers that start a recording, as `FAMILY:id,id,...` — for example
    /// `DICT_5X5_250:3,30,22`. Repeat the flag to mix families. Seeing any listed marker
    /// starts a recording; the recording runs for as long as any listed marker stays
    /// visible to any watched camera, and stops once none has been seen for
    /// `--aruco-stop-after` consecutive passes over the cameras.
    ///
    /// An unknown family name is rejected at startup with the list of valid names.
    #[arg(long, value_parser = parse_marker_selector)]
    pub aruco_markers: Vec<aruco::MarkerSelector>,

    /// Cameras to watch, comma-separated. Defaults to every camera, examined round-robin.
    /// Narrowing this to the cameras that can actually see the marker station cuts the
    /// time to trigger proportionally.
    #[arg(long, value_delimiter = ',')]
    pub aruco_cameras: Option<Vec<usize>>,

    /// Minimum milliseconds between detector dispatches.
    #[arg(long, default_value_t = aruco::DEFAULT_INTERVAL.as_millis() as u64)]
    pub aruco_interval_ms: u64,

    /// Quad stride for the Bayer downscale: the detector sees `width/(2k)` by
    /// `height/(2k)`. Larger is cheaper but shrinks the marker; aim to keep it at least
    /// 30 px across in that downscaled image.
    #[arg(long, default_value_t = aruco::DEFAULT_DOWNSCALE, value_parser = clap::value_parser!(u32).range(1..=16))]
    pub aruco_downscale: u32,

    /// How the mosaic is reduced to grayscale. `green` is cheaper and, at large
    /// downscales, more reliable; `box` antialiases and suppresses noise.
    #[arg(long, value_enum, default_value_t = aruco::DownscaleMode::Green)]
    pub aruco_downscale_mode: aruco::DownscaleMode,

    /// Sightings required within the window, on one camera, before starting.
    #[arg(long, default_value_t = aruco::DEFAULT_CONFIRMATIONS)]
    pub aruco_confirmations: u8,

    /// Length of the per-camera sighting window.
    #[arg(long, default_value_t = aruco::DEFAULT_WINDOW)]
    pub aruco_window: u8,

    /// Consecutive passes over every watched camera, finding no marker anywhere, before
    /// stopping. A pass examines one frame per camera, so it takes at least
    /// `--aruco-interval-ms` per camera. With synchronised cameras it takes one frame
    /// period per camera instead: their frames arrive together, and a frame that lands
    /// while another camera's is still being examined is skipped.
    #[arg(long, default_value_t = aruco::DEFAULT_STOP_AFTER)]
    pub aruco_stop_after: u32,

    /// Only markers whose centre lies in this rectangle count, as `x0,y0,x1,y1` in
    /// fractions of the frame (0,0 is top left). A marker outside it neither starts nor
    /// sustains a recording. Defaults to the whole frame.
    ///
    /// Use it to ignore markers that are in view but not at the station, such as pots
    /// waiting in a rack. Run with `RUST_LOG=acquisition_server::aruco=debug` to log
    /// every marker's position and size.
    #[arg(long, value_parser = parse_roi)]
    pub aruco_roi: Option<[f64; 4]>,

    /// Only markers whose mean side is at least this many pixels, measured in the
    /// full-resolution frame, count. Defaults to 0, which admits every size.
    #[arg(long, default_value_t = 0.0)]
    pub aruco_min_side_px: f64,

    /// Save a frame group of an auto-started recording only if some camera changed in at
    /// least this percentage of pixels since the last saved group. A stalled machine then
    /// saves nothing, and the frame limit counts only saved groups. Changes in overall
    /// brightness don't count. Off by default; manual recordings always keep every frame.
    ///
    /// Measured on the watering rig: a pot held still changes 0%, a turning pot 1.4-7%
    /// between frames at 1 fps.
    #[arg(long)]
    pub aruco_min_change_pct: Option<f64>,

    /// Debounce in seconds after any automatic start or stop.
    #[arg(long, default_value_t = aruco::DEFAULT_COOLDOWN.as_secs())]
    pub aruco_cooldown_secs: u64,

    /// Adaptive-threshold window size; must be odd. The detector's own default sweeps
    /// three window sizes, tripling the cost for no benefit on large markers.
    #[arg(long, default_value_t = aruco::DEFAULT_THRESH_WIN)]
    pub aruco_thresh_win: u32,

    /// Minimum contour size, as a fraction of the larger image dimension.
    #[arg(long, default_value_t = aruco::DEFAULT_MIN_PERIMETER_RATE)]
    pub aruco_min_perimeter_rate: f64,
}

/// Parse one `FAMILY:id,id,...` selector. Family names are validated later, against the
/// dictionary list, so that the error can enumerate what is valid.
#[cfg(feature = "aruco-auto-record")]
fn parse_marker_selector(s: &str) -> Result<aruco::MarkerSelector, String> {
    let (family, ids) = s
        .split_once(':')
        .ok_or_else(|| format!("expected `FAMILY:id,id,...`, got `{s}`"))?;
    let family = family.trim();
    if family.is_empty() {
        return Err(format!("missing family name in `{s}`"));
    }
    let ids = ids
        .split(',')
        .map(|id| {
            let id = id.trim();
            id.parse::<usize>()
                .map_err(|_| format!("`{id}` is not a valid marker id"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if ids.is_empty() {
        return Err(format!("no marker ids given in `{s}`"));
    }
    Ok(aruco::MarkerSelector { family: family.to_string(), ids })
}

/// Parse `x0,y0,x1,y1`, each a fraction of the frame, into a rectangle with positive area.
#[cfg(feature = "aruco-auto-record")]
fn parse_roi(s: &str) -> Result<[f64; 4], String> {
    let values = s
        .split(',')
        .map(|v| {
            let v = v.trim();
            v.parse::<f64>().map_err(|_| format!("`{v}` is not a number"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let [x0, y0, x1, y1] = values[..] else {
        return Err(format!("expected `x0,y0,x1,y1`, got `{s}`"));
    };
    if !values.iter().all(|v| (0.0..=1.0).contains(v)) {
        return Err(format!("every value must be within 0.0..=1.0, got `{s}`"));
    }
    if x0 >= x1 || y0 >= y1 {
        return Err(format!("need x0 < x1 and y0 < y1, got `{s}`"));
    }
    Ok([x0, y0, x1, y1])
}

impl Args {
    /// Assemble the watcher configuration.
    ///
    /// Without the feature there is nothing to configure, and the default is inert — which
    /// is what keeps the call site in the acquisition loop free of `#[cfg]`.
    #[cfg(feature = "aruco-auto-record")]
    pub fn auto_record_cfg(&self) -> aruco::AutoRecordCfg {
        aruco::AutoRecordCfg {
            markers: self.aruco.aruco_markers.clone(),
            cameras: self.aruco.aruco_cameras.clone(),
            interval: std::time::Duration::from_millis(self.aruco.aruco_interval_ms),
            downscale: self.aruco.aruco_downscale,
            downscale_mode: self.aruco.aruco_downscale_mode,
            confirmations: self.aruco.aruco_confirmations,
            window: self.aruco.aruco_window,
            stop_after: self.aruco.aruco_stop_after,
            cooldown: std::time::Duration::from_secs(self.aruco.aruco_cooldown_secs),
            gate: aruco::MarkerGate {
                roi: self.aruco.aruco_roi.unwrap_or(aruco::MarkerGate::OPEN.roi),
                min_side_px: self.aruco.aruco_min_side_px,
            },
            min_change: self.aruco.aruco_min_change_pct.map(|pct| pct / 100.0),
            thresh_win: self.aruco.aruco_thresh_win,
            min_perimeter_rate: self.aruco.aruco_min_perimeter_rate,
        }
    }

    #[cfg(not(feature = "aruco-auto-record"))]
    pub fn auto_record_cfg(&self) -> aruco::AutoRecordCfg {
        aruco::AutoRecordCfg::default()
    }

    /// Cross-field validation that no single `value_parser` can do. Called once at
    /// startup so a misconfiguration fails immediately rather than silently never firing.
    #[cfg(feature = "aruco-auto-record")]
    pub fn validate_aruco(&self) -> Result<(), String> {
        let a = &self.aruco;
        if a.aruco_markers.is_empty() {
            // Nothing configured is a valid, inert configuration; the rest is moot.
            return Ok(());
        }
        // Resolve the selectors here rather than leaving it to the watcher, which is built
        // on the acquisition thread once sockets are bound and cameras opened. A typo in a
        // family name should stop the launch, not quietly produce a rig that never fires.
        aruco::validate_markers(&a.aruco_markers).map_err(|e| e.to_string())?;
        if a.aruco_window == 0 || a.aruco_window > 8 {
            return Err(format!("--aruco-window must be 1..=8 (got {})", a.aruco_window));
        }
        if a.aruco_confirmations == 0 || a.aruco_confirmations > a.aruco_window {
            return Err(format!(
                "--aruco-confirmations must be 1..={} (got {})",
                a.aruco_window, a.aruco_confirmations
            ));
        }
        if a.aruco_stop_after == 0 {
            return Err("--aruco-stop-after must be at least 1".to_string());
        }
        if a.aruco_thresh_win < 3 || a.aruco_thresh_win.is_multiple_of(2) {
            return Err(format!(
                "--aruco-thresh-win must be an odd number >= 3 (got {})",
                a.aruco_thresh_win
            ));
        }
        if !(0.0..=1.0).contains(&a.aruco_min_perimeter_rate) {
            return Err(format!(
                "--aruco-min-perimeter-rate must be within 0.0..=1.0 (got {})",
                a.aruco_min_perimeter_rate
            ));
        }
        if !(a.aruco_min_side_px >= 0.0 && a.aruco_min_side_px.is_finite()) {
            return Err(format!(
                "--aruco-min-side-px must be a finite number >= 0 (got {})",
                a.aruco_min_side_px
            ));
        }
        if let Some(pct) = a.aruco_min_change_pct {
            if !(pct > 0.0 && pct <= 100.0) {
                return Err(format!("--aruco-min-change-pct must be within (0, 100] (got {pct})"));
            }
        }
        if let Some(cams) = &a.aruco_cameras {
            if cams.is_empty() {
                return Err("--aruco-cameras was given but lists no cameras".to_string());
            }
            if let Some(&bad) = cams.iter().find(|&&c| c >= self.n_cameras) {
                return Err(format!(
                    "--aruco-cameras names camera {bad}, but only {} are configured",
                    self.n_cameras
                ));
            }
        }
        Ok(())
    }

    #[cfg(not(feature = "aruco-auto-record"))]
    pub fn validate_aruco(&self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::parse_zoom_size;

    #[cfg(feature = "aruco-auto-record")]
    #[test]
    fn roi_parses_and_rejects_malformed_rectangles() {
        use super::parse_roi;
        assert_eq!(parse_roi("0.6, 0.15,1,0.8").unwrap(), [0.6, 0.15, 1.0, 0.8]);
        assert!(parse_roi("0.6,0.15,1").is_err(), "too few values");
        assert!(parse_roi("0.6,0.15,1,0.8,0").is_err(), "too many values");
        assert!(parse_roi("0.6,0.15,1.2,0.8").is_err(), "outside the frame");
        assert!(parse_roi("0.6,0.8,1,0.15").is_err(), "y0 > y1");
        assert!(parse_roi("0.6,0.15,0.6,0.8").is_err(), "zero width");
        assert!(parse_roi("a,0,1,1").is_err());
    }

    #[test]
    fn even_values_round_up_to_odd() {
        assert_eq!(parse_zoom_size("100").unwrap(), 101);
        assert_eq!(parse_zoom_size("4").unwrap(), 5);
    }

    #[test]
    fn odd_values_pass_through() {
        assert_eq!(parse_zoom_size("101").unwrap(), 101);
        assert_eq!(parse_zoom_size("3").unwrap(), 3);
        assert_eq!(parse_zoom_size("999").unwrap(), 999);
    }

    #[test]
    fn rejects_unreasonable_values() {
        assert!(parse_zoom_size("-10").is_err());
        assert!(parse_zoom_size("foo").is_err());
        assert!(parse_zoom_size("3.2").is_err());
        assert!(parse_zoom_size("1").is_err());
        assert!(parse_zoom_size("1000000").is_err());
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(FmtSpan::CLOSE)
        .init();

    let args = Args::parse();
    // Fail before opening any camera: a rig that starts but can never trigger is worse
    // than one that refuses to start.
    args.validate_aruco()?;

    plumbing::start_server(args).await
}
