use std::time::Duration;

use acquisition_common::protocol::{CameraPtpStatus, Notice, NoticeSeverity, Ptp4lStatus, PtpStatusReport};
use eframe::egui;

use super::{ConnectionAttempt, Timed};

const NOTICE_DISPLAY_DURATION: Duration = Duration::from_secs(10);
/// How long the background behind the latest notice flashes for after it
/// arrives, to catch the eye before it settles into the plain display.
const NOTICE_FLASH_DURATION: Duration = Duration::from_secs(2);

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

fn notice_color(severity: &NoticeSeverity) -> egui::Color32 {
    match severity {
        NoticeSeverity::Info => egui::Color32::WHITE,
        NoticeSeverity::Warn => egui::Color32::YELLOW,
        NoticeSeverity::Error => egui::Color32::RED,
    }
}

/// Background color for the "just arrived" notice flash: starts solid in
/// the notice's severity color and smoothly fades out over
/// `NOTICE_FLASH_DURATION`.
fn flash_color(severity: &NoticeSeverity, elapsed: Duration) -> egui::Color32 {
    let t = elapsed.as_secs_f32() / NOTICE_FLASH_DURATION.as_secs_f32();
    let alpha = (1.0 - t.min(1.0)) * 200.0;
    let c = notice_color(severity);
    egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha as u8)
}

#[derive(Default)]
pub(super) struct StatusBarState {}

impl StatusBarState {
    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        control_connected: bool,
        control_attempt: Option<&ConnectionAttempt>,
        preview_connected: bool,
        preview_attempt: Option<&ConnectionAttempt>,
        ptp: Option<&PtpStatusReport>,
        notices: &[Timed<Notice>],
    ) {
        let overall = match (control_connected, preview_connected) {
            (true, true) => Severity::Ok,
            (false, false) => Severity::Error,
            _ => Severity::Warn,
        };

        ui.horizontal(|ui| {
            status_dot(ui, overall, "Status").on_hover_ui(|ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.set_max_width(150.0);
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
                        ui.horizontal(|ui| {
                            status_dot(ui, control_status, "Control");
                            if let Some(attempt) = control_attempt {
                                ui.label(attempt_text(attempt));
                            }
                        });
                        ui.horizontal(|ui| {
                            status_dot(ui, preview_status, "Preview");
                            if let Some(attempt) = preview_attempt {
                                ui.label(attempt_text(attempt));
                            }
                        });
                        ui.separator();
                        match ptp {
                            None => {
                                status_dot(ui, Severity::Warn, "PTP unknown");
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

                    if !notices.is_empty() {
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .max_height(200.0)
                            .show(ui, |ui| {
                                ui.vertical(|ui| {
                                    ui.set_min_width(400.0);
                                    for notice in notices.iter().rev() {
                                        notice_label(ui, notice);
                                    }
                                });
                            });
                    }
                });
            });

            if let Some(text) = connection_status_text(control_attempt, preview_attempt) {
                ui.label(text);
            }

            if let Some(recent) = notices.last() {
                let elapsed = recent.at.elapsed();
                if elapsed < NOTICE_DISPLAY_DURATION {
                    if elapsed < NOTICE_FLASH_DURATION {
                        let bg = flash_color(&recent.value.severity, elapsed);
                        egui::Frame::NONE.fill(bg).show(ui, |ui| notice_label(ui, recent));
                        // Keep repainting so the flash animation actually plays.
                        ui.ctx().request_repaint();
                    } else {
                        notice_label(ui, recent);
                    }
                }
            }
        });
    }
}

/// Terse status text shown next to the overall status dot: "reconnecting"
/// takes priority over "connection lost" since it's the more specific,
/// more current state (an in-flight attempt vs. waiting out a backoff).
fn connection_status_text(
    control_attempt: Option<&ConnectionAttempt>,
    preview_attempt: Option<&ConnectionAttempt>,
) -> Option<&'static str> {
    let is_connecting = |a: Option<&ConnectionAttempt>| matches!(a, Some(ConnectionAttempt::Connecting));
    let is_waiting = |a: Option<&ConnectionAttempt>| matches!(a, Some(ConnectionAttempt::Waiting { .. }));

    if is_connecting(control_attempt) || is_connecting(preview_attempt) {
        Some("reconnecting")
    } else if is_waiting(control_attempt) || is_waiting(preview_attempt) {
        Some("connection lost")
    } else {
        None
    }
}

fn attempt_text(attempt: &ConnectionAttempt) -> String {
    match attempt {
        ConnectionAttempt::Waiting { attempt, next_at } => {
            let remaining = next_at.saturating_duration_since(std::time::Instant::now());
            format!("reconnecting in {}s (attempt {attempt})", remaining.as_secs() + 1)
        }
        ConnectionAttempt::Connecting => "connecting…".to_string(),
    }
}

fn notice_label(ui: &mut egui::Ui, notice: &Timed<Notice>) {
    let wall = notice.value.at;
    let ts = format!(
        "{:02}:{:02}:{:02}",
        wall.hour(),
        wall.minute(),
        wall.second(),
    );
    ui.horizontal(|ui| {
        ui.colored_label(notice_color(&notice.value.severity), &ts);
        ui.label(&notice.value.message);
    });
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
