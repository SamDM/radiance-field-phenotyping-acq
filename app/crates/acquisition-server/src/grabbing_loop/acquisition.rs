use std::collections::HashMap;
use std::path::{Path, PathBuf};

use acquisition_common::debayer::debayer_crop;
use acquisition_common::protocol as wire;
use acquisition_common::{AnyFrame, Bayer, Frame};
use image::imageops::FilterType;
use image::RgbImage;
use pylon::{CameraArray, GrabResult, PixelFormat};
use time::macros::format_description;
use time::OffsetDateTime;
use tokio::sync::broadcast;
use tracing::{info, warn};

use uuid::Uuid;

use crate::utils::zipper::{StreamZipper, ZipError, ZipErrorReason, Zipped};

fn accept_cmd(evt_tx: &broadcast::Sender<wire::ServerMessage>, cmd_uuid: Uuid, reason: &str) {
    info!("{reason}");
    let _ = evt_tx.send(wire::ServerMessage::ack_ok(cmd_uuid));
}

fn reject_cmd(evt_tx: &broadcast::Sender<wire::ServerMessage>, cmd_uuid: Uuid, reason: &str) {
    warn!("{reason}");
    let _ = evt_tx.send(wire::ServerMessage::ack_err(cmd_uuid, reason.to_string()));
}

enum AcquisitionActivity {
    Idle,
    Recording {
        start: OffsetDateTime,
        groups_saved: u32,
    },
}

pub(super) struct AcquisitionIterState {
    activity: AcquisitionActivity,
    zipper: StreamZipper<Frame<Bayer>>,
    debayer: bool,
    output_path_template: String,
    max_n_frames: u32,
    /// Per-camera crosshair (normalized [0,1]) for which a zoom crop is currently requested.
    zoom_requests: HashMap<String, (f32, f32)>,
}

impl AcquisitionIterState {
    pub(super) fn new(
        n_cameras: usize,
        grouper_buffer_len: usize,
        debayer: bool,
        output_path_template: String,
        max_n_frames: u32,
    ) -> Self {
        AcquisitionIterState {
            activity: AcquisitionActivity::Idle,
            zipper: StreamZipper::new(n_cameras, grouper_buffer_len),
            debayer,
            output_path_template,
            max_n_frames,
            zoom_requests: HashMap::new(),
        }
    }

    pub(super) fn is_recording(&self) -> bool {
        matches!(self.activity, AcquisitionActivity::Recording { .. })
    }
}

pub(super) fn iteration(
    messages: &[wire::ClientMessage],
    camera_array: &mut CameraArray,
    state: &mut AcquisitionIterState,
    is_busy: bool,
    evt_tx: &broadcast::Sender<wire::ServerMessage>,
    preview_tx: &broadcast::Sender<Vec<wire::PreviewFrame>>,
) -> Result<(), Box<dyn std::error::Error>> {
    enum RecordingCommand {
        Start,
        Stop,
    }

    let mut recording_cmd: Option<(Uuid, RecordingCommand)> = None;
    for message in messages {
        let cmd = match &message.command {
            wire::ClientCommand::StartRecording => RecordingCommand::Start,
            wire::ClientCommand::StopRecording => RecordingCommand::Stop,
            wire::ClientCommand::SetZoom { camera_name, position } => {
                match position {
                    Some(pos) => {
                        state.zoom_requests.insert(camera_name.clone(), *pos);
                    }
                    None => {
                        state.zoom_requests.remove(camera_name);
                    }
                }
                continue;
            }
            wire::ClientCommand::SetSensorCfg(_)
            | wire::ClientCommand::StartAutoAdjust { .. }
            | wire::ClientCommand::StopAutoAdjust
            | wire::ClientCommand::GetStatus => continue,
        };
        if let Some((superseded_uuid, _)) = recording_cmd.replace((message.uuid, cmd)) {
            reject_cmd(evt_tx, superseded_uuid, "superseded by newer recording command");
        }
    }
    if let Some((cmd_uuid, recording_cmd)) = recording_cmd {
        match recording_cmd {
            RecordingCommand::Start => {
                if is_busy {
                    reject_cmd(evt_tx, cmd_uuid, "start ignored: camera busy");
                } else if matches!(state.activity, AcquisitionActivity::Idle) {
                    let start = OffsetDateTime::now_local()?;
                    state.activity = AcquisitionActivity::Recording {
                        start,
                        groups_saved: 0,
                    };
                    accept_cmd(evt_tx, cmd_uuid, "recording started");
                } else {
                    reject_cmd(evt_tx, cmd_uuid, "start ignored: already recording");
                }
            }
            RecordingCommand::Stop => {
                if let AcquisitionActivity::Recording { .. } = state.activity {
                    state.activity = AcquisitionActivity::Idle;
                    accept_cmd(evt_tx, cmd_uuid, "recording stopped");
                } else {
                    reject_cmd(evt_tx, cmd_uuid, "stop ignored: not recording");
                }
            }
        }
    };

    match camera_array.retrieve_result(100) {
        Some(Ok(grab)) => {
            let cam_idx = grab.camera_index();
            let frame = bayer_frame_from_grab(&grab);
            let ts = frame.camera_timestamp_ns();
            // zipper will group frames with near-equal timestamps in a best-effort manner.
            for result in state.zipper.push(frame, cam_idx, ts) {
                let saved = process_grouped_frames_result(
                    result,
                    &state.output_path_template,
                    state.debayer,
                    &state.activity,
                    &state.zoom_requests,
                    preview_tx,
                )?;
                if let Some(new_count) = saved {
                    if new_count >= state.max_n_frames {
                        info!(
                            saved_groups = new_count,
                            "recording stopped (frame limit reached)"
                        );
                        state.activity = AcquisitionActivity::Idle;
                    } else if let AcquisitionActivity::Recording {
                        ref mut groups_saved,
                        ..
                    } = state.activity
                    {
                        *groups_saved = new_count;
                    }
                }
            }
        }
        Some(Err(e)) => {
            info!(
                camera = e.camera_index,
                error = e.description,
                "grab failed"
            );
            let _ = evt_tx.send(wire::ServerMessage::internal_err(
                format!(
                    "grab failed on camera {}: {}",
                    e.camera_index, e.description
                ),
            ));
        }
        None => {}
    }

    Ok(())
}

/// Returns `Some(new_groups_saved)` when a group was saved while recording, `None` otherwise.
fn process_grouped_frames_result(
    result: Result<Zipped<Frame<Bayer>>, ZipError<Frame<Bayer>>>,
    output_path_template: &str,
    debayer: bool,
    camera_state: &AcquisitionActivity,
    zoom_requests: &HashMap<String, (f32, f32)>,
    preview_tx: &broadcast::Sender<Vec<wire::PreviewFrame>>,
) -> Result<Option<u32>, Box<dyn std::error::Error>> {
    match result {
        Ok(group) => {
            let frames: Vec<AnyFrame> = if debayer {
                group
                    .items
                    .into_iter()
                    .map(|(f, _ts)| f.into_color().into_any())
                    .collect()
            } else {
                group
                    .items
                    .into_iter()
                    .map(|(f, _ts)| f.into_any())
                    .collect()
            };

            let previews: Vec<wire::PreviewFrame> = frames
                .iter()
                .map(|frame| {
                    let zoom = zoom_requests
                        .get(frame.camera_name())
                        .map(|&rel| extract_zoom(frame, rel));
                    wire::PreviewFrame { frame: make_preview(frame), zoom }
                })
                .collect();
            let _ = preview_tx.send(previews);

            if let AcquisitionActivity::Recording {
                start,
                groups_saved,
            } = camera_state
            {
                for (cam_idx, frame) in frames.iter().enumerate() {
                    let path = expand_path(output_path_template, *start, cam_idx, *groups_saved);
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    frame.save(&path)?;
                    info!(
                        camera = cam_idx,
                        group = *groups_saved,
                        timestamp_camera = %frame.camera_datetime_local(),
                        "saved frame"
                    );
                }
                return Ok(Some(groups_saved + 1));
            }
        }
        Err(err) => {
            let reason = match err.reason {
                ZipErrorReason::BufferOverflow => "buffer overflow",
                ZipErrorReason::OutOfOrder => "out of order",
                ZipErrorReason::Stranded => "stranded",
            };
            warn!(
                stream_id = err.stream_id,
                timestamp_ns = err.item.1,
                reason, "frame dropped (grouping failed)"
            );
        }
    }
    Ok(None)
}

fn expand_path(
    template: &str,
    recording_start: OffsetDateTime,
    cam_idx: usize,
    group_idx: u32,
) -> PathBuf {
    let date = recording_start
        .format(format_description!("[year]-[month]-[day]"))
        .expect("OffsetDateTime formatting cannot fail");
    let time = recording_start
        .format(format_description!("[hour]-[minute]-[second]"))
        .expect("OffsetDateTime formatting cannot fail");
    let datetime = format!("{date}T{time}");
    let home = std::env::var("HOME").unwrap_or_default();
    let s = template
        .replace("$HOME", &home)
        .replace("{local_datetime}", &datetime)
        .replace("{local_date}", &date)
        .replace("{local_time}", &time)
        .replace("{cam_idx}", &cam_idx.to_string())
        .replace("{frame_idx:05d}", &format!("{:05}", group_idx));
    Path::new(&s).to_path_buf()
}

/// Extract a small, unscaled, debayered RGB crop centered on `rel` (normalized [0,1])
/// from a full-resolution frame.
fn extract_zoom(frame: &AnyFrame, rel: (f32, f32)) -> wire::Zoom {
    const ZOOM_SIZE: u32 = 51;
    let w = frame.width();
    let h = frame.height();
    let cw = ZOOM_SIZE.min(w);
    let ch = ZOOM_SIZE.min(h);
    let cx = (rel.0 * w as f32).round().clamp(0.0, (w - 1) as f32) as u32;
    let cy = (rel.1 * h as f32).round().clamp(0.0, (h - 1) as f32) as u32;
    let x0 = cx.saturating_sub(cw / 2).min(w - cw);
    let y0 = cy.saturating_sub(ch / 2).min(h - ch);

    let crop: RgbImage = match frame {
        AnyFrame::Color(f) => {
            let img = f.image();
            let mut out = RgbImage::new(cw, ch);
            for yy in 0..ch {
                for xx in 0..cw {
                    out.put_pixel(xx, yy, *img.get_pixel(x0 + xx, y0 + yy));
                }
            }
            out
        }
        AnyFrame::Bayer(f) => {
            let img = f.image();
            debayer_crop(img.as_raw(), w, h, x0, y0, cw, ch)
        }
        AnyFrame::Gray(f) => {
            let img = f.image();
            let mut out = RgbImage::new(cw, ch);
            for yy in 0..ch {
                for xx in 0..cw {
                    let l = img.get_pixel(x0 + xx, y0 + yy).0[0];
                    out.put_pixel(xx, yy, image::Rgb([l, l, l]));
                }
            }
            out
        }
    };

    wire::Zoom { crop, rel_x: rel.0, rel_y: rel.1 }
}

fn make_preview(frame: &AnyFrame) -> AnyFrame {
    const SCALE: u32 = 8;
    let w = (frame.width() / SCALE).max(1);
    let h = (frame.height() / SCALE).max(1);
    match frame {
        AnyFrame::Color(f) => {
            let img = f.image();
            AnyFrame::Color(Frame::new(
                image::imageops::resize(&img, w, h, FilterType::Nearest).into_raw(),
                w,
                h,
                f.camera_timestamp_ns,
                f.host_timestamp,
                f.camera_name.clone(),
            ))
        }
        AnyFrame::Gray(f) => {
            let img = f.image();
            AnyFrame::Gray(Frame::new(
                image::imageops::resize(&img, w, h, FilterType::Triangle).into_raw(),
                w,
                h,
                f.camera_timestamp_ns,
                f.host_timestamp,
                f.camera_name.clone(),
            ))
        }
        AnyFrame::Bayer(f) => {
            // Bayer raw pixels are treated as gray for the preview
            let img = f.image();
            AnyFrame::Gray(Frame::new(
                image::imageops::resize(&img, w, h, FilterType::Triangle).into_raw(),
                w,
                h,
                f.camera_timestamp_ns,
                f.host_timestamp,
                f.camera_name.clone(),
            ))
        }
    }
}

fn bayer_frame_from_grab(grab: &GrabResult) -> Frame<Bayer> {
    let raw = grab.image_data().to_vec();
    let (w, h) = (grab.width(), grab.height());
    let ts = grab.camera_timestamp_ns();
    let host_ts = grab.host_datetime_local();
    let cam_name = grab.camera_name().to_string();
    match grab.pixel_format() {
        PixelFormat::BayerRG8 | PixelFormat::BayerRG10 | PixelFormat::BayerRG12 => {
            Frame::new(raw, w, h, ts, host_ts, cam_name)
        }
        fmt => panic!("expected bayer pixel format: {:?}", fmt),
    }
}
