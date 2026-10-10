//! The hub window (L4). Thin by design: it shows state and turns clicks into engine commands.
//! Long commands (installs, syncs) run on a worker thread; progress comes back over a channel.
//!
//! Layout: a top bar (brand, search, sync state, theme, settings), a sidebar (Apps, Files,
//! Assistant, and Settings at the bottom) and one content area. Cloud setup, assistant setup and
//! connections live on the Settings page, so the main screens stay uncluttered.
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod assets;
mod assistant;
mod generate;
mod marketplace;
mod mask;
mod projects;
mod settings;
mod sources;
mod splash;
mod stock;
mod terminal;
mod theme;

use creativecraft_catalog::Kind;
use creativecraft_engine::{Appearance, CloudChoice, Command, CustomMcp, Event, Session, Settings};
use creativecraft_install::{Progress, Status};
use egui::{RichText, vec2};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Apps,
    Projects,
    Stock,
    Generate,
    Files,
    Assistant,
    Marketplace,
    Settings,
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
    /// Where the installed copy came from, when not the official releases.
    source: Option<String>,
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
    /// What's on disk; the Settings page saves `draft` a moment after it differs.
    saved: Settings,
    dirty_since: Option<f64>,
    signed_in: bool,
    cli_path: String,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    busy: Option<String>,
    /// (app id, fraction, label) while an install runs.
    /// (app id, fraction or negative when unknown, label) while an install or build runs.
    install_progress: Option<(String, f32, String)>,
    /// Recent git and cargo output from the current or last git build.
    build_log: Vec<String>,
    /// External apps whose installed icon has been looked for.
    icons_tried: std::collections::HashSet<String>,
    source_dialog: Option<sources::SourceDraft>,
    /// The user's rclone remotes, looked up when the rclone settings are shown.
    cloud_remotes: Option<Vec<String>>,
    /// The Marketplace's plugin list, copied out of the session.
    plugins: Vec<creativecraft_engine::Plugin>,
    marketplace: marketplace::MarketplaceState,
    stock: stock::StockState,
    generate: generate::GenerateState,
    sync_progress: Option<f32>,
    /// Last result line, shown briefly in the top bar.
    last: Option<(bool, String, f64)>,
    log: Vec<(bool, String)>,
    new_mcp: CustomMcp,
    search: String,
    last_auto_sync: f64,
    settings_section: settings::Section,
    assistant: assistant::AssistantState,
    projects: projects::ProjectsState,
    splash: splash::Splash,
}

impl CreativeCraftApp {
    pub fn new(session: Session, ctx: egui::Context) -> CreativeCraftApp {
        let (tx, rx) = channel();
        let cli_path = std::env::current_exe()
            .ok()
            .map(|p| p.with_file_name(if cfg!(windows) { "creativecraft-cli.exe" } else { "creativecraft-cli" }).display().to_string())
            .unwrap_or_else(|| "creativecraft-cli".into());
        let draft = session.settings.clone();
        let splash = splash::Splash::new(draft.intro_animation);
        theme::apply(&ctx, wants_dark(&ctx, draft.appearance));
        assets::install_fallback_fonts(&ctx);
        let mut app = CreativeCraftApp {
            session: Arc::new(Mutex::new(session)),
            tex: assets::Textures::load(&ctx),
            ctx,
            page: Page::Apps,
            rows: Vec::new(),
            saved: draft.clone(),
            draft,
            dirty_since: None,
            signed_in: false,
            cli_path,
            tx,
            rx,
            busy: None,
            install_progress: None,
            build_log: Vec::new(),
            icons_tried: Default::default(),
            source_dialog: None,
            cloud_remotes: None,
            plugins: Vec::new(),
            marketplace: marketplace::MarketplaceState::default(),
            stock: stock::StockState::default(),
            generate: generate::GenerateState::default(),
            sync_progress: None,
            last: None,
            log: Vec::new(),
            new_mcp: CustomMcp { id: String::new(), name: String::new(), command: String::new(), args: Vec::new() },
            search: String::new(),
            last_auto_sync: 0.0,
            settings_section: settings::Section::General,
            assistant: assistant::AssistantState::default(),
            projects: projects::ProjectsState::default(),
            splash,
        };
        app.refresh();
        // debug builds only: open on a given page, for screenshots and manual testing
        #[cfg(debug_assertions)]
        match std::env::var("CREATIVECRAFT_DEBUG_PAGE").as_deref() {
            Ok("files") => app.page = Page::Files,
            Ok("projects") => app.page = Page::Projects,
            Ok("stock") => app.page = Page::Stock,
            Ok("generate") => app.page = Page::Generate,
            Ok(p) if p.starts_with("stock:") => {
                app.page = Page::Stock;
                app.search = p["stock:".len()..].to_string();
            }
            Ok(p) if p.starts_with("source:") => {
                if let Some(r) = app.rows.iter().find(|r| r.id == p["source:".len()..]) {
                    app.source_dialog = Some(sources::SourceDraft::for_row(r, &app.draft));
                }
            }
            Ok("assistant") => app.page = Page::Assistant,
            Ok(p) if p.starts_with("assistant:") => {
                app.page = Page::Assistant;
                for _ in 0..p["assistant:".len()..].parse::<usize>().unwrap_or(1) {
                    app.assistant.debug_add_tab(app.draft.agent.backend);
                }
            }
            Ok(p) if p.starts_with("generate-mask:") => {
                let picture = std::path::PathBuf::from(&p["generate-mask:".len()..]);
                app.debug_mask(&picture);
            }
            Ok(p) if p.starts_with("generation:") => {
                app.page = Page::Settings;
                app.settings_section = settings::Section::Generation;
                app.generate.debug_open = Some(p["generation:".len()..].to_string());
            }
            Ok("marketplace") => app.page = Page::Marketplace,
            Ok("settings") => app.page = Page::Settings,
            Ok("settings:appearance") | Ok("settings:assistant") | Ok("settings:connections") | Ok("settings:cloud") | Ok("settings:generation") => {
                app.page = Page::Settings;
                app.settings_section = match std::env::var("CREATIVECRAFT_DEBUG_PAGE").as_deref() {
                    Ok("settings:appearance") => settings::Section::Appearance,
                    Ok("settings:cloud") => settings::Section::Cloud,
                    Ok("settings:generation") => settings::Section::Generation,
                    Ok("settings:connections") => settings::Section::Connections,
                    _ => settings::Section::Assistant,
                };
            }
            _ => {}
        }
        app
    }

    /// Re-read app status from the session (cheap; skipped while a job holds the session).
    fn refresh(&mut self) {
        let Ok(s) = self.session.try_lock() else { return };
        // plugin commands change the session's settings; keep the draft in step, so saving other
        // settings never undoes an install
        self.plugins = s.marketplace.plugins.clone();
        self.draft.plugins = s.settings.plugins.clone();
        self.saved.plugins = s.settings.plugins.clone();
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
                source: s.installed(&a.id).and_then(|i| i.source.clone()),
            })
            .collect();
        self.signed_in = s.google_signed_in();
        // installed external apps show the icon they ship with (looked up once per app)
        let wanted: Vec<_> = s
            .catalog
            .apps
            .iter()
            .filter(|a| a.kind == Kind::External && !self.tex.has_app(&a.id) && self.icons_tried.insert(a.id.clone()))
            .filter_map(|a| s.installer.external_icon(a).map(|i| (a.id.clone(), i)))
            .collect();
        drop(s);
        for (id, icon) in wanted {
            self.tex.add_app_icon(&self.ctx, &id, icon);
        }
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
                        Progress::Fetching => {
                            self.build_log.clear();
                            (-1.0, "Getting the source…".into())
                        }
                        Progress::Building { crates: 0, .. } => (-1.0, "Building…".into()),
                        Progress::Building { crates, current } => (-1.0, format!("Building · {crates} crates · {current}")),
                        Progress::Log(line) => {
                            self.build_log.push(line);
                            if self.build_log.len() > 400 {
                                self.build_log.remove(0);
                            }
                            continue;
                        }
                        Progress::Done => (1.0, "Done".into()),
                    };
                    self.install_progress = Some((id, f, label));
                }
                Msg::Event(Event::Generate(status)) => {
                    if self.busy.is_some() {
                        self.busy = Some(status);
                    }
                }
                Msg::Event(Event::Sync { done, total, .. }) => {
                    self.sync_progress = Some(if total == 0 { 1.0 } else { done as f32 / total as f32 });
                }
                Msg::Done(label, r) => {
                    self.busy = None;
                    if let Ok(text) = &r {
                        self.stock_downloaded(text);
                    }
                    self.projects.mark_stale();
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
        // "System" follows the OS live; a change in Settings previews immediately
        let dark = wants_dark(ui.ctx(), self.draft.appearance);
        if dark != theme::is_dark() {
            theme::apply(ui.ctx(), dark);
        }
        self.pump();
        self.auto_sync();
        let t = theme::Tokens::get();

        egui::Panel::top("top").exact_size(52.0).frame(egui::Frame::NONE.fill(t.bar).inner_margin(egui::Margin::symmetric(18, 0))).show(ui, |ui| {
            self.top_bar(ui);
        });

        egui::Panel::left("nav")
            .exact_size(200.0)
            .frame(egui::Frame::NONE.fill(t.bar).inner_margin(egui::Margin { left: 10, right: 10, top: 14, bottom: 10 }))
            .show(ui, |ui| {
                for (page, label) in [
                    (Page::Apps, "Apps"),
                    (Page::Projects, "Projects"),
                    (Page::Stock, "Stock"),
                    (Page::Generate, "Generate"),
                    (Page::Files, "Files"),
                    (Page::Assistant, "Assistant"),
                    (Page::Marketplace, "Marketplace"),
                ] {
                    self.nav_item(ui, page, label);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    self.nav_item(ui, Page::Settings, "Settings");
                    self.report_hub_bug_link(ui);
                });
            });

        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.bg).inner_margin(egui::Margin::symmetric(32, 24))).show(ui, |ui| {
            // the terminal, the projects grid and the settings page lay out (and scroll) themselves
            match self.page {
                Page::Assistant => self.assistant_page(ui),
                Page::Marketplace => self.marketplace_page(ui),
                Page::Stock => self.stock_page(ui),
                Page::Generate => self.generate_page(ui),
                Page::Projects => self.projects_page(ui),
                Page::Settings => self.settings_page(ui),
                Page::Apps | Page::Files => {
                    egui::ScrollArea::vertical().auto_shrink(false).show(
                        ui,
                        |ui| {
                            if self.page == Page::Apps { self.apps_page(ui) } else { self.files_page(ui) }
                        },
                    );
                }
            }
        });
        if self.page != Page::Settings && self.draft != self.saved {
            // left the page (or toggled the theme) with an edit pending
            self.save_draft();
        }
        if self.source_dialog.is_some() {
            self.source_dialog_ui(ui.ctx().clone());
        }
        self.splash.show(ui.ctx(), &self.tex);
        #[cfg(debug_assertions)]
        debug_self_screenshot(ui.ctx());
    }

    /// "Report a bug" for CreativeCraft itself, quiet at the foot of the sidebar.
    fn report_hub_bug_link(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), egui::Sense::click());
        let color = if resp.hovered() { t.ink } else { t.muted };
        ui.painter().text(rect.left_center() + vec2(12.0, 0.0), egui::Align2::LEFT_CENTER, "Report a bug", egui::FontId::proportional(13.0), color);
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Report a problem with CreativeCraft").clicked() {
            self.run("Report a bug", Command::ReportBug(None));
        }
    }

    fn nav_item(&mut self, ui: &mut egui::Ui, page: Page, label: &str) {
        let t = theme::Tokens::get();
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

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        ui.horizontal_centered(|ui| {
            // brand: the ArtCraft wordmark as supplied, at a fixed height
            if let Some(logo) = self.tex.logo() {
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
            // searches projects on the Projects page, apps everywhere else
            let hint = match self.page {
                Page::Projects => "🔍  Search projects",
                Page::Marketplace => "🔍  Search plugins",
                Page::Stock => "🔍  Search free stock media",
                _ => "🔍  Search apps",
            };
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text(hint).desired_width(320.0).margin(vec2(10.0, 6.0)));
            if !self.search.is_empty() && !matches!(self.page, Page::Apps | Page::Projects | Page::Marketplace | Page::Stock) {
                self.page = Page::Apps;
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new(RichText::new("⚙").size(18.0)).frame(false)).on_hover_text("Settings").clicked() {
                    self.page = Page::Settings;
                }
                ui.add_space(4.0);
                let dark = theme::is_dark();
                if theme_toggle(ui, dark).on_hover_text(if dark { "Switch to light theme" } else { "Switch to dark theme" }).clicked() {
                    self.draft.appearance = if dark { Appearance::Light } else { Appearance::Dark };
                    self.save_draft();
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
            let configured = self.draft.app_sources.get(&r.id).filter(|s| **s != creativecraft_engine::Source::Release);
            let sub = match (&r.status, &r.source) {
                (Status::Installed { version }, Some(src)) => format!("{} · {} · {src}", r.category, version.trim_start_matches('v')),
                (Status::Installed { version }, None) => format!("{} · {}", r.category, version.trim_start_matches('v')),
                (Status::NotInstalled, _) if configured.is_some() => {
                    format!("{} · installs from {}", r.category, configured.map(|s| s.label()).unwrap_or_default())
                }
                _ => r.category.clone(),
            };
            ui.label(RichText::new(sub).size(12.0).color(t.muted));
        });

        // actions, right-aligned
        row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some((id, f, label)) = &self.install_progress
                && *id == r.id
            {
                if *f < 0.0 {
                    // a git build: no total to measure against; offer its log instead
                    if ui.add(egui::Button::new(RichText::new("Log").size(12.0)).frame(false)).on_hover_text("Show the build output").clicked() {
                        self.source_dialog = Some(sources::SourceDraft::for_row(r, &self.draft));
                    }
                    ui.label(RichText::new(label).size(12.0).color(t.muted));
                    ui.spinner();
                } else {
                    ui.label(RichText::new(label).size(12.0).color(t.muted));
                    ui.add(egui::ProgressBar::new(*f).desired_width(140.0).desired_height(6.0));
                }
                return;
            }
            // every app gets the same menu; what's in it depends on how it's installed
            self.app_menu(ui, r, idle);
            match (&r.status, r.kind) {
                (Status::Installed { .. } | Status::Detected { .. }, _) => {
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

    fn app_menu(&mut self, ui: &mut egui::Ui, r: &Row, idle: bool) {
        let t = theme::Tokens::get();
        let installed = matches!(r.status, Status::Installed { .. });
        ui.menu_button(RichText::new("…").size(16.0), |ui| {
            if installed && ui.add_enabled(idle, egui::Button::new("Check for update")).clicked() {
                self.run(format!("Update {}", r.name), Command::Install(r.id.clone()));
                ui.close();
            }
            if r.kind == Kind::Release
                && ui.add_enabled(idle, egui::Button::new("Install source…")).on_hover_text("Official releases, a fork, or a git build").clicked()
            {
                self.source_dialog = Some(sources::SourceDraft::for_row(r, &self.draft));
                ui.close();
            }
            if ui.button("Website").clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(&r.homepage));
                ui.close();
            }
            if ui.add_enabled(idle, egui::Button::new("Report a bug")).on_hover_text(format!("Opens {}'s issue tracker", r.name)).clicked() {
                self.run(format!("Report a bug in {}", r.name), Command::ReportBug(Some(r.id.clone())));
                ui.close();
            }
            if installed {
                ui.separator();
                if ui.add_enabled(idle, egui::Button::new(RichText::new("Uninstall").color(t.danger))).clicked() {
                    self.run(format!("Uninstall {}", r.name), Command::Uninstall(r.id.clone()));
                    ui.close();
                }
            }
        });
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
                    CloudChoice::Off => ("Cloud sync is off", "Choose a cloud service, a NAS or a synced folder in Settings.".to_string()),
                    CloudChoice::Folder { path } => ("Synced folder", path.display().to_string()),
                    CloudChoice::GoogleDrive { .. } if self.signed_in => ("Google Drive", "Signed in".to_string()),
                    CloudChoice::GoogleDrive { .. } => ("Google Drive", "Not signed in".to_string()),
                    CloudChoice::WebDav { url, .. } => ("WebDAV", url.clone()),
                    CloudChoice::S3 { endpoint, bucket, prefix, .. } => ("S3 storage", format!("{bucket}/{prefix} at {endpoint}")),
                    CloudChoice::Rclone { remote, folder } => ("rclone", format!("{}:{folder}", remote.trim_end_matches(':'))),
                };
                ui.vertical(|ui| {
                    ui.label(RichText::new("Cloud").size(12.0).color(t.muted));
                    ui.label(RichText::new(title).size(14.0));
                    ui.label(RichText::new(detail).size(12.0).color(t.muted));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match &self.draft.cloud {
                    CloudChoice::Off => {
                        if ui.add(theme::secondary("Set up")).clicked() {
                            self.settings_section = settings::Section::Cloud;
                            self.page = Page::Settings;
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

    /// Write the draft settings. Quiet on success (the page saves as you go); errors show in the
    /// top bar and the save is retried on the next edit.
    fn save_draft(&mut self) {
        let result = match self.session.try_lock() {
            Ok(mut s) => {
                if s.settings != self.draft {
                    s.settings = self.draft.clone();
                    s.save_settings().map_err(|e| e.to_string())
                } else {
                    Ok(())
                }
            }
            Err(_) => Err("busy; try again in a moment".into()),
        };
        match result {
            Ok(()) => self.saved = self.draft.clone(),
            Err(e) => {
                let now = self.ctx.input(|i| i.time);
                self.last = Some((false, format!("Settings: {e}"), now));
                // don't retry every frame
                self.saved = self.draft.clone();
            }
        }
        self.refresh();
    }
}

/// Resolve the appearance setting to "dark or not" (the OS setting decides "System"; dark if unknown).
fn wants_dark(ctx: &egui::Context, a: Appearance) -> bool {
    match a {
        Appearance::Dark => true,
        Appearance::Light => false,
        Appearance::System => ctx.system_theme() != Some(egui::Theme::Light),
    }
}

/// A small sun (in the dark theme: click for light) or crescent moon (in the light theme).
fn theme_toggle(ui: &mut egui::Ui, dark: bool) -> egui::Response {
    let t = theme::Tokens::get();
    let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), egui::Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let p = ui.painter();
    if resp.hovered() {
        p.circle_filled(rect.center(), 13.0, t.row_hover);
    }
    let c = rect.center();
    let ink = if resp.hovered() { t.ink } else { t.muted };
    if dark {
        p.circle_filled(c, 4.0, ink);
        for i in 0..8 {
            let a = i as f32 * std::f32::consts::TAU / 8.0;
            let d = vec2(a.cos(), a.sin());
            p.line_segment([c + d * 6.5, c + d * 8.5], egui::Stroke::new(1.5, ink));
        }
    } else {
        p.circle_filled(c, 7.0, ink);
        p.circle_filled(c + vec2(3.5, -3.0), 6.0, if resp.hovered() { t.row_hover } else { t.bar });
    }
    resp
}

/// Debug builds only: with `CREATIVECRAFT_DEBUG_SHOT=<file.png>`, save a picture of this window
/// (nothing else on screen) a few seconds after it opens, then close. For checking layouts.
#[cfg(debug_assertions)]
fn debug_self_screenshot(ctx: &egui::Context) {
    let Ok(out) = std::env::var("CREATIVECRAFT_DEBUG_SHOT") else { return };
    let asked = egui::Id::new("debug_shot_asked");
    let shot = ctx.input(|i| {
        i.events.iter().find_map(|e| match e {
            egui::Event::Screenshot { image, .. } => Some(image.clone()),
            _ => None,
        })
    });
    if let Some(image) = shot {
        let [w, h] = image.size;
        let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
        if let Some(img) = image::RgbaImage::from_raw(w as u32, h as u32, rgba) {
            let _ = img.save(&out);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        return;
    }
    let wait: f64 = std::env::var("CREATIVECRAFT_DEBUG_SHOT_AFTER").ok().and_then(|s| s.parse().ok()).unwrap_or(4.0);
    if ctx.input(|i| i.time) > wait && !ctx.data(|d| d.get_temp::<bool>(asked).unwrap_or(false)) {
        ctx.data_mut(|d| d.insert_temp(asked, true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
}
