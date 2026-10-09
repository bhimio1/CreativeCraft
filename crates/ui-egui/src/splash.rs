//! The start-up animation: the ArtCraft mark pops in, the Craft app icons fly out of it into a
//! slowly turning ring, the name rises underneath, then the ring opens outward and the whole layer
//! fades to reveal the hub (which is already drawn underneath, so nothing waits on it).
//!
//! About two seconds. A click or any key skips it; Settings → General turns it off.

use crate::{assets, theme};
use egui::{Align2, FontId, Id, Order, Pos2, Rect, Sense, vec2};
use std::f32::consts::TAU;

/// Seconds from first frame to the start of the exit, and the exit's length.
const HOLD: f32 = 1.6;
const EXIT: f32 = 0.55;

pub struct Splash {
    start: Option<f64>,
    done: bool,
}

impl Splash {
    pub fn new(enabled: bool) -> Splash {
        Splash { start: None, done: !enabled }
    }

    /// Draw one frame of the animation over everything else. Returns false once it has finished.
    pub fn show(&mut self, ctx: &egui::Context, tex: &assets::Textures) -> bool {
        if self.done {
            return false;
        }
        let now = ctx.input(|i| i.time);
        let start = *self.start.get_or_insert(now);
        let t = (now - start) as f32;
        let skipped = ctx.input(|i| i.pointer.any_click() || i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. })));
        if t >= HOLD + EXIT {
            self.done = true;
            return false;
        }
        // a skipping click still lands on the overlay this frame, so it doesn't press what's beneath
        self.done = skipped;
        ctx.request_repaint();

        let screen = ctx.content_rect();
        egui::Area::new(Id::new("splash")).order(Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
            // swallow input so nothing underneath reacts mid-animation
            ui.allocate_rect(screen, Sense::click());
            paint(ui.painter(), screen, t, tex);
        });
        true
    }
}

fn paint(p: &egui::Painter, screen: Rect, t: f32, tex: &assets::Textures) {
    let k = theme::Tokens::get();
    // exit: 0 while holding, 0→1 over the exit
    let exit = theme::ease_out_cubic((t - HOLD) / EXIT);
    let fade = 1.0 - exit;
    let scale = (screen.width().min(screen.height()) / 640.0).clamp(0.7, 1.3);
    let center = screen.center() - vec2(0.0, 24.0 * scale);

    p.rect_filled(screen, 0.0, k.bg.gamma_multiply(fade));

    // a soft halo behind the mark that breathes in
    let halo = theme::ease_out_cubic(t / 0.9);
    for (i, a) in [(0, 0.05), (1, 0.035), (2, 0.02)] {
        let r = (70.0 + 38.0 * i as f32) * scale * halo * (1.0 + 0.4 * exit);
        p.circle_filled(center, r, k.ink.gamma_multiply(a * fade));
    }

    // the ring of app icons
    let icons = tex.app_icons();
    let n = icons.len().max(1) as f32;
    let radius = 168.0 * scale * (1.0 + 0.9 * exit);
    let spin = t * 0.35;
    for (i, icon) in icons.iter().enumerate() {
        let delay = 0.25 + i as f32 * 0.055;
        let pop = theme::ease_out_back((t - delay) / 0.55);
        if pop <= 0.0 {
            continue;
        }
        let angle = i as f32 / n * TAU - TAU / 4.0 + spin;
        let pos = center + vec2(angle.cos(), angle.sin()) * radius * pop;
        let size = 44.0 * scale * pop.min(1.15);
        let rect = Rect::from_center_size(pos, vec2(size, size));
        assets::paint_faded(p, icon, rect, ((t - delay) / 0.25).min(1.0) * fade);
    }

    // the mark, popping in at the centre
    if let Some(mark) = &tex.mark {
        let pop = theme::ease_out_back(t / 0.6);
        let size = 104.0 * scale * pop.max(0.0) * (1.0 - 0.15 * exit);
        assets::paint_faded(p, mark, Rect::from_center_size(center, vec2(size, size)), (t / 0.3).min(1.0) * fade);
    }

    // the name and a thin progress line, rising in below the ring
    let rise = theme::ease_out_cubic((t - 0.55) / 0.5);
    let below = center.y + 168.0 * scale + 58.0 * scale;
    let title_pos = Pos2::new(center.x, below + 12.0 * (1.0 - rise));
    p.text(title_pos, Align2::CENTER_CENTER, "CreativeCraft", FontId::proportional(26.0 * scale), k.ink.gamma_multiply(rise * fade));
    let w = 160.0 * scale;
    let y = below + 26.0 * scale;
    let track = Rect::from_min_size(Pos2::new(center.x - w / 2.0, y), vec2(w, 2.0));
    p.rect_filled(track, 1.0, k.line.gamma_multiply(rise * fade));
    let done = theme::ease_out_cubic(t / HOLD);
    p.rect_filled(Rect::from_min_size(track.min, vec2(w * done, 2.0)), 1.0, k.ink.gamma_multiply(rise * fade));
}
