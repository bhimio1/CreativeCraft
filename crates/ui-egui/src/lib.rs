//! The hub window (L4). Thin by design: it shows state and turns clicks into engine commands.
//! Long commands (installs, syncs) run on a worker thread; progress comes back over a channel.
//!
//! The look is CreativeCraft's own: its own palette, layout and wording. It intentionally does not
//! imitate any commercial launcher's trade dress.
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod chat;
mod theme;

use creativecraft_catalog::Kind;
use creativecraft_engine::{CloudChoice, Command, CustomMcp, Event, Session, Settings};
use creativecraft_install::{Progress, Status};
use egui::RichText;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Chat,
    Apps,
    Projects,
    Agent,
    Settings,
}

/// One row of the app list, copied out of the session so drawing never waits on a running job.
#[derive(Debug, Clone)]
struct Row {
    id: String,
    name: String,
    category: String,
    summary: String,
    license: String,
    kind: Kind,
    status: Status,
    mcp: bool,
}

enum Msg {
    Event(Event),
    Done(String, Result<String, String>),
}

pub struct CreativeCraftApp {
    session: Arc<Mutex<Session>>,
    ctx: egui::Context,
    page: Page,
    rows: Vec<Row>,
    draft: Settings,
    signed_in: bool,
    cli_path: String,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    busy: Option<String>,
    progress: Option<(f32, String)>,
    log: Vec<(bool, String)>,
    new_mcp: CustomMcp,
    filter: String,
    last_auto_sync: f64,
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
            ctx,
            page: Page::Chat,
            rows: Vec::new(),
            draft,
            signed_in: false,
            cli_path,
            tx,
            rx,
            busy: None,
            progress: None,
            log: Vec::new(),
            new_mcp: CustomMcp { id: String::new(), name: String::new(), command: String::new(), args: Vec::new() },
            filter: String::new(),
            last_auto_sync: 0.0,
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
                license: a.license.clone(),
                kind: a.kind,
                status: s.status(a),
                mcp: a.has_mcp(),
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
        self.progress = None;
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
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Event(Event::Install(id, p)) => {
                    self.progress = Some(match p {
                        Progress::Resolving => (0.0, format!("{id}: finding the latest release…")),
                        Progress::Downloading { done, total: Some(t) } if t > 0 => {
                            (done as f32 / t as f32, format!("{id}: downloading {:.1} / {:.1} MB", done as f64 / 1e6, t as f64 / 1e6))
                        }
                        Progress::Downloading { done, .. } => (0.5, format!("{id}: downloading {:.1} MB", done as f64 / 1e6)),
                        Progress::Verifying => (1.0, format!("{id}: verifying…")),
                        Progress::Unpacking => (1.0, format!("{id}: unpacking…")),
                        Progress::Done => (1.0, format!("{id}: done")),
                    })
                }
                Msg::Event(Event::Sync { done, total, path }) => {
                    let f = if total == 0 { 1.0 } else { done as f32 / total as f32 };
                    self.progress = Some((f, if path.is_empty() { "Syncing…".into() } else { format!("Syncing {path}") }));
                }
                Msg::Done(label, r) => {
                    self.busy = None;
                    self.progress = None;
                    self.log.push(match r {
                        Ok(t) => (true, t),
                        Err(e) => (false, format!("{label}: {e}")),
                    });
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

        egui::Panel::left("nav").exact_size(196.0).frame(egui::Frame::NONE.fill(t.rail).inner_margin(14)).show(ui, |ui| {
            ui.add_space(6.0);
            ui.label(RichText::new("CreativeCraft").size(22.0).strong().color(t.ink));
            ui.label(RichText::new("your creative studio").size(12.0).color(t.muted));
            ui.add_space(18.0);
            for (page, label) in [
                (Page::Chat, "AI chat"),
                (Page::Apps, "Apps"),
                (Page::Projects, "Projects & cloud"),
                (Page::Agent, "Connections"),
                (Page::Settings, "Settings"),
            ] {
                let selected = self.page == page;
                let text = RichText::new(label).size(15.0).color(if selected { t.accent } else { t.ink });
                if ui.add(egui::Button::selectable(selected, text).min_size(egui::vec2(168.0, 32.0))).clicked() {
                    self.page = page;
                }
            }
        });

        egui::Panel::bottom("status").exact_size(30.0).frame(egui::Frame::NONE.fill(t.rail).inner_margin(egui::Margin::symmetric(14, 4))).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                if let Some((f, text)) = &self.progress {
                    ui.add(egui::ProgressBar::new(*f).desired_width(220.0));
                    ui.label(RichText::new(text).size(12.0));
                } else if let Some(b) = &self.busy {
                    ui.spinner();
                    ui.label(RichText::new(b).size(12.0));
                } else if let Some((ok, last)) = self.log.last() {
                    ui.label(RichText::new(last).size(12.0).color(if *ok { t.muted } else { t.danger }));
                }
            });
        });

        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.paper).inner_margin(24)).show(ui, |ui| {
            if self.page == Page::Chat {
                // the chat lays out its own scrolling transcript and input box
                self.chat_page(ui);
                return;
            }
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| match self.page {
                Page::Chat => {}
                Page::Apps => self.apps_page(ui),
                Page::Projects => self.projects_page(ui),
                Page::Agent => self.agent_page(ui),
                Page::Settings => self.settings_page(ui),
            });
        });
    }

    fn apps_page(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        ui.horizontal(|ui| {
            ui.heading("Apps");
            ui.add_space(12.0);
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter").desired_width(200.0));
        });
        ui.label(RichText::new("Open-source apps, installed per-user straight from each project's own releases.").color(t.muted));
        ui.add_space(12.0);

        let filter = self.filter.to_lowercase();
        let rows: Vec<Row> = self
            .rows
            .iter()
            .filter(|r| filter.is_empty() || r.name.to_lowercase().contains(&filter) || r.category.to_lowercase().contains(&filter))
            .cloned()
            .collect();
        for (section, kind) in [("Craft apps", Kind::Release), ("More open-source tools", Kind::External)] {
            let in_section: Vec<&Row> = rows.iter().filter(|r| r.kind == kind).collect();
            if in_section.is_empty() {
                continue;
            }
            ui.label(RichText::new(section).size(13.0).strong().color(t.muted));
            ui.add_space(4.0);
            for r in in_section {
                self.app_card(ui, r);
                ui.add_space(8.0);
            }
            ui.add_space(12.0);
        }
    }

    fn app_card(&mut self, ui: &mut egui::Ui, r: &Row) {
        let t = theme::Tokens::get();
        let idle = self.busy.is_none();
        egui::Frame::NONE.fill(t.card).stroke(egui::Stroke::new(1.0, t.line)).corner_radius(10).inner_margin(14).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::monogram(ui, &r.name, &r.category);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&r.name).size(16.0).strong().color(t.ink));
                        theme::chip(ui, &r.category, t.chip);
                        if r.mcp {
                            theme::chip(ui, "agent-ready", t.accent_soft);
                        }
                    });
                    ui.label(RichText::new(&r.summary).color(t.muted));
                    ui.label(RichText::new(&r.license).size(11.0).color(t.muted));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match (&r.status, r.kind) {
                    (Status::Installed { version }, _) => {
                        if ui.add_enabled(idle, egui::Button::new("Remove")).clicked() {
                            self.run(format!("Remove {}", r.name), Command::Uninstall(r.id.clone()));
                        }
                        if ui.add_enabled(idle, egui::Button::new("Update")).on_hover_text("Install the latest release if it's newer").clicked() {
                            self.run(format!("Update {}", r.name), Command::Install(r.id.clone()));
                        }
                        if ui.add_enabled(idle, theme::primary("Open")).clicked() {
                            self.run(format!("Open {}", r.name), Command::Launch(r.id.clone(), vec![]));
                        }
                        ui.label(RichText::new(version).color(t.muted));
                    }
                    (Status::Detected { .. }, _) => {
                        if ui.add_enabled(idle, theme::primary("Open")).clicked() {
                            self.run(format!("Open {}", r.name), Command::Launch(r.id.clone(), vec![]));
                        }
                        ui.label(RichText::new("installed").color(t.muted));
                    }
                    (Status::NotInstalled, Kind::Release) => {
                        if ui.add_enabled(idle, theme::primary("Install")).clicked() {
                            self.run(format!("Install {}", r.name), Command::Install(r.id.clone()));
                        }
                    }
                    (Status::NotInstalled, Kind::External) => {
                        if ui.button("Get it ↗").on_hover_text("Opens the project's official download page").clicked() {
                            self.run(format!("Open {} website", r.name), Command::OpenHomepage(r.id.clone()));
                        }
                    }
                });
            });
        });
    }

    fn projects_page(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        ui.heading("Projects & cloud");
        ui.label(RichText::new("Save your work anywhere inside your projects folder; CreativeCraft keeps it in sync across your computers.").color(t.muted));
        ui.add_space(14.0);
        egui::Grid::new("proj").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
            ui.label("Projects folder");
            ui.horizontal(|ui| {
                ui.monospace(self.draft.projects_dir.display().to_string());
                if ui.small_button("Show").clicked() {
                    let _ = std::fs::create_dir_all(&self.draft.projects_dir);
                    let _ = open::that_detached(&self.draft.projects_dir);
                }
            });
            ui.end_row();
            ui.label("Cloud");
            ui.label(match &self.draft.cloud {
                CloudChoice::Off => "Off (choose one in Settings)".to_string(),
                CloudChoice::Folder { path } => format!("Folder: {}", path.display()),
                CloudChoice::GoogleDrive { .. } if self.signed_in => "Google Drive (signed in)".to_string(),
                CloudChoice::GoogleDrive { .. } => "Google Drive (not signed in)".to_string(),
            });
            ui.end_row();
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let idle = self.busy.is_none();
            let needs_sign_in = matches!(self.draft.cloud, CloudChoice::GoogleDrive { .. }) && !self.signed_in;
            if needs_sign_in {
                if ui.add_enabled(idle, theme::primary("Sign in to Google Drive")).clicked() {
                    self.run("Sign in", Command::SignInGoogle);
                }
            } else if ui.add_enabled(idle && !matches!(self.draft.cloud, CloudChoice::Off), theme::primary("Sync now")).clicked() {
                self.run("Sync", Command::Sync);
            }
            if matches!(self.draft.cloud, CloudChoice::GoogleDrive { .. }) && self.signed_in && ui.add_enabled(idle, egui::Button::new("Sign out")).clicked() {
                self.run("Sign out", Command::SignOutGoogle);
            }
        });
        ui.add_space(16.0);
        ui.label(RichText::new("If both computers change the same file before syncing, nothing is overwritten: the other version is saved next to yours as \"name (cloud copy for …)\".").size(12.0).color(t.muted));
        ui.add_space(16.0);
        ui.label(RichText::new("Recent activity").strong());
        for (ok, line) in self.log.iter().rev().take(12) {
            ui.label(RichText::new(line).size(12.0).color(if *ok { t.ink } else { t.danger }));
        }
    }

    fn agent_page(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        ui.heading("Connections");
        ui.label(
            RichText::new("CreativeCraft is one MCP server for all your apps. The built-in AI chat connects to it automatically. You can also connect any other MCP-capable assistant: it can then list, install and open apps, sync projects, and use every agent-ready app's tools.")
                .color(t.muted),
        );
        ui.add_space(12.0);
        let config = serde_json::json!({ "mcpServers": { "creativecraft": { "command": self.cli_path, "args": ["mcp"] } } });
        let text = serde_json::to_string_pretty(&config).unwrap_or_default();
        ui.label(RichText::new("Add this to your assistant's MCP configuration:").strong());
        egui::Frame::NONE.fill(t.card).stroke(egui::Stroke::new(1.0, t.line)).corner_radius(8).inner_margin(10).show(ui, |ui| {
            ui.monospace(&text);
        });
        if ui.button("Copy").clicked() {
            ui.ctx().copy_text(text);
        }

        ui.add_space(18.0);
        ui.label(RichText::new("Agent-ready apps").strong());
        for r in self.rows.iter().filter(|r| r.mcp) {
            let ready = matches!(r.status, Status::Installed { .. });
            ui.label(RichText::new(format!("{} {}", if ready { "●" } else { "○" }, r.name)).color(if ready { t.ink } else { t.muted }));
        }

        ui.add_space(18.0);
        ui.label(RichText::new("Your own MCP servers").strong());
        ui.label(RichText::new("For apps that offer MCP through a plug-in or add-on (for example a Blender add-on bridge you've installed). The agent reaches them as <id>__<tool>.").size(12.0).color(t.muted));
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
            self.save_draft();
        }
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.new_mcp.id).hint_text("id (e.g. blender)").desired_width(120.0));
            ui.add(egui::TextEdit::singleline(&mut self.new_mcp.command).hint_text("command").desired_width(200.0));
            let mut args = self.new_mcp.args.join(" ");
            if ui.add(egui::TextEdit::singleline(&mut args).hint_text("arguments").desired_width(200.0)).changed() {
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
                self.save_draft();
            }
        });
    }

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        ui.heading("Settings");
        ui.add_space(10.0);
        egui::Grid::new("settings").num_columns(2).spacing([16.0, 12.0]).show(ui, |ui| {
            ui.label("Projects folder");
            let mut p = self.draft.projects_dir.display().to_string();
            if ui.add(egui::TextEdit::singleline(&mut p).desired_width(380.0)).changed() {
                self.draft.projects_dir = p.into();
            }
            ui.end_row();

            ui.label("This computer's name");
            ui.add(egui::TextEdit::singleline(&mut self.draft.device_name).desired_width(240.0));
            ui.end_row();

            ui.label("Cloud saves");
            ui.vertical(|ui| {
                let is =
                    |c: &CloudChoice, k: u8| matches!((c, k), (CloudChoice::Off, 0) | (CloudChoice::GoogleDrive { .. }, 1) | (CloudChoice::Folder { .. }, 2));
                if ui.radio(is(&self.draft.cloud, 0), "Off").clicked() {
                    self.draft.cloud = CloudChoice::Off;
                }
                if ui.radio(is(&self.draft.cloud, 1), "Google Drive").clicked() && !is(&self.draft.cloud, 1) {
                    self.draft.cloud = CloudChoice::GoogleDrive { client_id: String::new(), client_secret: String::new() };
                }
                if ui.radio(is(&self.draft.cloud, 2), "A folder (Dropbox, OneDrive, Drive for desktop, NAS…)").clicked() && !is(&self.draft.cloud, 2) {
                    self.draft.cloud = CloudChoice::Folder { path: Default::default() };
                }
                match &mut self.draft.cloud {
                    CloudChoice::GoogleDrive { client_id, client_secret } => {
                        ui.add(egui::TextEdit::singleline(client_id).hint_text("OAuth client id").desired_width(380.0));
                        ui.add(egui::TextEdit::singleline(client_secret).hint_text("OAuth client secret").password(true).desired_width(380.0));
                        ui.label(
                            RichText::new("CreativeCraft only gets access to files it creates (the drive.file scope), never the rest of your Drive.")
                                .size(11.0)
                                .color(t.muted),
                        );
                    }
                    CloudChoice::Folder { path } => {
                        let mut p = path.display().to_string();
                        if ui.add(egui::TextEdit::singleline(&mut p).hint_text("/path/to/synced/folder").desired_width(380.0)).changed() {
                            *path = p.into();
                        }
                    }
                    CloudChoice::Off => {}
                }
            });
            ui.end_row();

            ui.label("Sync automatically");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut self.draft.auto_sync_minutes).range(0..=240));
                ui.label("minutes (0 = only when I click Sync)");
            });
            ui.end_row();

            ui.label("AI chat");
            ui.vertical(|ui| {
                let a = &mut self.draft.agent;
                egui::ComboBox::from_id_salt("settings_backend").selected_text(a.backend.name()).show_ui(ui, |ui| {
                    for b in creativecraft_engine::agent::Backend::ALL {
                        ui.selectable_value(&mut a.backend, b, b.name());
                    }
                });
                if a.backend == creativecraft_engine::agent::Backend::Custom {
                    ui.add(egui::TextEdit::singleline(&mut a.custom_command).hint_text("mytool run --mcp {mcp_config} {prompt}").desired_width(380.0));
                    ui.label(
                        RichText::new("{prompt} is the message; {mcp_config} is a JSON file listing the CreativeCraft MCP server.").size(11.0).color(t.muted),
                    );
                } else {
                    let mut bin = a.binary.clone().unwrap_or_default();
                    if ui
                        .add(egui::TextEdit::singleline(&mut bin).hint_text(format!("program (default: {})", a.backend.default_binary())).desired_width(380.0))
                        .changed()
                    {
                        a.binary = (!bin.trim().is_empty()).then_some(bin);
                    }
                }
                let mut model = a.model.clone().unwrap_or_default();
                if ui.add(egui::TextEdit::singleline(&mut model).hint_text("model (blank = the agent's default)").desired_width(380.0)).changed() {
                    a.model = (!model.trim().is_empty()).then_some(model);
                }
                ui.checkbox(&mut a.auto_approve, "Let the agent use all its tools without asking (shell, file edits…)");
                ui.label(
                    RichText::new("Off: only CreativeCraft's own tools are pre-approved (where the agent supports it); other actions are refused in chat.")
                        .size(11.0)
                        .color(t.muted),
                );
            });
            ui.end_row();
        });
        ui.add_space(14.0);
        if ui.add_enabled(self.busy.is_none(), theme::primary("Save settings")).clicked() {
            self.save_draft();
        }
    }

    fn save_draft(&mut self) {
        let result = match self.session.try_lock() {
            Ok(mut s) => {
                s.settings = self.draft.clone();
                s.save_settings().map(|_| "Settings saved".to_string()).map_err(|e| e.to_string())
            }
            Err(_) => Err("busy; try again in a moment".into()),
        };
        self.log.push(match result {
            Ok(t) => (true, t),
            Err(e) => (false, format!("Settings: {e}")),
        });
        self.refresh();
    }
}
