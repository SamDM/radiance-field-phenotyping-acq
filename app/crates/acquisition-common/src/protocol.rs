use image::RgbImage;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AnyFrame;


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
    /// Request (or, with `position: None`, clear) a full-resolution zoom crop for a camera.
    /// `position` is the crosshair location in normalized [0,1] image coordinates.
    SetZoom {
        camera_name: String,
        position: Option<(f32, f32)>,
    },
}

/// A single preview entry sent over the preview stream: the (downscaled) preview frame
/// plus, when a zoom is active for that camera, an unscaled full-resolution color crop.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PreviewFrame {
    pub frame: AnyFrame,
    pub zoom: Option<Zoom>,
}

/// A 21×21 (or otherwise small) unscaled, debayered RGB crop centered on the crosshair.
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
pub struct ServerStatus {
    pub activity: Activity,
    pub ptp: PtpStatusReport,
    pub sensor_cfg: SensorCfg,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum ServerMessage {
    ServerStatus(ServerStatus),
    Ack(ServerAck),
    InternalErr(String),
}

impl ServerMessage {
    pub fn ack_ok(cmd_uuid: Uuid) -> ServerMessage {
        ServerMessage::Ack(ServerAck { cmd_uuid, result: Ok(()) })
    }

    pub fn ack_err(cmd_uuid: Uuid, reason: String) -> ServerMessage {
        ServerMessage::Ack(ServerAck { cmd_uuid, result: Err(reason) })
    }

    pub fn internal_err(reason: String) -> ServerMessage {
        ServerMessage::InternalErr(reason)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Idle,
    Recording,
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
}
