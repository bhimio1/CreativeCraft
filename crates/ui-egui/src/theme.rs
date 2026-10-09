//! CreativeCraft's look: a quiet dark shell that lets the app icons carry the colour. Neutral greys,
//! the ArtCraft cream (from its wordmark) for primary actions, generous spacing, one weight of
//! detail per screen.

use egui::{Color32, RichText};

#[derive(Clone, Copy)]
pub struct Tokens {
    pub bg: Color32,
    pub bar: Color32,
    pub row: Color32,
    pub row_hover: Color32,
    pub line: Color32,
    pub ink: Color32,
    pub muted: Color32,
    pub cream: Color32,
    pub on_cream: Color32,
    pub danger: Color32,
    pub ok: Color32,
    // kept for the chat bubbles
    pub accent: Color32,
    pub accent_soft: Color32,
    pub card: Color32,
}

impl Tokens {
    pub fn get() -> Tokens {
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
            accent: Color32::from_rgb(0xf0, 0xef, 0xe9),
            accent_soft: Color32::from_rgb(0x33, 0x33, 0x31),
            card: Color32::from_rgb(0x23, 0x23, 0x23),
        }
    }
}

pub fn apply(ctx: &egui::Context) {
    let t = Tokens::get();
    let mut v = egui::Visuals::dark();
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
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
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
