use std::collections::HashMap;

use acquisition_common::protocol as wire;
use pylon::CameraArray;
use tokio::sync::broadcast;
use tracing::{info, warn};

mod recordings;
mod replay;

pub(super) struct PreviewIterState {
    recordings: recordings::RecordingsIterState,
    replay: Option<replay::ReplayIterState>,
    zoom_requests: HashMap<String, (f32, f32)>,
    selected_name: Option<String>,
}

impl PreviewIterState {
    pub(super) fn new(
        n_cameras: usize,
        grouper_buffer_len: usize,
        output_dir_template: String,
        max_n_frames: u32,
        frame_format: crate::FrameFormat,
    ) -> Self {
        PreviewIterState {
            recordings: recordings::RecordingsIterState::new(
                n_cameras,
                grouper_buffer_len,
                output_dir_template,
                max_n_frames,
                frame_format,
            ),
            replay: None,
            zoom_requests: HashMap::new(),
            selected_name: None,
        }
    }

    pub(super) fn is_recording(&self) -> bool {
        self.recordings.is_recording()
    }

    pub(super) fn recordings(&self) -> Vec<wire::PastRecording> {
        self.recordings.past_recordings()
    }

    pub(super) fn selected_name(&self) -> Option<&str> {
        self.selected_name.as_deref()
    }

    /// Returns the current activity, accounting for replay, auto-adjusting, and live states.
    pub(super) fn wire_activity(&self, auto_adjusting: bool) -> wire::Activity {
        if let Some(ref rs) = self.replay {
            rs.wire_activity()
        } else if auto_adjusting {
            wire::Activity::AutoAdjusting
        } else {
            self.recordings.wire_activity()
        }
    }
}

/// Process one preview iteration: handle mode/zoom commands, drain camera, emit previews.
pub(super) fn iteration(
    messages: &[wire::ClientMessage],
    camera_array: &mut CameraArray,
    state: &mut PreviewIterState,
    is_busy: bool,
    evt_tx: &broadcast::Sender<wire::ServerMessage>,
    preview_tx: &broadcast::Sender<Vec<wire::PreviewFrame>>,
) -> Result<(), Box<dyn std::error::Error>> {
    for message in messages {
        match &message.command {
            wire::ClientCommand::SetZoom { camera_name, position } => {
                match position {
                    Some(pos) => {
                        state.zoom_requests.insert(camera_name.clone(), *pos);
                        info!("zoom set");
                    }
                    None => {
                        state.zoom_requests.remove(camera_name);
                        info!("zoom cleared");
                    }
                }
                // While paused, replay won't emit frames on its own — re-emit the current
                // frame immediately so the zoom crop updates without unpausing.
                if let Some(ref rs) = state.replay {
                    if rs.is_paused() {
                        let paths = rs.current_frame_paths().to_vec();
                        replay::emit_frame(&paths, &state.zoom_requests, preview_tx);
                    }
                }
                let _ = evt_tx.send(wire::ServerMessage::ack_ok(message.uuid));
            }
            wire::ClientCommand::SelectRecording { name } => {
                state.selected_name = Some(name.clone());
                let already_replaying_this = state.replay.as_ref()
                    .map_or(false, |rs| rs.name() == name);
                if state.replay.is_some() && !already_replaying_this {
                    let was_paused = state.replay.as_ref().map_or(false, |rs| rs.is_paused());
                    match state.recordings.find_by_name(name) {
                        Some(rec) => {
                            match replay::ReplayIterState::new(&rec.path, &rec.name) {
                                Ok(mut rs) => {
                                    if was_paused {
                                        rs.pause();
                                        let paths = rs.current_frame_paths().to_vec();
                                        replay::emit_frame(&paths, &state.zoom_requests, preview_tx);
                                    }
                                    state.replay = Some(rs);
                                    info!(name, "replay restarted for new selection");
                                }
                                Err(e) => {
                                    warn!(error = %e, "replay restart failed, stopping replay");
                                    state.replay = None;
                                }
                            }
                        }
                        None => {
                            warn!(name, "select recording: no matching recording, stopping replay");
                            state.replay = None;
                        }
                    }
                }
                let _ = evt_tx.send(wire::ServerMessage::ack_ok(message.uuid));
            }
            wire::ClientCommand::StartReplay { name } => {
                if state.recordings.is_recording() {
                    warn!("replay start rejected: recording in progress");
                    let _ = evt_tx.send(wire::ServerMessage::ack_err(
                        message.uuid,
                        "cannot start replay while recording".to_string(),
                    ));
                    continue;
                }
                match state.recordings.find_by_name(name) {
                    Some(rec) => match replay::ReplayIterState::new(&rec.path, &rec.name) {
                        Ok(rs) => {
                            state.replay = Some(rs);
                            state.selected_name = Some(name.clone());
                            info!(name, "replay started");
                            let _ = evt_tx.send(wire::ServerMessage::ack_ok(message.uuid));
                        }
                        Err(e) => {
                            warn!(error = %e, "replay start failed");
                            let _ = evt_tx.send(wire::ServerMessage::ack_err(message.uuid, e));
                        }
                    },
                    None => {
                        let _ = evt_tx.send(wire::ServerMessage::ack_err(
                            message.uuid,
                            "no matching recording".to_string(),
                        ));
                    }
                }
            }
            wire::ClientCommand::StopReplay => {
                state.replay = None;
                info!("replay stopped");
                let _ = evt_tx.send(wire::ServerMessage::ack_ok(message.uuid));
            }
            wire::ClientCommand::PauseReplay => {
                if let Some(ref mut rs) = state.replay {
                    rs.pause();
                    let paths = rs.current_frame_paths().to_vec();
                    replay::emit_frame(&paths, &state.zoom_requests, preview_tx);
                    let _ = evt_tx.send(wire::ServerMessage::ack_ok(message.uuid));
                } else {
                    let _ = evt_tx.send(wire::ServerMessage::ack_err(message.uuid, "not replaying".to_string()));
                }
            }
            wire::ClientCommand::ResumeReplay => {
                if let Some(ref mut rs) = state.replay {
                    rs.resume();
                    let _ = evt_tx.send(wire::ServerMessage::ack_ok(message.uuid));
                } else {
                    let _ = evt_tx.send(wire::ServerMessage::ack_err(message.uuid, "not replaying".to_string()));
                }
            }
            wire::ClientCommand::RewindReplay => {
                if let Some(ref mut rs) = state.replay {
                    rs.rewind();
                    let paths = rs.current_frame_paths().to_vec();
                    replay::emit_frame(&paths, &state.zoom_requests, preview_tx);
                    let _ = evt_tx.send(wire::ServerMessage::ack_ok(message.uuid));
                } else {
                    let _ = evt_tx.send(wire::ServerMessage::ack_err(message.uuid, "not replaying".to_string()));
                }
            }
            wire::ClientCommand::StepReplayForward => {
                if let Some(ref mut rs) = state.replay {
                    let paths = rs.step_forward().to_vec();
                    replay::emit_frame(&paths, &state.zoom_requests, preview_tx);
                    let _ = evt_tx.send(wire::ServerMessage::ack_ok(message.uuid));
                } else {
                    let _ = evt_tx.send(wire::ServerMessage::ack_err(message.uuid, "not replaying".to_string()));
                }
            }
            wire::ClientCommand::StepReplayBackward => {
                if let Some(ref mut rs) = state.replay {
                    let paths = rs.step_backward().to_vec();
                    replay::emit_frame(&paths, &state.zoom_requests, preview_tx);
                    let _ = evt_tx.send(wire::ServerMessage::ack_ok(message.uuid));
                } else {
                    let _ = evt_tx.send(wire::ServerMessage::ack_err(message.uuid, "not replaying".to_string()));
                }
            }
            wire::ClientCommand::SeekReplayToFrame { frame_idx } => {
                if let Some(ref mut rs) = state.replay {
                    let paths = rs.seek_to_frame(*frame_idx).to_vec();
                    replay::emit_frame(&paths, &state.zoom_requests, preview_tx);
                    let _ = evt_tx.send(wire::ServerMessage::ack_ok(message.uuid));
                } else {
                    let _ = evt_tx.send(wire::ServerMessage::ack_err(message.uuid, "not replaying".to_string()));
                }
            }
            wire::ClientCommand::GetStatus => {
                // A freshly connected client sends GetStatus on connect. If we're paused,
                // the preview_tx won't emit another frame on its own — re-emit the current
                // frame so the new subscriber gets an image immediately.
                if let Some(ref rs) = state.replay {
                    if rs.is_paused() {
                        let paths = rs.current_frame_paths().to_vec();
                        replay::emit_frame(&paths, &state.zoom_requests, preview_tx);
                    }
                }
            }
            wire::ClientCommand::StartRecording => {
                // Recording always wins: stop any active replay immediately.
                if state.replay.take().is_some() {
                    info!("replay stopped: recording started");
                }
                // StartRecording is then handled by recordings::iteration below.
            }
            _ => {}
        }
    }

    let in_replay = state.replay.is_some();

    // Snapshot name order before iteration so we can find the adjacent selection if a
    // recording is trashed (the library removes it before we can query its former index).
    let pre_names: Vec<String> = state
        .recordings
        .past_recordings()
        .iter()
        .map(|r| r.name.clone())
        .collect();

    // Always drain the camera frame buffer, even during replay, to prevent
    // Pylon SDK buffer buildup that would add latency when live view resumes.
    recordings::iteration(
        messages,
        camera_array,
        &mut state.recordings,
        is_busy,
        in_replay,
        &state.zoom_requests,
        evt_tx,
        preview_tx,
    )?;

    // Finds the nearest still-existing neighbor of `name` in the pre-iteration name
    // order, used to pick a fallback when `name` was just trashed.
    let nearest_neighbor = |name: &str| -> Option<String> {
        pre_names.iter().position(|n| n == name).and_then(|i| {
            // Prefer the previous recording; fall back to what is now at the same index.
            let prev = i.checked_sub(1).and_then(|j| pre_names.get(j));
            let next = pre_names.get(i + 1);
            (prev.or(next)).and_then(|name| {
                if state.recordings.past_recordings().iter().any(|r| &r.name == name) {
                    Some(name.clone())
                } else {
                    None
                }
            })
        })
    };

    // If the selected recording was trashed, advance to the nearest remaining neighbor.
    // (This is display-only bookkeeping for the dropdown; the actual replay engine
    // below is keyed off its own name, since StartReplay can diverge from selected_name.)
    if let Some(ref sel) = state.selected_name.clone() {
        let still_exists = state.recordings.past_recordings().iter().any(|r| &r.name == sel);
        if !still_exists {
            state.selected_name = nearest_neighbor(sel);
        }
    }

    // If the recording actively being replayed was trashed, restart replay against the
    // nearest neighbor (or stop it if none remains), rather than leaving a stale
    // ReplayIterState pointing at deleted files.
    if let Some(replaying_name) = state.replay.as_ref().map(|rs| rs.name().to_owned()) {
        let still_exists = state.recordings.past_recordings().iter().any(|r| r.name == replaying_name);
        if !still_exists {
            let candidate = nearest_neighbor(&replaying_name);
            let was_paused = state.replay.as_ref().map_or(false, |rs| rs.is_paused());
            match candidate.as_deref().and_then(|name| state.recordings.find_by_name(name)) {
                Some(rec) => match replay::ReplayIterState::new(&rec.path, &rec.name) {
                    Ok(mut rs) => {
                        if was_paused {
                            rs.pause();
                            let paths = rs.current_frame_paths().to_vec();
                            replay::emit_frame(&paths, &state.zoom_requests, preview_tx);
                        }
                        info!(name = %rec.name, "replay restarted after playing recording was trashed");
                        state.replay = Some(rs);
                    }
                    Err(e) => {
                        warn!(error = %e, "replay restart failed after trash, stopping replay");
                        state.replay = None;
                    }
                },
                None => {
                    info!("replay stopped: trashed recording had no remaining neighbor");
                    state.replay = None;
                }
            }
            state.selected_name = candidate;
        }
    }

    if let Some(ref mut rs) = state.replay {
        replay::iteration(rs, &state.zoom_requests, preview_tx);
    }

    Ok(())
}
