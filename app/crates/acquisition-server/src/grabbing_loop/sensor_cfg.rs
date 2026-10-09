use std::time::Duration;

use acquisition_common::protocol as wire;
use pylon::cfg::RateControl;
use pylon::CameraArray;
use tokio::sync::broadcast;
use tracing::{info, warn};
use uuid::Uuid;

enum SensorCfgActivity {
    Unitialized,
    Unknown,
    Set(wire::SensorCfg),
    AutoAdjusting {
        prev: wire::SensorCfg,
        handle: pylon::cfg::AutoAdjustHandle,
    },
    RestoreFailed,
}

pub(super) struct SensorCfgIterState {
    cfg_values: SensorCfgActivity,
    auto_adjust_duration_secs: u64,
    /// What the startup auto-adjust aims for; client-started ones bring their own.
    startup_target_brightness: f64,
    auto_adjust_minimize_gain: bool,
    auto_adjust_no_boost_framerate: bool,
    /// Settings fixed on the command line. Applied at startup; the startup auto-adjust
    /// tunes only the ones left `None`.
    fixed: wire::SensorCfg,
    /// Which camera feature the framerate setting has to be written to and read back
    /// from. Fixed at startup; see [`RateControl`].
    rate_control: RateControl,
}

impl SensorCfgIterState {
    pub(super) fn new(
        auto_adjust_duration_secs: u64,
        startup_target_brightness: f64,
        auto_adjust_minimize_gain: bool,
        auto_adjust_no_boost_framerate: bool,
        fixed: wire::SensorCfg,
        rate_control: RateControl,
    ) -> Self {
        SensorCfgIterState {
            cfg_values: SensorCfgActivity::Unitialized,
            auto_adjust_duration_secs,
            startup_target_brightness,
            auto_adjust_minimize_gain,
            auto_adjust_no_boost_framerate,
            fixed,
            rate_control,
        }
    }

    pub fn cfg(&self) -> wire::SensorCfg {
        match &self.cfg_values {
            SensorCfgActivity::Set(cfg) => cfg.clone(),
            SensorCfgActivity::AutoAdjusting { prev, handle } => wire::SensorCfg {
                exposure_us: if handle.tunes_exposure() {
                    None
                } else {
                    prev.exposure_us
                },
                gain: if handle.tunes_gain() { None } else { prev.gain },
                white_balance: if handle.tunes_whitebalance() {
                    None
                } else {
                    prev.white_balance
                },
                framerate: prev.framerate,
            },
            SensorCfgActivity::Unitialized
            | SensorCfgActivity::Unknown
            | SensorCfgActivity::RestoreFailed => wire::SensorCfg::default(),
        }
    }

    pub(super) fn is_auto_adjusting(&self) -> bool {
        matches!(self.cfg_values, SensorCfgActivity::AutoAdjusting { .. })
    }
}

pub(super) fn iteration(
    messages: &[wire::ClientMessage],
    camera_array: &mut CameraArray,
    state: &mut SensorCfgIterState,
    is_busy: bool,
    evt_tx: &broadcast::Sender<wire::ServerMessage>,
) {
    handle_unitialized(camera_array, state, is_busy, evt_tx);
    handle_commands(messages, camera_array, state, is_busy, evt_tx);
    auto_adjust_tick(camera_array, state, evt_tx);
    handle_unknown_settings(camera_array, state, evt_tx);
}

fn sensor_cfg_ok(evt_tx: &broadcast::Sender<wire::ServerMessage>, cmd_uuid: Uuid) {
    let _ = evt_tx.send(wire::ServerMessage::ack_ok(cmd_uuid));
}

fn sensor_cfg_rejected(
    evt_tx: &broadcast::Sender<wire::ServerMessage>,
    cmd_uuid: Option<Uuid>,
    reason: String,
) {
    warn!("{reason}");
    if let Some(cmd_uuid) = cmd_uuid {
        let _ = evt_tx.send(wire::ServerMessage::ack_err(cmd_uuid, reason));
    } else {
        let _ = evt_tx.send(wire::ServerMessage::notice(wire::NoticeSeverity::Error, reason));
    }
}

fn sensor_cfg_failed(
    cfg_values: &mut SensorCfgActivity,
    evt_tx: &broadcast::Sender<wire::ServerMessage>,
    cmd_uuid: Option<Uuid>,
    reason: String,
) {
    *cfg_values = SensorCfgActivity::Unknown;
    sensor_cfg_rejected(evt_tx, cmd_uuid, reason);
}

fn handle_commands(
    messages: &[wire::ClientMessage],
    camera_array: &mut CameraArray,
    state: &mut SensorCfgIterState,
    is_busy: bool,
    evt_tx: &broadcast::Sender<wire::ServerMessage>,
) {
    for message in messages {
        match &message.command {
            wire::ClientCommand::SetSensorCfg(settings) => {
                if is_busy || state.is_auto_adjusting() {
                    sensor_cfg_rejected(
                        evt_tx,
                        Some(message.uuid),
                        "set-camera-settings ignored: camera busy".into(),
                    );
                } else {
                    match apply_settings(camera_array, settings, state.rate_control) {
                        Ok(merged_settings) => {
                            state.cfg_values = SensorCfgActivity::Set(merged_settings);
                            sensor_cfg_ok(evt_tx, message.uuid);
                        }
                        Err(e) => sensor_cfg_failed(
                            &mut state.cfg_values,
                            evt_tx,
                            Some(message.uuid),
                            format!("failed to apply camera settings: {e}"),
                        ),
                    }
                }
            }
            wire::ClientCommand::StartAutoAdjust {
                target_brightness,
                gain_limit,
                exposure_limit_us,
                auto_whitebalance,
            } => {
                if is_busy || state.is_auto_adjusting() {
                    sensor_cfg_rejected(
                        evt_tx,
                        Some(message.uuid),
                        "auto-adjust ignored: camera busy".into(),
                    );
                } else {
                    start_auto_adjust(
                        camera_array,
                        state,
                        *target_brightness,
                        gain_limit.map_or(pylon::cfg::AutoParam::NoAdjust, |l| {
                            pylon::cfg::AutoParam::Adjust(Some(l))
                        }),
                        exposure_limit_us.map_or(pylon::cfg::AutoParam::NoAdjust, |l| {
                            pylon::cfg::AutoParam::Adjust(Some(l))
                        }),
                        *auto_whitebalance,
                        Some(message.uuid),
                        evt_tx,
                    );
                }
            }
            wire::ClientCommand::StopAutoAdjust => {
                if let SensorCfgActivity::AutoAdjusting { .. } = &state.cfg_values {
                    let old = std::mem::replace(&mut state.cfg_values, SensorCfgActivity::Unknown);
                    let SensorCfgActivity::AutoAdjusting { handle, .. } = old else {
                        unreachable!()
                    };
                    abort_auto_adjust(
                        handle,
                        camera_array,
                        &mut state.cfg_values,
                        evt_tx,
                        Some(message.uuid),
                        "auto-adjust stopped by user".to_string(),
                    );
                } else {
                    sensor_cfg_rejected(
                        evt_tx,
                        Some(message.uuid),
                        "stop auto-adjust ignored: not running".into(),
                    );
                }
            }
            _ => {}
        }
    }
}

fn auto_adjust_tick(
    camera_array: &mut CameraArray,
    state: &mut SensorCfgIterState,
    evt_tx: &broadcast::Sender<wire::ServerMessage>,
) {
    let tick_result = if let SensorCfgActivity::AutoAdjusting { handle, .. } = &mut state.cfg_values
    {
        handle.tick(camera_array)
    } else {
        return;
    };
    match tick_result {
        Ok(pylon::cfg::AutoAdjustTick::Converging) => {}
        Ok(pylon::cfg::AutoAdjustTick::Converged) => {
            let old = std::mem::replace(&mut state.cfg_values, SensorCfgActivity::Unknown);
            let SensorCfgActivity::AutoAdjusting { handle, .. } = old else {
                unreachable!()
            };
            match handle.finish(camera_array) {
                Ok(averaged) => {
                    info!(?averaged, "auto-adjust converged");
                    // State is already Unknown; handle_unknown_settings will call
                    // sync_sensor_cfg to read all values (including framerate) back.
                }
                Err(e) => {
                    sensor_cfg_failed(
                        &mut state.cfg_values,
                        evt_tx,
                        None,
                        format!("auto-adjust failed to apply settings: {e}"),
                    );
                }
            }
        }
        Ok(pylon::cfg::AutoAdjustTick::TimedOut) => {
            let old = std::mem::replace(&mut state.cfg_values, SensorCfgActivity::Unknown);
            let SensorCfgActivity::AutoAdjusting { handle, .. } = old else {
                unreachable!()
            };
            abort_auto_adjust(
                handle,
                camera_array,
                &mut state.cfg_values,
                evt_tx,
                None,
                "auto-adjust timed out".to_string(),
            );
        }
        Err(e) => {
            let old = std::mem::replace(&mut state.cfg_values, SensorCfgActivity::Unknown);
            let SensorCfgActivity::AutoAdjusting { handle, .. } = old else {
                unreachable!()
            };
            abort_auto_adjust(
                handle,
                camera_array,
                &mut state.cfg_values,
                evt_tx,
                None,
                format!("auto-adjust error: {e}"),
            );
        }
    }
}

fn handle_unknown_settings(
    camera_array: &mut CameraArray,
    state: &mut SensorCfgIterState,
    evt_tx: &broadcast::Sender<wire::ServerMessage>,
) {
    if !matches!(state.cfg_values, SensorCfgActivity::Unknown) {
        return;
    }
    match sync_sensor_cfg(camera_array, state.rate_control) {
        Ok(s) => state.cfg_values = SensorCfgActivity::Set(s),
        Err(e) => {
            warn!("{e}");
            let _ = evt_tx.send(wire::ServerMessage::notice(wire::NoticeSeverity::Error, e.clone()));
            state.cfg_values = SensorCfgActivity::RestoreFailed;
        }
    }
}

fn handle_unitialized(
    camera_array: &mut CameraArray,
    state: &mut SensorCfgIterState,
    is_busy: bool,
    evt_tx: &broadcast::Sender<wire::ServerMessage>,
) {
    if !matches!(state.cfg_values, SensorCfgActivity::Unitialized) || is_busy {
        return;
    }

    // Settings fixed on the command line go on first, so the auto-adjust below starts
    // from them and they show as set while it runs.
    let fixed = state.fixed.clone();
    let any_fixed =
        fixed.exposure_us.is_some() || fixed.gain.is_some() || fixed.white_balance.is_some();
    if any_fixed {
        match apply_settings(camera_array, &fixed, state.rate_control) {
            Ok(applied) => {
                info!(?fixed, "applied sensor settings from the command line");
                state.cfg_values = SensorCfgActivity::Set(applied);
            }
            Err(e) => {
                sensor_cfg_failed(
                    &mut state.cfg_values,
                    evt_tx,
                    None,
                    format!("failed to apply sensor settings from the command line: {e}"),
                );
                return;
            }
        }
    }

    let adjust = |value: Option<f64>| match value {
        Some(_) => pylon::cfg::AutoParam::NoAdjust,
        None => pylon::cfg::AutoParam::Adjust(None),
    };
    let tune_gain = adjust(fixed.gain);
    let tune_exposure = adjust(fixed.exposure_us);
    let tune_whitebalance = fixed.white_balance.is_none();
    if matches!(tune_gain, pylon::cfg::AutoParam::NoAdjust)
        && matches!(tune_exposure, pylon::cfg::AutoParam::NoAdjust)
        && !tune_whitebalance
    {
        // Everything is fixed: nothing left to auto-adjust.
        return;
    }
    let target_brightness = state.startup_target_brightness;
    start_auto_adjust(
        camera_array,
        state,
        target_brightness,
        tune_gain,
        tune_exposure,
        tune_whitebalance,
        None, // no specific client to inform if this fails
        evt_tx,
    );
}

fn start_auto_adjust(
    camera_array: &mut CameraArray,
    state: &mut SensorCfgIterState,
    target_brightness: f64,
    gain: pylon::cfg::AutoParam,
    exposure_us: pylon::cfg::AutoParam,
    auto_whitebalance: bool,
    cmd_uuid: Option<Uuid>,
    evt_tx: &broadcast::Sender<wire::ServerMessage>,
) {
    let cfg = pylon::cfg::AutoAdjustConfig {
        timeout: Duration::from_secs(state.auto_adjust_duration_secs),
        boost_framerate: !state.auto_adjust_no_boost_framerate,
        profile: if state.auto_adjust_minimize_gain {
            pylon::cfg::AutoFunctionProfile::MinimizeGain
        } else {
            pylon::cfg::AutoFunctionProfile::MinimizeExposureTime
        },
        target_brightness,
        gain,
        exposure_us,
        auto_whitebalance,
        ..Default::default()
    };
    match pylon::cfg::begin_auto_adjust(camera_array, &cfg, state.rate_control) {
        Ok(handle) => {
            let prev = match std::mem::replace(&mut state.cfg_values, SensorCfgActivity::Unknown) {
                SensorCfgActivity::Set(cfg) => cfg,
                _ => wire::SensorCfg::default(),
            };
            state.cfg_values = SensorCfgActivity::AutoAdjusting { prev, handle };
            if let Some(cmd_uuid) = cmd_uuid {
                sensor_cfg_ok(evt_tx, cmd_uuid);
            }
        }
        Err(e) => {
            sensor_cfg_failed(
                &mut state.cfg_values,
                evt_tx,
                cmd_uuid,
                format!("auto-adjust failed to start: {e}"),
            );
        }
    }
}

fn abort_auto_adjust(
    handle: pylon::cfg::AutoAdjustHandle,
    cameras: &mut pylon::CameraArray,
    cfg_values: &mut SensorCfgActivity,
    evt_tx: &broadcast::Sender<wire::ServerMessage>,
    cmd_uuid: Option<Uuid>,
    reason: String,
) {
    sensor_cfg_failed(cfg_values, evt_tx, cmd_uuid, reason);
    match handle.finish(cameras) {
        Ok(_) => {
            // State is already Unknown; handle_unknown_settings will call
            // sync_sensor_cfg to read all values (including framerate) back.
        }
        Err(e) => sensor_cfg_failed(
            cfg_values,
            evt_tx,
            cmd_uuid,
            format!("auto-adjust: failed to synchronize cameras: {e}"),
        ),
    }
}

fn sync_sensor_cfg(
    cameras: &mut CameraArray,
    rate_control: RateControl,
) -> Result<wire::SensorCfg, String> {
    let exposure_us = pylon::cfg::sync_exposure(cameras).map_err(|e| e.to_string())?;
    let gain = pylon::cfg::sync_gain(cameras).map_err(|e| e.to_string())?;
    let white_balance = pylon::cfg::sync_whitebalance(cameras).map_err(|e| e.to_string())?;
    let framerate = pylon::cfg::sync_framerate(cameras, rate_control).map_err(|e| e.to_string())?;
    Ok(wire::SensorCfg {
        exposure_us: Some(exposure_us),
        gain: Some(gain),
        white_balance: Some(white_balance),
        framerate: Some(framerate),
    })
}


fn apply_settings(
    cameras: &mut pylon::CameraArray,
    settings: &wire::SensorCfg,
    rate_control: RateControl,
) -> Result<wire::SensorCfg, String> {
    for i in 0..cameras.len() {
        let mut cam = cameras.get_camera(i);
        if let Some(v) = settings.exposure_us {
            cam.set_feature("ExposureTime", &format!("{v}"))
                .map_err(|e| e.to_string())?;
        }
        if let Some(v) = settings.gain {
            cam.set_feature("Gain", &format!("{v}"))
                .map_err(|e| e.to_string())?;
        }
        if let Some((r, g, b)) = settings.white_balance {
            cam.set_feature("BalanceRatioSelector", "Red")
                .map_err(|e| e.to_string())?;
            cam.set_feature("BalanceRatio", &format!("{r}"))
                .map_err(|e| e.to_string())?;
            cam.set_feature("BalanceRatioSelector", "Green")
                .map_err(|e| e.to_string())?;
            cam.set_feature("BalanceRatio", &format!("{g}"))
                .map_err(|e| e.to_string())?;
            cam.set_feature("BalanceRatioSelector", "Blue")
                .map_err(|e| e.to_string())?;
            cam.set_feature("BalanceRatio", &format!("{b}"))
                .map_err(|e| e.to_string())?;
        }
        if let Some(v) = settings.framerate {
            pylon::cfg::set_framerate(&mut cam, v, rate_control)
                .map_err(|e| e.to_string())?;
        }
    }
    sync_sensor_cfg(cameras, rate_control)
}
