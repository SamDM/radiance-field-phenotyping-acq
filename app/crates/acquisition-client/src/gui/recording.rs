use std::collections::HashMap;

use acquisition_common::protocol::{Activity, ClientCommand, ClientMessage, ServerStatus};
use eframe::egui;
use eframe::egui::accesskit::Uuid;
use tokio::sync::mpsc as tokio_mpsc;

use super::Timed;

#[derive(Default)]
pub(super) struct RecordingState {
    pending: Option<Uuid>,
}

impl RecordingState {
    fn reconcile_from_server(
        &mut self,
        server_ack: &mut HashMap<Uuid, Timed<Result<(), String>>>,
        server_status: &Timed<ServerStatus>,
    ) {
        if let Some(cmd_uuid) = self.pending {
            if let Some(ack) = server_ack.get(&cmd_uuid) {
                if server_status.at >= ack.at {
                    self.pending = None;
                    server_ack.remove(&cmd_uuid);
                }
            }
        }
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

        ui.horizontal(|ui| {
            let is_recording = server_status
                .as_ref()
                .map_or(false, |t| t.value.activity == Activity::Recording);
            let is_pending = self.pending.is_some();
            let btn_label = if is_pending {
                if is_recording {
                    "Stopping..."
                } else {
                    "Starting..."
                }
            } else if is_recording {
                "Stop Recording"
            } else {
                "Start Recording"
            };
            let btn_color = match (is_recording, is_pending) {
                (false, false) => egui::Color32::from_rgb(56, 142, 60),
                (false, true) => egui::Color32::from_rgb(30, 80, 33),
                (true, false) => egui::Color32::from_rgb(198, 40, 40),
                (true, true) => egui::Color32::from_rgb(110, 22, 22),
            };
            let icon = if is_recording { "■  " } else { "▶  " };
            let fmt = egui::text::TextFormat {
                color: egui::Color32::WHITE,
                font_id: egui::FontId::default(),
                ..Default::default()
            };
            let mut job = egui::text::LayoutJob::default();
            job.halign = egui::Align::LEFT;
            job.append(icon, 0.0, fmt.clone());
            job.append(btn_label, 0.0, fmt);
            // Fixed width to fit the longest label ("Start Recording")
            let btn = egui::Button::new(job)
                .fill(btn_color)
                .min_size(egui::vec2(130.0, 0.0));
            let space_pressed = !is_pending
                && !ui.ctx().egui_wants_keyboard_input()
                && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space));
            // Use main_align = Min so the button widget positions text at the left of its rect
            let left_layout = egui::Layout {
                main_align: egui::Align::Min,
                ..egui::Layout::left_to_right(egui::Align::Center)
            };
            let clicked = ui
                .with_layout(left_layout, |ui| {
                    ui.add_enabled(!is_pending, btn)
                        .on_hover_text("Press Space to start/stop recording")
                        .clicked()
                })
                .inner;
            if clicked || space_pressed {
                let cmd_uuid = Uuid::new_v4();
                let cmd = if is_recording {
                    ClientCommand::StopRecording
                } else {
                    ClientCommand::StartRecording
                };
                self.pending = Some(cmd_uuid);
                let _ = cmd_tx.send(ClientMessage {
                    uuid: cmd_uuid,
                    command: cmd,
                });
            }
        });
    }
}
