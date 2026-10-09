//! The Marketplace page. A placeholder for now: plugins for the hub and the Craft apps will be
//! browsed and installed here once plugin support exists (see ROADMAP.md).

use crate::{CreativeCraftApp, theme};
use egui::{RichText, vec2};

impl CreativeCraftApp {
    pub(crate) fn marketplace_page(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        ui.label(RichText::new("Marketplace").size(22.0).strong().color(t.ink));
        ui.label(RichText::new("Plugins for CreativeCraft and your apps.").size(13.0).color(t.muted));
        ui.add_space(18.0);
        theme::card(ui, |ui| {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(84.0, 26.0), egui::Sense::hover());
                ui.painter().rect_filled(r, 13.0, t.cream);
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "Coming soon", egui::FontId::proportional(12.0), t.on_cream);
                ui.add_space(14.0);
                ui.label(RichText::new("Plugins are on the way").size(20.0).strong().color(t.ink));
                ui.add_space(4.0);
                ui.label(
                    RichText::new("You'll be able to find, install and update plugins that add tools, effects and integrations to your apps, right here.")
                        .color(t.muted),
                );
            });
            ui.add_space(48.0);
        });
    }
}
