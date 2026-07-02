use eframe::egui;

const ARMED_RED: egui::Color32 = egui::Color32::from_rgb(200, 40, 40);

/// A trash button paired with a separate padlock button. Long-clicking the
/// padlock fills a progress bar (bottom-to-top) inside it; once full, the
/// padlock flips to "unlocked" and stays red, and the trash button turns red
/// to indicate it is armed. Clicking the trash button while armed confirms
/// the action (and re-arms, so users can spam-click to remove several items
/// in a row). Once the armed window elapses, the trash button reverts to its
/// normal color and the padlock's progress bar drains back down.
pub struct TrashLockButton<'a> {
    hold_seconds: f32,
    unlock_seconds: f32,
    enabled: bool,
    trash_hover_text: Option<&'a str>,
    lock_hover_text: Option<&'a str>,
}

impl<'a> TrashLockButton<'a> {
    pub fn new() -> Self {
        Self {
            hold_seconds: 3.0,
            unlock_seconds: 3.0,
            enabled: true,
            trash_hover_text: None,
            lock_hover_text: None,
        }
    }

    pub fn hold_seconds(mut self, secs: f32) -> Self {
        self.hold_seconds = secs;
        self
    }

    pub fn unlock_seconds(mut self, secs: f32) -> Self {
        self.unlock_seconds = secs;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn trash_hover_text(mut self, text: &'a str) -> Self {
        self.trash_hover_text = Some(text);
        self
    }

    pub fn lock_hover_text(mut self, text: &'a str) -> Self {
        self.lock_hover_text = Some(text);
        self
    }

    pub fn show(self, ui: &mut egui::Ui) -> bool {
        let font_id = egui::TextStyle::Button.resolve(ui.style());
        let padding = ui.spacing().button_padding;

        let trash_text = "🗑 trash it";
        let trash_measure = ui.painter().layout_no_wrap(
            trash_text.to_string(),
            font_id.clone(),
            egui::Color32::WHITE,
        );
        let trash_size = egui::vec2(
            trash_measure.size().x + padding.x * 2.0,
            trash_measure.size().y + padding.y * 2.0,
        );
        let lock_size = egui::vec2(trash_size.y, trash_size.y);
        let connector_w = 6.0;
        let total_size = egui::vec2(trash_size.x + connector_w + lock_size.x, trash_size.y);

        let (outer_rect, outer_response) =
            ui.allocate_exact_size(total_size, egui::Sense::hover());
        let id = outer_response.id;

        let trash_rect = egui::Rect::from_min_size(outer_rect.min, trash_size);
        let connector_rect = egui::Rect::from_min_size(
            egui::pos2(trash_rect.right(), outer_rect.top()),
            egui::vec2(connector_w, trash_size.y),
        );
        let lock_rect = egui::Rect::from_min_size(
            egui::pos2(connector_rect.right(), outer_rect.top()),
            lock_size,
        );

        let progress_id = id.with("progress");
        let unlock_id = id.with("unlock_until");
        let flash_id = id.with("flash_at");
        let lock_denied_flash_id = id.with("lock_denied_flash_at");

        let lock_sense = if self.enabled {
            egui::Sense::click_and_drag()
        } else {
            egui::Sense::hover()
        };
        let trash_sense = if self.enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        };
        let lock_response = ui.interact(lock_rect, id.with("lock"), lock_sense);
        let trash_response = ui.interact(trash_rect, id.with("trash"), trash_sense);

        let progress: f32 = ui.data(|d| d.get_temp(progress_id).unwrap_or(0.0));
        let unlock_until: f64 = ui.data(|d| d.get_temp(unlock_id).unwrap_or(0.0));
        let now: f64 = ui.input(|i| i.time);
        let dt = ui.input(|i| i.stable_dt);
        let armed = unlock_until > now;

        let holding = lock_response.is_pointer_button_down_on();
        let rate = 1.0 / self.hold_seconds;

        if armed {
            // Stay full while armed; once the armed window elapses this
            // branch stops firing and the "drain" branch below takes over.
            ui.data_mut(|d| d.insert_temp(progress_id, 1.0f32));
            ui.ctx().request_repaint();
        } else if holding {
            let new_progress = (progress + dt * rate).min(1.0);
            ui.data_mut(|d| d.insert_temp(progress_id, new_progress));
            if new_progress >= 1.0 {
                ui.data_mut(|d| d.insert_temp(unlock_id, now + self.unlock_seconds as f64));
                ui.data_mut(|d| d.insert_temp(flash_id, now));
            }
            ui.ctx().request_repaint();
        } else if progress > 0.0 {
            let new_progress = (progress - dt * rate).max(0.0);
            ui.data_mut(|d| d.insert_temp(progress_id, new_progress));
            ui.ctx().request_repaint();
        }

        let mut confirmed = false;
        if trash_response.clicked() {
            if armed {
                confirmed = true;
                // Re-arm on confirm so spam-clicking removes several recordings.
                ui.data_mut(|d| d.insert_temp(unlock_id, now + self.unlock_seconds as f64));
                ui.data_mut(|d| d.insert_temp(flash_id, now));
            } else {
                // Trash was clicked while still locked: draw attention to the
                // padlock by flashing its icon color, since clicking trash
                // does nothing until the lock is held/armed.
                ui.data_mut(|d| d.insert_temp(lock_denied_flash_id, now));
            }
        }

        let cur_progress: f32 = ui.data(|d| d.get_temp(progress_id).unwrap_or(0.0));
        let is_armed: bool = ui.data(|d| d.get_temp::<f64>(unlock_id).unwrap_or(0.0)) > now;

        if ui.is_rect_visible(outer_rect) {
            // ---- trash button ----
            let trash_visuals = if !self.enabled {
                &ui.style().visuals.widgets.inactive
            } else if trash_response.hovered() {
                &ui.style().visuals.widgets.hovered
            } else {
                &ui.style().visuals.widgets.inactive
            };
            let trash_rounding = trash_visuals.corner_radius;
            let trash_bg = if is_armed { ARMED_RED } else { trash_visuals.bg_fill };
            ui.painter().rect(
                trash_rect,
                trash_rounding,
                trash_bg,
                trash_visuals.bg_stroke,
                egui::StrokeKind::Inside,
            );

            let trash_text_color = if !self.enabled {
                ui.visuals().weak_text_color()
            } else if is_armed {
                egui::Color32::WHITE
            } else {
                trash_visuals.text_color()
            };
            let trash_galley =
                ui.painter()
                    .layout_no_wrap(trash_text.to_string(), font_id.clone(), trash_text_color);
            let trash_pos = trash_rect.center() - trash_galley.size() / 2.0;
            ui.painter().galley(trash_pos, trash_galley, trash_text_color);

            let flash_at: f64 = ui.data(|d| d.get_temp(flash_id).unwrap_or(0.0));
            let flash_duration = 0.3;
            let flash_elapsed = now - flash_at;
            if flash_at > 0.0 && flash_elapsed < flash_duration {
                let alpha = (1.0 - flash_elapsed / flash_duration) as f32;
                let a = (alpha * 180.0) as u8;
                ui.painter().rect_filled(
                    trash_rect,
                    trash_rounding,
                    egui::Color32::from_rgba_premultiplied(a, a, a, a),
                );
            }

            // ---- connector, to visually group the two buttons ----
            let connector_color = if is_armed { ARMED_RED } else { trash_visuals.bg_fill };
            let mid_y = outer_rect.center().y;
            ui.painter().line_segment(
                [
                    egui::pos2(connector_rect.left(), mid_y),
                    egui::pos2(connector_rect.right(), mid_y),
                ],
                egui::Stroke::new(2.0, connector_color),
            );

            // ---- lock button ----
            let lock_visuals = if !self.enabled {
                &ui.style().visuals.widgets.inactive
            } else if holding || is_armed {
                &ui.style().visuals.widgets.active
            } else if lock_response.hovered() {
                &ui.style().visuals.widgets.hovered
            } else {
                &ui.style().visuals.widgets.inactive
            };
            let lock_rounding = lock_visuals.corner_radius;
            ui.painter().rect(
                lock_rect,
                lock_rounding,
                lock_visuals.bg_fill,
                lock_visuals.bg_stroke,
                egui::StrokeKind::Inside,
            );

            if cur_progress > 0.0 {
                // Fills bottom-to-top.
                let fill_h = lock_rect.height() * cur_progress;
                let fill_rect = egui::Rect::from_min_max(
                    egui::pos2(lock_rect.left(), lock_rect.bottom() - fill_h),
                    lock_rect.max,
                );
                let red = if is_armed {
                    ARMED_RED
                } else {
                    egui::Color32::from_rgba_premultiplied(200, 40, 40, 140)
                };
                ui.painter().rect_filled(fill_rect, lock_rounding, red);
            }

            // Blink the lock's icon color red twice (on-off-on-off) when the
            // user clicked trash while it was still locked.
            let denied_flash_at: f64 = ui.data(|d| d.get_temp(lock_denied_flash_id).unwrap_or(0.0));
            let denied_flash_half_period = 0.15;
            let denied_flash_elapsed = now - denied_flash_at;
            let denied_flash_total = denied_flash_half_period * 4.0;
            let denied_flashing = denied_flash_at > 0.0 && denied_flash_elapsed < denied_flash_total;
            if denied_flashing {
                ui.ctx().request_repaint();
            }
            let denied_flash_on = denied_flashing
                && ((denied_flash_elapsed / denied_flash_half_period) as u32) % 2 == 0;

            let lock_icon = if is_armed { "🔓" } else { "🔒" };
            let lock_text_color = if !self.enabled {
                ui.visuals().weak_text_color()
            } else if is_armed {
                egui::Color32::WHITE
            } else if denied_flash_on {
                ARMED_RED
            } else {
                lock_visuals.text_color()
            };
            let lock_galley =
                ui.painter()
                    .layout_no_wrap(lock_icon.to_string(), font_id.clone(), lock_text_color);
            let lock_pos = lock_rect.center() - lock_galley.size() / 2.0;
            ui.painter().galley(lock_pos, lock_galley, lock_text_color);
        }

        if let Some(text) = self.trash_hover_text {
            trash_response.on_hover_text(text);
        }
        if let Some(text) = self.lock_hover_text {
            lock_response.on_hover_text(text);
        }

        confirmed
    }
}

pub struct LeftAlignedButton<'a> {
    icon: &'a str,
    label: &'a str,
    fill: Option<egui::Color32>,
    enabled: bool,
    width: f32,
}

impl<'a> LeftAlignedButton<'a> {
    pub fn new(icon: &'a str, label: &'a str) -> Self {
        Self {
            icon,
            label,
            fill: None,
            enabled: true,
            width: 130.0,
        }
    }

    pub fn fill(mut self, fill: egui::Color32) -> Self {
        self.fill = Some(fill);
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    #[allow(dead_code)]
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub fn show(self, ui: &mut egui::Ui) -> egui::Response {
        let h = ui.spacing().interact_size.y;
        let (rect, response) = ui.allocate_exact_size(egui::vec2(self.width, h), egui::Sense::click());

        if ui.is_rect_visible(rect) {
            let visuals = if self.enabled {
                ui.style().interact(&response)
            } else {
                &ui.style().visuals.widgets.inactive
            };
            let bg = self.fill.unwrap_or(visuals.bg_fill);
            ui.painter().rect(rect, visuals.corner_radius, bg, visuals.bg_stroke, egui::StrokeKind::Inside);

            let text_color = if self.fill.is_some() {
                egui::Color32::WHITE
            } else {
                visuals.text_color()
            };
            let font_id = egui::TextStyle::Body.resolve(ui.style());
            let padding = ui.spacing().button_padding.x;
            let text = format!("{} {}", self.icon, self.label);
            let galley = ui.painter().layout_no_wrap(text, font_id, text_color);
            let pos = egui::pos2(rect.left() + padding, rect.center().y - galley.size().y / 2.0);
            ui.painter().galley(pos, galley, text_color);
        }

        response
    }
}
