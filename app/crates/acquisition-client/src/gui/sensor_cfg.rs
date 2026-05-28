use std::collections::HashMap;

use acquisition_common::protocol::{ClientCommand, ClientMessage, SensorCfg, ServerStatus};
use super::Timed;
use eframe::egui;
use eframe::egui::accesskit::Uuid;
use tokio::sync::mpsc as tokio_mpsc;

use crate::numeric_input_widget::{NumericInput, NumericInputState};

// ----------------------------------------------------------------------------
// Single numeric field

pub(super) struct SensorCfgFieldState {
    pending: Option<(Uuid, f64)>,
    pub(super) state: NumericInputState,
}

impl SensorCfgFieldState {
    pub(super) fn new(round_to: i32) -> SensorCfgFieldState {
        SensorCfgFieldState {
            pending: None,
            state: NumericInputState::new_editing_in_progress("", round_to),
        }
    }

    pub(super) fn reconcile_from_server(
        &mut self,
        server_ack: &HashMap<Uuid, Timed<Result<(), String>>>,
        server_val_opt: Option<Timed<f64>>,
    ) -> Option<Uuid> {
        let mut clear = None;

        if let Some((cmd_uuid, _)) = self.pending {
            if let Some(ack) = server_ack.get(&cmd_uuid) {
                if server_val_opt.as_ref().map_or(false, |sv| sv.at >= ack.at) {
                    self.pending = None;
                    clear = Some(cmd_uuid);
                }
            }
        }
        // Update if all are met:
        // - server has a value
        // - this field is not pending (i.e. not awaiting an ACK)
        // - this field is not being edited OR is empty for debounce period
        // - the server disagrees with the current value
        if let Some(server_val) = server_val_opt {
            if self.pending.is_none() {
                let replace = match self.state.value() {
                    Ok(done) => !done.agrees_up_to_rounding(server_val.value),
                    Err(editing) => editing.text_value() == "" && editing.debounce_elapsed(),
                };
                if replace {
                    self.state.to_done(server_val.value);
                }
            }
        }

        clear
    }

    pub(super) fn reconcile_to_server(
        &mut self,
        cmd_uuid: Uuid,
        server_val_opt: Option<f64>,
    ) -> Option<f64> {
        let Ok(done) = self.state.value() else {
            return server_val_opt;
        };

        let needs_update = match self.pending {
            // if a new value is set but one is still pending, discard the old pending value
            Some((_, pending_val)) => done.num_value() != pending_val,
            None => server_val_opt.map_or(true, |v| !done.agrees_up_to_rounding(v)),
        };

        if needs_update {
            self.pending = Some((cmd_uuid, done.num_value()));
            Some(done.num_value())
        } else {
            server_val_opt
        }
    }
}

// ----------------------------------------------------------------------------
// Gui with all input fields

/// Per-field inputs for the camera settings UI.
pub(super) struct SensorCfgUiState {
    pub(super) exposure_us: SensorCfgFieldState,
    pub(super) gain: SensorCfgFieldState,
    pub(super) balance_r: SensorCfgFieldState,
    pub(super) balance_g: SensorCfgFieldState,
    pub(super) balance_b: SensorCfgFieldState,
}

impl Default for SensorCfgUiState {
    fn default() -> Self {
        Self {
            exposure_us: SensorCfgFieldState::new(0),
            gain: SensorCfgFieldState::new(-2),
            balance_r: SensorCfgFieldState::new(-2),
            balance_g: SensorCfgFieldState::new(-2),
            balance_b: SensorCfgFieldState::new(-2),
        }
    }
}

impl SensorCfgUiState {
    pub(super) fn reconcile_from_server(
        &mut self,
        server_ack: &mut HashMap<Uuid, Timed<Result<(), String>>>,
        timed_status: &Timed<ServerStatus>,
    ) {
        let at = timed_status.at;
        let server_cfg = &timed_status.value.sensor_cfg;
        let timed = |v: f64| Timed::at(v, at);

        // TODO: fix VSCode ignoring my rustfmt and then fix the uglyness below
        let mut to_clear = vec![
            self.exposure_us
                .reconcile_from_server(server_ack, server_cfg.exposure_us.map(timed)),
            self.gain.reconcile_from_server(server_ack, server_cfg.gain.map(timed)),
        ];

        if let Some((r, g, b)) = server_cfg.white_balance {
            to_clear.push(self.balance_r.reconcile_from_server(server_ack, Some(timed(r))));
            to_clear.push(self.balance_g.reconcile_from_server(server_ack, Some(timed(g))));
            to_clear.push(self.balance_b.reconcile_from_server(server_ack, Some(timed(b))));
        }

        // only clear AFTER updating all UI values,
        // because some values may have been set at once with the same command ID.
        for cmd_id in to_clear.into_iter().flatten() {
            server_ack.remove(&cmd_id);
        }
    }

    pub(super) fn reconcile_to_server(&mut self, server_cfg: &SensorCfg) -> (Uuid, SensorCfg) {
        let cmd_uuid = Uuid::new_v4();
        let mut new_cfg = server_cfg.clone();

        // TODO: fix VSCode ignoring my rustfmt and then fix the uglyness below
        new_cfg.exposure_us = self
            .exposure_us
            .reconcile_to_server(cmd_uuid, new_cfg.exposure_us);
        new_cfg.gain = self.gain.reconcile_to_server(cmd_uuid, new_cfg.gain);

        if let Some((r, g, b)) = &mut new_cfg.white_balance {
            let rec = |field: &mut SensorCfgFieldState, val: &mut f64| {
                *val = field
                    .reconcile_to_server(cmd_uuid, Some(*val))
                    .unwrap_or(*val);
            };
            rec(&mut self.balance_r, r);
            rec(&mut self.balance_g, g);
            rec(&mut self.balance_b, b);
        }

        (cmd_uuid, new_cfg)
    }

    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        server_ack: &mut HashMap<Uuid, Timed<Result<(), String>>>,
        server_status: &Option<Timed<ServerStatus>>,
        cmd_tx: &tokio_mpsc::UnboundedSender<ClientMessage>,
    ) {
        if let Some(timed_status) = server_status {
            self.reconcile_from_server(server_ack, timed_status);
        }

        let opt_sensor_cfg = server_status.as_ref().map(|t| &t.value.sensor_cfg);

        egui::Grid::new("cam_settings_grid")
            .num_columns(2)
            .show(ui, |ui| {
                sensor_cfg_row(
                    ui,
                    "Exposure (µs)",
                    &mut self.exposure_us.state,
                    opt_sensor_cfg.and_then(|s| s.exposure_us),
                );
                sensor_cfg_row(
                    ui,
                    "Gain",
                    &mut self.gain.state,
                    opt_sensor_cfg.and_then(|s| s.gain),
                );
                sensor_cfg_row(
                    ui,
                    "R ratio",
                    &mut self.balance_r.state,
                    opt_sensor_cfg
                        .and_then(|s| s.white_balance)
                        .map(|(r, _, _)| r),
                );
                sensor_cfg_row(
                    ui,
                    "G ratio",
                    &mut self.balance_g.state,
                    opt_sensor_cfg
                        .and_then(|s| s.white_balance)
                        .map(|(_, g, _)| g),
                );
                sensor_cfg_row(
                    ui,
                    "B ratio",
                    &mut self.balance_b.state,
                    opt_sensor_cfg
                        .and_then(|s| s.white_balance)
                        .map(|(_, _, b)| b),
                );
            });

        if let Some(status) = server_status {
            let server_sensor_cfg = &status.value.sensor_cfg;
            let (cmd_uuid, new_sensor_cfg) = self.reconcile_to_server(server_sensor_cfg);
            if &new_sensor_cfg != server_sensor_cfg {
                let _ = cmd_tx.send(ClientMessage {
                    uuid: cmd_uuid,
                    command: ClientCommand::SetSensorCfg(new_sensor_cfg),
                });
            }
        }
    }
}

pub(super) fn sensor_cfg_row(
    ui: &mut egui::Ui,
    label: &str,
    field: &mut NumericInputState,
    server_val: Option<f64>,
) {
    ui.label(label);
    let matches_server = server_val.map_or(false, |sv| {
        field
            .value()
            .is_ok_and(|done| done.agrees_up_to_rounding(sv))
    });
    let mut widget = NumericInput::new(field);
    if !matches_server {
        widget = widget.text_color(egui::Color32::from_rgb(255, 140, 0));
    }
    ui.add(widget);
    ui.end_row();
}
