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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(FmtSpan::CLOSE)
        .init();

    let args = Args::parse();

    plumbing::start_server(args).await
}
