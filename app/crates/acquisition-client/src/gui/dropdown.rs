use eframe::egui;

pub struct DropdownRow {
    pub cols: Vec<DropdownCol>,
    pub selected: bool,
}

pub enum DropdownCol {
    Text(String),
    FixedWidth(String, f32),
}

pub struct Dropdown<'a> {
    id: &'a str,
    button_text: String,
    button_width: f32,
    max_height: f32,
    rows: Vec<DropdownRow>,
    enabled: bool,
    footer: Option<Vec<(String, String)>>,
}

impl<'a> Dropdown<'a> {
    pub fn new(id: &'a str, button_text: impl Into<String>) -> Self {
        Self {
            id,
            button_text: button_text.into(),
            button_width: 200.0,
            max_height: 300.0,
            rows: Vec::new(),
            enabled: true,
            footer: None,
        }
    }

    pub fn button_width(mut self, w: f32) -> Self {
        self.button_width = w;
        self
    }

    pub fn rows(mut self, rows: Vec<DropdownRow>) -> Self {
        self.rows = rows;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// A row of (label, hover text) shown below all rows, explaining what each column means.
    pub fn footer(mut self, footer: Vec<(String, String)>) -> Self {
        self.footer = Some(footer);
        self
    }

    /// Returns the index of the row that was clicked, if any.
    pub fn show(self, ui: &mut egui::Ui) -> Option<usize> {
        let w = self.button_width;
        let h = ui.spacing().interact_size.y;
        let sense = if self.enabled { egui::Sense::click() } else { egui::Sense::hover() };
        let (rect, response) = ui.allocate_exact_size(egui::vec2(w, h), sense);

        if ui.is_rect_visible(rect) {
            let visuals = if self.enabled {
                ui.style().interact(&response)
            } else {
                &ui.style().visuals.widgets.inactive
            };
            ui.painter().rect(rect, visuals.corner_radius, visuals.bg_fill, visuals.bg_stroke, egui::StrokeKind::Inside);

            let padding = ui.spacing().button_padding.x;
            let text_color = visuals.text_color();
            let font_id = egui::TextStyle::Body.resolve(ui.style());

            let text_galley = ui.painter().layout_no_wrap(
                self.button_text.clone(),
                font_id.clone(),
                text_color,
            );
            let arrow_galley = ui.painter().layout_no_wrap(
                "⏷".into(),
                font_id,
                text_color,
            );

            let text_pos = egui::pos2(rect.left() + padding, rect.center().y - text_galley.size().y / 2.0);
            let arrow_pos = egui::pos2(rect.right() - padding - arrow_galley.size().x, rect.center().y - arrow_galley.size().y / 2.0);

            let text_clip = egui::Rect::from_min_max(rect.left_top(), egui::pos2(arrow_pos.x - 2.0, rect.bottom()));
            ui.painter().with_clip_rect(text_clip).galley(text_pos, text_galley, text_color);
            ui.painter().galley(arrow_pos, arrow_galley, text_color);
        }

        if !self.enabled {
            return None;
        }

        let num_cols = self.rows.first().map_or(1, |r| r.cols.len());
        let mut clicked_index = None;

        let row_spacing = 2.0;
        let row_h = ui.spacing().interact_size.y + row_spacing;
        let footer_rows = if self.footer.is_some() { 1 } else { 0 };
        let max_height_from_space = ui.ctx().content_rect().bottom() - rect.bottom() - ui.spacing().window_margin.sum().y;
        let content_height = ((self.rows.len() + footer_rows) as f32 * row_h).min(self.max_height).min(max_height_from_space);

        let font_id = egui::TextStyle::Body.resolve(ui.style());
        let col_spacing = 8.0;
        let mut col_widths = vec![0.0_f32; num_cols];
        for row in &self.rows {
            for (ci, col) in row.cols.iter().enumerate() {
                let w = match col {
                    DropdownCol::Text(text) => {
                        let galley = ui.painter().layout_no_wrap(
                            text.clone(),
                            font_id.clone(),
                            egui::Color32::TRANSPARENT,
                        );
                        galley.size().x + if ci == 0 { ui.spacing().button_padding.x * 2.0 } else { 0.0 }
                    }
                    DropdownCol::FixedWidth(_, width) => *width,
                };
                col_widths[ci] = col_widths[ci].max(w);
            }
        }
        if let Some(footer) = &self.footer {
            for (ci, (text, _hover)) in footer.iter().enumerate().take(num_cols) {
                let galley = ui.painter().layout_no_wrap(
                    text.clone(),
                    font_id.clone(),
                    egui::Color32::TRANSPARENT,
                );
                let w = galley.size().x + if ci == 0 { ui.spacing().button_padding.x * 2.0 } else { 0.0 };
                col_widths[ci] = col_widths[ci].max(w);
            }
        }
        let content_width: f32 = col_widths.iter().sum::<f32>() + col_spacing * (num_cols.saturating_sub(1) as f32);
        let max_width = ui.ctx().content_rect().right() - rect.left() - ui.spacing().window_margin.sum().x;
        let popup_width = content_width.max(self.button_width).min(max_width);

        egui::containers::Popup::from_toggle_button_response(&response)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
            .show(|ui| {
                ui.set_width(popup_width);
                ui.set_min_height(content_height);
                egui::ScrollArea::vertical().max_height(content_height).show(ui, |ui| {
                    egui::Grid::new(self.id)
                        .num_columns(num_cols)
                        .spacing(egui::vec2(8.0, 2.0))
                        .show(ui, |ui| {
                            for (i, row) in self.rows.iter().enumerate() {
                                for (ci, col) in row.cols.iter().enumerate() {
                                    match col {
                                        DropdownCol::Text(text) => {
                                            if ci == 0 {
                                                if ui.selectable_label(row.selected, text).clicked() {
                                                    clicked_index = Some(i);
                                                }
                                            } else {
                                                ui.add(egui::Label::new(text).truncate());
                                            }
                                        }
                                        DropdownCol::FixedWidth(text, width) => {
                                            let h = ui.text_style_height(&egui::TextStyle::Body);
                                            ui.allocate_ui_with_layout(
                                                egui::vec2(*width, h),
                                                egui::Layout::left_to_right(egui::Align::Center),
                                                |ui| { ui.label(text); },
                                            );
                                        }
                                    }
                                }
                                ui.end_row();
                            }

                            if let Some(footer) = &self.footer {
                                for (text, hover) in footer.iter().take(num_cols) {
                                    ui.weak(text).on_hover_text(hover);
                                }
                                ui.end_row();
                            }
                        });
                });
            });

        clicked_index
    }
}
