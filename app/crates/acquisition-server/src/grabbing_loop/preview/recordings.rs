use std::collections::HashMap;
use std::path::{Path, PathBuf};

use acquisition_common;
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
use turbojpeg::{Compressor, Image, PixelFormat as TjPixelFormat};

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

use super::super::library::{self, write_metadata};

enum AcquisitionActivity {
    Idle,
    Recording {
        start: OffsetDateTime,
        groups_saved: u32,
    },
}

pub(super) struct RecordingsIterState {
    activity: AcquisitionActivity,
    n_cameras: usize,
    zipper: StreamZipper<Frame<Bayer>>,
    debayer: bool,
    output_path_template: String,
    max_n_frames: u32,
    library: library::RecordingLibrary,
}

impl RecordingsIterState {
    pub(super) fn new(
        n_cameras: usize,
        grouper_buffer_len: usize,
        debayer: bool,
        output_path_template: String,
        max_n_frames: u32,
    ) -> Self {
        let library = library::RecordingLibrary::new(&output_path_template);
        RecordingsIterState {
            activity: AcquisitionActivity::Idle,
            n_cameras,
            zipper: StreamZipper::new(n_cameras, grouper_buffer_len),
            debayer,
            output_path_template,
            max_n_frames,
            library,
        }
    }

    pub(super) fn is_recording(&self) -> bool {
        matches!(self.activity, AcquisitionActivity::Recording { .. })
    }

    pub(super) fn wire_activity(&self) -> wire::Activity {
        match self.activity {
            AcquisitionActivity::Idle => wire::Activity::Idle,
            AcquisitionActivity::Recording { start, groups_saved } => wire::Activity::Recording {
                elapsed: OffsetDateTime::now_utc() - start,
                groups_saved,
                folder: self.recording_folder(start),
                name: self.recording_name(start),
            },
        }
    }

    pub(super) fn past_recordings(&self) -> &[wire::PastRecording] {
        self.library.recordings()
    }

    fn recording_folder(&self, start: OffsetDateTime) -> String {
        let sample_path = expand_path(&self.output_path_template, start, 0, 0);
        sample_path
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn recording_name(&self, start: OffsetDateTime) -> String {
        let folder = self.recording_folder(start);
        let base = library::base_dir_from_template(&self.output_path_template);
        acquisition_common::recording_display_name(&folder, &base)
    }

    fn finish_recording(&mut self, start: OffsetDateTime, ok: bool, evt_tx: &broadcast::Sender<wire::ServerMessage>) {
        let folder = self.recording_folder(start);
        let name = self.recording_name(start);
        let elapsed = OffsetDateTime::now_utc() - start;

        let groups_saved = match self.activity {
            AcquisitionActivity::Recording { groups_saved, .. } => groups_saved,
            _ => 0,
        };

        self.activity = AcquisitionActivity::Idle;

        if groups_saved == 0 {
            warn!("recording not saved: 0 frames captured");
            let _ = evt_tx.send(wire::ServerMessage::notice(
                wire::NoticeSeverity::Warn,
                "recording not saved: 0 frames captured".to_string(),
            ));
            return;
        }

        let verified_ok = if ok {
            let mut missing: Vec<String> = Vec::new();
            for cam_idx in 0..self.n_cameras {
                let first = expand_path(&self.output_path_template, start, cam_idx, 0);
                if !first.exists() {
                    missing.push(format!("cam {cam_idx} first frame: {}", first.display()));
                }
                let last = expand_path(&self.output_path_template, start, cam_idx, groups_saved - 1);
                if !last.exists() {
                    missing.push(format!("cam {cam_idx} last frame: {}", last.display()));
                }
            }

            if missing.is_empty() {
                true
            } else {
                let _ = evt_tx.send(wire::ServerMessage::notice(
                    wire::NoticeSeverity::Error,
                    format!("recording verification failed, missing files:\n{}", missing.join("\n")),
                ));
                false
            }
        } else {
            false
        };

        let lr = wire::PastRecording {
            folder, name, ok: verified_ok, elapsed, groups_saved,
            finished_at: Some(OffsetDateTime::now_utc()),
            rating: None, comment: None,
        };
        write_metadata(&lr.folder, &lr);
        self.library.push(lr);
    }
}

/// Process one iteration of live camera acquisition.
///
/// `in_replay` suppresses preview emission — the camera is still polled to drain
/// the Pylon frame buffer, but previews are served by the replay path instead.
pub(super) fn iteration(
    messages: &[wire::ClientMessage],
    camera_array: &mut CameraArray,
    state: &mut RecordingsIterState,
    is_busy: bool,
    in_replay: bool,
    zoom_requests: &HashMap<String, (f32, f32)>,
    evt_tx: &broadcast::Sender<wire::ServerMessage>,
    preview_tx: &broadcast::Sender<Vec<wire::PreviewFrame>>,
) -> Result<(), Box<dyn std::error::Error>> {
    enum RecordingCommand {
        Start,
        Stop,
    }

    enum MetadataCommand {
        Trash { folder: String },
        Rate { folder: String, rating: u8 },
        Comment { folder: String, comment: String },
    }

    let mut recording_cmds: Vec<(Uuid, RecordingCommand)> = Vec::new();
    let mut metadata_cmds: Vec<(Uuid, MetadataCommand)> = Vec::new();
    for message in messages {
        match &message.command {
            wire::ClientCommand::StartRecording => {
                recording_cmds.push((message.uuid, RecordingCommand::Start));
            }
            wire::ClientCommand::StopRecording => {
                recording_cmds.push((message.uuid, RecordingCommand::Stop));
            }
            wire::ClientCommand::TrashRecording { folder } => {
                metadata_cmds.push((message.uuid, MetadataCommand::Trash { folder: folder.clone() }));
            }
            wire::ClientCommand::RateRecording { folder, rating } => {
                metadata_cmds.push((message.uuid, MetadataCommand::Rate { folder: folder.clone(), rating: *rating }));
            }
            wire::ClientCommand::CommentRecording { folder, comment } => {
                metadata_cmds.push((message.uuid, MetadataCommand::Comment { folder: folder.clone(), comment: comment.clone() }));
            }
            wire::ClientCommand::SetSensorCfg(_)
            | wire::ClientCommand::StartAutoAdjust { .. }
            | wire::ClientCommand::StopAutoAdjust
            | wire::ClientCommand::SetZoom { .. }
            | wire::ClientCommand::SelectRecording { .. }
            | wire::ClientCommand::StartReplay { .. }
            | wire::ClientCommand::StopReplay
            | wire::ClientCommand::PauseReplay
            | wire::ClientCommand::ResumeReplay
            | wire::ClientCommand::RewindReplay
            | wire::ClientCommand::StepReplayForward
            | wire::ClientCommand::StepReplayBackward
            | wire::ClientCommand::SeekReplayToFrame { .. }
            | wire::ClientCommand::GetStatus => {}
        }
    }

    for (uuid, _) in &recording_cmds[..recording_cmds.len().saturating_sub(1)] {
        reject_cmd(evt_tx, *uuid, "superseded by newer recording command");
    }
    if let Some((cmd_uuid, recording_cmd)) = recording_cmds.pop() {
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
                if let AcquisitionActivity::Recording { start, .. } = state.activity {
                    state.finish_recording(start, true, evt_tx);
                    accept_cmd(evt_tx, cmd_uuid, "recording stopped");
                } else {
                    reject_cmd(evt_tx, cmd_uuid, "stop ignored: not recording");
                }
            }
        }
    }

    for (cmd_uuid, meta_cmd) in metadata_cmds {
        match meta_cmd {
            MetadataCommand::Trash { ref folder } => {
                if let Some(rec) = state.library.find_mut(folder) {
                    let name = rec.name.clone();
                    let elapsed = rec.elapsed;
                    let groups_saved = rec.groups_saved;
                    match std::fs::remove_dir_all(folder) {
                        Ok(()) => {
                            state.library.remove(folder);
                            let total_secs = elapsed.whole_seconds();
                            let mins = total_secs / 60;
                            let secs = total_secs % 60;
                            let msg = format!(
                                "recording deleted: {name} ({mins}m{secs:02}s, {groups_saved} frames)"
                            );
                            info!("{msg}");
                            let _ = evt_tx.send(wire::ServerMessage::notice(
                                wire::NoticeSeverity::Info,
                                msg,
                            ));
                            let _ = evt_tx.send(wire::ServerMessage::ack_ok(cmd_uuid));
                        }
                        Err(e) => {
                            reject_cmd(evt_tx, cmd_uuid, &format!("trash failed: {e}"));
                        }
                    }
                } else {
                    reject_cmd(evt_tx, cmd_uuid, "no matching recording for metadata command");
                }
            }
            MetadataCommand::Rate { ref folder, rating } => {
                if !(1..=5).contains(&rating) {
                    reject_cmd(evt_tx, cmd_uuid, "invalid rating (must be 1-5)");
                } else if let Some(lr) = state.library.find_mut(folder) {
                    lr.rating = Some(rating);
                    write_metadata(folder, lr);
                    accept_cmd(evt_tx, cmd_uuid, &format!("rating set to {rating}"));
                } else {
                    reject_cmd(evt_tx, cmd_uuid, "no matching recording for metadata command");
                }
            }
            MetadataCommand::Comment { ref folder, ref comment } => {
                if let Some(lr) = state.library.find_mut(folder) {
                    lr.comment = Some(comment.clone());
                    write_metadata(folder, lr);
                    accept_cmd(evt_tx, cmd_uuid, "comment saved");
                } else {
                    reject_cmd(evt_tx, cmd_uuid, "no matching recording for metadata command");
                }
            }
        }
    }

    match camera_array.retrieve_result(100) {
        Some(Ok(grab)) => {
            let cam_idx = grab.camera_index();
            let frame = bayer_frame_from_grab(&grab);
            let ts = frame.camera_timestamp_ns();
            for result in state.zipper.push(frame, cam_idx, ts) {
                let saved = match process_grouped_frames_result(
                    result,
                    &state.output_path_template,
                    state.debayer,
                    &state.activity,
                    in_replay,
                    zoom_requests,
                    preview_tx,
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(error = %e, "recording write failed, stopping recording");
                        if let AcquisitionActivity::Recording { start, .. } = state.activity {
                            state.finish_recording(start, false, evt_tx);
                        }
                        let _ = evt_tx.send(wire::ServerMessage::notice(
                            wire::NoticeSeverity::Error,
                            format!("recording stopped: write failed: {e}"),
                        ));
                        continue;
                    }
                };
                if let Some(new_count) = saved {
                    if new_count >= state.max_n_frames {
                        info!(
                            saved_groups = new_count,
                            "recording stopped (frame limit reached)"
                        );
                        if let AcquisitionActivity::Recording { start, .. } = state.activity {
                            state.finish_recording(start, true, evt_tx);
                        }
                        let _ = evt_tx.send(wire::ServerMessage::notice(
                            wire::NoticeSeverity::Info,
                            format!("recording stopped: frame limit reached ({new_count} groups saved)"),
                        ));
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
            let _ = evt_tx.send(wire::ServerMessage::notice(
                wire::NoticeSeverity::Error,
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

fn process_grouped_frames_result(
    result: Result<Zipped<Frame<Bayer>>, ZipError<Frame<Bayer>>>,
    output_path_template: &str,
    debayer: bool,
    camera_state: &AcquisitionActivity,
    in_replay: bool,
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

            if !in_replay {
                let previews = make_jpeg_previews(&frames, zoom_requests);
                let _ = preview_tx.send(previews);
            }

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

pub(super) fn extract_zoom(frame: &AnyFrame, rel: (f32, f32)) -> wire::Zoom {
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

pub(super) fn make_jpeg_previews(
    frames: &[AnyFrame],
    zoom_requests: &HashMap<String, (f32, f32)>,
) -> Vec<wire::PreviewFrame> {
    let mut compressor = Compressor::new().expect("turbojpeg compressor");
    compressor.set_quality(75).expect("set jpeg quality");
    frames
        .iter()
        .map(|frame| {
            let zoom = zoom_requests
                .get(frame.camera_name())
                .map(|&rel| extract_zoom(frame, rel));
            let preview = make_preview(frame).into_color();
            let jpeg = compressor
                .compress_to_vec(Image {
                    pixels: preview.data.as_slice(),
                    width: preview.width as usize,
                    pitch: preview.width as usize * 3,
                    height: preview.height as usize,
                    format: TjPixelFormat::RGB,
                })
                .expect("jpeg compression");
            wire::PreviewFrame {
                jpeg,
                width: preview.width,
                height: preview.height,
                camera_name: preview.camera_name,
                camera_timestamp_ns: preview.camera_timestamp_ns,
                host_timestamp: preview.host_timestamp,
                zoom,
            }
        })
        .collect()
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
