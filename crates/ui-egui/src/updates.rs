//! Updates: checked when the window opens (and from each installed app's row), never installed
//! without being asked. The check runs on its own thread from a job copied out of the session;
//! "Update all" installs one app after another.
//!
//! CreativeCraft updates itself too: when the launch check finds a newer CreativeCraft it offers
//! to update right away; otherwise the Apps page banner and Settings → General keep the offer.
//! After the update, "Restart now" starts the new version and closes this one.

use crate::{CreativeCraftApp, theme};
use creativecraft_engine::Command;
use creativecraft_engine::updates::{HUB_ID, UpdateReport, show_version};
use creativecraft_install::{HubInstall, Status};
use egui::RichText;

/// The busy label of CreativeCraft's own update.
const HUB_LABEL: &str = "Update CreativeCraft";
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
    /// How this copy of CreativeCraft was installed (decides how it updates itself).
    pub install: HubInstall,
    /// The launch check is still running (its result may open the update offer).
    launch_check: bool,
    /// The "CreativeCraft X is available" window is open.
    offer: bool,
    /// CreativeCraft's update is in place; a restart starts it.
    pub hub_ready: bool,
    tx: Sender<(Option<String>, Result<UpdateReport, String>)>,
    rx: Receiver<(Option<String>, Result<UpdateReport, String>)>,
}

impl Default for UpdatesState {
    fn default() -> UpdatesState {
        let (tx, rx) = channel();
        UpdatesState {
            report: UpdateReport::default(),
            checking: Vec::new(),
            queue: Vec::new(),
            started: false,
            install: HubInstall::detect(),
            launch_check: false,
            offer: false,
            hub_ready: false,
            tx,
            rx,
        }
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

    /// The check of everything (apps and CreativeCraft) is running.
    pub fn checking_all(&self) -> bool {
        self.checking.contains(&None)
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
            if let Ok(s) = self.session.try_lock() {
                creativecraft_install::clean_up_after_update(&self.updates.install, &s.dirs);
            }
            if self.draft.check_updates_on_start {
                self.updates.launch_check = true;
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
                    // at launch, offer CreativeCraft's own update when it can install it
                    if std::mem::take(&mut self.updates.launch_check)
                        && report.hub.is_some()
                        && self.updates.install.can_update().is_ok()
                        && !self.updates.hub_ready
                    {
                        self.updates.offer = true;
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
                (only, Err(e)) => {
                    if only.is_none() {
                        self.updates.launch_check = false;
                    }
                    self.last = Some((false, format!("Couldn't check for updates: {e}"), now));
                }
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
                self.hub_update_row(ui, version, url);
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

impl CreativeCraftApp {
    /// Update CreativeCraft itself (download, verify, install; a restart finishes it).
    pub(crate) fn update_hub(&mut self) {
        self.run(HUB_LABEL, Command::UpdateHub);
    }

    /// Called when a command finishes: note that CreativeCraft's update is ready.
    pub(crate) fn hub_update_done(&mut self, label: &str, ok: bool) {
        if label == HUB_LABEL && ok {
            self.updates.hub_ready = true;
        }
    }

    fn hub_updating(&self) -> bool {
        self.busy.as_deref() == Some(HUB_LABEL)
    }

    /// Start the new CreativeCraft (on Windows: run its installer first) and close this one.
    pub(crate) fn restart_hub(&mut self) {
        let now = self.ctx.input(|i| i.time);
        if self.draft != self.saved {
            self.save_draft();
        }
        let dirs = match self.session.try_lock() {
            Ok(s) => s.dirs.clone(),
            Err(_) => {
                self.last = Some((false, "Busy; restart again in a moment".into(), now));
                return;
            }
        };
        match creativecraft_install::restart_hub(&self.updates.install, &dirs) {
            Ok(()) => self.ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(e) => self.last = Some((false, format!("Couldn't restart CreativeCraft: {e}"), now)),
        }
    }

    /// Settings → General: this version, how it's installed, and its update.
    pub(crate) fn hub_version_row(&mut self, ui: &mut egui::Ui) {
        let install = &self.updates.install;
        let mut about = format!("Version {} · {}", env!("CARGO_PKG_VERSION"), install.label());
        if let Err(how) = install.can_update() {
            about = format!("{about}. {how}");
        }
        let hub = self.updates.report.hub.clone();
        theme::setting_row(ui, "CreativeCraft", &about, |ui| match &hub {
            Some(_) if self.updates.hub_ready => {
                if ui.add(theme::primary("Restart now")).clicked() {
                    self.restart_hub();
                }
            }
            Some((version, url)) if self.updates.install.can_update().is_ok() => {
                let label = if self.hub_updating() { "Updating…".to_string() } else { format!("Update to {version}") };
                if ui.add_enabled(self.busy.is_none(), theme::primary(&label)).clicked() {
                    self.update_hub();
                }
                if ui.add(theme::secondary("What's new")).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                }
            }
            Some((version, url)) => {
                if ui.add(theme::secondary(&format!("Get {version}"))).on_hover_text("Opens the release page").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                }
            }
            None => {
                ui.label(RichText::new(if self.updates.checking_all() { "Checking…" } else { "Up to date" }).size(13.0).color(theme::Tokens::get().muted));
            }
        });
        self.hub_progress(ui);
    }

    /// What to say under "CreativeCraft X is available", and the buttons for it.
    fn hub_update_row(&mut self, ui: &mut egui::Ui, version: &str, url: &str) {
        let t = theme::Tokens::get();
        let ready = self.updates.hub_ready;
        let updating = self.hub_updating();
        let can = self.updates.install.can_update();
        let title = if ready { format!("CreativeCraft {version} is ready") } else { format!("CreativeCraft {version} is available") };
        let note = match (&can, ready) {
            (Err(how), _) => how.clone(),
            (Ok(()), true) if self.updates.install.installs_on_restart() => {
                "Restart to install it. Windows asks for permission, then CreativeCraft opens again.".into()
            }
            (Ok(()), true) => "Restart CreativeCraft to start using it.".into(),
            (Ok(()), false) => format!("You have {}. Your apps, projects and settings stay as they are.", env!("CARGO_PKG_VERSION")),
        };
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(title).size(14.0).color(t.ink));
                ui.label(RichText::new(note).size(12.0).color(t.muted));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if can.is_err() {
                    if ui.add(theme::secondary("Get it")).on_hover_text("Opens the release page").clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                    }
                    return;
                }
                if ready {
                    if ui.add(theme::primary("Restart now")).clicked() {
                        self.restart_hub();
                    }
                } else if ui.add_enabled(self.busy.is_none(), theme::primary(if updating { "Updating…" } else { "Update now" })).clicked() {
                    self.update_hub();
                }
                if ui.add(theme::secondary("What's new")).on_hover_text("Opens the release page").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                }
            });
        });
        self.hub_progress(ui);
    }

    /// The download bar while CreativeCraft updates itself.
    fn hub_progress(&self, ui: &mut egui::Ui) {
        if let Some((id, f, label)) = &self.install_progress
            && id == HUB_ID
        {
            ui.add_space(6.0);
            ui.add(egui::ProgressBar::new(f.max(0.0)).desired_height(4.0));
            ui.label(RichText::new(label).size(12.0).color(theme::Tokens::get().muted));
        }
    }

    /// At launch: "CreativeCraft X is available", with Update now and Later. Later (or closing it)
    /// leaves the offer on the Apps page and in Settings.
    pub(crate) fn hub_update_offer(&mut self, ctx: &egui::Context) {
        if !self.updates.offer {
            return;
        }
        let Some((version, url)) = self.updates.report.hub.clone() else {
            self.updates.offer = false;
            return;
        };
        let t = theme::Tokens::get();
        let (ready, updating) = (self.updates.hub_ready, self.hub_updating());
        let modal = egui::Modal::new(egui::Id::new("hub_update_offer")).show(ctx, |ui| {
            ui.set_width(440.0);
            let title = if ready { format!("CreativeCraft {version} is ready") } else { format!("CreativeCraft {version} is available") };
            ui.label(RichText::new(title).size(18.0).strong().color(t.ink));
            ui.add_space(6.0);
            let text = if ready && self.updates.install.installs_on_restart() {
                "Restart to install it. Windows asks for permission, then CreativeCraft opens again.".to_string()
            } else if ready {
                "Restart CreativeCraft to start using it.".to_string()
            } else {
                format!("You have {}. Update now, or later from the Apps page or Settings.", env!("CARGO_PKG_VERSION"))
            };
            ui.label(RichText::new(text).color(t.muted));
            self.hub_progress(ui);
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ready {
                    if ui.add(theme::primary("Restart now")).clicked() {
                        self.restart_hub();
                    }
                    if ui.add(theme::secondary("Later")).clicked() {
                        self.updates.offer = false;
                    }
                } else {
                    if ui.add_enabled(self.busy.is_none(), theme::primary(if updating { "Updating…" } else { "Update now" })).clicked() {
                        self.update_hub();
                    }
                    if ui.add(theme::secondary(if updating { "Hide" } else { "Later" })).clicked() {
                        self.updates.offer = false;
                    }
                    if ui.add(theme::secondary("What's new")).on_hover_text("Opens the release page").clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(&url));
                    }
                }
            });
        });
        if modal.should_close() {
            self.updates.offer = false;
        }
    }
}
