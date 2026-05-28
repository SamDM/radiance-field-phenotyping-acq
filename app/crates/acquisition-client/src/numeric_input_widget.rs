use std::time::{Duration, Instant};

use eframe::egui;

// Speed halves every HALF_LIFE seconds without scrolling.
const SCROLL_HALF_LIFE: f64 = 0.5;
// Each scroll event multiplies/divides the current speed by this (decayed) factor.
const SCROLL_ACCELERATION: f64 = 1.1;
const SCROLL_DECCELERATION: f64 = 2.0;
// Time it takes to try to confirm an value that is being edited
const DEFAULT_DEBOUNCE_SECS: u64 = 1;

fn rounding_tick(round_to: i32) -> f64 {
    (10.0_f64).powi(round_to)
}

#[derive(Debug)]
pub struct EditingInProgress {
    text: String,
    last_edit: Instant,
    debounce_period: Duration,
    scroll_speed: f64,
    last_scroll: Instant,
    last_scroll_dir: f32,
}

impl EditingInProgress {
    fn new(text: impl ToString, debounce_period: Duration) -> Self {
        let now = Instant::now();
        let debounce_time_ago = now.checked_sub(debounce_period).unwrap();
        let long_time_ago = now
            .checked_sub(Duration::from_secs_f64(SCROLL_HALF_LIFE * 100.0))
            .unwrap();
        EditingInProgress {
            text: text.to_string(),
            last_edit: debounce_time_ago,
            debounce_period,
            scroll_speed: 1.0,
            last_scroll: long_time_ago,
            last_scroll_dir: 0.0,
        }
    }

    /// Parse the current text as a float, ignoring any grouping spaces.
    pub fn try_parse(&self) -> Option<f64> {
        self.text.replace([' ', '_'], "").parse().ok()
    }

    pub fn text_value(&self) -> &str {
        &self.text
    }

    pub fn debounce_elapsed(&self) -> bool {
        return self.last_edit.elapsed() > self.debounce_period;
    }
}

#[derive(Debug)]
pub struct EditingDone {
    value: f64,
    round_to: i32,
}

impl EditingDone {
    fn new(value: f64, round_to: i32) -> Self {
        EditingDone { value, round_to }
    }

    pub fn num_value(&self) -> f64 {
        self.value
    }

    pub fn text_value(&self) -> String {
        format_grouped(self.value, self.round_to)
    }

    pub fn agrees_up_to_rounding(&self, v_other: f64) -> bool {
        let v_self = self.value;
        (v_self - v_other).abs() < 0.5 * rounding_tick(self.round_to)
    }
}

pub struct NumericInputState {
    value: Result<EditingDone, EditingInProgress>,
    round_to: i32,
    debounce_period: Duration,
}

impl NumericInputState {
    pub fn new_editing_in_progress(text: impl ToString, round_to: i32) -> Self {
        let debounce_period = Duration::from_secs(DEFAULT_DEBOUNCE_SECS);
        Self {
            value: Err(EditingInProgress::new(text, debounce_period)),
            round_to,
            debounce_period,
        }
    }

    #[allow(dead_code)]
    pub fn new_editing_done(value: f64, round_to: i32) -> Self {
        let debounce_period = Duration::from_secs(DEFAULT_DEBOUNCE_SECS);
        Self {
            value: Ok(EditingDone::new(value, round_to)),
            round_to,
            debounce_period,
        }
    }

    pub fn value(&self) -> Result<&EditingDone, &EditingInProgress> {
        self.value.as_ref()
    }

    // exposing as `pub` is safe because return type new constructor is not public
    pub fn to_editing(&mut self) -> &mut EditingInProgress {
        match self.value.as_ref() {
            Ok(done) => {
                self.value = Err(EditingInProgress::new(
                    done.text_value(),
                    self.debounce_period,
                ))
            }
            Err(_) => (), // nothing to do, already editing
        }
        self.value.as_mut().unwrap_err()
    }

    // exposing as `pub` is safe because return type new constructor is not public
    pub fn to_done(&mut self, value: f64) -> &mut EditingDone {
        match self.value.as_mut() {
            Ok(done) => done.value = value,
            Err(_) => self.value = Ok(EditingDone::new(value, self.round_to)),
        }
        self.value.as_mut().unwrap()
    }
}

/// Format `v` with digit-group spaces on the integer part and `(-round_to).max(0)` decimal places.
/// e.g. `format_grouped(1_000_000.5, -1)` → `"1 000 000.5"`
fn format_grouped(v: f64, round_to: i32) -> String {
    let decimal_places = (-round_to).max(0) as usize;
    let formatted = format!("{:.prec$}", v, prec = decimal_places);

    let (int_str, dec_str) = match formatted.split_once('.') {
        Some((i, d)) => (i, Some(d)),
        None => (formatted.as_str(), None),
    };

    let (sign, digits) = int_str
        .strip_prefix('-')
        .map_or(("", int_str), |d| ("-", d));

    // Reverse the digit string, insert a space before every 3rd character, then reverse back.
    let grouped_rev: String = digits
        .chars()
        .rev()
        .enumerate()
        .flat_map(|(i, c)| {
            let space = if i > 0 && i % 3 == 0 { Some(' ') } else { None };
            space.into_iter().chain(std::iter::once(c))
        })
        .collect();
    let grouped: String = grouped_rev.chars().rev().collect();

    match dec_str {
        Some(d) => format!("{sign}{grouped}.{d}"),
        None => format!("{sign}{grouped}"),
    }
}

pub struct NumericInput<'a> {
    state: &'a mut NumericInputState,
    text_color: Option<egui::Color32>,
}

impl<'a> NumericInput<'a> {
    pub fn new(state: &'a mut NumericInputState) -> Self {
        Self {
            state,
            text_color: None,
        }
    }

    pub fn text_color(mut self, color: egui::Color32) -> Self {
        self.text_color = Some(color);
        self
    }
}

impl<'a> egui::Widget for NumericInput<'a> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        // helpers
        let debounce_period = self.state.debounce_period;
        let round_to = self.state.round_to;
        let reset_debounce = |editing: &mut EditingInProgress, ui: &mut egui::Ui| {
            editing.last_edit = Instant::now();
            ui.ctx().request_repaint_after(debounce_period);
        };

        // draw widget
        let mut text = match self.state.value.as_ref() {
            Ok(done) => done.text_value(),
            Err(editing) => editing.text.clone(),
        };
        let mut te = egui::TextEdit::singleline(&mut text);
        if let Some(color) = self.text_color {
            te = te.text_color(color);
        }
        let mut resp = ui.add(te);

        // check for text edits
        if resp.changed() {
            let editing = self.state.to_editing();
            text.retain(|c| matches!(c, '0'..='9' | '.' | '-' | 'e' | 'E' | '+' | ' ' | '_'));
            editing.text = text;
            reset_debounce(editing, ui);
        }

        // check for scroll on the numeric input
        if resp.hovered() {
            // Use raw MouseWheel events so one physical wheel click = one step.
            // smooth_scroll_delta spreads a single click across ~15 frames,
            // which would accelerate the speed to the cap on every click.
            let wheel_y: f32 = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::MouseWheel { delta, .. } => Some(delta.y),
                        _ => None,
                    })
                    .sum()
            });
            if wheel_y != 0.0 {
                // scoll detected on numeric input widget, go to editing mode
                let editing = self.state.to_editing();

                if let Some(value) = editing.try_parse() {
                    // apply scroll decay,
                    // every SCROLL_HALF_LIFE secs, decay becomes 2x lower
                    // - elapsed near zero? -> decay approaches 1.0
                    // - elapsed very high? -> decay approaches 0.0
                    let elapsed = editing.last_scroll.elapsed().as_secs_f64();
                    let scroll_decay = (0.5_f64).powf(elapsed / SCROLL_HALF_LIFE);
                    editing.scroll_speed *= scroll_decay;

                    // apply scroll speed increase/decrease depending on scroll direction change
                    // - kept scrolling -> increase speed
                    // - reversed scroll dir -> decrease speed
                    let scroll_dir = wheel_y.signum();
                    if scroll_dir == editing.last_scroll_dir {
                        editing.scroll_speed *= SCROLL_ACCELERATION
                    } else {
                        editing.scroll_speed /= SCROLL_DECCELERATION
                    };

                    // update scroll state
                    editing.scroll_speed = editing.scroll_speed.max(1.0);
                    editing.last_scroll_dir = scroll_dir;
                    editing.last_scroll = Instant::now();

                    // update text value
                    let scroll_step_size = editing.scroll_speed * rounding_tick(round_to);
                    let scroll_step = scroll_step_size * scroll_dir as f64;
                    editing.text = format_grouped(value + scroll_step, round_to);

                    // reset debounce period and notify ui
                    reset_debounce(editing, ui);
                    resp.mark_changed();
                }
            }
        }

        // check if user is done editing
        match self.state.value.as_mut() {
            Err(editing) => {
                let lost_focus = resp.lost_focus();
                let enter_pressed = ui.input(|i| i.key_pressed(egui::Key::Enter));
                let debounce_elapsed = editing.debounce_elapsed();
                if (lost_focus && enter_pressed) || debounce_elapsed {
                    if let Some(value) = editing.try_parse() {
                        self.state.to_done(value);
                    }
                }
            }
            _ => (),
        }

        resp
    }
}
