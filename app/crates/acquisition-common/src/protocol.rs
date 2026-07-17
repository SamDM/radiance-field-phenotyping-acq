use image::RgbImage;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use time::OffsetDateTime;


#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ClientMessage {
    pub uuid: Uuid,
    pub command: ClientCommand,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum ClientCommand {
    GetStatus,
    SetSensorCfg(SensorCfg),
    StartAutoAdjust {
        target_brightness: f64,
        gain_limit: Option<[f64; 2]>,
        exposure_limit_us: Option<[f64; 2]>,
        auto_whitebalance: bool,
    },
    StopAutoAdjust,
    StartRecording,
    StopRecording,
    StartReplay,
    StopReplay,
    PauseReplay,
    ResumeReplay,
    RewindReplay,
    StepReplayForward,
    StepReplayBackward,
    SeekReplayToFrame { frame_idx: u32 },
    TrashRecording,
    RateRecording { rating: u8 },
    CommentRecording { comment: String },
    SelectRecording { name: String },
    /// Request (or, with `position: None`, clear) a full-resolution zoom crop for a camera.
    /// `position` is the crosshair location in normalized [0,1] image coordinates.
    SetZoom {
        camera_name: String,
        position: Option<(f32, f32)>,
    },
}

/// A single preview entry sent over the preview stream: the (downscaled) JPEG-compressed
/// preview frame plus, when a zoom is active for that camera, an unscaled full-resolution
/// color crop (uncompressed, to preserve exact pixel data for sharpness estimation).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PreviewFrame {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub camera_name: String,
    pub camera_timestamp_ns: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub host_timestamp: OffsetDateTime,
    pub zoom: Option<Zoom>,
}

/// A 201×201 (or otherwise small) unscaled, debayered RGB crop centered on the crosshair.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Zoom {
    #[serde(with = "serde_rgb")]
    pub crop: RgbImage,
    pub rel_x: f32,
    pub rel_y: f32,
}

/// `serde` shim for `image::RgbImage`, which is not itself `Serialize`/`Deserialize`.
/// Stored on the wire as `(width, height, raw_rgb_bytes)`.
mod serde_rgb {
    use image::{ImageBuffer, RgbImage};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(img: &RgbImage, s: S) -> Result<S::Ok, S::Error> {
        (img.width(), img.height(), img.as_raw().as_slice()).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<RgbImage, D::Error> {
        let (w, h, bytes): (u32, u32, Vec<u8>) = Deserialize::deserialize(d)?;
        ImageBuffer::from_raw(w, h, bytes)
            .ok_or_else(|| serde::de::Error::custom("rgb buffer size does not match dimensions"))
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ServerAck {
    pub cmd_uuid: Uuid,
    pub result: Result<(), String>
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PastRecording {
    pub name: String,
    pub ok: bool,
    pub elapsed: time::Duration,
    pub groups_saved: u32,
    #[serde(default)]
    pub finished_at: Option<time::OffsetDateTime>,
    #[serde(default)]
    pub rating: Option<u8>,
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ServerStatus {
    #[serde(with = "time::serde::rfc3339")]
    pub server_time: OffsetDateTime,
    pub activity: Activity,
    pub ptp: PtpStatusReport,
    pub sensor_cfg: SensorCfg,
    #[serde(default)]
    pub recordings: Vec<PastRecording>,
    #[serde(default)]
    pub selected_recording: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeSeverity {
    Info,
    Warn,
    Error,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Notice {
    pub severity: NoticeSeverity,
    pub message: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum ServerMessage {
    ServerStatus(ServerStatus),
    Ack(ServerAck),
    Notice(Notice),
}

impl ServerMessage {
    pub fn ack_ok(cmd_uuid: Uuid) -> ServerMessage {
        ServerMessage::Ack(ServerAck { cmd_uuid, result: Ok(()) })
    }

    pub fn ack_err(cmd_uuid: Uuid, reason: String) -> ServerMessage {
        ServerMessage::Ack(ServerAck { cmd_uuid, result: Err(reason) })
    }

    pub fn notice(severity: NoticeSeverity, message: String) -> ServerMessage {
        ServerMessage::Notice(Notice { severity, message, at: OffsetDateTime::now_utc() })
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    Idle,
    Recording {
        elapsed: time::Duration,
        groups_saved: u32,
        name: String,
    },
    Replaying {
        name: String,
        frame_idx: u32,
        total_frames: u32,
        /// Camera-timestamp offset of the current frame from the recording start (nanoseconds).
        #[serde(default)]
        elapsed_ns: u64,
        /// Camera-timestamp span of the full recording (nanoseconds).
        #[serde(default)]
        total_ns: u64,
        #[serde(default)]
        paused: bool,
    },
    AutoAdjusting,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Ptp4lStatus {
    Starting,
    Master,
    Failed(String),
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraPtpStatus {
    Master,
    PreMaster,
    Slave,
    Faulty,
    Listening,
    Uncalibrated,
    Disabled,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraServoStatus {
    Unlocked,
    Locked,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CameraPtpInfo {
    pub ptp: CameraPtpStatus,
    pub servo: CameraServoStatus,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PtpStatusReport {
    pub ptp4l: Ptp4lStatus,
    pub cameras: Vec<CameraPtpInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct SensorCfg {
    pub exposure_us: Option<f64>,
    pub gain: Option<f64>,
    /// (red, green, blue) balance ratios.
    pub white_balance: Option<(f64, f64, f64)>,
    pub framerate: Option<f64>,
}
