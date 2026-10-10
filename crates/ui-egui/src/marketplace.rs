//! The Marketplace page: plugins that give agents tools for more apps. Each is an MCP server run
//! from its publisher's own package (see `creativecraft-marketplace`); installing shows exactly what
//! will run on the user's computer before anything does.

use crate::{CreativeCraftApp, Page, assets, settings, theme};
use creativecraft_engine::{Command, Plugin, Session};
use egui::{RichText, vec2};
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct MarketplaceState {
    /// The plugin waiting for "Install" to be confirmed.
    confirm: Option<String>,
    /// plugin id → (checked at, missing programs as (name, url)); `PATH` is checked every few seconds.
    missing: HashMap<String, (f64, Vec<(String, String)>)>,
}

impl CreativeCraftApp {
    fn plugin_missing(&mut self, p: &Plugin, now: f64) -> Vec<(String, String)> {
        if let Some((at, m)) = self.marketplace.missing.get(&p.id)
            && now - at < 5.0
        {
            return m.clone();
        }
        let m = Session::missing_requirements(p);
        self.marketplace.missing.insert(p.id.clone(), (now, m.clone()));
        m
    }

    pub(crate) fn marketplace_page(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        let now = ui.input(|i| i.time);
        let idle = self.busy.is_none();

        // ── header
        ui.horizontal(|ui| {
            ui.label(RichText::new("Marketplace").size(22.0).strong().color(t.ink));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(theme::secondary("Add your own")).on_hover_text("Any MCP server, in Settings → Connections").clicked() {
                    self.settings_section = settings::Section::Connections;
                    self.page = Page::Settings;
                }
                if ui.add_enabled(idle, theme::secondary("Check for new plugins")).clicked() {
                    self.run("Check for new plugins", Command::RefreshMarketplace);
                }
            });
        });
        ui.label(
            RichText::new("Plugins give your agents tools for more apps. They run from their publishers' own packages, on your computer.")
                .size(13.0)
                .color(t.muted),
        );
        ui.add_space(16.0);

        let q = self.search.trim().to_lowercase();
        let matches = |p: &Plugin| {
            q.is_empty()
                || [&p.name, &p.summary, &p.category, &p.author].iter().any(|s| s.to_lowercase().contains(&q))
                || p.for_app.as_deref().is_some_and(|a| a.contains(&q))
        };
        let plugins: Vec<Plugin> = self.plugins.iter().filter(|p| matches(p)).cloned().collect();
        let (installed, available): (Vec<Plugin>, Vec<Plugin>) = plugins.into_iter().partition(|p| self.draft.plugins.contains_key(&p.id));

        egui::ScrollArea::vertical().id_salt("marketplace").auto_shrink(false).show(ui, |ui| {
            for (title, list) in [("Installed", &installed), ("Available", &available)] {
                if list.is_empty() {
                    continue;
                }
                ui.label(RichText::new(title).size(16.0).strong().color(t.ink));
                ui.add_space(6.0);
                for p in list {
                    self.plugin_card(ui, p, now);
                    ui.add_space(10.0);
                }
                ui.add_space(14.0);
            }
            if installed.is_empty() && available.is_empty() {
                ui.label(RichText::new(if q.is_empty() { "No plugins listed yet." } else { "No plugins match your search." }).color(t.muted));
            }
            ui.add_space(8.0);
            ui.label(
                RichText::new("Plugins are made by their own authors, not by CreativeCraft. Read what one does before you install it.")
                    .size(12.0)
                    .color(t.muted),
            );
        });
    }

    fn plugin_card(&mut self, ui: &mut egui::Ui, p: &Plugin, now: f64) {
        let t = theme::Tokens::get();
        let idle = self.busy.is_none();
        let state = self.draft.plugins.get(&p.id).cloned();
        let missing = if state.is_none() { self.plugin_missing(p, now) } else { Vec::new() };
        theme::card(ui, |ui| {
            ui.add_space(12.0);
            ui.horizontal_top(|ui| {
                // the app it works with, if that app shows an icon
                let (icon, _) = ui.allocate_exact_size(vec2(40.0, 40.0), egui::Sense::hover());
                match p.for_app.as_deref().and_then(|a| self.tex.app(a)) {
                    Some(tex) => assets::paint(ui, tex, icon),
                    None => theme::letter_tile(ui, icon, &p.name),
                }
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.set_max_width((ui.available_width() - 230.0).max(200.0));
                    ui.label(RichText::new(&p.name).size(15.0).strong().color(t.ink));
                    ui.label(RichText::new(format!("{} · by {} · {}", p.category, p.author, p.license)).size(12.0).color(t.muted));
                    ui.add_space(4.0);
                    ui.label(RichText::new(&p.summary).size(13.0).color(t.ink));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    match &state {
                        Some(st) => {
                            ui.menu_button(RichText::new("…").size(16.0), |ui| {
                                if ui.button("Website").clicked() {
                                    ui.ctx().open_url(egui::OpenUrl::new_tab(&p.homepage));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.add_enabled(idle, egui::Button::new(RichText::new("Remove").color(t.danger))).clicked() {
                                    self.run(format!("Remove {}", p.name), Command::RemovePlugin(p.id.clone()));
                                    ui.close();
                                }
                            });
                            let mut on = st.enabled;
                            if theme::toggle(ui, &mut on).on_hover_text(if on { "Agents can use it" } else { "Turned off for agents" }).changed() && idle {
                                self.run(format!("{} {}", if on { "Turn on" } else { "Turn off" }, p.name), Command::SetPluginEnabled(p.id.clone(), on));
                            }
                        }
                        None if !missing.is_empty() => {
                            let (name, url) = &missing[0];
                            if ui
                                .add(theme::secondary(&format!("Get {name} first")))
                                .on_hover_text(format!("{} needs {name} on this computer", p.name))
                                .clicked()
                            {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                                self.marketplace.missing.remove(&p.id);
                            }
                        }
                        None => {
                            if ui.add_enabled(idle, theme::primary("Install")).clicked() {
                                self.marketplace.confirm = Some(p.id.clone());
                            }
                        }
                    }
                    if ui.add(egui::Button::new(RichText::new("Website").size(12.0)).frame(false)).clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(&p.homepage));
                    }
                });
            });

            // installed: what's left for the user to do
            if state.is_some() && !p.notes.is_empty() {
                ui.add_space(8.0);
                ui.label(RichText::new(&p.notes).size(12.0).color(t.muted));
            }

            // install: show exactly what runs, then ask
            if self.marketplace.confirm.as_deref() == Some(&p.id) && state.is_none() {
                ui.add_space(10.0);
                theme::row_divider(ui);
                ui.add_space(8.0);
                ui.label(RichText::new("Installing runs this on your computer:").size(13.0).color(t.ink));
                for s in &p.setup {
                    ui.label(RichText::new(format!("{}:  {} {}", s.label, s.command, s.args.join(" "))).monospace().size(12.0).color(t.muted));
                }
                ui.label(
                    RichText::new(format!("Then agents can start it with:  {} {}", p.run.command, p.run.args.join(" "))).monospace().size(12.0).color(t.muted),
                );
                if !p.notes.is_empty() {
                    ui.add_space(4.0);
                    ui.label(RichText::new(&p.notes).size(12.0).color(t.muted));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(idle, theme::primary("Install")).clicked() {
                        self.run(format!("Install {}", p.name), Command::InstallPlugin(p.id.clone()));
                        self.marketplace.confirm = None;
                    }
                    if ui.add(theme::secondary("Cancel")).clicked() {
                        self.marketplace.confirm = None;
                    }
                });
            }
            ui.add_space(12.0);
        });
    }
}
