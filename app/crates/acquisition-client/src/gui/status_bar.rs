use acquisition_common::protocol::{CameraPtpStatus, Ptp4lStatus, PtpStatusReport};
use eframe::egui;

enum Severity {
    Ok,
    Warn,
    Error,
}

impl Severity {
    fn color(&self) -> egui::Color32 {
        match self {
            Severity::Ok => egui::Color32::GREEN,
            Severity::Warn => egui::Color32::YELLOW,
            Severity::Error => egui::Color32::RED,
        }
    }
}

#[derive(Default)]
pub(super) struct StatusBarState {}

impl StatusBarState {
    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        control_connected: bool,
        preview_connected: bool,
        ptp: Option<&PtpStatusReport>,
    ) {
        let overall = match (control_connected, preview_connected) {
            (true, true) => Severity::Ok,
            (false, false) => Severity::Error,
            _ => Severity::Warn,
        };
        status_dot(ui, overall, "Status").on_hover_ui(|ui| {
            ui.vertical(|ui| {
                let control_status = if control_connected {
                    Severity::Ok
                } else {
                    Severity::Error
                };
                let preview_status = if preview_connected {
                    Severity::Ok
                } else {
                    Severity::Error
                };
                status_dot(ui, control_status, "Control");
                status_dot(ui, preview_status, "Preview");
                ui.separator();
                match ptp {
                    None => {
                        status_dot(ui, Severity::Warn, "PTP disabled");
                    }
                    Some(ptp) => {
                        let ptp4l_severity = if ptp.ptp4l == Ptp4lStatus::Master {
                            Severity::Ok
                        } else {
                            Severity::Warn
                        };
                        status_dot(ui, ptp4l_severity, "PTP4l");
                        for (i, camera) in ptp.cameras.iter().enumerate() {
                            let severity = if camera.ptp == CameraPtpStatus::Slave {
                                Severity::Ok
                            } else {
                                Severity::Warn
                            };
                            status_dot(ui, severity, &format!("Camera {i} PTP"));
                        }
                    }
                }
            });
        });
    }
}

fn status_dot(ui: &mut egui::Ui, status: Severity, label: &str) -> egui::Response {
    ui.horizontal(|ui| {
        let row_height = ui.text_style_height(&egui::TextStyle::Body);
        let (rect, r1) =
            ui.allocate_exact_size(egui::vec2(row_height, row_height), egui::Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), row_height / 4.0, status.color());
        let r2 = ui.label(label);
        r1 | r2
    })
    .inner
}
