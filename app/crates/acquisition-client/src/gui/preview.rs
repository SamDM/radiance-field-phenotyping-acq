use std::collections::HashMap;

use acquisition_common::protocol::{ClientCommand, ClientMessage, Zoom};
use acquisition_common::{Color, Frame};
use eframe::egui;
use eframe::egui::accesskit::Uuid;
use time::OffsetDateTime;
use tokio::sync::mpsc as tokio_mpsc;

use super::Timed;

pub(super) struct FramePreviewState {
    preview_last_seen: HashMap<String, OffsetDateTime>,
    preview_pending_frames: HashMap<String, Frame<Color>>,
    textures: HashMap<String, egui::TextureHandle>,
    /// Orange crosshair position, shown while a zoom request is pending ACK.
    /// `None` position means a pending *clear* (no crosshair to draw).
    pending_crosshairs: HashMap<String, (Uuid, Option<egui::Vec2>)>,
    /// Server-authoritative zoom state, updated unconditionally from preview frames.
    server_zoom: HashMap<String, Zoom>,
    zoom_textures: HashMap<String, egui::TextureHandle>,
}

impl Default for FramePreviewState {
    fn default() -> Self {
        Self {
            preview_last_seen: HashMap::new(),
            preview_pending_frames: HashMap::new(),
            textures: HashMap::new(),
            pending_crosshairs: HashMap::new(),
            server_zoom: HashMap::new(),
            zoom_textures: HashMap::new(),
        }
    }
}

impl FramePreviewState {
    pub(super) fn ingest_frames(&mut self, frames: Vec<(Frame<Color>, Option<Zoom>)>) {
        for (frame, zoom) in frames {
            let name = frame.camera_name.clone();
            self.preview_last_seen
                .insert(name.clone(), frame.camera_datetime_local());
            match zoom {
                Some(z) => {
                    self.server_zoom.insert(name.clone(), z);
                }
                None => {
                    self.server_zoom.remove(&name);
                    self.zoom_textures.remove(&name);
                }
            }
            self.preview_pending_frames.insert(name, frame);
        }
    }

    fn reconcile_from_server(
        &mut self,
        server_ack: &mut HashMap<Uuid, Timed<Result<(), String>>>,
    ) {
        self.pending_crosshairs.retain(|_cam, (cmd_uuid, _pos)| {
            server_ack.remove(cmd_uuid).is_none()
        });
    }

    fn upload_textures(&mut self, ctx: &egui::Context) {
        for (cam_name, frame) in self.preview_pending_frames.drain() {
            let img = frame.image();
            let color_image = egui::ColorImage::from_rgb(
                [img.width() as usize, img.height() as usize],
                img.as_raw(),
            );
            let handle = ctx.load_texture(
                format!("camera-{cam_name}"),
                color_image,
                Default::default(),
            );
            self.textures.insert(cam_name, handle);
        }

        let mut stale_zoom_keys: Vec<String> = self.zoom_textures.keys().cloned().collect();
        for (cam_name, zoom) in &self.server_zoom {
            stale_zoom_keys.retain(|k| k != cam_name);
            let img = &zoom.crop;
            let color_image = egui::ColorImage::from_rgb(
                [img.width() as usize, img.height() as usize],
                img.as_raw(),
            );
            let handle = ctx.load_texture(
                format!("zoom-{cam_name}"),
                color_image,
                egui::TextureOptions::NEAREST,
            );
            self.zoom_textures.insert(cam_name.clone(), handle);
        }
        for key in stale_zoom_keys {
            self.zoom_textures.remove(&key);
        }
    }

    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        server_ack: &mut HashMap<Uuid, Timed<Result<(), String>>>,
        cmd_tx: &tokio_mpsc::UnboundedSender<ClientMessage>,
    ) {
        self.reconcile_from_server(server_ack);
        self.upload_textures(ui.ctx());

        if self.textures.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label("Waiting for camera frames...");
            });
        } else {
            let mut entries: Vec<_> = self
                .textures
                .iter()
                .map(|(name, handle)| {
                    let ts = self.preview_last_seen.get(name).copied();
                    (name.as_str(), ts, handle)
                })
                .collect();
            entries.sort_by(|(a, _, _), (b, _, _)| a.cmp(b));
            draw_camera_previews(
                ui,
                &entries,
                &mut self.pending_crosshairs,
                &self.server_zoom,
                &self.zoom_textures,
                cmd_tx,
            );
        }
    }
}

fn send_set_zoom(
    cmd_tx: &tokio_mpsc::UnboundedSender<ClientMessage>,
    camera_name: &str,
    position: Option<(f32, f32)>,
) -> Uuid {
    let uuid = Uuid::new_v4();
    let _ = cmd_tx.send(ClientMessage {
        uuid,
        command: ClientCommand::SetZoom {
            camera_name: camera_name.to_string(),
            position,
        },
    });
    uuid
}

fn format_camera_timestamp(timestamp: &OffsetDateTime) -> String {
    if timestamp.year() >= 2000 {
        let fmt = time::format_description::parse(
            "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:6]",
        )
        .unwrap();
        timestamp.format(&fmt).unwrap_or_default()
    } else {
        let d = time::Duration::nanoseconds(timestamp.unix_timestamp_nanos() as i64);
        let days = d.whole_days();
        let hours = d.whole_hours() % 24;
        let minutes = d.whole_minutes() % 60;
        let secs = d.whole_seconds() % 60;
        let ms = (d.whole_milliseconds() % 1000) as i64;
        format!("{days}d {hours:02}:{minutes:02}:{secs:02}.{ms:03} (since start)")
    }
}

fn draw_crosshairs(painter: &egui::Painter, rect: egui::Rect, rel: egui::Vec2, color: egui::Color32) {
    let x = rect.min.x + rel.x * rect.width();
    let y = rect.min.y + rel.y * rect.height();
    let stroke = egui::Stroke::new(1.0, color);
    painter.line_segment([egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)], stroke);
    painter.line_segment([egui::pos2(x, rect.min.y), egui::pos2(x, rect.max.y)], stroke);
}

fn draw_zoom_inset(
    painter: &egui::Painter,
    rect: egui::Rect,
    rel: egui::Vec2,
    zoom_tex: &egui::TextureHandle,
) {
    const ZOOM_PIXEL_SCALE: f32 = 3.0;
    let native_side = zoom_tex.size()[0].max(zoom_tex.size()[1]) as f32;
    let max_side = rect.width().min(rect.height()) / 2.0;
    let side = (native_side * ZOOM_PIXEL_SCALE).min(max_side);
    let margin = 4.0;
    let min_x = if rel.x < 0.5 {
        rect.max.x - side - margin
    } else {
        rect.min.x + margin
    };
    let min_y = if rel.y < 0.5 {
        rect.max.y - side - margin
    } else {
        rect.min.y + margin
    };
    let inset_rect = egui::Rect::from_min_size(egui::pos2(min_x, min_y), egui::vec2(side, side));
    painter.image(
        zoom_tex.id(),
        inset_rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    painter.rect_stroke(
        inset_rect,
        egui::CornerRadius::ZERO,
        egui::Stroke::new(1.0, egui::Color32::RED),
        egui::StrokeKind::Inside,
    );
}

fn handle_zoom_clicks(
    response: &egui::Response,
    cam_name: &str,
    pending_crosshairs: &mut HashMap<String, (Uuid, Option<egui::Vec2>)>,
    cmd_tx: &tokio_mpsc::UnboundedSender<ClientMessage>,
) {
    if response.double_clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let rel = (pos - response.rect.min) / response.rect.size();
            let uuid = send_set_zoom(cmd_tx, cam_name, Some((rel.x, rel.y)));
            pending_crosshairs.insert(cam_name.to_string(), (uuid, Some(rel)));
        }
    } else if response.clicked() {
        let uuid = send_set_zoom(cmd_tx, cam_name, None);
        pending_crosshairs.insert(cam_name.to_string(), (uuid, None));
    }
}

fn draw_camera_previews(
    ui: &mut egui::Ui,
    cameras: &[(&str, Option<OffsetDateTime>, &egui::TextureHandle)],
    pending_crosshairs: &mut HashMap<String, (Uuid, Option<egui::Vec2>)>,
    server_zoom: &HashMap<String, Zoom>,
    zoom_textures: &HashMap<String, egui::TextureHandle>,
    cmd_tx: &tokio_mpsc::UnboundedSender<ClientMessage>,
) {
    let mono_font_id = egui::TextStyle::Monospace.resolve(ui.style());
    let timestamp_sample = "YYYY-MM-DD HH:MM:SS.ffffff";
    let min_col_width = ui.ctx().fonts_mut(|f| {
        let mut measure = |s: &str| {
            f.layout_no_wrap(s.to_owned(), mono_font_id.clone(), egui::Color32::WHITE)
                .rect
                .width()
        };
        let ts_w = measure(timestamp_sample);
        cameras
            .iter()
            .map(|(name, _, _)| measure(name).max(ts_w))
            .fold(0.0_f32, f32::max)
    });

    let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + ui.spacing().item_spacing.y;
    let image_avail_h = (ui.available_height() - 2.0 * row_h).max(0.0);

    ui.horizontal(|ui| {
        let n = cameras.len() as f32;
        let spacing = ui.spacing().item_spacing.x * (n - 1.0);
        let per_camera_width = ((ui.available_width() - spacing) / n).max(min_col_width);

        for (cam_name, timestamp, handle) in cameras {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.monospace(*cam_name);
                });
                if let Some(timestamp) = timestamp {
                    let text = format_camera_timestamp(timestamp);
                    ui.horizontal(|ui| {
                        ui.monospace(text);
                    });
                }

                let native = egui::vec2(handle.size()[0] as f32, handle.size()[1] as f32);
                let scale_w = per_camera_width / native.x;
                let scale_h = if image_avail_h > 0.0 {
                    image_avail_h / native.y
                } else {
                    scale_w
                };
                let image_size = native * scale_w.min(scale_h);
                let response = ui.add(
                    egui::Image::new((handle.id(), image_size))
                        .sense(egui::Sense::click()),
                );

                handle_zoom_clicks(&response, cam_name, pending_crosshairs, cmd_tx);

                let rect = response.rect;
                let painter = ui.painter();

                if let Some(zoom) = server_zoom.get(*cam_name) {
                    let rel = egui::vec2(zoom.rel_x, zoom.rel_y);
                    draw_crosshairs(painter, rect, rel, egui::Color32::RED);
                    if let Some(zoom_tex) = zoom_textures.get(*cam_name) {
                        draw_zoom_inset(painter, rect, rel, zoom_tex);
                    }
                }

                if let Some((_uuid, Some(rel))) = pending_crosshairs.get(*cam_name) {
                    draw_crosshairs(painter, rect, *rel, egui::Color32::from_rgb(255, 140, 0));
                }
            });
        }
    });
}
