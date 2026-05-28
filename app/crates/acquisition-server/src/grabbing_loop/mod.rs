use acquisition_common::protocol as wire;
use pylon::{pylon_init, CameraArray};
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use tracing::{debug, info, warn};

use crate::Args;

mod acquisition;
mod ptp;
mod sensor_cfg;

struct MutableServerState {
    pub camera_array: CameraArray,
    pub ptp_state: ptp::PtpIterState,
    pub sensor_cfg_state: sensor_cfg::SensorCfgIterState,
    pub acquisition_state: acquisition::AcquisitionIterState,
    pub prev_wire_status: Option<wire::ServerStatus>,
}

pub fn run(
    args: Args,
    mut cmd_rx: mpsc::UnboundedReceiver<wire::ClientMessage>,
    evt_tx: broadcast::Sender<wire::ServerMessage>,
    preview_tx: broadcast::Sender<Vec<wire::PreviewFrame>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (camera_array, _pylon_guard) = start_cameras(&args)?;

    let mut server_state = MutableServerState {
        camera_array,
        ptp_state: ptp::PtpIterState::new(args.ptp_poll_interval_secs),
        sensor_cfg_state: sensor_cfg::SensorCfgIterState::new(
            args.auto_adjust_duration_secs,
            args.auto_adjust_minimize_gain,
            args.auto_adjust_no_boost_framerate,
        ),
        acquisition_state: acquisition::AcquisitionIterState::new(
            args.n_cameras,
            args.grouper_buffer_len,
            args.debayer,
            args.output.clone(),
            args.frames,
        ),
        prev_wire_status: None,
    };

    info!("ready — awaiting start/stop commands over control channel");

    // Main application loop
    // Most time should be spent here
    loop {
        match drain_commands(&mut cmd_rx) {
            // Channel open with messages from client(s): process them, then notify new status
            // Note that `messages` will be empty most of the time
            Some(messages) => {
                process_commands(&messages, &mut server_state, &evt_tx, &preview_tx)?;
                notify_status_change(&messages, &mut server_state, &evt_tx)
            }
            // Channel closed: this is the happy path shutdown signal
            None => break, // TODO: broadcast shutdown
        }

        if !server_state.camera_array.is_grabbing() {
            warn!("cameras stopped grabbing unexpectedly");
            break; // TODO: broadcast application crashed
        }
    }

    server_state.camera_array.stop_grabbing();
    server_state.camera_array.close();
    Ok(())
}

fn start_cameras(args: &Args) -> Result<(CameraArray, impl Drop), Box<dyn std::error::Error>> {
    let pylon = pylon_init();
    let mut camera_array = CameraArray::new(args.n_cameras);
    camera_array.open();
    for i in 0..args.n_cameras {
        let mut cam = camera_array.get_camera(i);
        let camera_name = cam.device_info().friendly_name()?;
        info!(camera_name, "camera found");
        pylon::cfg::load_user_set(&mut cam, "Default")?;
        cam.set_feature("GevSCPSPacketSize", "8192")?;
        pylon::cfg::set_acquisition_framerate(&mut cam, args.framerate)?;
    }
    camera_array.start_grabbing()?;
    Ok((camera_array, pylon))
}

/// Drains all pending commands from the channel.
fn drain_commands(
    cmd_rx: &mut mpsc::UnboundedReceiver<wire::ClientMessage>,
) -> Option<Vec<wire::ClientMessage>> {
    let mut messages = vec![];
    loop {
        match cmd_rx.try_recv() {
            Ok(message) => {
                messages.push(message);
            }
            Err(TryRecvError::Empty) => return Some(messages),
            Err(TryRecvError::Disconnected) => {
                info!("command channel closed, shutting down acquisition loop");
                return None;
            }
        }
    }
}

fn process_commands(
    messages: &[wire::ClientMessage],
    server_state: &mut MutableServerState,
    evt_tx: &broadcast::Sender<wire::ServerMessage>,
    preview_tx: &broadcast::Sender<Vec<wire::PreviewFrame>>,
) -> Result<(), Box<dyn std::error::Error>> {
    ptp::iteration(
        &server_state.camera_array,
        &mut server_state.ptp_state,
        evt_tx,
    );
    sensor_cfg::iteration(
        &messages,
        &mut server_state.camera_array,
        &mut server_state.sensor_cfg_state,
        server_state.acquisition_state.is_recording(),
        evt_tx,
    );
    acquisition::iteration(
        &messages,
        &mut server_state.camera_array,
        &mut server_state.acquisition_state,
        server_state.sensor_cfg_state.is_auto_adjusting(),
        evt_tx,
        preview_tx,
    )?;
    Ok(())
}

fn notify_status_change(
    messages: &[wire::ClientMessage],
    server_state: &mut MutableServerState,
    evt_tx: &broadcast::Sender<wire::ServerMessage>,
) {
    // Construct new status
    let ptp = server_state.ptp_state.ptp_report().clone();
    let activity = if server_state.sensor_cfg_state.is_auto_adjusting() {
        wire::Activity::AutoAdjusting
    } else if server_state.acquisition_state.is_recording() {
        wire::Activity::Recording
    } else {
        wire::Activity::Idle
    };
    let sensor_cfg = server_state.sensor_cfg_state.cfg();
    let new_status = wire::ServerStatus { activity, ptp, sensor_cfg };

    // See if broacast needed
    let status_changed = server_state.prev_wire_status.as_ref() != Some(&new_status);
    // If any message was sent be a client, we re-emit the status.
    // This handles cases where the client attempted a status changed that was not possible
    // (e.g. setting and invalid sensor value)
    // let get_status_requested = messages.iter().any(|m| matches!(m.command, wire::ClientCommand::GetStatus));
    let get_status_requested = messages.len() > 0;

    if status_changed {
        server_state.prev_wire_status = Some(new_status.clone());
        debug!(?new_status, "server status changed");
    }
    if status_changed || get_status_requested {
        let _ = evt_tx.send(wire::ServerMessage::ServerStatus(new_status));
    }
}
