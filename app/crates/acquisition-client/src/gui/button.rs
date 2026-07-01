use eframe::egui;

pub struct HoldToConfirmButton<'a> {
    icon: &'a str,
    label: &'a str,
    hold_seconds: f32,
    unlock_seconds: f32,
    enabled: bool,
    hover_text: Option<&'a str>,
}

impl<'a> HoldToConfirmButton<'a> {
    pub fn new(icon: &'a str, label: &'a str) -> Self {
        Self {
            icon,
            label,
            hold_seconds: 3.0,
            unlock_seconds: 3.0,
            enabled: true,
            hover_text: None,
        }
    }

    #[allow(dead_code)]
    pub fn hold_seconds(mut self, secs: f32) -> Self {
        self.hold_seconds = secs;
        self
    }

    #[allow(dead_code)]
    pub fn unlock_seconds(mut self, secs: f32) -> Self {
        self.unlock_seconds = secs;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn hover_text(mut self, text: &'a str) -> Self {
        self.hover_text = Some(text);
        self
    }

    pub fn show(self, ui: &mut egui::Ui) -> bool {
        let text = format!("{} {}", self.icon, self.label);
        let font_id = egui::TextStyle::Button.resolve(ui.style());
        let measure = ui.painter().layout_no_wrap(
            text.clone(),
            font_id.clone(),
            egui::Color32::WHITE,
        );
        let padding = ui.spacing().button_padding;
        let desired = egui::vec2(
            measure.size().x + padding.x * 2.0,
            measure.size().y + padding.y * 2.0,
        );

        let sense = if self.enabled {
            egui::Sense::click_and_drag()
        } else {
            egui::Sense::hover()
        };
        let (rect, response) = ui.allocate_exact_size(desired, sense);
        let id = response.id;

        let progress_id = id;
        let unlock_id = id.with("unlock_until");
        let flash_id = id.with("flash_at");

        let progress: f32 = ui.data(|d| d.get_temp(progress_id).unwrap_or(0.0));
        let unlock_until: f64 = ui.data(|d| d.get_temp(unlock_id).unwrap_or(0.0));
        let now: f64 = ui.input(|i| i.time);
        let dt = ui.input(|i| i.stable_dt);
        let unlocked = unlock_until > now;

        let holding = response.is_pointer_button_down_on();
        let clicked = response.clicked();

        let mut confirmed = false;
        let rate = 1.0 / self.hold_seconds;

        if unlocked {
            ui.data_mut(|d| d.insert_temp(progress_id, 1.0f32));
            if clicked {
                confirmed = true;
                ui.data_mut(|d| d.insert_temp(unlock_id, now + self.unlock_seconds as f64));
            }
            ui.ctx().request_repaint();
        } else if holding {
            let new_progress = (progress + dt * rate).min(1.0);
            ui.data_mut(|d| d.insert_temp(progress_id, new_progress));
            if new_progress >= 1.0 && !unlocked {
                ui.data_mut(|d| d.insert_temp(unlock_id, now + self.unlock_seconds as f64));
                ui.data_mut(|d| d.insert_temp(flash_id, now));
            }
            ui.ctx().request_repaint();
        } else if progress > 0.0 {
            let new_progress = (progress - dt * rate).max(0.0);
            ui.data_mut(|d| d.insert_temp(progress_id, new_progress));
            ui.ctx().request_repaint();
        }

        let cur_progress: f32 = ui.data(|d| d.get_temp(progress_id).unwrap_or(0.0));
        let is_unlocked: bool = ui.data(|d| d.get_temp::<f64>(unlock_id).unwrap_or(0.0)) > now;

        if ui.is_rect_visible(rect) {
            let visuals = if !self.enabled {
                &ui.style().visuals.widgets.inactive
            } else if holding {
                &ui.style().visuals.widgets.active
            } else if response.hovered() {
                &ui.style().visuals.widgets.hovered
            } else {
                &ui.style().visuals.widgets.inactive
            };

            let rounding = visuals.corner_radius;
            ui.painter().rect(rect, rounding, visuals.bg_fill, visuals.bg_stroke, egui::StrokeKind::Inside);

            if cur_progress > 0.0 {
                let fill_rect = egui::Rect::from_min_size(
                    rect.min,
                    egui::vec2(rect.width() * cur_progress, rect.height()),
                );
                let red = if is_unlocked {
                    egui::Color32::from_rgba_premultiplied(220, 40, 40, 180)
                } else {
                    egui::Color32::from_rgba_premultiplied(200, 40, 40, 140)
                };
                ui.painter().rect_filled(fill_rect, rounding, red);
            }

            let flash_at: f64 = ui.data(|d| d.get_temp(flash_id).unwrap_or(0.0));
            let flash_duration = 0.3;
            let flash_elapsed = now - flash_at;
            if flash_at > 0.0 && flash_elapsed < flash_duration {
                let alpha = (1.0 - flash_elapsed / flash_duration) as f32;
                let a = (alpha * 180.0) as u8;
                ui.painter().rect_filled(rect, rounding, egui::Color32::from_rgba_premultiplied(a, a, a, a));
            }

            let text_color = if !self.enabled {
                ui.visuals().weak_text_color()
            } else {
                visuals.text_color()
            };
            let galley = ui.painter().layout_no_wrap(text.clone(), font_id.clone(), text_color);
            let text_pos = egui::pos2(
                rect.left() + padding.x,
                rect.center().y - galley.size().y / 2.0,
            );
            ui.painter().galley(text_pos, galley, text_color);
        }

        if let Some(text) = self.hover_text {
            response.on_hover_text(text);
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
