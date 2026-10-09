//! CreativeCraft's own look: warm paper, ink, and a terracotta accent. Original palette and layout.

use egui::{Color32, RichText};

#[derive(Clone, Copy)]
pub struct Tokens {
    pub paper: Color32,
    pub rail: Color32,
    pub card: Color32,
    pub line: Color32,
    pub ink: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub chip: Color32,
    pub danger: Color32,
    pub ok: Color32,
}

impl Tokens {
    pub fn get() -> Tokens {
        Tokens {
            paper: Color32::from_rgb(0x1c, 0x1a, 0x18),
            rail: Color32::from_rgb(0x15, 0x14, 0x12),
            card: Color32::from_rgb(0x25, 0x22, 0x1f),
            line: Color32::from_rgb(0x36, 0x32, 0x2d),
            ink: Color32::from_rgb(0xee, 0xe8, 0xdf),
            muted: Color32::from_rgb(0xa3, 0x98, 0x8c),
            accent: Color32::from_rgb(0xe0, 0x7a, 0x52),
            accent_soft: Color32::from_rgb(0x4a, 0x2e, 0x22),
            chip: Color32::from_rgb(0x30, 0x2c, 0x28),
            danger: Color32::from_rgb(0xe5, 0x6b, 0x6b),
            ok: Color32::from_rgb(0x8c, 0xc4, 0x8a),
        }
    }
}

pub fn apply(ctx: &egui::Context) {
    let t = Tokens::get();
    let mut v = egui::Visuals::dark();
    v.panel_fill = t.paper;
    v.window_fill = t.card;
    v.extreme_bg_color = t.rail;
    v.selection.bg_fill = t.accent_soft;
    v.selection.stroke = egui::Stroke::new(1.0, t.accent);
    v.hyperlink_color = t.accent;
    v.override_text_color = Some(t.ink);
    ctx.set_visuals(v);
}

pub fn primary(text: &str) -> egui::Button<'static> {
    let t = Tokens::get();
    egui::Button::new(RichText::new(text.to_string()).color(Color32::from_rgb(0x1c, 0x12, 0x0d)).strong()).fill(t.accent)
}

pub fn chip(ui: &mut egui::Ui, text: &str, fill: Color32) {
    egui::Frame::NONE.fill(fill).corner_radius(6).inner_margin(egui::Margin::symmetric(6, 1)).show(ui, |ui| {
        ui.label(RichText::new(text).size(11.0));
    });
}

/// A tile with the app's initials, coloured by category (no third-party artwork needed).
pub fn monogram(ui: &mut egui::Ui, name: &str, category: &str) {
    let hue = category.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b))) % 360;
    let fill: Color32 = egui::ecolor::Hsva::new(hue as f32 / 360.0, 0.45, 0.55, 1.0).into();
    let initials: String = name.chars().filter(|c| c.is_uppercase()).take(2).collect();
    let initials = if initials.is_empty() { name.chars().take(2).collect() } else { initials };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 10.0, fill);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, initials, egui::FontId::proportional(17.0), Color32::WHITE);
}
