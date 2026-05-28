use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Instant;

use acquisition_common::protocol::{ClientCommand, ClientMessage, ServerMessage, ServerStatus, Zoom};
use eframe::egui;
use eframe::egui::accesskit::Uuid;

use tokio::sync::mpsc as tokio_mpsc;
use tracing::warn;

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
    PreviewConnected,
    PreviewDisconnected,
    ServerMessage(ServerMessage),
    PreviewFrameGroup(Vec<(acquisition_common::Frame<acquisition_common::Color>, Option<Zoom>)>),
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
    preview_connected: bool,
    server_last_known_status: Option<Timed<ServerStatus>>,
    server_ack: HashMap<Uuid, Timed<Result<(), String>>>,
}

impl Default for ServerState {
    fn default() -> Self {
        Self {
            control_connected: false,
            preview_connected: false,
            server_last_known_status: None,
            server_ack: HashMap::new(),
        }
    }
}

impl ServerState {
    fn update_state(&mut self, event: AppEvent) {
        match event {
            AppEvent::ControlConnected => self.control_connected = true,
            AppEvent::ControlDisconnected => self.control_connected = false,
            AppEvent::PreviewConnected => self.preview_connected = true,
            AppEvent::PreviewDisconnected => self.preview_connected = false,
            AppEvent::ServerMessage(msg) => match msg {
                ServerMessage::ServerStatus(status) => {
                    self.server_last_known_status = Some(Timed::new(status));
                }
                ServerMessage::InternalErr(reason) => {
                    // TODO: propagate to GUI as well
                    warn!(%reason, "internal server error");
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

        egui::Panel::bottom("status").show_inside(ui, |ui| {
            self.ui_state.status_bar.show(
                ui,
                self.server_state.control_connected,
                self.server_state.preview_connected,
                self.server_state.server_last_known_status.as_ref().map(|s| &s.value.ptp),
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

        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.ui_state.preview.show(ui, &self.cmd_tx);
        });
    }
}

