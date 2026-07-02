use std::collections::HashMap;
use std::time::Instant;

use acquisition_common::protocol::{
    Activity, ClientCommand, ClientMessage, PastRecording, ServerStatus,
};
use eframe::egui;
use eframe::egui::accesskit::Uuid;
use tokio::sync::mpsc as tokio_mpsc;

use super::Timed;

fn format_duration_aligned(d: &time::Duration) -> String {
    let total_cs = d.whole_milliseconds() / 10;
    let cs = total_cs % 100;
    let total_secs = d.whole_seconds();
    let secs = total_secs % 60;
    let total_mins = total_secs / 60;
    let mins = total_mins % 60;
    let hours = total_mins / 60;
    // Always 11 chars wide: "HH:MM:SS.CC"
    if hours > 0 {
        format!("{hours:2}:{mins:02}:{secs:02}.{cs:02}")
    } else if mins > 0 {
        format!("   {mins:2}:{secs:02}.{cs:02}")
    } else {
        format!("      {secs:2}.{cs:02}")
    }
}

fn format_elapsed(d: time::Duration) -> String {
    let total_secs = d.whole_seconds().max(0) as u64;
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Format a nanosecond offset as `M:SS` or `H:MM:SS`, with field widths derived
/// from `total_ns` so the string length is stable throughout playback.
fn format_replay_time(ns: u64, total_ns: u64) -> String {
    let secs = ns / 1_000_000_000;
    let mins = secs / 60;
    let secs_rem = secs % 60;
    let total_mins = (total_ns / 1_000_000_000) / 60;
    if total_mins >= 60 {
        let hours = mins / 60;
        let mins_rem = mins % 60;
        let total_hours = total_mins / 60;
        let h_w = total_hours.to_string().len();
        format!("{hours:>h_w$}:{mins_rem:02}:{secs_rem:02}")
    } else {
        let m_w = total_mins.to_string().len().max(1);
        format!("{mins:>m_w$}:{secs_rem:02}")
    }
}

fn format_time_ago(secs: u64) -> String {
    match secs {
        0..=1 => "1s ago".into(),
        2..=59 => format!("{secs}s ago"),
        60..=119 => "a minute ago".into(),
        120..=3599 => format!("{} minutes ago", secs / 60),
        3600..=7199 => "an hour ago".into(),
        7200..=86399 => format!("{} hours ago", secs / 3600),
        86400..=172799 => "yesterday".into(),
        172800..=604799 => format!("{} days ago", secs / 86400),
        604800..=1209599 => "a week ago".into(),
        1209600..=2629799 => format!("{} weeks ago", secs / 604800),
        2629800..=5259599 => "a month ago".into(),
        5259600..=31557599 => format!("{} months ago", secs / 2629800),
        31557600..=63115199 => "a year ago".into(),
        _ => format!("{} years ago", secs / 31557600),
    }
}


const COLOR_ORANGE: egui::Color32 = egui::Color32::from_rgb(255, 165, 0);
const COLOR_YELLOW: egui::Color32 = egui::Color32::from_rgb(255, 215, 0);
const COLOR_GRAY: egui::Color32 = egui::Color32::GRAY;

/// Red broadcast-style "● LIVE" badge with a pulsing dot. When `connected`
/// is false the preview feed is actually lost, so the badge freezes to a
/// dim gray "NO SIGNAL" instead of pulsing red as if still live.
fn show_live_badge(ui: &mut egui::Ui, connected: bool) {
    let red = egui::Color32::from_rgb(198, 40, 40);
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let dot_r = 4.0f32;
    let padding_x = 8.0;
    let dot_text_gap = 5.0;
    // Match button height so the mode bar doesn't shift when switching modes.
    let h = ui.spacing().interact_size.y;

    let label = if connected { "LIVE" } else { "NO SIGNAL" };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font_id.clone(), egui::Color32::WHITE);

    let content_w = dot_r * 2.0 + dot_text_gap + galley.size().x;
    let total = egui::vec2(content_w + padding_x * 2.0, h);
    let (rect, _) = ui.allocate_exact_size(total, egui::Sense::hover());

    if ui.is_rect_visible(rect) {
        let dot_alpha = if connected {
            let t = ui.input(|i| i.time);
            let pulse = ((t * std::f64::consts::PI).sin() * 0.5 + 0.5) as f32;
            ui.ctx().request_repaint();
            (160.0 + pulse * 95.0) as u8
        } else {
            160
        };
        let bg = if connected { red } else { COLOR_GRAY };

        let painter = ui.painter();
        let corner_r = (rect.height() / 4.0) as u8;
        painter.rect_filled(rect, egui::CornerRadius::same(corner_r), bg);

        let dot_cx = rect.left() + padding_x + dot_r;
        let dot_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, dot_alpha);
        painter.circle_filled(egui::pos2(dot_cx, rect.center().y), dot_r, dot_color);

        let text_pos = egui::pos2(
            rect.left() + padding_x + dot_r * 2.0 + dot_text_gap,
            rect.center().y - galley.size().y / 2.0,
        );
        painter.galley(text_pos, galley, egui::Color32::WHITE);
    }
}

/// Step-frame button: draws a ▷| (forward) or |◁ (backward) glyph directly with the painter.
fn step_frame_button(
    ui: &mut egui::Ui,
    forward: bool,
    enabled: bool,
    hover_text: &str,
    size: egui::Vec2,
) -> egui::Response {
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let response = response.on_hover_text(hover_text);

    if ui.is_rect_visible(rect) {
        let visuals = if !enabled {
            ui.visuals().widgets.noninteractive
        } else if response.is_pointer_button_down_on() {
            ui.visuals().widgets.active
        } else if response.hovered() {
            ui.visuals().widgets.hovered
        } else {
            ui.visuals().widgets.inactive
        };

        let painter = ui.painter();
        painter.rect(
            rect,
            visuals.corner_radius,
            visuals.weak_bg_fill,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );

        let center = rect.center();
        let th = rect.height() * 0.22;   // triangle half-height
        let tw = th * 1.15;              // triangle width (base to tip)
        let gap = 2.5_f32;
        let bar_w = 2.0_f32;
        let icon_color = visuals.fg_stroke.color;
        let bar_stroke = egui::Stroke::new(bar_w, icon_color);

        // Center the bar+gap+triangle block horizontally.
        let total = bar_w + gap + tw;
        let x0 = center.x - total / 2.0;

        if forward {
            // ▷| — right-pointing triangle, then vertical bar
            let pts = vec![
                egui::pos2(x0, center.y - th),
                egui::pos2(x0, center.y + th),
                egui::pos2(x0 + tw, center.y),
            ];
            painter.add(egui::Shape::convex_polygon(pts, icon_color, egui::Stroke::NONE));
            let bx = x0 + tw + gap + bar_w / 2.0;
            painter.line_segment(
                [egui::pos2(bx, center.y - th), egui::pos2(bx, center.y + th)],
                bar_stroke,
            );
        } else {
            // |◁ — vertical bar, then left-pointing triangle
            let bx = x0 + bar_w / 2.0;
            painter.line_segment(
                [egui::pos2(bx, center.y - th), egui::pos2(bx, center.y + th)],
                bar_stroke,
            );
            let tri_x0 = x0 + bar_w + gap; // tip
            let tri_x1 = tri_x0 + tw;      // base
            let pts = vec![
                egui::pos2(tri_x1, center.y - th),
                egui::pos2(tri_x1, center.y + th),
                egui::pos2(tri_x0, center.y),
            ];
            painter.add(egui::Shape::convex_polygon(pts, icon_color, egui::Stroke::NONE));
        }
    }

    response
}

/// Pill-shaped segmented switch with "Live" on the left and "Replay" on the right.
/// Returns `Some(false)` when the user clicks Live, `Some(true)` when they click Replay,
/// or `None` when there is no actionable click.
/// When `disabled` is true the entire control is rendered dimmed and does not respond to clicks.
fn show_mode_switch(
    ui: &mut egui::Ui,
    in_replay: bool,
    live_enabled: bool,
    replay_enabled: bool,
    disabled: bool,
) -> Option<bool> {
    let total_width = 130.0;
    let h = ui.spacing().interact_size.y;
    let sense = if disabled { egui::Sense::hover() } else { egui::Sense::click() };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(total_width, h), sense);
    let mid_x = rect.left() + total_width / 2.0;
    let r = (h / 2.0) as u8;

    let left_rect = egui::Rect::from_min_max(rect.min, egui::pos2(mid_x, rect.max.y));
    let right_rect =
        egui::Rect::from_min_max(egui::pos2(mid_x, rect.min.y), rect.max);

    if ui.is_rect_visible(rect) {
        let red = egui::Color32::from_rgb(198, 40, 40);
        let blue = egui::Color32::from_rgb(42, 100, 170);
        let outer_bg = ui.visuals().widgets.noninteractive.bg_fill;
        let outer_stroke = ui.visuals().widgets.noninteractive.bg_stroke;
        let font_id = egui::TextStyle::Body.resolve(ui.style());
        let text_color = ui.visuals().text_color();
        let weak_color = ui.visuals().weak_text_color();

        let painter = ui.painter();

        // Outer pill container
        painter.rect(
            rect,
            egui::CornerRadius::same(r),
            outer_bg,
            outer_stroke,
            egui::StrokeKind::Inside,
        );

        if disabled {
            // Dim the active thumb to signal the control is locked
            let thumb_color = egui::Color32::from_rgba_unmultiplied(128, 128, 128, 80);
            let corner = if !in_replay {
                egui::CornerRadius { nw: r, sw: r, ne: 0, se: 0 }
            } else {
                egui::CornerRadius { nw: 0, sw: 0, ne: r, se: r }
            };
            let thumb_rect = if !in_replay { left_rect } else { right_rect };
            painter.rect_filled(thumb_rect, corner, thumb_color);
        } else if !in_replay {
            painter.rect_filled(
                left_rect,
                egui::CornerRadius { nw: r, sw: r, ne: 0, se: 0 },
                red,
            );
        } else {
            painter.rect_filled(
                right_rect,
                egui::CornerRadius { nw: 0, sw: 0, ne: r, se: r },
                blue,
            );
        }

        // Live label — white on active, normal/dim on inactive; all dim when disabled
        let live_color = if disabled {
            weak_color
        } else if !in_replay {
            egui::Color32::WHITE
        } else if live_enabled {
            text_color
        } else {
            weak_color
        };
        let live_galley =
            painter.layout_no_wrap("📡 Live".into(), font_id.clone(), live_color);
        painter.galley(
            egui::pos2(
                left_rect.center().x - live_galley.size().x / 2.0,
                left_rect.center().y - live_galley.size().y / 2.0,
            ),
            live_galley,
            live_color,
        );

        // Replay label
        let replay_color = if disabled {
            weak_color
        } else if in_replay {
            egui::Color32::WHITE
        } else if replay_enabled {
            text_color
        } else {
            weak_color
        };
        let replay_galley =
            painter.layout_no_wrap("↺ Replay".into(), font_id.clone(), replay_color);
        painter.galley(
            egui::pos2(
                right_rect.center().x - replay_galley.size().x / 2.0,
                right_rect.center().y - replay_galley.size().y / 2.0,
            ),
            replay_galley,
            replay_color,
        );
    }

    let response = if disabled {
        response.on_hover_text("Cannot switch modes while recording")
    } else {
        response
    };

    if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            if pos.x < mid_x && live_enabled {
                return Some(false);
            }
            if pos.x >= mid_x && replay_enabled {
                return Some(true);
            }
        }
    }

    None
}

pub(super) struct RecordingState {
    pending: Option<Uuid>,
    pending_trash: Option<Uuid>,
    pending_rating: Option<Uuid>,
    pending_comment: Option<Uuid>,
    pending_replay: Option<Uuid>,
    pending_selection: Option<Uuid>,
    local_star_value: Option<u8>,
    hovered_star: Option<u8>,
    comment_text: String,
    comment_focused: bool,
    selected_name: Option<String>,
    /// True when the server reports Activity::Replaying.
    pub(super) replay_mode: bool,
    replay_paused: bool,
    /// (frame_idx, total_frames, elapsed_ns, total_ns) from the last Replaying status.
    replay_progress: Option<(u32, u32, u64, u64)>,
    pending_transport: Option<Uuid>,
    seek_flash_at: Option<(Instant, u32)>,
}

impl Default for RecordingState {
    fn default() -> Self {
        Self {
            pending: None,
            pending_trash: None,
            pending_rating: None,
            pending_comment: None,
            pending_replay: None,
            pending_selection: None,
            local_star_value: None,
            hovered_star: None,
            comment_text: String::new(),
            comment_focused: false,
            selected_name: None,
            replay_mode: false,
            replay_paused: false,
            replay_progress: None,
            pending_transport: None,
            seek_flash_at: None,
        }
    }
}

fn reconcile_pending(
    pending: &mut Option<Uuid>,
    server_ack: &mut HashMap<Uuid, Timed<Result<(), String>>>,
    server_status: &Timed<ServerStatus>,
) {
    if let Some(cmd_uuid) = *pending {
        if let Some(ack) = server_ack.get(&cmd_uuid) {
            if server_status.at >= ack.at {
                *pending = None;
                server_ack.remove(&cmd_uuid);
            }
        }
    }
}

impl RecordingState {
    fn selected_recording<'a>(&self, recordings: &'a [PastRecording]) -> Option<&'a PastRecording> {
        match &self.selected_name {
            Some(n) => recordings.iter().find(|r| r.name == *n),
            None => recordings.first(),
        }
    }

    fn reconcile_from_server(
        &mut self,
        server_ack: &mut HashMap<Uuid, Timed<Result<(), String>>>,
        server_status: &Timed<ServerStatus>,
    ) {
        reconcile_pending(&mut self.pending, server_ack, server_status);
        reconcile_pending(&mut self.pending_trash, server_ack, server_status);
        reconcile_pending(&mut self.pending_rating, server_ack, server_status);
        reconcile_pending(&mut self.pending_comment, server_ack, server_status);
        reconcile_pending(&mut self.pending_replay, server_ack, server_status);
        reconcile_pending(&mut self.pending_selection, server_ack, server_status);
        reconcile_pending(&mut self.pending_transport, server_ack, server_status);

        // Server is authoritative on replay mode.
        match &server_status.value.activity {
            Activity::Replaying { frame_idx, total_frames, elapsed_ns, total_ns, paused, .. } => {
                self.replay_mode = true;
                self.replay_paused = *paused;
                self.replay_progress = Some((*frame_idx, *total_frames, *elapsed_ns, *total_ns));
            }
            _ => {
                self.replay_mode = false;
                self.replay_paused = false;
                self.replay_progress = None;
            }
        }

        let recordings = &server_status.value.recordings;

        // If the selected recording was removed (trashed), reset selection
        if let Some(ref name) = self.selected_name {
            if !recordings.iter().any(|r| r.name == *name) {
                self.selected_name = None;
                self.pending_trash = None;
                self.pending_rating = None;
                self.pending_comment = None;
                self.local_star_value = None;
                self.comment_text = String::new();
            }
        }

        // Server is authoritative on which recording is selected.
        // Sync only when we have no pending selection (avoid stomping optimistic local state).
        if self.pending_selection.is_none() {
            let server_sel = server_status.value.selected_recording.as_deref();
            if self.selected_name.as_deref() != server_sel {
                self.selected_name = server_sel.map(str::to_owned);
                self.pending_rating = None;
                self.pending_comment = None;
                self.local_star_value = self
                    .selected_recording(recordings)
                    .and_then(|r| r.rating);
                self.comment_text = self
                    .selected_recording(recordings)
                    .and_then(|r| r.comment.clone())
                    .unwrap_or_default();
            }
        }

        if let Some(sel) = self.selected_recording(recordings) {
            if self.pending_rating.is_none() {
                self.local_star_value = sel.rating;
            }
            if self.pending_comment.is_none() && !self.comment_focused {
                self.comment_text = sel.comment.clone().unwrap_or_default();
            }
        }
    }

    fn select_recording(&mut self, rec: &PastRecording) {
        self.selected_name = Some(rec.name.clone());
        self.local_star_value = rec.rating;
        self.comment_text = rec.comment.clone().unwrap_or_default();
        self.pending_rating = None;
        self.pending_comment = None;
        self.pending_trash = None;
    }

    fn show_record_button(&self, ui: &mut egui::Ui, is_recording: bool) -> bool {
        let is_pending = self.pending.is_some();
        let btn_label = if is_pending {
            if is_recording { "Stopping..." } else { "Starting..." }
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
        let icon = if is_recording { "■" } else { "▶" };
        let response = super::button::LeftAlignedButton::new(icon, btn_label)
            .fill(btn_color)
            .enabled(!is_pending)
            .show(ui)
            .on_hover_text("Press Spacebar to start/stop recording");
        let space_pressed = !is_pending
            && !ui.ctx().egui_wants_keyboard_input()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space));
        response.clicked() || space_pressed
    }

    fn show_recording_status(&self, ui: &mut egui::Ui, elapsed: &time::Duration, groups_saved: u32) {
        let mono = egui::TextStyle::Monospace.resolve(ui.style());
        ui.label(egui::RichText::new(format_duration_aligned(elapsed)).font(mono.clone()))
            .on_hover_text("Elapsed time");
        ui.label("⏱");
        ui.label(egui::RichText::new(format!("{groups_saved:6}")).font(mono))
            .on_hover_text("Number of frame groups saved");
        ui.label("#");
    }

    fn show_time_ago(ui: &mut egui::Ui, finished_at: Option<time::OffsetDateTime>, server_time: time::OffsetDateTime) {
        if let Some(ago) = finished_at.and_then(|t| {
            let diff = server_time - t;
            u64::try_from(diff.whole_seconds()).ok().map(format_time_ago)
        }) {
            ui.label(ago);
        }
    }

    fn show_saved_failed_label(ui: &mut egui::Ui, ok: bool) {
        let (label, color) = if ok {
            ("saved", egui::Color32::from_rgb(56, 142, 60))
        } else {
            ("failed", egui::Color32::from_rgb(198, 40, 40))
        };
        ui.label(egui::RichText::new(label).color(color).strong());
    }

    fn show_library_dropdown(
        &mut self,
        ui: &mut egui::Ui,
        recordings: &[PastRecording],
        selected: &Option<PastRecording>,
        button_width: f32,
        enabled: bool,
    ) -> Option<usize> {
        use super::dropdown::{Dropdown, DropdownCol, DropdownRow};

        let selected_text = selected
            .as_ref()
            .map(|s| s.name.clone())
            .unwrap_or_default();

        let rows: Vec<DropdownRow> = recordings
            .iter()
            .map(|rec| {
                let stars = match rec.rating {
                    Some(n) => "★".repeat(n as usize),
                    None => String::new(),
                };
                DropdownRow {
                    cols: vec![
                        DropdownCol::Text(rec.name.clone()),
                        DropdownCol::Text(rec.groups_saved.to_string()),
                        DropdownCol::Text(format_elapsed(rec.elapsed)),
                        DropdownCol::FixedWidth(stars, 60.0),
                        DropdownCol::Text(rec.comment.clone().unwrap_or_default()),
                    ],
                    selected: selected.as_ref().map_or(false, |s| s.name == rec.name),
                }
            })
            .collect();

        Dropdown::new("library_popup_grid", selected_text)
            .button_width(button_width)
            .enabled(enabled)
            .rows(rows)
            .show(ui)
    }

    fn send_comment(&mut self, name: &str, cmd_tx: &tokio_mpsc::UnboundedSender<ClientMessage>) {
        let cmd_uuid = Uuid::new_v4();
        self.pending_comment = Some(cmd_uuid);
        let _ = cmd_tx.send(ClientMessage {
            uuid: cmd_uuid,
            command: ClientCommand::CommentRecording {
                name: name.to_string(),
                comment: self.comment_text.clone(),
            },
        });
    }

    fn show_trash_and_stars(
        &mut self,
        ui: &mut egui::Ui,
        name: &str,
        cmd_tx: &tokio_mpsc::UnboundedSender<ClientMessage>,
    ) {
        let trash_pending = self.pending_trash.is_some();
        if super::button::HoldToConfirmButton::new("🗑", "trash it")
            .hold_seconds(1.5)
            .unlock_seconds(5.0)
            .enabled(!trash_pending)
            .hover_text("Long-click to unlock, then click to trash")
            .show(ui)
        {
            let cmd_uuid = Uuid::new_v4();
            self.pending_trash = Some(cmd_uuid);
            let _ = cmd_tx.send(ClientMessage {
                uuid: cmd_uuid,
                command: ClientCommand::TrashRecording { name: name.to_string() },
            });
        }

        ui.add_space(8.0);

        let rating_pending = self.pending_rating.is_some();
        let display_value = self.hovered_star.or(self.local_star_value);
        let mut new_hover: Option<u8> = None;
        for i in 1..=5u8 {
            let filled = display_value.map_or(false, |v| i <= v);
            let color = if self.hovered_star.map_or(false, |v| i <= v) {
                COLOR_ORANGE
            } else if filled {
                if rating_pending { COLOR_ORANGE } else { COLOR_YELLOW }
            } else {
                COLOR_GRAY
            };
            let star = egui::RichText::new("★").color(color);
            let btn = egui::Button::new(star).frame(false);
            let response = ui.add(btn);
            if response.hovered() {
                new_hover = Some(i);
            }
            if response.clicked() {
                let cmd_uuid = Uuid::new_v4();
                self.pending_rating = Some(cmd_uuid);
                self.local_star_value = Some(i);
                let _ = cmd_tx.send(ClientMessage {
                    uuid: cmd_uuid,
                    command: ClientCommand::RateRecording {
                        name: name.to_string(),
                        rating: i,
                    },
                });
            }
        }
        self.hovered_star = new_hover;
    }

    fn show_comment(
        &mut self,
        ui: &mut egui::Ui,
        name: &str,
        cmd_tx: &tokio_mpsc::UnboundedSender<ClientMessage>,
    ) {
        let comment_pending = self.pending_comment.is_some();
        let text_color = if comment_pending { COLOR_ORANGE } else { ui.visuals().text_color() };
        let remaining = ui.available_width();
        let response = ui.add(
            egui::TextEdit::singleline(&mut self.comment_text)
                .hint_text("comment")
                .text_color(text_color)
                .desired_width(remaining),
        );
        self.comment_focused = response.has_focus();
        if response.lost_focus() {
            self.send_comment(name, cmd_tx);
        }
    }

    pub(super) fn show_mode_bar(
        &mut self,
        ui: &mut egui::Ui,
        cmd_tx: &tokio_mpsc::UnboundedSender<ClientMessage>,
        preview_connected: bool,
    ) {
        let bar_h = ui.spacing().interact_size.y;
        ui.horizontal(|ui| {
            ui.set_min_height(bar_h);
            if self.replay_mode {
                let transport_free = self.pending_transport.is_none();
                let btn_size = {
                    let font_id = egui::TextStyle::Button.resolve(ui.style());
                    let glyph_w = ui.painter()
                        .layout_no_wrap("▶".to_string(), font_id, egui::Color32::WHITE)
                        .size().x;
                    let pad = ui.spacing().button_padding.x;
                    egui::vec2((glyph_w + pad * 2.0).max(bar_h), bar_h)
                };
                let mut send_transport = |cmd: ClientCommand| {
                    let uuid = Uuid::new_v4();
                    self.pending_transport = Some(uuid);
                    let _ = cmd_tx.send(ClientMessage { uuid, command: cmd });
                };

                if ui.add_enabled(transport_free, egui::Button::new("⏮").min_size(btn_size))
                    .on_hover_text("Rewind to start").clicked()
                {
                    send_transport(ClientCommand::RewindReplay);
                }
                if step_frame_button(ui, false, transport_free, "Previous frame", btn_size).clicked() {
                    send_transport(ClientCommand::StepReplayBackward);
                }
                let play_pause_label = if self.replay_paused { "▶" } else { "⏸" };
                let play_pause_cmd = if self.replay_paused {
                    ClientCommand::ResumeReplay
                } else {
                    ClientCommand::PauseReplay
                };
                if ui.add_enabled(transport_free, egui::Button::new(play_pause_label).min_size(btn_size))
                    .on_hover_text(if self.replay_paused { "Resume" } else { "Pause" }).clicked()
                {
                    send_transport(play_pause_cmd);
                }
                if step_frame_button(ui, true, transport_free, "Next frame", btn_size).clicked() {
                    send_transport(ClientCommand::StepReplayForward);
                }
                ui.add_space(4.0);

                let (progress, left_text, right_text) =
                    if let Some((frame_idx, total_frames, elapsed_ns, total_ns)) = self.replay_progress {
                        let p = if total_frames > 0 { (frame_idx + 1) as f32 / total_frames as f32 } else { 0.0 };
                        let n_digits = total_frames.to_string().len().max(1);
                        let left = format!("{:>n_digits$} / {total_frames}", frame_idx + 1);
                        let right = format!(
                            "{} / {}",
                            format_replay_time(elapsed_ns, total_ns),
                            format_replay_time(total_ns, total_ns),
                        );
                        (p, left, right)
                    } else {
                        (0.0, String::new(), String::new())
                    };

                let bar_response = ui
                    .add(egui::ProgressBar::new(progress).desired_width(ui.available_width()));
                let bar_rect = bar_response.rect;

                if let Some((_, total_frames, _, _)) = self.replay_progress {
                    if total_frames > 0 {
                        // Flash pill: drawn regardless of transport_free so the animation
                        // isn't dropped while waiting for the server ack.
                        const FLASH_MS: f32 = 250.0;
                        let flash_pill: Option<(u32, f32)> = if let Some((t, f)) = self.seek_flash_at {
                            let elapsed = t.elapsed().as_millis() as f32;
                            if elapsed < FLASH_MS {
                                ui.ctx().request_repaint();
                                Some((f, egui::lerp(220.0..=70.0, elapsed / FLASH_MS)))
                            } else {
                                self.seek_flash_at = None;
                                None
                            }
                        } else {
                            None
                        };

                        // Hover pill and click — only when a seek isn't already in-flight.
                        let hover_pill: Option<(u32, f32)> = if transport_free {
                            let interact = bar_response.interact(egui::Sense::click());
                            let hovered_frame = interact.hover_pos().map(|ptr| {
                                let t = ((ptr.x - bar_rect.left()) / bar_rect.width()).clamp(0.0, 1.0);
                                ((t * total_frames as f32) as u32).min(total_frames - 1)
                            });
                            if interact.clicked() {
                                if let Some(f) = hovered_frame {
                                    send_transport(ClientCommand::SeekReplayToFrame { frame_idx: f });
                                    self.seek_flash_at = Some((Instant::now(), f));
                                }
                            }
                            hovered_frame.map(|f| (f, 70.0))
                        } else {
                            None
                        };

                        if let Some((f, alpha)) = flash_pill.or(hover_pill) {
                            let x0 = bar_rect.left() + (f as f32 / total_frames as f32) * bar_rect.width();
                            let x1 = bar_rect.left() + ((f + 1) as f32 / total_frames as f32) * bar_rect.width();
                            let pill = egui::Rect::from_min_max(
                                egui::pos2(x0, bar_rect.top()),
                                egui::pos2(x1, bar_rect.bottom()),
                            );
                            let radius = (pill.height() / 2.0) as u8;
                            ui.painter().rect_filled(pill, egui::CornerRadius::same(radius), egui::Color32::from_white_alpha(alpha as u8));
                        }
                    }
                }

                if !left_text.is_empty() {
                    let mono = egui::TextStyle::Monospace.resolve(ui.style());
                    let painter = ui.painter();
                    painter.text(
                        bar_rect.left_center() + egui::vec2(6.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        left_text,
                        mono.clone(),
                        egui::Color32::WHITE,
                    );
                    painter.text(
                        bar_rect.right_center() - egui::vec2(6.0, 0.0),
                        egui::Align2::RIGHT_CENTER,
                        right_text,
                        mono,
                        egui::Color32::WHITE,
                    );
                }
            } else {
                show_live_badge(ui, preview_connected);
            }
        });
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

        let is_recording = server_status
            .as_ref()
            .map_or(false, |t| matches!(t.value.activity, Activity::Recording { .. }));

        let recordings = server_status
            .as_ref()
            .map(|t| t.value.recordings.as_slice())
            .unwrap_or(&[]);

        let selected = self.selected_recording(recordings).cloned();

        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 10.0;
        let col1_width = 220.0;

        let mut activated = false;

        egui::Grid::new("recording_grid")
            .num_columns(3)
            .min_row_height(row_height)
            .show(ui, |ui| {
                // Row 1, Col 1: start/stop button
                ui.set_min_width(col1_width);
                activated = self.show_record_button(ui, is_recording);

                // Row 1, Col 2: recording name
                if is_recording {
                    let name = if let Some(Activity::Recording { ref name, .. }) =
                        server_status.as_ref().map(|t| &t.value.activity)
                    {
                        name.clone()
                    } else {
                        String::new()
                    };
                    let dummy_selected = Some(PastRecording {
                        name,
                        ok: false,
                        elapsed: time::Duration::ZERO,
                        groups_saved: 0,
                        finished_at: None,
                        rating: None,
                        comment: None,
                    });
                    let _ = self.show_library_dropdown(ui, &[], &dummy_selected, col1_width - 8.0, false);
                } else if !recordings.is_empty() {
                    if let Some(idx) = self.show_library_dropdown(ui, recordings, &selected, col1_width - 8.0, true) {
                        let rec = &recordings[idx];
                        self.select_recording(rec);
                        let cmd_uuid = Uuid::new_v4();
                        self.pending_selection = Some(cmd_uuid);
                        let _ = cmd_tx.send(ClientMessage {
                            uuid: cmd_uuid,
                            command: ClientCommand::SelectRecording { name: rec.name.clone() },
                        });
                    }
                } else {
                    ui.label("");
                }

                // Row 1, Col 3: timer ; n-frames ; time-ago ; saved/failed
                ui.horizontal(|ui| {
                    if let Some(Activity::Recording { elapsed, groups_saved, .. }) =
                        server_status.as_ref().map(|t| &t.value.activity)
                    {
                        self.show_recording_status(ui, elapsed, *groups_saved);
                    } else if let Some(ref sel) = selected {
                        self.show_recording_status(ui, &sel.elapsed, sel.groups_saved);
                        if let Some(timed_status) = server_status.as_ref() {
                            ui.add_space(8.0);
                            Self::show_time_ago(ui, sel.finished_at, timed_status.value.server_time);
                        }
                        ui.add_space(8.0);
                        Self::show_saved_failed_label(ui, sel.ok);
                    }
                });

                ui.end_row();

                // Row 2, Col 1: Live / Replay pill switch
                ui.set_min_width(col1_width);
                let replay_pending = self.pending_replay.is_some();
                let live_enabled = self.replay_mode && !replay_pending;
                let replay_enabled = !is_recording && selected.is_some() && !self.replay_mode && !replay_pending;

                if let Some(want_replay) = show_mode_switch(ui, self.replay_mode, live_enabled, replay_enabled, is_recording) {
                    let cmd_uuid = Uuid::new_v4();
                    self.pending_replay = Some(cmd_uuid);
                    let cmd = if want_replay {
                        ClientCommand::StartReplay {
                            name: selected.as_ref().map(|s| s.name.clone()).unwrap_or_default(),
                        }
                    } else {
                        ClientCommand::StopReplay
                    };
                    let _ = cmd_tx.send(ClientMessage { uuid: cmd_uuid, command: cmd });
                }

                // Row 2, Col 2: trash ; stars
                if !is_recording {
                    if let Some(ref sel) = selected {
                        ui.horizontal(|ui| {
                            self.show_trash_and_stars(ui, &sel.name, cmd_tx);
                        });
                    } else {
                        ui.label("");
                    }
                } else {
                    ui.label("");
                }

                // Row 2, Col 3: comment
                if !is_recording {
                    if let Some(ref sel) = selected {
                        self.show_comment(ui, &sel.name, cmd_tx);
                    } else {
                        ui.label("");
                    }
                } else {
                    ui.label("");
                }

                ui.end_row();
            });

        if activated {
            let cmd_uuid = Uuid::new_v4();
            let cmd = if is_recording {
                ClientCommand::StopRecording
            } else {
                self.selected_name = None;
                ClientCommand::StartRecording
            };
            self.pending = Some(cmd_uuid);
            let _ = cmd_tx.send(ClientMessage {
                uuid: cmd_uuid,
                command: cmd,
            });
        }
    }
}
