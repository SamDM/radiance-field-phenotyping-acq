use acquisition_common::protocol as wire;
use pylon::cfg::RateControl;
use pylon::{pylon_init, CameraArray};
use time::OffsetDateTime;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use tracing::{debug, info, warn};

use crate::Args;

mod library;
mod naming;
mod preview;
mod ptp;
mod sensor_cfg;

struct MutableServerState {
    pub camera_array: CameraArray,
    pub ptp_state: ptp::PtpIterState,
    pub sensor_cfg_state: sensor_cfg::SensorCfgIterState,
    pub preview_state: preview::PreviewIterState,
    pub prev_wire_status: Option<wire::ServerStatus>,
}

pub fn run(
    args: Args,
    mut cmd_rx: mpsc::UnboundedReceiver<wire::ClientMessage>,
    evt_tx: broadcast::Sender<wire::ServerMessage>,
    preview_tx: broadcast::Sender<Vec<wire::PreviewFrame>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (camera_array, rate_control, _pylon_guard) = start_cameras(&args)?;

    // Built before anything else touches the cameras: a bad marker configuration is a
    // startup failure, not something to discover hours into an unattended run.
    let watcher = acquisition_server::aruco::AutoRecordWatcher::new(
        &args.auto_record_cfg(),
        args.n_cameras,
    )?;

    let mut server_state = MutableServerState {
        camera_array,
        ptp_state: ptp::PtpIterState::new(
            args.ptp_poll_interval_secs,
            args.no_ptp,
            rate_control,
        ),
        sensor_cfg_state: sensor_cfg::SensorCfgIterState::new(
            args.auto_adjust_duration_secs,
            args.auto_adjust_minimize_gain,
            args.auto_adjust_no_boost_framerate,
            rate_control,
        ),
        preview_state: preview::PreviewIterState::new(
            args.n_cameras,
            args.grouper_buffer_len,
            args.output_dir.clone(),
            args.frames,
            args.frame_format,
            args.zoom_size,
            watcher,
        ),
        prev_wire_status: None,
    };

    info!("ready — awaiting start/stop commands over control channel");

    loop {
        match drain_commands(&mut cmd_rx) {
            Some(messages) => {
                process_commands(&messages, &mut server_state, &evt_tx, &preview_tx)?;
                notify_status_change(&messages, &mut server_state, &evt_tx)
            }
            None => break,
        }

        if !server_state.camera_array.is_grabbing() {
            warn!("cameras stopped grabbing unexpectedly");
            break;
        }
    }

    server_state.camera_array.stop_grabbing();
    server_state.camera_array.close();
    Ok(())
}

fn start_cameras(
    args: &Args,
) -> Result<(CameraArray, RateControl, impl Drop), Box<dyn std::error::Error>> {
    let pylon = pylon_init();
    let mut camera_array = CameraArray::new(args.n_cameras);
    camera_array.open();
    for i in 0..args.n_cameras {
        let mut cam = camera_array.get_camera(i);
        let camera_name = cam.device_info().friendly_name()?;
        info!(camera_name, "camera found");
        pylon::cfg::load_user_set(&mut cam, "Default")?;
        cam.set_feature("GevSCPSPacketSize", "8192")?;
    }
    // Must happen before start_grabbing: TriggerMode and TriggerSource are read-only
    // while the camera is grabbing, which is also why the mode is fixed from here on.
    let rate_control = configure_rate_control(&mut camera_array, args);
    camera_array.start_grabbing()?;
    Ok((camera_array, rate_control, pylon))
}

/// Put every camera on the PTP-synchronized periodic-signal trigger, falling back to
/// free run if that is unavailable or opted out of.
///
/// The fallback is all-or-nothing: if any camera fails, every camera is reverted. A
/// mixed array would run at subtly different rates, which is worse than either uniform
/// mode. Never fails — the fallback is always available.
fn configure_rate_control(camera_array: &mut CameraArray, args: &Args) -> RateControl {
    if !args.no_sync_trigger {
        match try_enable_sync_trigger(camera_array, args.framerate) {
            Ok(realized) => {
                info!(
                    requested_fps = args.framerate,
                    realized_fps = realized,
                    "cameras triggered by PTP-synchronized periodic signal"
                );
                return RateControl::PeriodicSignal;
            }
            Err(e) => warn!(
                error = %e,
                "periodic-signal trigger unavailable, falling back to free run \
                 (captures will not be simultaneous across cameras)"
            ),
        }
    }

    // Undo any partial trigger configuration before handing the rate to the limiter,
    // so a camera can't be left triggered but unpaced.
    for i in 0..camera_array.len() {
        let mut cam = camera_array.get_camera(i);
        if let Err(e) = pylon::cfg::disable_sync_trigger(&mut cam) {
            warn!(camera = i, error = %e, "failed to clear trigger configuration");
        }
        if let Err(e) = pylon::cfg::set_acquisition_framerate(&mut cam, args.framerate) {
            warn!(camera = i, error = %e, "failed to set acquisition framerate");
        }
    }
    info!(fps = args.framerate, "cameras free-running on the framerate limiter");
    RateControl::AcquisitionLimiter
}

fn try_enable_sync_trigger(
    camera_array: &mut CameraArray,
    fps: f64,
) -> Result<f64, Box<dyn std::error::Error>> {
    let mut realized = fps;
    for i in 0..camera_array.len() {
        let mut cam = camera_array.get_camera(i);
        realized = pylon::cfg::enable_sync_trigger(&mut cam, fps)?;
    }
    Ok(realized)
}

fn drain_commands(
    cmd_rx: &mut mpsc::UnboundedReceiver<wire::ClientMessage>,
) -> Option<Vec<wire::ClientMessage>> {
    let mut messages = vec![];
    loop {
        match cmd_rx.try_recv() {
            Ok(message) => messages.push(message),
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
        messages,
        &mut server_state.camera_array,
        &mut server_state.sensor_cfg_state,
        server_state.preview_state.is_recording(),
        evt_tx,
    );
    preview::iteration(
        messages,
        &mut server_state.camera_array,
        &mut server_state.preview_state,
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
    let ptp = server_state.ptp_state.ptp_report().clone();
    let activity = server_state
        .preview_state
        .wire_activity(server_state.sensor_cfg_state.is_auto_adjusting());
    let sensor_cfg = server_state.sensor_cfg_state.cfg();
    let recordings = server_state.preview_state.recordings().to_vec();
    let new_status = wire::ServerStatus {
        server_time: OffsetDateTime::now_utc(),
        activity,
        ptp,
        sensor_cfg,
        recordings,
        selected_recording: server_state.preview_state.selected_name().map(str::to_owned),
    };

    let status_changed = server_state.prev_wire_status.as_ref() != Some(&new_status);
    let get_status_requested = messages.len() > 0;

    if status_changed {
        server_state.prev_wire_status = Some(new_status.clone());
        debug!(?new_status, "server status changed");
    }
    if status_changed || get_status_requested {
        let _ = evt_tx.send(wire::ServerMessage::ServerStatus(new_status));
    }
}
