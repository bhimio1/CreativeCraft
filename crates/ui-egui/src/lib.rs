//! The hub window (L4). Thin by design: it shows state and turns clicks into engine commands.
//! Long commands (installs, syncs) run on a worker thread; progress comes back over a channel.
//!
//! Layout: a top bar (brand, search, sync state, settings), a three-item sidebar (Apps, Files,
//! Assistant) and one content area. Everything else (cloud setup, assistant setup, connections)
//! lives in the Settings dialog, so the main screens stay uncluttered.
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod assets;
mod chat;
mod theme;

use creativecraft_catalog::Kind;
use creativecraft_engine::{CloudChoice, Command, CustomMcp, Event, Session, Settings};
use creativecraft_install::{Progress, Status};
use egui::{RichText, vec2};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Apps,
    Files,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    General,
    Cloud,
    Assistant,
    Connections,
}

/// One app, copied out of the session so drawing never waits on a running job.
#[derive(Debug, Clone)]
struct Row {
    id: String,
    name: String,
    category: String,
    summary: String,
    homepage: String,
    kind: Kind,
    status: Status,
}

enum Msg {
    Event(Event),
    Done(String, Result<String, String>),
}

pub struct CreativeCraftApp {
    session: Arc<Mutex<Session>>,
    ctx: egui::Context,
    tex: assets::Textures,
    page: Page,
    rows: Vec<Row>,
    draft: Settings,
    signed_in: bool,
    cli_path: String,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    busy: Option<String>,
    /// (app id, fraction, label) while an install runs.
    install_progress: Option<(String, f32, String)>,
    sync_progress: Option<f32>,
    /// Last result line, shown briefly in the top bar.
    last: Option<(bool, String, f64)>,
    log: Vec<(bool, String)>,
    new_mcp: CustomMcp,
    search: String,
    last_auto_sync: f64,
    settings_open: bool,
    settings_tab: SettingsTab,
    chat: chat::ChatState,
}

impl CreativeCraftApp {
    pub fn new(session: Session, ctx: egui::Context) -> CreativeCraftApp {
        theme::apply(&ctx);
        let (tx, rx) = channel();
        let cli_path = std::env::current_exe()
            .ok()
            .map(|p| p.with_file_name(if cfg!(windows) { "creativecraft-cli.exe" } else { "creativecraft-cli" }).display().to_string())
            .unwrap_or_else(|| "creativecraft-cli".into());
        let draft = session.settings.clone();
        let chat = chat::ChatState::new(session.chat_file());
        let mut app = CreativeCraftApp {
            session: Arc::new(Mutex::new(session)),
            tex: assets::Textures::load(&ctx),
            ctx,
            page: Page::Apps,
            rows: Vec::new(),
            draft,
            signed_in: false,
            cli_path,
            tx,
            rx,
            busy: None,
            install_progress: None,
            sync_progress: None,
            last: None,
            log: Vec::new(),
            new_mcp: CustomMcp { id: String::new(), name: String::new(), command: String::new(), args: Vec::new() },
            search: String::new(),
            last_auto_sync: 0.0,
            settings_open: false,
            settings_tab: SettingsTab::General,
            chat,
        };
        app.refresh();
        app
    }

    /// Re-read app status from the session (cheap; skipped while a job holds the session).
    fn refresh(&mut self) {
        let Ok(s) = self.session.try_lock() else { return };
        self.rows = s
            .catalog
            .apps
            .iter()
            .map(|a| Row {
                id: a.id.clone(),
                name: a.name.clone(),
                category: a.category.clone(),
                summary: a.summary.clone(),
                homepage: a.homepage.clone(),
                kind: a.kind,
                status: s.status(a),
            })
            .collect();
        self.signed_in = s.google_signed_in();
    }

    fn run(&mut self, label: impl Into<String>, cmd: Command) {
        if self.busy.is_some() {
            return;
        }
        let label = label.into();
        self.busy = Some(label.clone());
        let (session, tx, ctx) = (self.session.clone(), self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let result = match session.lock() {
                Ok(mut s) => {
                    let tx2 = tx.clone();
                    let ctx2 = ctx.clone();
                    s.execute(cmd, &mut |e| {
                        let _ = tx2.send(Msg::Event(e));
                        ctx2.request_repaint();
                    })
                    .map_err(|e| e.to_string())
                }
                Err(_) => Err("internal error: session lock poisoned".into()),
            };
            let _ = tx.send(Msg::Done(label, result));
            ctx.request_repaint();
        });
    }

    fn pump(&mut self) {
        let now = self.ctx.input(|i| i.time);
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Event(Event::Install(id, p)) => {
                    let (f, label) = match p {
                        Progress::Resolving => (0.0, "Preparing…".to_string()),
                        Progress::Downloading { done, total: Some(t) } if t > 0 => {
                            (done as f32 / t as f32, format!("{:.0} of {:.0} MB", done as f64 / 1e6, t as f64 / 1e6))
                        }
                        Progress::Downloading { done, .. } => (0.5, format!("{:.0} MB", done as f64 / 1e6)),
                        Progress::Verifying => (1.0, "Verifying…".into()),
                        Progress::Unpacking => (1.0, "Installing…".into()),
                        Progress::Done => (1.0, "Done".into()),
                    };
                    self.install_progress = Some((id, f, label));
                }
                Msg::Event(Event::Sync { done, total, .. }) => {
                    self.sync_progress = Some(if total == 0 { 1.0 } else { done as f32 / total as f32 });
                }
                Msg::Done(label, r) => {
                    self.busy = None;
                    self.install_progress = None;
                    self.sync_progress = None;
                    let entry = match r {
                        Ok(t) => (true, t),
                        Err(e) => (false, format!("{label}: {e}")),
                    };
                    self.last = Some((entry.0, entry.1.clone(), now));
                    self.log.push(entry);
                    if self.log.len() > 50 {
                        self.log.remove(0);
                    }
                    self.refresh();
                }
            }
        }
    }

    fn auto_sync(&mut self) {
        let mins = self.draft.auto_sync_minutes;
        if mins == 0 || self.busy.is_some() || matches!(self.draft.cloud, CloudChoice::Off) {
            return;
        }
        let now = self.ctx.input(|i| i.time);
        if now - self.last_auto_sync > f64::from(mins) * 60.0 {
            self.last_auto_sync = now;
            self.run("Sync", Command::Sync);
        }
        self.ctx.request_repaint_after(std::time::Duration::from_secs(30));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.pump();
        self.pump_chat();
        self.auto_sync();
        let t = theme::Tokens::get();

        egui::Panel::top("top").exact_size(52.0).frame(egui::Frame::NONE.fill(t.bar).inner_margin(egui::Margin::symmetric(18, 0))).show(ui, |ui| {
            self.top_bar(ui);
        });

        egui::Panel::left("nav")
            .exact_size(200.0)
            .frame(egui::Frame::NONE.fill(t.bar).inner_margin(egui::Margin { left: 10, right: 10, top: 14, bottom: 10 }))
            .show(ui, |ui| {
                for (page, label) in [(Page::Apps, "Apps"), (Page::Files, "Files"), (Page::Assistant, "Assistant")] {
                    let selected = self.page == page;
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), egui::Sense::click());
                    if selected || resp.hovered() {
                        ui.painter().rect_filled(rect, 6.0, t.row_hover);
                    }
                    let color = if selected { t.ink } else { t.muted };
                    ui.painter().text(rect.left_center() + vec2(12.0, 0.0), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(14.0), color);
                    if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        self.page = page;
                    }
                    ui.add_space(4.0);
                }
            });

        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.bg).inner_margin(egui::Margin::symmetric(32, 24))).show(ui, |ui| {
            if self.page == Page::Assistant {
                // the chat lays out its own scrolling transcript and input box
                self.chat_page(ui);
                return;
            }
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| match self.page {
                Page::Apps => self.apps_page(ui),
                Page::Files => self.files_page(ui),
                Page::Assistant => {}
            });
        });

        if self.settings_open {
            self.settings_dialog(ui.ctx().clone());
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        ui.horizontal_centered(|ui| {
            // brand: the ArtCraft wordmark as supplied, at a fixed height
            if let Some(logo) = &self.tex.logo {
                let h = 15.0;
                let w = h * logo.size()[0] as f32 / logo.size()[1].max(1) as f32;
                let (rect, _) = ui.allocate_exact_size(vec2(w, h), egui::Sense::hover());
                assets::paint(ui, logo, rect);
                ui.add_space(10.0);
            }
            let (div, _) = ui.allocate_exact_size(vec2(1.0, 16.0), egui::Sense::hover());
            ui.painter().rect_filled(div, 0.0, t.line);
            ui.add_space(10.0);
            ui.label(RichText::new("CreativeCraft").size(14.0).color(t.muted));

            // search, roughly centred
            ui.add_space((ui.available_width() / 2.0 - 220.0).max(16.0));
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("🔍  Search apps").desired_width(320.0).margin(vec2(10.0, 6.0)));
            if !self.search.is_empty() && self.page != Page::Apps {
                self.page = Page::Apps;
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new(RichText::new("⚙").size(18.0)).frame(false)).on_hover_text("Settings").clicked() {
                    self.settings_open = true;
                }
                ui.add_space(8.0);
                self.sync_indicator(ui);
            });
        });
    }

    /// One small status: syncing / last result / cloud state. Nothing when there's nothing to say.
    fn sync_indicator(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        let now = ui.input(|i| i.time);
        if let Some(f) = self.sync_progress {
            ui.add(egui::ProgressBar::new(f).desired_width(70.0).desired_height(4.0));
            ui.label(RichText::new("Syncing").size(12.0).color(t.muted));
        } else if let Some(b) = &self.busy {
            ui.spinner();
            ui.label(RichText::new(b).size(12.0).color(t.muted));
        } else if let Some((ok, text, at)) = &self.last
            && now - at < 6.0
        {
            ui.label(RichText::new(text).size(12.0).color(if *ok { t.muted } else { t.danger }));
            ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
        } else if !matches!(self.draft.cloud, CloudChoice::Off) {
            let synced = !matches!(self.draft.cloud, CloudChoice::GoogleDrive { .. }) || self.signed_in;
            let (dot, label) = if synced { (t.ok, "Cloud on") } else { (t.danger, "Cloud signed out") };
            ui.label(RichText::new(label).size(12.0).color(t.muted));
            ui.label(RichText::new("●").size(10.0).color(dot));
        }
    }

    // ─────────────────────────── Apps ───────────────────────────

    fn apps_page(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        let q = self.search.trim().to_lowercase();
        let matches = |r: &Row| q.is_empty() || r.name.to_lowercase().contains(&q) || r.category.to_lowercase().contains(&q);
        let rows: Vec<Row> = self.rows.iter().filter(|r| matches(r)).cloned().collect();

        let installed: Vec<&Row> = rows.iter().filter(|r| !matches!(r.status, Status::NotInstalled)).collect();
        let available: Vec<&Row> = rows.iter().filter(|r| matches!(r.status, Status::NotInstalled) && r.kind == Kind::Release).collect();
        let more: Vec<&Row> = rows.iter().filter(|r| matches!(r.status, Status::NotInstalled) && r.kind == Kind::External).collect();

        for (title, list) in [("Installed", &installed), ("Available", &available), ("More apps", &more)] {
            if list.is_empty() {
                continue;
            }
            ui.label(RichText::new(title).size(18.0).strong().color(t.ink));
            ui.add_space(6.0);
            for r in list.iter() {
                self.app_row(ui, r);
            }
            ui.add_space(22.0);
        }
        if installed.is_empty() && available.is_empty() && more.is_empty() {
            ui.label(RichText::new("No apps match your search.").color(t.muted));
        }
    }

    fn app_row(&mut self, ui: &mut egui::Ui, r: &Row) {
        let t = theme::Tokens::get();
        let idle = self.busy.is_none();
        let height = 64.0;
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), egui::Sense::hover());
        if resp.hovered() {
            ui.painter().rect_filled(rect, 8.0, t.row_hover);
        }
        let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(12.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));

        // icon
        let (icon, _) = row.allocate_exact_size(vec2(44.0, 44.0), egui::Sense::hover());
        match self.tex.app(&r.id) {
            Some(tex) => assets::paint(&row, tex, icon),
            None => theme::letter_tile(&mut row, icon, &r.name),
        }
        row.add_space(14.0);

        // name + one quiet line
        row.vertical(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new(&r.name).size(15.0).strong().color(t.ink)).on_hover_text(&r.summary);
            let sub = match &r.status {
                Status::Installed { version } => format!("{} · {}", r.category, version.trim_start_matches('v')),
                _ => r.category.clone(),
            };
            ui.label(RichText::new(sub).size(12.0).color(t.muted));
        });

        // actions, right-aligned
        row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some((id, f, label)) = &self.install_progress
                && *id == r.id
            {
                ui.label(RichText::new(label).size(12.0).color(t.muted));
                ui.add(egui::ProgressBar::new(*f).desired_width(140.0).desired_height(6.0));
                return;
            }
            match (&r.status, r.kind) {
                (Status::Installed { .. }, _) => {
                    ui.menu_button(RichText::new("…").size(16.0), |ui| {
                        if ui.add_enabled(idle, egui::Button::new("Check for update")).clicked() {
                            self.run(format!("Update {}", r.name), Command::Install(r.id.clone()));
                            ui.close();
                        }
                        if ui.button("Website").clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(&r.homepage));
                            ui.close();
                        }
                        ui.separator();
                        if ui.add_enabled(idle, egui::Button::new(RichText::new("Uninstall").color(t.danger))).clicked() {
                            self.run(format!("Uninstall {}", r.name), Command::Uninstall(r.id.clone()));
                            ui.close();
                        }
                    });
                    if ui.add_enabled(idle, theme::primary("Open")).clicked() {
                        self.run(format!("Open {}", r.name), Command::Launch(r.id.clone(), vec![]));
                    }
                }
                (Status::Detected { .. }, _) => {
                    if ui.add_enabled(idle, theme::primary("Open")).clicked() {
                        self.run(format!("Open {}", r.name), Command::Launch(r.id.clone(), vec![]));
                    }
                }
                (Status::NotInstalled, Kind::Release) => {
                    if ui.add_enabled(idle, theme::secondary("Install")).clicked() {
                        self.run(format!("Install {}", r.name), Command::Install(r.id.clone()));
                    }
                }
                (Status::NotInstalled, Kind::External) => {
                    if ui.add(theme::secondary("Get")).on_hover_text("Opens the official download page").clicked() {
                        self.run(format!("Open {} website", r.name), Command::OpenHomepage(r.id.clone()));
                    }
                }
            }
        });
        ui.add_space(4.0);
    }

    // ─────────────────────────── Files ───────────────────────────

    fn files_page(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        ui.label(RichText::new("Files").size(18.0).strong());
        ui.add_space(4.0);
        ui.label(RichText::new("Save your work anywhere in your projects folder. It stays in sync across your computers.").color(t.muted));
        ui.add_space(18.0);

        egui::Frame::NONE.fill(t.row).corner_radius(10).inner_margin(18).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("Projects folder").size(12.0).color(t.muted));
                    ui.label(RichText::new(self.draft.projects_dir.display().to_string()).size(14.0));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(theme::secondary("Show folder")).clicked() {
                        let _ = std::fs::create_dir_all(&self.draft.projects_dir);
                        let _ = open::that_detached(&self.draft.projects_dir);
                    }
                });
            });
            ui.add_space(14.0);
            ui.separator();
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                let idle = self.busy.is_none();
                let (title, detail) = match &self.draft.cloud {
                    CloudChoice::Off => ("Cloud sync is off", "Choose Google Drive or a synced folder in Settings.".to_string()),
                    CloudChoice::Folder { path } => ("Synced folder", path.display().to_string()),
                    CloudChoice::GoogleDrive { .. } if self.signed_in => ("Google Drive", "Signed in".to_string()),
                    CloudChoice::GoogleDrive { .. } => ("Google Drive", "Not signed in".to_string()),
                };
                ui.vertical(|ui| {
                    ui.label(RichText::new("Cloud").size(12.0).color(t.muted));
                    ui.label(RichText::new(title).size(14.0));
                    ui.label(RichText::new(detail).size(12.0).color(t.muted));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match &self.draft.cloud {
                    CloudChoice::Off => {
                        if ui.add(theme::secondary("Set up")).clicked() {
                            self.settings_tab = SettingsTab::Cloud;
                            self.settings_open = true;
                        }
                    }
                    CloudChoice::GoogleDrive { .. } if !self.signed_in => {
                        if ui.add_enabled(idle, theme::primary("Sign in")).clicked() {
                            self.run("Sign in", Command::SignInGoogle);
                        }
                    }
                    _ => {
                        if ui.add_enabled(idle, theme::primary("Sync now")).clicked() {
                            self.run("Sync", Command::Sync);
                        }
                    }
                });
            });
        });

        let recent: Vec<_> = self.log.iter().rev().take(6).cloned().collect();
        if !recent.is_empty() {
            ui.add_space(24.0);
            ui.label(RichText::new("Recent activity").size(14.0).strong());
            ui.add_space(4.0);
            for (ok, line) in recent {
                ui.label(RichText::new(line).size(12.0).color(if ok { t.muted } else { t.danger }));
            }
        }
    }

    // ─────────────────────────── Settings dialog ───────────────────────────

    fn settings_dialog(&mut self, ctx: egui::Context) {
        let t = theme::Tokens::get();
        let modal = egui::Modal::new(egui::Id::new("settings")).show(&ctx, |ui| {
            ui.set_width(560.0);
            ui.label(RichText::new("Settings").size(18.0).strong());
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for (tab, label) in [
                    (SettingsTab::General, "General"),
                    (SettingsTab::Cloud, "Cloud"),
                    (SettingsTab::Assistant, "Assistant"),
                    (SettingsTab::Connections, "Connections"),
                ] {
                    let sel = self.settings_tab == tab;
                    if ui
                        .add(egui::Button::new(RichText::new(label).color(if sel { t.ink } else { t.muted })).fill(if sel {
                            t.row_hover
                        } else {
                            egui::Color32::TRANSPARENT
                        }))
                        .clicked()
                    {
                        self.settings_tab = tab;
                    }
                }
            });
            ui.separator();
            ui.add_space(8.0);
            egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| match self.settings_tab {
                SettingsTab::General => self.settings_general(ui),
                SettingsTab::Cloud => self.settings_cloud(ui),
                SettingsTab::Assistant => self.settings_assistant(ui),
                SettingsTab::Connections => self.settings_connections(ui),
            });
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(theme::primary("Done")).clicked() {
                    self.save_draft();
                    self.settings_open = false;
                }
                if ui.add(theme::secondary("Cancel")).clicked() {
                    if let Ok(s) = self.session.try_lock() {
                        self.draft = s.settings.clone();
                    }
                    self.settings_open = false;
                }
            });
        });
        if self.settings_open && modal.should_close() {
            self.save_draft();
            self.settings_open = false;
        }
    }

    fn field(ui: &mut egui::Ui, label: &str) {
        let t = theme::Tokens::get();
        ui.label(RichText::new(label).size(12.0).color(t.muted));
    }

    fn settings_general(&mut self, ui: &mut egui::Ui) {
        Self::field(ui, "Projects folder");
        let mut p = self.draft.projects_dir.display().to_string();
        if ui.add(egui::TextEdit::singleline(&mut p).desired_width(f32::INFINITY)).changed() {
            self.draft.projects_dir = p.into();
        }
        ui.add_space(8.0);
        Self::field(ui, "This computer's name (labels conflicting copies)");
        ui.add(egui::TextEdit::singleline(&mut self.draft.device_name).desired_width(260.0));
    }

    fn settings_cloud(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        let is = |c: &CloudChoice, k: u8| matches!((c, k), (CloudChoice::Off, 0) | (CloudChoice::GoogleDrive { .. }, 1) | (CloudChoice::Folder { .. }, 2));
        if ui.radio(is(&self.draft.cloud, 0), "Off").clicked() {
            self.draft.cloud = CloudChoice::Off;
        }
        if ui.radio(is(&self.draft.cloud, 1), "Google Drive").clicked() && !is(&self.draft.cloud, 1) {
            self.draft.cloud = CloudChoice::GoogleDrive { client_id: String::new(), client_secret: String::new() };
        }
        if ui.radio(is(&self.draft.cloud, 2), "A folder your sync app already backs up").clicked() && !is(&self.draft.cloud, 2) {
            self.draft.cloud = CloudChoice::Folder { path: Default::default() };
        }
        ui.add_space(8.0);
        match &mut self.draft.cloud {
            CloudChoice::GoogleDrive { client_id, client_secret } => {
                Self::field(ui, "OAuth client (Desktop app)");
                ui.add(egui::TextEdit::singleline(client_id).hint_text("Client ID").desired_width(f32::INFINITY));
                ui.add(egui::TextEdit::singleline(client_secret).hint_text("Client secret").password(true).desired_width(f32::INFINITY));
                ui.label(RichText::new("CreativeCraft can only see the files it creates in your Drive.").size(11.0).color(t.muted));
            }
            CloudChoice::Folder { path } => {
                Self::field(ui, "Folder");
                let mut p = path.display().to_string();
                if ui.add(egui::TextEdit::singleline(&mut p).hint_text("e.g. ~/Dropbox/Art").desired_width(f32::INFINITY)).changed() {
                    *path = p.into();
                }
            }
            CloudChoice::Off => {}
        }
        if !matches!(self.draft.cloud, CloudChoice::Off) {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Sync every");
                ui.add(egui::DragValue::new(&mut self.draft.auto_sync_minutes).range(0..=240));
                ui.label("minutes (0 = manual)");
            });
        }
        if matches!(self.draft.cloud, CloudChoice::GoogleDrive { .. })
            && self.signed_in
            && ui.add_enabled(self.busy.is_none(), theme::secondary("Sign out")).clicked()
        {
            self.run("Sign out", Command::SignOutGoogle);
        }
    }

    fn settings_assistant(&mut self, ui: &mut egui::Ui) {
        use creativecraft_engine::agent::Backend;
        let t = theme::Tokens::get();
        let a = &mut self.draft.agent;
        Self::field(ui, "Agent");
        egui::ComboBox::from_id_salt("settings_backend").selected_text(a.backend.name()).width(260.0).show_ui(ui, |ui| {
            for b in Backend::ALL {
                ui.selectable_value(&mut a.backend, b, b.name());
            }
        });
        ui.add_space(8.0);
        if a.backend == Backend::Custom {
            Self::field(ui, "Command ({prompt} = your message, {mcp_config} = CreativeCraft's MCP config file)");
            ui.add(egui::TextEdit::singleline(&mut a.custom_command).hint_text("mytool run --mcp {mcp_config} {prompt}").desired_width(f32::INFINITY));
        } else {
            Self::field(ui, "Program");
            let mut bin = a.binary.clone().unwrap_or_default();
            if ui.add(egui::TextEdit::singleline(&mut bin).hint_text(a.backend.default_binary()).desired_width(f32::INFINITY)).changed() {
                a.binary = (!bin.trim().is_empty()).then_some(bin);
            }
        }
        ui.add_space(8.0);
        Self::field(ui, "Model");
        let mut model = a.model.clone().unwrap_or_default();
        if ui.add(egui::TextEdit::singleline(&mut model).hint_text("the agent's default").desired_width(f32::INFINITY)).changed() {
            a.model = (!model.trim().is_empty()).then_some(model);
        }
        ui.add_space(8.0);
        ui.checkbox(&mut a.auto_approve, "Let the assistant use all its tools without asking");
        ui.label(RichText::new("Off: only CreativeCraft's own tools are allowed.").size(11.0).color(t.muted));
    }

    fn settings_connections(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        ui.label(
            RichText::new("CreativeCraft is an MCP server for all your apps. The Assistant uses it automatically; to use it from another app, add:")
                .color(t.muted),
        );
        let config = serde_json::json!({ "mcpServers": { "creativecraft": { "command": self.cli_path, "args": ["mcp"] } } });
        let text = serde_json::to_string_pretty(&config).unwrap_or_default();
        egui::Frame::NONE.fill(t.bar).corner_radius(6).inner_margin(10).show(ui, |ui| {
            ui.monospace(&text);
        });
        if ui.add(theme::secondary("Copy")).clicked() {
            ui.ctx().copy_text(text);
        }
        ui.add_space(14.0);
        Self::field(ui, "Extra MCP servers (e.g. a Blender add-on bridge)");
        let mut remove = None;
        for (i, c) in self.draft.custom_mcp.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.monospace(format!("{}  →  {} {}", c.id, c.command, c.args.join(" ")));
                if ui.small_button("Remove").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            self.draft.custom_mcp.remove(i);
        }
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.new_mcp.id).hint_text("id").desired_width(90.0));
            ui.add(egui::TextEdit::singleline(&mut self.new_mcp.command).hint_text("command").desired_width(180.0));
            let mut args = self.new_mcp.args.join(" ");
            if ui.add(egui::TextEdit::singleline(&mut args).hint_text("arguments").desired_width(150.0)).changed() {
                self.new_mcp.args = args.split_whitespace().map(String::from).collect();
            }
            let valid = !self.new_mcp.id.is_empty()
                && self.new_mcp.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                && !self.new_mcp.command.is_empty()
                && !self.rows.iter().any(|r| r.id == self.new_mcp.id);
            if ui.add_enabled(valid, egui::Button::new("Add")).clicked() {
                let mut c = self.new_mcp.clone();
                c.name = c.id.clone();
                self.draft.custom_mcp.push(c);
                self.new_mcp = CustomMcp { id: String::new(), name: String::new(), command: String::new(), args: Vec::new() };
            }
        });
    }

    fn save_draft(&mut self) {
        let result = match self.session.try_lock() {
            Ok(mut s) => {
                if s.settings == self.draft {
                    return;
                }
                s.settings = self.draft.clone();
                s.save_settings().map(|_| "Settings saved".to_string()).map_err(|e| e.to_string())
            }
            Err(_) => Err("busy; try again in a moment".into()),
        };
        let now = self.ctx.input(|i| i.time);
        let (ok, text) = match result {
            Ok(t) => (true, t),
            Err(e) => (false, format!("Settings: {e}")),
        };
        self.last = Some((ok, text, now));
        self.refresh();
    }
}
