//! Updates: checked when the window opens (and from each installed app's row), never installed
//! without being asked. The check runs on its own thread from a job copied out of the session;
//! "Update all" installs one app after another.

use crate::{CreativeCraftApp, theme};
use creativecraft_engine::Command;
use creativecraft_engine::updates::{UpdateReport, show_version};
use creativecraft_install::Status;
use egui::RichText;
use std::sync::mpsc::{Receiver, Sender, channel};

pub(crate) struct UpdatesState {
    /// What's known: newer versions of apps, and of CreativeCraft.
    pub report: UpdateReport,
    /// Checks running: `None` = everything, `Some(id)` = one app.
    checking: Vec<Option<String>>,
    /// Apps still to update after "Update all".
    queue: Vec<String>,
    /// The launch check has been started.
    started: bool,
    tx: Sender<(Option<String>, Result<UpdateReport, String>)>,
    rx: Receiver<(Option<String>, Result<UpdateReport, String>)>,
}

impl Default for UpdatesState {
    fn default() -> UpdatesState {
        let (tx, rx) = channel();
        UpdatesState { report: UpdateReport::default(), checking: Vec::new(), queue: Vec::new(), started: false, tx, rx }
    }
}

impl UpdatesState {
    /// The newer version known for `id`, as shown to people.
    pub fn available(&self, id: &str) -> Option<String> {
        self.report.updates.iter().find(|u| u.id == id).map(|u| show_version(&u.latest))
    }

    pub fn checking(&self, id: &str) -> bool {
        self.checking.iter().any(|c| c.is_none() || c.as_deref() == Some(id))
    }

    /// How many things have updates (for the sidebar).
    pub fn count(&self) -> usize {
        self.report.updates.len() + usize::from(self.report.hub.is_some())
    }
}

impl CreativeCraftApp {
    /// Look for updates (all apps and CreativeCraft, or one app) on a thread of its own.
    pub(crate) fn check_updates(&mut self, only: Option<String>) {
        if self.updates.checking.contains(&only) {
            return;
        }
        self.updates.checking.push(only.clone());
        let (session, tx, ctx) = (self.session.clone(), self.updates.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            // copy what's needed and let go of the session before touching the network
            let job = match session.lock() {
                Ok(s) => s.update_job(only.as_deref()).map_err(|e| e.to_string()),
                Err(_) => Err("internal error: session lock poisoned".into()),
            };
            let result = job.map(|j| j.run());
            let _ = tx.send((only, result));
            ctx.request_repaint();
        });
    }

    /// Start the launch check once, if the setting allows; take in finished checks; keep "Update
    /// all" going.
    pub(crate) fn updates_pump(&mut self) {
        if !self.updates.started {
            self.updates.started = true;
            if self.draft.check_updates_on_start {
                self.check_updates(None);
            }
        }
        while let Ok((only, result)) = self.updates.rx.try_recv() {
            self.updates.checking.retain(|c| *c != only);
            let now = self.ctx.input(|i| i.time);
            match (only, result) {
                (None, Ok(report)) => {
                    let n = report.updates.len() + usize::from(report.hub.is_some());
                    if n > 0 {
                        self.last = Some((true, format!("{n} update{} available", if n == 1 { "" } else { "s" }), now));
                    }
                    self.updates.report = report;
                }
                (Some(id), Ok(found)) => {
                    // replace what was known about this app
                    let r = &mut self.updates.report;
                    r.updates.retain(|u| u.id != id);
                    r.updates.extend(found.updates.iter().cloned());
                    self.last = Some((true, found.summary(), now));
                }
                (_, Err(e)) => self.last = Some((false, format!("Couldn't check for updates: {e}"), now)),
            }
        }
        // what was just updated isn't an update any more
        let rows = &self.rows;
        self.updates.report.updates.retain(|u| rows.iter().any(|r| r.id == u.id && matches!(&r.status, Status::Installed { version } if *version != u.latest)));
        if self.busy.is_none()
            && let Some(id) = self.updates.queue.first().cloned()
        {
            self.updates.queue.remove(0);
            let name = self.rows.iter().find(|r| r.id == id).map_or(id.clone(), |r| r.name.clone());
            self.run(format!("Update {name}"), Command::Install(id));
        }
    }

    /// The bar at the top of the Apps page when something is newer.
    pub(crate) fn updates_banner(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        let report = self.updates.report.clone();
        if report.updates.is_empty() && report.hub.is_none() {
            return;
        }
        theme::card(ui, |ui| {
            ui.add_space(10.0);
            if let Some((version, url)) = &report.hub {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("CreativeCraft {version} is available")).size(14.0).color(t.ink));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(theme::secondary("Get it")).on_hover_text("Opens the release page").clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                        }
                    });
                });
                ui.label(
                    RichText::new("Install it the way you installed CreativeCraft (AUR: your package manager; Flatpak, .msi or .dmg: the file on that page).")
                        .size(12.0)
                        .color(t.muted),
                );
            }
            if !report.updates.is_empty() {
                if report.hub.is_some() {
                    ui.add_space(8.0);
                    theme::row_divider(ui);
                    ui.add_space(8.0);
                }
                let list: Vec<String> = report.updates.iter().map(|u| format!("{} {}", u.name, show_version(&u.latest))).collect();
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        let n = report.updates.len();
                        ui.label(RichText::new(format!("{n} app update{} available", if n == 1 { "" } else { "s" })).size(14.0).color(t.ink));
                        ui.label(RichText::new(list.join(" · ")).size(12.0).color(t.muted));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let queued = !self.updates.queue.is_empty();
                        if ui.add_enabled(!queued, theme::primary(if queued { "Updating…" } else { "Update all" })).clicked() {
                            self.updates.queue = report.updates.iter().map(|u| u.id.clone()).collect();
                        }
                    });
                });
            }
            ui.add_space(10.0);
        });
        ui.add_space(14.0);
    }
}
