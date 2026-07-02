use std::collections::HashMap;
use std::path::{Path, PathBuf};

use acquisition_common::debayer::debayer_crop;
use acquisition_common::protocol as wire;
use acquisition_common::{AnyFrame, Bayer, Frame};
use image::imageops::FilterType;
use image::RgbImage;
use pylon::{CameraArray, GrabResult, PixelFormat};
use time::OffsetDateTime;
use tokio::sync::broadcast;
use tracing::{info, warn};
use turbojpeg::{Compressor, Image, PixelFormat as TjPixelFormat, Subsamp};

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

use super::super::library::{self, write_metadata, RecordingEntry};
use super::super::naming::{expand_dir, frame_filename};

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
    output_dir_template: String,
    max_n_frames: u32,
    library: library::RecordingLibrary,
    frame_format: crate::FrameFormat,
}

impl RecordingsIterState {
    pub(super) fn new(
        n_cameras: usize,
        grouper_buffer_len: usize,
        output_dir_template: String,
        max_n_frames: u32,
        frame_format: crate::FrameFormat,
    ) -> Self {
        let library = library::RecordingLibrary::new(&output_dir_template);
        RecordingsIterState {
            activity: AcquisitionActivity::Idle,
            n_cameras,
            zipper: StreamZipper::new(n_cameras, grouper_buffer_len),
            output_dir_template,
            max_n_frames,
            library,
            frame_format,
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
                name: self.recording_name(start),
            },
        }
    }

    pub(super) fn past_recordings(&self) -> Vec<wire::PastRecording> {
        self.library.wire_recordings()
    }

    pub(super) fn find_by_name(&self, name: &str) -> Option<&RecordingEntry> {
        self.library.find_by_name(name)
    }

    fn recording_folder(&self, start: OffsetDateTime) -> PathBuf {
        expand_dir(&self.output_dir_template, start)
    }

    fn recording_name(&self, start: OffsetDateTime) -> String {
        let folder = self.recording_folder(start);
        let base = library::base_dir_from_template(&self.output_dir_template);
        acquisition_common::recording_display_name(&folder.to_string_lossy(), &base)
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
                let first = folder.join(frame_filename(cam_idx, 0, self.frame_format.extension()));
                if !first.exists() {
                    missing.push(format!("cam {cam_idx} first frame: {}", first.display()));
                }
                let last = folder.join(frame_filename(cam_idx, groups_saved - 1, self.frame_format.extension()));
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

        let lr = RecordingEntry {
            path: folder, name, ok: verified_ok, elapsed, groups_saved,
            finished_at: Some(OffsetDateTime::now_utc()),
            rating: None, comment: None,
        };
        write_metadata(&lr.path, &lr);
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
        Trash { name: String },
        Rate { name: String, rating: u8 },
        Comment { name: String, comment: String },
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
            wire::ClientCommand::TrashRecording { name } => {
                metadata_cmds.push((message.uuid, MetadataCommand::Trash { name: name.clone() }));
            }
            wire::ClientCommand::RateRecording { name, rating } => {
                metadata_cmds.push((message.uuid, MetadataCommand::Rate { name: name.clone(), rating: *rating }));
            }
            wire::ClientCommand::CommentRecording { name, comment } => {
                metadata_cmds.push((message.uuid, MetadataCommand::Comment { name: name.clone(), comment: comment.clone() }));
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
            MetadataCommand::Trash { ref name } => {
                if let Some(rec) = state.library.find_by_name(name) {
                    let path = rec.path.clone();
                    let elapsed = rec.elapsed;
                    let groups_saved = rec.groups_saved;
                    match std::fs::remove_dir_all(&path) {
                        Ok(()) => {
                            state.library.remove(name);
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
            MetadataCommand::Rate { ref name, rating } => {
                if !(1..=5).contains(&rating) {
                    reject_cmd(evt_tx, cmd_uuid, "invalid rating (must be 1-5)");
                } else if let Some(lr) = state.library.find_by_name_mut(name) {
                    lr.rating = Some(rating);
                    write_metadata(&lr.path, lr);
                    accept_cmd(evt_tx, cmd_uuid, &format!("rating set to {rating}"));
                } else {
                    reject_cmd(evt_tx, cmd_uuid, "no matching recording for metadata command");
                }
            }
            MetadataCommand::Comment { ref name, ref comment } => {
                if let Some(lr) = state.library.find_by_name_mut(name) {
                    lr.comment = Some(comment.clone());
                    write_metadata(&lr.path, lr);
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
                    &state.output_dir_template,
                    &state.activity,
                    in_replay,
                    zoom_requests,
                    preview_tx,
                    state.frame_format,
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

/// Debayers `frame` to RGB and JPEG-compresses it with "SfM-safe" settings: high
/// quality and no chroma subsampling (4:4:4). Lossy compression must never run
/// directly on a raw Bayer mosaic — its checkerboard color-filter pattern isn't real
/// image content, and turbojpeg's DCT blocks / chroma subsampling would bake mosaic
/// artifacts into the debayered result. Recorded frames feed 3D reconstruction, where
/// even subtle, position-correlated artifacts can bias feature matching.
fn save_frame_jpeg_sfm_safe(frame: &AnyFrame, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let color = frame.clone().into_color();

    let mut compressor = Compressor::new()?;
    compressor.set_quality(95)?;
    compressor.set_subsamp(Subsamp::None)?;
    let jpeg = compressor.compress_to_vec(Image {
        pixels: color.data.as_slice(),
        width: color.width as usize,
        pitch: color.width as usize * 3,
        height: color.height as usize,
        format: TjPixelFormat::RGB,
    })?;

    std::fs::write(path, &jpeg)?;
    std::fs::write(path.with_extension("json"), serde_json::to_string_pretty(&color.metadata())?)?;
    Ok(())
}

fn process_grouped_frames_result(
    result: Result<Zipped<Frame<Bayer>>, ZipError<Frame<Bayer>>>,
    output_dir_template: &str,
    camera_state: &AcquisitionActivity,
    in_replay: bool,
    zoom_requests: &HashMap<String, (f32, f32)>,
    preview_tx: &broadcast::Sender<Vec<wire::PreviewFrame>>,
    frame_format: crate::FrameFormat,
) -> Result<Option<u32>, Box<dyn std::error::Error>> {
    match result {
        Ok(group) => {
            // Always save the raw Bayer mosaic for tiff/png (1 byte/pixel vs. 3 for
            // debayered RGB) — downstream processing debayers from storage. Live
            // preview below already debayers on the fly regardless of what gets saved.
            let frames: Vec<AnyFrame> = group
                .items
                .into_iter()
                .map(|(f, _ts)| f.into_any())
                .collect();

            if !in_replay {
                let previews = make_jpeg_previews(&frames, zoom_requests);
                let _ = preview_tx.send(previews);
            }

            if let AcquisitionActivity::Recording {
                start,
                groups_saved,
            } = camera_state
            {
                let dir = expand_dir(output_dir_template, *start);
                std::fs::create_dir_all(&dir)?;
                let paths: Vec<PathBuf> = (0..frames.len())
                    .map(|cam_idx| dir.join(frame_filename(cam_idx, *groups_saved, frame_format.extension())))
                    .collect();

                // Every camera's encode is independent (no shared state), and even the
                // fast lossless paths leave most cores idle — so save all cameras
                // concurrently rather than one after another. Scoped threads join
                // before this call returns, so there's no persistent worker/queue:
                // just a bounded fan-out for this one frame group.
                let results: Vec<Result<(), String>> = std::thread::scope(|scope| {
                    let handles: Vec<_> = frames
                        .iter()
                        .zip(paths.iter())
                        .map(|(frame, path)| {
                            scope.spawn(move || {
                                if frame_format.is_lossy() {
                                    save_frame_jpeg_sfm_safe(frame, path).map_err(|e| e.to_string())
                                } else {
                                    frame.save(path).map_err(|e| e.to_string())
                                }
                            })
                        })
                        .collect();
                    handles.into_iter().map(|h| h.join().unwrap()).collect()
                });

                for (cam_idx, (result, frame)) in results.into_iter().zip(frames.iter()).enumerate() {
                    result.map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
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
