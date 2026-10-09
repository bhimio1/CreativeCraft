//! CreativeCraft's look: a quiet shell, dark or light, that lets the app icons carry the colour.
//! Neutral warm greys, the ArtCraft cream (from its wordmark) for primary actions in the dark theme
//! and its ink in the light one, generous spacing, one weight of detail per screen.

use egui::{Color32, RichText};
use std::sync::atomic::{AtomicBool, Ordering};

/// Which palette `Tokens::get` returns. Set by `apply`; read on every draw.
static DARK: AtomicBool = AtomicBool::new(true);

#[derive(Clone, Copy)]
pub struct Tokens {
    pub bg: Color32,
    pub bar: Color32,
    pub row: Color32,
    pub row_hover: Color32,
    pub line: Color32,
    pub ink: Color32,
    pub muted: Color32,
    /// Fill of the one strong button (cream on dark, ink on light).
    pub cream: Color32,
    pub on_cream: Color32,
    pub danger: Color32,
    pub ok: Color32,
    pub accent_soft: Color32,
}

impl Tokens {
    pub fn get() -> Tokens {
        if DARK.load(Ordering::Relaxed) { Tokens::dark() } else { Tokens::light() }
    }

    fn dark() -> Tokens {
        Tokens {
            bg: Color32::from_rgb(0x1b, 0x1b, 0x1b),
            bar: Color32::from_rgb(0x14, 0x14, 0x14),
            row: Color32::from_rgb(0x23, 0x23, 0x23),
            row_hover: Color32::from_rgb(0x2b, 0x2b, 0x2b),
            line: Color32::from_rgb(0x30, 0x30, 0x30),
            ink: Color32::from_rgb(0xf0, 0xef, 0xe9),
            muted: Color32::from_rgb(0x98, 0x97, 0x92),
            cream: Color32::from_rgb(0xf0, 0xef, 0xe9),
            on_cream: Color32::from_rgb(0x16, 0x16, 0x16),
            danger: Color32::from_rgb(0xe8, 0x6a, 0x5f),
            ok: Color32::from_rgb(0x7f, 0xc4, 0x8c),
            accent_soft: Color32::from_rgb(0x33, 0x33, 0x31),
        }
    }

    fn light() -> Tokens {
        Tokens {
            bg: Color32::from_rgb(0xf6, 0xf5, 0xf1),
            bar: Color32::from_rgb(0xec, 0xea, 0xe4),
            row: Color32::from_rgb(0xff, 0xff, 0xfd),
            row_hover: Color32::from_rgb(0xe6, 0xe4, 0xdd),
            line: Color32::from_rgb(0xd8, 0xd5, 0xcc),
            ink: Color32::from_rgb(0x1c, 0x1c, 0x1a),
            muted: Color32::from_rgb(0x6a, 0x69, 0x63),
            cream: Color32::from_rgb(0x1c, 0x1c, 0x1a),
            on_cream: Color32::from_rgb(0xf6, 0xf5, 0xf1),
            danger: Color32::from_rgb(0xc2, 0x41, 0x35),
            ok: Color32::from_rgb(0x2e, 0x86, 0x45),
            accent_soft: Color32::from_rgb(0xe4, 0xe2, 0xdb),
        }
    }
}

/// Is the dark palette active?
pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

/// Switch the whole window to the dark or light palette.
pub fn apply(ctx: &egui::Context, dark: bool) {
    DARK.store(dark, Ordering::Relaxed);
    let t = Tokens::get();
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = t.bg;
    v.window_fill = t.row;
    v.window_stroke = egui::Stroke::new(1.0, t.line);
    v.extreme_bg_color = t.bar;
    v.selection.bg_fill = t.accent_soft;
    v.selection.stroke = egui::Stroke::new(1.0, t.cream);
    v.hyperlink_color = t.cream;
    v.override_text_color = Some(t.ink);
    v.widgets.inactive.corner_radius = egui::CornerRadius::same(6);
    v.widgets.hovered.corner_radius = egui::CornerRadius::same(6);
    v.widgets.active.corner_radius = egui::CornerRadius::same(6);
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, t.line);
    v.widgets.inactive.weak_bg_fill = t.row_hover;
    v.widgets.inactive.bg_fill = t.row_hover;
    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(14.0, 6.0);
    });
}

/// The one strong button per row/screen.
pub fn primary(text: &str) -> egui::Button<'static> {
    let t = Tokens::get();
    egui::Button::new(RichText::new(text.to_string()).color(t.on_cream).strong()).fill(t.cream).corner_radius(14).min_size(egui::vec2(76.0, 28.0))
}

/// A quiet outlined button.
pub fn secondary(text: &str) -> egui::Button<'static> {
    let t = Tokens::get();
    egui::Button::new(RichText::new(text.to_string()).color(t.ink))
        .fill(Color32::TRANSPARENT)
        .stroke(egui::Stroke::new(1.0, t.muted))
        .corner_radius(14)
        .min_size(egui::vec2(76.0, 28.0))
}

/// Placeholder tile for apps without a bundled icon: a neutral rounded square with one letter.
pub fn letter_tile(ui: &mut egui::Ui, rect: egui::Rect, name: &str) {
    let t = Tokens::get();
    ui.painter().rect_filled(rect, rect.width() * 0.22, t.line);
    let letter: String = name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, letter, egui::FontId::proportional(rect.height() * 0.45), t.muted);
}

/// Ease-out with a small overshoot, for things that pop into place.
pub fn ease_out_back(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    let (c1, c3) = (1.4, 2.4);
    1.0 + c3 * (x - 1.0).powi(3) + c1 * (x - 1.0).powi(2)
}

/// Smooth ease-out for fades and slides.
pub fn ease_out_cubic(x: f32) -> f32 {
    1.0 - (1.0 - x.clamp(0.0, 1.0)).powi(3)
}

// ─────────────────────────── settings widgets ───────────────────────────

/// A rounded panel grouping related rows.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let t = Tokens::get();
    egui::Frame::NONE
        .fill(t.row)
        .stroke(egui::Stroke::new(1.0, t.line))
        .corner_radius(12)
        .inner_margin(egui::Margin::symmetric(18, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// One setting: title and a quiet description on the left, its control on the right. In a narrow
/// card the control goes under the text instead, so the two never overlap.
pub fn setting_row(ui: &mut egui::Ui, title: &str, desc: &str, control: impl FnOnce(&mut egui::Ui)) {
    let t = Tokens::get();
    let text = |ui: &mut egui::Ui| {
        ui.label(RichText::new(title).size(14.0).color(t.ink));
        if !desc.is_empty() {
            ui.label(RichText::new(desc).size(12.0).color(t.muted));
        }
    };
    ui.add_space(10.0);
    if ui.available_width() < 620.0 {
        text(ui);
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            // controls are written right-to-left for the wide layout; keep that order here
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center).with_main_align(egui::Align::Min), control);
        });
    } else {
        ui.horizontal(|ui| {
            let text_w = ui.available_width() - 320.0;
            ui.vertical(|ui| {
                ui.set_width(text_w);
                text(ui);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
        });
    }
    ui.add_space(10.0);
}

/// A thin rule between rows inside a card.
pub fn row_divider(ui: &mut egui::Ui) {
    let t = Tokens::get();
    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, t.line);
}

/// An on/off switch with a sliding knob.
pub fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let t = Tokens::get();
    let size = egui::vec2(38.0, 22.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let k = ui.ctx().animate_bool_with_time(resp.id, *on, 0.12);
    let track = if *on { t.cream } else { t.muted.gamma_multiply(0.45) };
    ui.painter().rect_filled(rect, 11.0, track);
    let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, k);
    let knob = if *on { t.on_cream } else { t.row };
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), 8.0, knob);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A row of mutually exclusive choices drawn as one pill. Returns true when the choice changed.
pub fn segmented<T: PartialEq + Copy>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let t = Tokens::get();
    let font = egui::FontId::proportional(13.0);
    let widths: Vec<f32> = options.iter().map(|(_, l)| ui.fonts_mut(|f| f.layout_no_wrap(l.to_string(), font.clone(), t.ink).size().x) + 24.0).collect();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(widths.iter().sum::<f32>() + 6.0, 30.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 15.0, t.bar);
    let mut x = rect.left() + 3.0;
    let mut changed = false;
    for ((v, label), w) in options.iter().zip(widths) {
        let r = egui::Rect::from_min_size(egui::pos2(x, rect.top() + 3.0), egui::vec2(w, 24.0));
        let resp = ui.interact(r, ui.id().with(("seg", label)), egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        let sel = *value == *v;
        if sel {
            ui.painter().rect_filled(r, 12.0, t.cream);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, 12.0, t.row_hover);
        }
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, *label, font.clone(), if sel { t.on_cream } else { t.muted });
        if resp.clicked() && !sel {
            *value = *v;
            changed = true;
        }
        x += w;
    }
    changed
}

/// A small status pill: coloured dot and text.
pub fn status_pill(ui: &mut egui::Ui, dot: Color32, text: &str) -> egui::Response {
    let t = Tokens::get();
    egui::Frame::NONE
        .fill(t.bar)
        .corner_radius(11)
        .inner_margin(egui::Margin::symmetric(10, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (r, _) = ui.allocate_exact_size(egui::vec2(7.0, 7.0), egui::Sense::hover());
                ui.painter().circle_filled(r.center(), 3.5, dot);
                ui.label(RichText::new(text).size(12.0).color(t.muted));
            });
        })
        .response
}
