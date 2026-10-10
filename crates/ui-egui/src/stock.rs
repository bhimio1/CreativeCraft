//! The Stock page: free public-domain photos, artworks, illustrations, videos and audio from open
//! collections (through StockCraft's library), searched from the top bar and downloaded into the
//! projects folder's `Stock/`, ready for the apps. Searches and preview images load on background
//! threads; downloads are engine commands.

use crate::{CreativeCraftApp, Page, theme};
use creativecraft_engine::Command;
use creativecraft_engine::stock::STOCK_DIR;
use creativecraft_stock::{Asset, LocalItem, MediaKind, StockLibrary, rendition_labels};
use egui::{Color32, Rect, RichText, vec2};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

/// Wait this long after typing stops before searching.
const DEBOUNCE: f64 = 0.7;
const ROW_H: f32 = 170.0;
const GAP: f32 = 8.0;
/// Previews are decoded to at most this many pixels on their longest side.
const THUMB_PX: u32 = 480;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    All,
    Kind(MediaKind),
    Downloaded,
}

enum Request {
    Search { text: String, kind: Option<MediaKind>, page: u32, root: PathBuf },
    Local { text: String, root: PathBuf },
}

enum Reply {
    Page { query: (String, Option<MediaKind>), page: u32, assets: Vec<Asset>, has_more: bool, failed: Vec<String> },
    Local(Vec<(LocalItem, Option<PathBuf>)>),
    Error(String),
    Image(String, Option<egui::ColorImage>),
}

enum Thumb {
    Loading,
    Ready(egui::TextureHandle),
    Failed,
}

pub(crate) struct StockState {
    tab: Tab,
    /// The query the results are for, and when the search text last changed.
    shown: Option<(String, Option<MediaKind>)>,
    typed_at: f64,
    last_text: String,
    searching: bool,
    page: u32,
    assets: Vec<Asset>,
    has_more: bool,
    failed: Vec<String>,
    local: Vec<(LocalItem, Option<PathBuf>)>,
    error: Option<String>,
    selected: Option<Asset>,
    /// After a download: the saved file (relative to the projects folder).
    saved: Option<String>,
    thumbs: HashMap<String, Thumb>,
    tx: Option<Sender<Request>>,
    img_tx: Option<Sender<String>>,
    reply_tx: Sender<Reply>,
    rx: Receiver<Reply>,
}

impl Default for StockState {
    fn default() -> StockState {
        let (reply_tx, rx) = channel();
        StockState {
            tab: Tab::All,
            shown: None,
            typed_at: 0.0,
            last_text: String::new(),
            searching: false,
            page: 1,
            assets: Vec::new(),
            has_more: false,
            failed: Vec::new(),
            local: Vec::new(),
            error: None,
            selected: None,
            saved: None,
            thumbs: HashMap::new(),
            tx: None,
            img_tx: None,
            reply_tx,
            rx,
        }
    }
}

impl StockState {
    fn kind(&self) -> Option<MediaKind> {
        match self.tab {
            Tab::Kind(k) => Some(k),
            _ => None,
        }
    }

    /// The search thread: owns a library (and its runtime) for the projects folder it was given.
    fn sender(&mut self, ctx: &egui::Context) -> Sender<Request> {
        if let Some(tx) = &self.tx {
            return tx.clone();
        }
        let (tx, rx) = channel::<Request>();
        let (out, ctx) = (self.reply_tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let mut lib: Option<(PathBuf, StockLibrary)> = None;
            for req in rx {
                let root = match &req {
                    Request::Search { root, .. } | Request::Local { root, .. } => root.clone(),
                };
                if lib.as_ref().is_none_or(|(r, _)| *r != root) {
                    match StockLibrary::open(&root) {
                        Ok(l) => lib = Some((root, l)),
                        Err(e) => {
                            let _ = out.send(Reply::Error(e.to_string()));
                            ctx.request_repaint();
                            continue;
                        }
                    }
                }
                let Some((_, l)) = &lib else { continue };
                let reply = match req {
                    Request::Search { text, kind, page, .. } => {
                        let p = l.search(&text, kind, page);
                        Reply::Page {
                            query: (text, kind),
                            page,
                            has_more: p.has_more,
                            failed: p.failures.iter().map(|f| f.source.clone()).collect(),
                            assets: p.assets,
                        }
                    }
                    Request::Local { text, .. } => Reply::Local(
                        l.local(&text)
                            .into_iter()
                            .map(|i| {
                                let path = i.files.first().and_then(|f| l.path(&i, f));
                                (i, path)
                            })
                            .collect(),
                    ),
                };
                if out.send(reply).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        self.tx = Some(tx.clone());
        tx
    }

    /// Preview images: four fetchers sharing one queue.
    fn request_image(&mut self, url: &str, ctx: &egui::Context) {
        if url.is_empty() || self.thumbs.contains_key(url) {
            return;
        }
        self.thumbs.insert(url.to_string(), Thumb::Loading);
        let tx = self.img_tx.get_or_insert_with(|| {
            let (tx, rx) = channel::<String>();
            let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));
            for _ in 0..4 {
                let (rx, out, ctx) = (rx.clone(), self.reply_tx.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(30)).build();
                    loop {
                        let next = rx.lock().ok().and_then(|r| r.recv().ok());
                        let Some(url) = next else { return };
                        let img = fetch_image(&agent, &url);
                        if out.send(Reply::Image(url, img)).is_err() {
                            return;
                        }
                        ctx.request_repaint();
                    }
                });
            }
            tx
        });
        let _ = tx.send(url.to_string());
    }
}

fn fetch_image(agent: &ureq::Agent, url: &str) -> Option<egui::ColorImage> {
    use std::io::Read;
    let resp = agent.get(url).set("User-Agent", concat!("CreativeCraft/", env!("CARGO_PKG_VERSION"))).call().ok()?;
    let mut bytes = Vec::new();
    resp.into_reader().take(20 * 1024 * 1024).read_to_end(&mut bytes).ok()?;
    let img = image::load_from_memory(&bytes).ok()?;
    let img = if img.width() > THUMB_PX || img.height() > THUMB_PX { img.thumbnail(THUMB_PX, THUMB_PX) } else { img };
    let rgba = img.to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
}

/// "Lighthouse Village" · CC0 → plain words for the detail panel.
fn rights_words(a: &Asset) -> &'static str {
    match a.rights.kind {
        creativecraft_stock::RightsKind::Cc0 => {
            "Free to use for anything, including commercially, with no need to ask or credit (CC0). A credit is still kind."
        }
        creativecraft_stock::RightsKind::PublicDomainMark => "In the public domain: no known copyright. Free to use for anything; a credit is still kind.",
        creativecraft_stock::RightsKind::UsGovernmentWork => {
            "A US federal government work, free of US copyright. Free to use; don't imply endorsement, and a credit is customary."
        }
        creativecraft_stock::RightsKind::PublicDomainDedication => {
            "Its author released it into the public domain. Free to use for anything; a credit is still kind."
        }
    }
}

impl CreativeCraftApp {
    fn stock_pump(&mut self) {
        while let Ok(r) = self.stock.rx.try_recv() {
            match r {
                Reply::Page { query, page, assets, has_more, failed } => {
                    // a reply for a search the user has since replaced is dropped
                    if self.stock.shown.as_ref() != Some(&query) {
                        continue;
                    }
                    self.stock.searching = false;
                    self.stock.error = None;
                    if page == 1 {
                        self.stock.assets = assets;
                    } else {
                        let seen: HashSet<(String, String)> = self.stock.assets.iter().map(|a| (a.source.clone(), a.id.clone())).collect();
                        self.stock.assets.extend(assets.into_iter().filter(|a| !seen.contains(&(a.source.clone(), a.id.clone()))));
                    }
                    self.stock.page = page;
                    self.stock.has_more = has_more;
                    self.stock.failed = failed;
                    // debug builds: CREATIVECRAFT_DEBUG_SELECT=<n> opens the n-th result, for screenshots
                    #[cfg(debug_assertions)]
                    if let Some(n) = std::env::var("CREATIVECRAFT_DEBUG_SELECT").ok().and_then(|v| v.parse::<usize>().ok())
                        && self.stock.selected.is_none()
                    {
                        self.stock.selected = self.stock.assets.get(n).cloned();
                    }
                }
                Reply::Local(items) => {
                    self.stock.searching = false;
                    self.stock.local = items;
                }
                Reply::Error(e) => {
                    self.stock.searching = false;
                    self.stock.error = Some(e);
                }
                Reply::Image(url, img) => {
                    let t = match img {
                        Some(i) => Thumb::Ready(self.ctx.load_texture(format!("stock:{url}"), i, egui::TextureOptions::LINEAR)),
                        None => Thumb::Failed,
                    };
                    self.stock.thumbs.insert(url, t);
                }
            }
        }
    }

    fn stock_search(&mut self, page: u32) {
        let text = self.search.trim().to_string();
        let root = self.draft.projects_dir.join(STOCK_DIR);
        let tx = self.stock.sender(&self.ctx);
        self.stock.searching = true;
        if self.stock.tab == Tab::Downloaded {
            let _ = tx.send(Request::Local { text, root });
            return;
        }
        if text.is_empty() {
            self.stock.searching = false;
            self.stock.assets.clear();
            self.stock.shown = None;
            return;
        }
        let kind = self.stock.kind();
        self.stock.shown = Some((text.clone(), kind));
        let _ = tx.send(Request::Search { text, kind, page, root });
    }

    /// After a download finishes (the engine's result line), show it in the panel and refresh
    /// the Downloaded tab.
    pub(crate) fn stock_downloaded(&mut self, result: &str) {
        if let Some(rest) = result.strip_prefix("Saved ") {
            let path = rest.split(" (").next().unwrap_or(rest).to_string();
            self.stock.saved = Some(path);
            if self.stock.tab == Tab::Downloaded {
                self.stock_search(1);
            }
        }
    }

    pub(crate) fn stock_page(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        let now = ui.input(|i| i.time);
        self.stock_pump();

        // search as you type, after a pause (Enter searches at once)
        if self.search != self.stock.last_text {
            self.stock.last_text = self.search.clone();
            self.stock.typed_at = now;
        }
        let wanted = (self.search.trim().to_string(), self.stock.kind());
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        if self.stock.tab != Tab::Downloaded && self.stock.shown.as_ref() != Some(&wanted) && (enter || now - self.stock.typed_at > DEBOUNCE) {
            self.stock_search(1);
        } else if self.stock.tab != Tab::Downloaded && self.stock.shown.as_ref() != Some(&wanted) {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        }

        // ── header and tabs
        ui.horizontal(|ui| {
            ui.label(RichText::new("Stock").size(22.0).strong().color(t.ink));
            ui.add_space(8.0);
            if self.stock.searching {
                ui.spinner();
            }
        });
        ui.label(
            RichText::new("Free public-domain photos, art, video and audio from museums, NASA and open archives. Search above; downloads go to Stock/ in your projects folder.")
                .size(13.0)
                .color(t.muted),
        );
        ui.add_space(10.0);
        let mut tab = self.stock.tab;
        ui.horizontal(|ui| {
            let mut tabs = vec![(Tab::All, "All")];
            tabs.extend(MediaKind::ALL.into_iter().map(|k| (Tab::Kind(k), k.plural())));
            tabs.push((Tab::Downloaded, "Downloaded"));
            theme::segmented(ui, &mut tab, &tabs);
        });
        if tab != self.stock.tab {
            self.stock.tab = tab;
            self.stock.selected = None;
            self.stock_search(1);
        }
        ui.add_space(12.0);

        // ── results, with the detail panel on the right when something is selected
        let panel_w = if self.stock.selected.is_some() { 360.0_f32.min(ui.available_width() * 0.45) } else { 0.0 };
        let avail = ui.available_size();
        ui.horizontal_top(|ui| {
            // columns stack their contents top to bottom (the row around them lays out sideways)
            let down = egui::Layout::top_down(egui::Align::Min);
            ui.allocate_ui_with_layout(vec2(avail.x - panel_w - if panel_w > 0.0 { 16.0 } else { 0.0 }, avail.y), down, |ui| {
                egui::ScrollArea::vertical().id_salt("stock_grid").auto_shrink(false).show(ui, |ui| {
                    if self.stock.tab == Tab::Downloaded {
                        self.stock_local_grid(ui);
                    } else {
                        self.stock_grid(ui);
                    }
                });
            });
            if self.stock.selected.is_some() {
                ui.add_space(16.0);
                ui.allocate_ui_with_layout(vec2(panel_w, avail.y), down, |ui| {
                    egui::ScrollArea::vertical().id_salt("stock_detail").auto_shrink(false).show(ui, |ui| self.stock_detail(ui));
                });
            }
        });
    }

    fn stock_grid(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        if let Some(e) = &self.stock.error {
            ui.label(RichText::new(e).color(t.danger));
        }
        if self.stock.shown.is_none() {
            theme::card(ui, |ui| {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("Search free stock media").size(17.0).strong().color(t.ink));
                    ui.label(RichText::new("Type in the search box above: lighthouse, saturn, botanical illustration…").color(t.muted));
                });
                ui.add_space(30.0);
            });
            return;
        }
        if self.stock.assets.is_empty() && !self.stock.searching {
            ui.label(RichText::new("Nothing found. Try other words or another tab.").color(t.muted));
        }
        if !self.stock.failed.is_empty() {
            ui.label(RichText::new(format!("Didn't answer this time: {}", self.stock.failed.join(", "))).size(12.0).color(t.muted));
            ui.add_space(6.0);
        }
        let assets = self.stock.assets.clone();
        if self.stock.tab == Tab::Kind(MediaKind::Audio) {
            for a in &assets {
                self.stock_audio_row(ui, a);
            }
        } else {
            // justified rows: each item keeps its shape, rows fill the width
            let width = ui.available_width();
            let aspect = |a: &Asset| match (a.width, a.height) {
                (Some(w), Some(h)) if w > 0 && h > 0 => (w as f32 / h as f32).clamp(0.4, 3.0),
                _ => 1.5,
            };
            let mut i = 0;
            while i < assets.len() {
                let mut row = Vec::new();
                let mut sum = 0.0;
                while i < assets.len() {
                    let a = aspect(&assets[i]);
                    row.push((i, a));
                    sum += a;
                    i += 1;
                    if sum * ROW_H + GAP * (row.len() as f32 - 1.0) >= width {
                        break;
                    }
                }
                let full = sum * ROW_H + GAP * (row.len() as f32 - 1.0) >= width;
                let h = if full { (width - GAP * (row.len() as f32 - 1.0)) / sum } else { ROW_H };
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    for (idx, a) in &row {
                        self.stock_tile(ui, &assets[*idx], vec2(a * h, h));
                    }
                });
                ui.add_space(GAP);
            }
        }
        if self.stock.has_more && !self.stock.searching {
            ui.add_space(8.0);
            ui.vertical_centered(|ui| {
                if ui.add(theme::secondary("Show more")).clicked() {
                    let next = self.stock.page + 1;
                    self.stock_search(next);
                }
            });
        }
        ui.add_space(16.0);
    }

    fn stock_tile(&mut self, ui: &mut egui::Ui, a: &Asset, size: egui::Vec2) {
        let t = theme::Tokens::get();
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
        if ui.is_rect_visible(rect) {
            self.stock.request_image(&a.thumbnail, &self.ctx.clone());
            let p = ui.painter_at(rect);
            p.rect_filled(rect, 6.0, t.bar);
            match self.stock.thumbs.get(&a.thumbnail) {
                Some(Thumb::Ready(tex)) => {
                    // cover the tile, cropping the longer side
                    let s = tex.size_vec2();
                    let scale = (rect.width() / s.x).max(rect.height() / s.y);
                    let shown = vec2(rect.width() / (s.x * scale), rect.height() / (s.y * scale));
                    let uv = Rect::from_center_size(egui::pos2(0.5, 0.5), shown);
                    p.image(tex.id(), rect, uv, Color32::WHITE);
                }
                Some(Thumb::Failed) => {
                    p.text(rect.center(), egui::Align2::CENTER_CENTER, a.kind.label(), egui::FontId::proportional(12.0), t.muted);
                }
                _ => {}
            }
            if a.kind == MediaKind::Video {
                let badge = Rect::from_min_size(rect.left_top() + vec2(6.0, 6.0), vec2(44.0, 18.0));
                p.rect_filled(badge, 4.0, Color32::from_black_alpha(170));
                p.text(badge.center(), egui::Align2::CENTER_CENTER, "Video", egui::FontId::proportional(11.0), Color32::WHITE);
            }
            let selected = self.stock.selected.as_ref().is_some_and(|s| s.source == a.source && s.id == a.id);
            if resp.hovered() || selected {
                p.rect_stroke(rect, 6.0, egui::Stroke::new(if selected { 3.0 } else { 2.0 }, t.ink), egui::StrokeKind::Inside);
            }
        }
        let resp = resp.on_hover_text(format!("{}\n{} · {}", a.title, a.origin, a.rights.kind.short_label())).on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.clicked() {
            self.stock.selected = Some(a.clone());
            self.stock.saved = None;
        }
    }

    fn stock_audio_row(&mut self, ui: &mut egui::Ui, a: &Asset) {
        let t = theme::Tokens::get();
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), egui::Sense::click());
        let selected = self.stock.selected.as_ref().is_some_and(|s| s.source == a.source && s.id == a.id);
        if resp.hovered() || selected {
            ui.painter().rect_filled(rect, 6.0, t.row_hover);
        }
        let dur = a.duration_ms.map(|ms| format!("{}:{:02}", ms / 60_000, (ms / 1000) % 60)).unwrap_or_default();
        ui.painter().text(rect.left_center() + vec2(12.0, -8.0), egui::Align2::LEFT_CENTER, &a.title, egui::FontId::proportional(14.0), t.ink);
        ui.painter().text(
            rect.left_center() + vec2(12.0, 10.0),
            egui::Align2::LEFT_CENTER,
            format!("{} · {} {dur}", a.origin, a.rights.kind.short_label()),
            egui::FontId::proportional(11.5),
            t.muted,
        );
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            self.stock.selected = Some(a.clone());
            self.stock.saved = None;
        }
    }

    fn stock_detail(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        let Some(a) = self.stock.selected.clone() else { return };
        let idle = self.busy.is_none();
        theme::card(ui, |ui| {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(a.kind.label()).size(12.0).color(t.muted));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new(RichText::new("×").size(16.0)).frame(false)).on_hover_text("Close").clicked() {
                        self.stock.selected = None;
                    }
                });
            });
            // a larger preview (images: the preview URL; video and audio: their thumbnail)
            let url = if a.kind.is_visual() && !a.preview.is_empty() { a.preview.clone() } else { a.thumbnail.clone() };
            self.stock.request_image(&url, &self.ctx.clone());
            if let Some(Thumb::Ready(tex)) = self.stock.thumbs.get(&url).or_else(|| self.stock.thumbs.get(&a.thumbnail)) {
                let s = tex.size_vec2();
                let w = ui.available_width();
                let h = (w * s.y / s.x).min(320.0);
                let (r, _) = ui.allocate_exact_size(vec2(w, h), egui::Sense::hover());
                let img = Rect::from_center_size(r.center(), s * (r.width() / s.x).min(r.height() / s.y));
                ui.painter().image(tex.id(), img, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            }
            ui.add_space(8.0);
            ui.label(RichText::new(&a.title).size(16.0).strong().color(t.ink));
            let mut by = Vec::new();
            if let Some(c) = a.creator.as_ref().filter(|c| !c.is_empty()) {
                by.push(c.clone());
            }
            if let Some(d) = a.date.as_ref().filter(|d| !d.is_empty()) {
                by.push(d.clone());
            }
            by.push(a.origin.clone());
            ui.label(RichText::new(by.join(" · ")).size(12.5).color(t.muted));
            ui.add_space(8.0);

            ui.label(RichText::new(a.rights.kind.long_label()).size(12.5).strong().color(t.ink));
            ui.label(RichText::new(rights_words(&a)).size(12.0).color(t.muted));
            // source-specific caveats, e.g. logos and insignia that aren't covered
            if let Some(note) = a.rights.note.as_ref().filter(|n| !n.trim().is_empty()) {
                ui.label(RichText::new(format!("Note: {note}")).size(12.0).color(t.danger));
            }
            ui.add_space(6.0);
            let credit = a.credit_line();
            ui.label(RichText::new(&credit).size(12.0).italics().color(t.ink));
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("Copy credit").size(12.0))).clicked() {
                    ui.ctx().copy_text(credit.clone());
                }
                if !a.landing_url.is_empty() && ui.add(egui::Button::new(RichText::new("Source page").size(12.0))).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&a.landing_url));
                }
            });
            ui.add_space(10.0);
            theme::row_divider(ui);
            ui.add_space(8.0);
            ui.label(RichText::new("Download").size(13.0).strong().color(t.ink));
            for (label, detail) in rendition_labels(&a) {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&label).size(13.0).color(t.ink));
                        if !detail.is_empty() {
                            ui.label(RichText::new(&detail).size(11.5).color(t.muted));
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(idle, theme::secondary("Download")).clicked() {
                            self.stock.saved = None;
                            self.run(
                                format!("Download {}", a.title),
                                Command::StockDownload { source: a.source.clone(), id: a.id.clone(), size: Some(label.clone()), max_width: None },
                            );
                        }
                    });
                });
                ui.add_space(4.0);
            }
            if let Some(saved) = self.stock.saved.clone() {
                ui.add_space(6.0);
                ui.label(RichText::new(format!("Saved to {saved}")).size(12.0).color(t.ok));
                let path = self.draft.projects_dir.join(&saved);
                ui.horizontal(|ui| {
                    if ui.add(theme::primary("Open")).clicked() {
                        self.open_stock_file(&path);
                    }
                    if ui.add(theme::secondary("Show in folder")).clicked()
                        && let Some(d) = path.parent()
                    {
                        let _ = open::that_detached(d);
                    }
                });
            }
            ui.add_space(10.0);
        });
    }

    /// Open a downloaded file in the app that opens its kind of file (an installed one first).
    fn open_stock_file(&mut self, path: &std::path::Path) {
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let app = self.session.try_lock().ok().and_then(|s| {
            let apps = s.catalog.apps_for_extension(&ext);
            apps.iter().find(|a| !matches!(s.status(a), creativecraft_install::Status::NotInstalled)).or(apps.first()).map(|a| a.id.clone())
        });
        match app {
            Some(id) => self.run("Open", Command::Launch(id, vec![path.to_path_buf()])),
            None => {
                let _ = open::that_detached(path);
            }
        }
    }

    fn stock_local_grid(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        if self.stock.local.is_empty() && !self.stock.searching {
            if self.stock.tx.is_none() {
                self.stock_search(1);
            }
            ui.label(RichText::new("Nothing downloaded yet. What you download is listed here, and stays searchable offline.").color(t.muted));
            return;
        }
        let items = self.stock.local.clone();
        for (item, path) in &items {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 64.0), egui::Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(rect, 8.0, t.row_hover);
            }
            let thumb = Rect::from_min_size(rect.left_top() + vec2(8.0, 6.0), vec2(72.0, 52.0));
            ui.painter().rect_filled(thumb, 4.0, t.bar);
            self.stock.request_image(&item.asset.thumbnail, &self.ctx.clone());
            if let Some(Thumb::Ready(tex)) = self.stock.thumbs.get(&item.asset.thumbnail) {
                let s = tex.size_vec2();
                let img = Rect::from_center_size(thumb.center(), s * (thumb.width() / s.x).min(thumb.height() / s.y));
                ui.painter().image(tex.id(), img, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            }
            let files = item.files.iter().map(|f| f.label.clone()).collect::<Vec<_>>().join(", ");
            ui.painter().text(rect.left_top() + vec2(94.0, 14.0), egui::Align2::LEFT_TOP, &item.asset.title, egui::FontId::proportional(14.0), t.ink);
            ui.painter().text(
                rect.left_top() + vec2(94.0, 36.0),
                egui::Align2::LEFT_TOP,
                format!("{} · {} · {files}", item.asset.origin, item.asset.rights.kind.short_label()),
                egui::FontId::proportional(11.5),
                t.muted,
            );
            let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Open it; right-click for more");
            if resp.clicked()
                && let Some(p) = path
            {
                let p = p.clone();
                self.open_stock_file(&p);
            }
            resp.context_menu(|ui| {
                if ui.button("Show details").clicked() {
                    self.stock.selected = Some(item.asset.clone());
                    ui.close();
                }
                if ui.button("Copy credit").clicked() {
                    ui.ctx().copy_text(item.asset.credit_line());
                    ui.close();
                }
                if let Some(p) = path
                    && ui.button("Show in folder").clicked()
                {
                    if let Some(d) = p.parent() {
                        let _ = open::that_detached(d);
                    }
                    ui.close();
                }
            });
            ui.add_space(4.0);
        }
        let _ = Page::Stock;
    }
}
