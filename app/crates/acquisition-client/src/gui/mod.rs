use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Instant;

use acquisition_common::protocol::{ClientCommand, ClientMessage, Notice, NoticeSeverity, ServerMessage, ServerStatus, Zoom};
use eframe::egui;
use eframe::egui::accesskit::Uuid;

use tokio::sync::mpsc as tokio_mpsc;
use tracing::warn;

mod button;
mod dropdown;
mod preview;
mod recording;
mod sensor_cfg;
mod status_bar;
use preview::FramePreviewState;
use recording::RecordingState;
use sensor_cfg::SensorCfgUiState;
use status_bar::StatusBarState;

/// Wrapper around a `std::sync::mpsc::SyncSender` and `egui::Context`,
/// to send a wake-up call to the gui on every external `send` event.
#[derive(Clone)]
pub struct EventSender {
    tx: mpsc::SyncSender<AppEvent>,
    ctx: egui::Context,
}

impl EventSender {
    pub fn new(tx: mpsc::SyncSender<AppEvent>, ctx: egui::Context) -> Self {
        Self { tx, ctx }
    }

    pub fn send(&self, evt: AppEvent) {
        let _ = self.tx.send(evt);
        self.ctx.request_repaint();
    }
}

pub enum AppEvent {
    ControlConnected,
    ControlDisconnected,
    ControlConnecting,
    ControlReconnecting { attempt: u32, next_at: Instant },
    PreviewConnected,
    PreviewDisconnected,
    PreviewConnecting,
    PreviewReconnecting { attempt: u32, next_at: Instant },
    ServerMessage(ServerMessage),
    PreviewFrameGroup(Vec<(acquisition_common::Frame<acquisition_common::Color>, Option<Zoom>)>),
}

/// State of an in-progress (re)connection attempt, as far as the UI cares.
pub(super) enum ConnectionAttempt {
    /// Waiting out the backoff delay before the next attempt.
    Waiting { attempt: u32, next_at: Instant },
    /// A connect attempt is in flight right now (may take up to the
    /// connect timeout, during which no further events arrive).
    Connecting,
}

pub(super) struct Timed<T> {
    pub(super) at: Instant,
    pub(super) value: T,
}

impl<T> Timed<T> {
    pub(super) fn new(value: T) -> Self {
        Self { at: Instant::now(), value }
    }

    pub(super) fn at(value: T, at: Instant) -> Self {
        Self { at, value }
    }
}

struct ServerState {
    control_connected: bool,
    control_attempt: Option<ConnectionAttempt>,
    preview_connected: bool,
    preview_attempt: Option<ConnectionAttempt>,
    server_last_known_status: Option<Timed<ServerStatus>>,
    server_ack: HashMap<Uuid, Timed<Result<(), String>>>,
    notices: Vec<Timed<Notice>>,
}

impl Default for ServerState {
    fn default() -> Self {
        Self {
            control_connected: false,
            control_attempt: None,
            preview_connected: false,
            preview_attempt: None,
            server_last_known_status: None,
            server_ack: HashMap::new(),
            notices: Vec::new(),
        }
    }
}

impl ServerState {
    fn update_state(&mut self, event: AppEvent) {
        match event {
            AppEvent::ControlConnected => {
                self.control_connected = true;
                self.control_attempt = None;
            }
            AppEvent::ControlDisconnected => self.control_connected = false,
            AppEvent::ControlConnecting => {
                self.control_attempt = Some(ConnectionAttempt::Connecting);
            }
            AppEvent::ControlReconnecting { attempt, next_at } => {
                self.control_attempt = Some(ConnectionAttempt::Waiting { attempt, next_at });
            }
            AppEvent::PreviewConnected => {
                self.preview_connected = true;
                self.preview_attempt = None;
            }
            AppEvent::PreviewDisconnected => self.preview_connected = false,
            AppEvent::PreviewConnecting => {
                self.preview_attempt = Some(ConnectionAttempt::Connecting);
            }
            AppEvent::PreviewReconnecting { attempt, next_at } => {
                self.preview_attempt = Some(ConnectionAttempt::Waiting { attempt, next_at });
            }
            AppEvent::ServerMessage(msg) => match msg {
                ServerMessage::ServerStatus(status) => {
                    self.server_last_known_status = Some(Timed::new(status));
                }
                ServerMessage::Notice(notice) => {
                    self.notices.push(Timed::new(notice));
                }
                ServerMessage::Ack(ack) => {
                    self.server_ack.insert(ack.cmd_uuid, Timed::new(ack.result));
                }
            },
            AppEvent::PreviewFrameGroup(_) => unreachable!("handled before update_state"),
        }
    }
}

struct UiState {
    recording_ctrl: RecordingState,
    sensor_cfg: SensorCfgUiState,
    preview: FramePreviewState,
    status_bar: StatusBarState,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            recording_ctrl: RecordingState::default(),
            sensor_cfg: SensorCfgUiState::default(),
            preview: FramePreviewState::default(),
            status_bar: StatusBarState::default(),
        }
    }
}

pub struct App {
    event_rx: mpsc::Receiver<AppEvent>,
    cmd_tx: tokio_mpsc::UnboundedSender<ClientMessage>,
    server_state: ServerState,
    ui_state: UiState,
}

impl App {
    pub fn new(
        event_rx: mpsc::Receiver<AppEvent>,
        cmd_tx: tokio_mpsc::UnboundedSender<ClientMessage>,
    ) -> Self {
        Self {
            event_rx,
            cmd_tx,
            server_state: ServerState::default(),
            ui_state: UiState::default(),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // drain server events to get the last known state
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                AppEvent::ControlConnected => {
                    let _ = self.cmd_tx.send(ClientMessage {
                        uuid: Uuid::new_v4(),
                        command: ClientCommand::GetStatus,
                    });
                    self.server_state.update_state(AppEvent::ControlConnected);
                }
                AppEvent::PreviewFrameGroup(frames) => self.ui_state.preview.ingest_frames(frames),
                AppEvent::ServerMessage(ServerMessage::Notice(ref notice)) => {
                    match notice.severity {
                        NoticeSeverity::Info => {
                            tracing::info!(message = %notice.message, "server notice");
                        }
                        NoticeSeverity::Warn => {
                            warn!(message = %notice.message, "server notice");
                        }
                        NoticeSeverity::Error => {
                            tracing::error!(message = %notice.message, "server notice");
                        }
                    }
                    self.server_state.update_state(event);
                }
                event => self.server_state.update_state(event),
            }
        }

        egui::Panel::top("recording_ctrl").show_inside(ui, |ui| {
            self.ui_state.recording_ctrl.show(
                ui,
                &mut self.server_state.server_ack,
                &self.server_state.server_last_known_status,
                &self.cmd_tx,
            );
        });

        // Tick the UI while a countdown is live, so it stays current even
        // without new events (a "connecting" attempt has no countdown to
        // tick, so it doesn't need this).
        let is_waiting = |a: &Option<ConnectionAttempt>| matches!(a, Some(ConnectionAttempt::Waiting { .. }));
        if is_waiting(&self.server_state.control_attempt) || is_waiting(&self.server_state.preview_attempt) {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }

        egui::Panel::bottom("status").show_inside(ui, |ui| {
            self.ui_state.status_bar.show(
                ui,
                self.server_state.control_connected,
                self.server_state.control_attempt.as_ref(),
                self.server_state.preview_connected,
                self.server_state.preview_attempt.as_ref(),
                // Without a live control connection we can't know the PTP
                // state is still current, so don't show the stale value.
                self.server_state.control_connected.then(|| {
                    self.server_state.server_last_known_status.as_ref().map(|s| &s.value.ptp)
                }).flatten(),
                &self.server_state.notices,
            );
        });

        egui::Panel::bottom("camera_settings").show_inside(ui, |ui| {
            self.ui_state.sensor_cfg.show(
                ui,
                &mut self.server_state.server_ack,
                &self.server_state.server_last_known_status,
                &self.cmd_tx,
            );
        });

        egui::Panel::bottom("mode_indicator").show_inside(ui, |ui| {
            self.ui_state.recording_ctrl.show_mode_bar(
                ui,
                &self.cmd_tx,
                self.server_state.preview_connected,
            );
        });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.ui_state
                .preview
                .show(
                    ui,
                    &mut self.server_state.server_ack,
                    &self.cmd_tx,
                );
        });
    }
}

