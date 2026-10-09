//! The Projects page: recent files in the projects folder as a grid of cards, each with a preview
//! taken from the file itself (see `creativecraft_engine::projects`). Click a card to open it in
//! its app. Scanning and preview decoding run on background threads, so the page never waits.

use crate::{CreativeCraftApp, Row, assets, theme};
use creativecraft_catalog::Catalog;
use creativecraft_engine::Command;
use creativecraft_engine::projects::{self, ProjectFile, Thumbnail};
use creativecraft_install::Status;
use egui::{Color32, Rect, RichText, vec2};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::SystemTime;

/// How many files the page shows at most, and how often it rescans while visible.
const LIMIT: usize = 240;
const RESCAN_SECS: f64 = 15.0;
/// Longest side of a decoded preview, in pixels.
const THUMB_PX: u32 = 360;
const CARD_W: f32 = 196.0;
const THUMB_H: f32 = 140.0;
const CARD_H: f32 = THUMB_H + 58.0;
const GAP: f32 = 16.0;

enum Msg {
    Scanned(Vec<ProjectFile>),
    Thumb(PathBuf, SystemTime, Decoded),
}

enum Decoded {
    Image(egui::ColorImage),
    Text(String),
    Nothing,
}

enum Preview {
    Pending,
    Texture(egui::TextureHandle),
    Text(String),
    Nothing,
}

pub(crate) struct ProjectsState {
    files: Vec<ProjectFile>,
    scanned_at: Option<f64>,
    scanning: bool,
    previews: HashMap<PathBuf, (SystemTime, Preview)>,
    /// Requests for the preview thread, started on first use.
    thumb_tx: Option<Sender<(PathBuf, SystemTime)>>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    /// Show only files for this app.
    filter: Option<String>,
}

impl Default for ProjectsState {
    fn default() -> ProjectsState {
        let (tx, rx) = channel();
        ProjectsState { files: Vec::new(), scanned_at: None, scanning: false, previews: HashMap::new(), thumb_tx: None, tx, rx, filter: None }
    }
}

impl ProjectsState {
    /// Rescan the next time the page shows (after a sync, an install, …).
    pub(crate) fn mark_stale(&mut self) {
        self.scanned_at = None;
    }
}

impl CreativeCraftApp {
    fn catalog_snapshot(&self) -> Option<Catalog> {
        self.session.try_lock().ok().map(|s| s.catalog.clone())
    }

    fn rescan(&mut self, now: f64) {
        if self.projects.scanning {
            return;
        }
        let Some(catalog) = self.catalog_snapshot() else { return };
        self.projects.scanning = true;
        self.projects.scanned_at = Some(now);
        let (root, tx, ctx) = (self.draft.projects_dir.clone(), self.projects.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Scanned(projects::scan(&root, &catalog, LIMIT)));
            ctx.request_repaint();
        });
    }

    fn request_preview(&mut self, f: &ProjectFile) {
        if let Some((m, _)) = self.projects.previews.get(&f.path)
            && *m == f.modified
        {
            return;
        }
        self.projects.previews.insert(f.path.clone(), (f.modified, Preview::Pending));
        let tx = self.projects.thumb_tx.get_or_insert_with(|| {
            let (req_tx, req_rx) = channel::<(PathBuf, SystemTime)>();
            let (out, ctx) = (self.projects.tx.clone(), self.ctx.clone());
            std::thread::spawn(move || {
                for (path, modified) in req_rx {
                    let decoded = decode(projects::thumbnail(&path));
                    if out.send(Msg::Thumb(path, modified, decoded)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            });
            req_tx
        });
        let _ = tx.send((f.path.clone(), f.modified));
    }

    fn pump_projects(&mut self) {
        while let Ok(m) = self.projects.rx.try_recv() {
            match m {
                Msg::Scanned(files) => {
                    self.projects.scanning = false;
                    // forget previews of files that are gone
                    self.projects.previews.retain(|p, _| files.iter().any(|f| &f.path == p));
                    self.projects.files = files;
                }
                Msg::Thumb(path, modified, d) => {
                    let p = match d {
                        Decoded::Image(img) => Preview::Texture(self.ctx.load_texture(path.display().to_string(), img, egui::TextureOptions::LINEAR)),
                        Decoded::Text(t) => Preview::Text(t),
                        Decoded::Nothing => Preview::Nothing,
                    };
                    self.projects.previews.insert(path, (modified, p));
                }
            }
        }
    }

    pub(crate) fn projects_page(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        let now = ui.input(|i| i.time);
        self.pump_projects();
        if self.projects.scanned_at.is_none_or(|at| now - at > RESCAN_SECS) {
            self.rescan(now);
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(RESCAN_SECS));

        // ── header
        ui.horizontal(|ui| {
            ui.label(RichText::new("Projects").size(22.0).strong().color(t.ink));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(theme::secondary("Show folder")).clicked() {
                    let _ = std::fs::create_dir_all(&self.draft.projects_dir);
                    let _ = open::that_detached(&self.draft.projects_dir);
                }
                let refresh = ui.add_enabled(!self.projects.scanning, theme::secondary("Refresh"));
                if refresh.clicked() {
                    self.projects.scanned_at = None;
                    self.rescan(now);
                }
                // filter by app, listing only apps that have files here
                let mut apps: Vec<&str> = self.projects.files.iter().filter_map(|f| f.app.as_deref()).collect();
                apps.sort_unstable();
                apps.dedup();
                let label = |id: &str| self.rows.iter().find(|r| r.id == id).map_or(id.to_string(), |r| r.name.clone());
                let current = self.projects.filter.as_deref().map_or("All apps".to_string(), label);
                let mut pick = self.projects.filter.clone();
                egui::ComboBox::from_id_salt("projects_filter").selected_text(current).width(150.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut pick, None, "All apps");
                    for id in &apps {
                        ui.selectable_value(&mut pick, Some((*id).to_string()), label(id));
                    }
                });
                self.projects.filter = pick;
            });
        });
        ui.label(RichText::new("Recent work in your projects folder. Click a project to open it.").size(13.0).color(t.muted));
        ui.add_space(18.0);

        let q = self.search.trim().to_lowercase();
        let shown: Vec<ProjectFile> = self
            .projects
            .files
            .iter()
            .filter(|f| self.projects.filter.as_ref().is_none_or(|a| f.app.as_ref() == Some(a)))
            .filter(|f| q.is_empty() || f.path.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().contains(&q)))
            .cloned()
            .collect();

        if shown.is_empty() {
            self.projects_empty(ui, !q.is_empty() || self.projects.filter.is_some());
            return;
        }

        // ── grid: as many columns as fit, cards stretched to fill the row
        egui::ScrollArea::vertical().id_salt("projects_grid").auto_shrink(false).show(ui, |ui| {
            let width = ui.available_width();
            let cols = (((width + GAP) / (CARD_W + GAP)).floor() as usize).max(1);
            let card_w = (width - GAP * (cols as f32 - 1.0)) / cols as f32;
            for chunk in shown.chunks(cols) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    for f in chunk {
                        self.project_card(ui, f, card_w);
                    }
                });
                ui.add_space(GAP);
            }
        });
    }

    fn projects_empty(&mut self, ui: &mut egui::Ui, filtered: bool) {
        let t = theme::Tokens::get();
        theme::card(ui, |ui| {
            ui.add_space(28.0);
            ui.vertical_centered(|ui| {
                if filtered {
                    ui.label(RichText::new("No projects match").size(16.0).strong().color(t.ink));
                    ui.label(RichText::new("Try another name or app.").color(t.muted));
                } else if self.projects.scanning && self.projects.files.is_empty() {
                    ui.spinner();
                } else {
                    ui.label(RichText::new("No projects yet").size(16.0).strong().color(t.ink));
                    ui.label(
                        RichText::new("Save your work anywhere in the projects folder and it shows up here, with a preview where the file has one.")
                            .color(t.muted),
                    );
                    ui.add_space(10.0);
                    if ui.add(theme::secondary("Show folder")).clicked() {
                        let _ = std::fs::create_dir_all(&self.draft.projects_dir);
                        let _ = open::that_detached(&self.draft.projects_dir);
                    }
                }
            });
            ui.add_space(28.0);
        });
    }

    fn project_card(&mut self, ui: &mut egui::Ui, f: &ProjectFile, w: f32) {
        let t = theme::Tokens::get();
        self.request_preview(f);
        let (rect, resp) = ui.allocate_exact_size(vec2(w, CARD_H), egui::Sense::click());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let hovered = resp.hovered();
        let painter = ui.painter_at(rect.expand(2.0));
        painter.rect_filled(rect, 12.0, t.row);
        painter.rect_stroke(rect, 12.0, egui::Stroke::new(1.0, if hovered { t.muted } else { t.line }), egui::StrokeKind::Inside);

        // preview area
        let thumb = Rect::from_min_size(rect.min + vec2(6.0, 6.0), vec2(w - 12.0, THUMB_H - 6.0));
        painter.rect_filled(thumb, 8.0, t.bar);
        let app_row = f.app.as_ref().and_then(|a| self.rows.iter().find(|r| &r.id == a)).cloned();
        match self.projects.previews.get(&f.path).map(|(_, p)| p) {
            Some(Preview::Texture(tex)) => {
                let size = tex.size_vec2();
                let scale = ((thumb.width() - 12.0) / size.x).min((thumb.height() - 12.0) / size.y).min(4.0);
                let img = Rect::from_center_size(thumb.center(), size * scale);
                // a soft checkerboard-free backing so transparent images still read
                painter.rect_filled(img, 2.0, if theme::is_dark() { Color32::from_gray(40) } else { Color32::WHITE });
                painter.image(tex.id(), img, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            }
            Some(Preview::Text(text)) => {
                let page = thumb.shrink2(vec2(14.0, 10.0));
                painter.rect_filled(page, 3.0, if theme::is_dark() { Color32::from_gray(34) } else { Color32::WHITE });
                let galley = painter.layout(text.clone(), egui::FontId::monospace(8.5), t.muted, page.width() - 12.0);
                painter.with_clip_rect(page.shrink(4.0)).galley(page.min + vec2(6.0, 6.0), galley, t.muted);
            }
            other => {
                // no preview (yet): the app's icon, faded while loading
                let s = 56.0;
                let icon_rect = Rect::from_center_size(thumb.center(), vec2(s, s));
                let alpha = if matches!(other, Some(Preview::Pending)) { 0.5 } else { 1.0 };
                match f.app.as_deref().and_then(|a| self.tex.app(a)) {
                    Some(tex) => assets::paint_faded(&painter, tex, icon_rect, alpha),
                    None => {
                        painter.rect_filled(icon_rect, 12.0, t.line);
                        painter.text(icon_rect.center(), egui::Align2::CENTER_CENTER, f.ext.to_uppercase(), egui::FontId::proportional(12.0), t.muted);
                    }
                }
            }
        }

        // name and details
        let name = f.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let text_x = rect.left() + 12.0;
        let name_galley = painter.layout_no_wrap(name.clone(), egui::FontId::proportional(13.5), t.ink);
        let max_w = w - 24.0;
        let name_pos = egui::pos2(text_x, rect.top() + THUMB_H + 10.0);
        if name_galley.size().x > max_w {
            painter.with_clip_rect(Rect::from_min_size(name_pos, vec2(max_w - 10.0, 20.0))).galley(name_pos, name_galley, t.ink);
            painter.text(egui::pos2(text_x + max_w - 10.0, name_pos.y), egui::Align2::LEFT_TOP, "…", egui::FontId::proportional(13.5), t.ink);
        } else {
            painter.galley(name_pos, name_galley, t.ink);
        }
        let mut sub_x = text_x;
        let sub_y = rect.top() + THUMB_H + 36.0;
        if let Some(tex) = f.app.as_deref().and_then(|a| self.tex.app(a)) {
            assets::paint_faded(&painter, tex, Rect::from_min_size(egui::pos2(sub_x, sub_y - 1.0), vec2(14.0, 14.0)), 1.0);
            sub_x += 19.0;
        }
        let app_name = app_row.as_ref().map_or(f.ext.to_uppercase(), |r| r.name.clone());
        painter.text(egui::pos2(sub_x, sub_y), egui::Align2::LEFT_TOP, format!("{app_name} · {}", ago(f.modified)), egui::FontId::proportional(11.5), t.muted);

        // open on click; more in the context menu
        let installed = app_row.as_ref().is_some_and(|r| !matches!(r.status, Status::NotInstalled));
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(if installed {
            format!("{}\nOpen in {app_name}", f.path.display())
        } else {
            format!("{}\nInstall {app_name} to open this", f.path.display())
        });
        if resp.clicked() {
            self.open_project(f, app_row.as_ref());
        }
        resp.context_menu(|ui| {
            if ui.button(if installed { format!("Open in {app_name}") } else { format!("Install {app_name}") }).clicked() {
                self.open_project(f, app_row.as_ref());
                ui.close();
            }
            // any other installed app that opens this kind of file
            let others: Vec<Row> = self.catalog_snapshot().map_or_else(Vec::new, |c| {
                c.apps_for_extension(&f.ext)
                    .iter()
                    .filter(|a| Some(&a.id) != f.app.as_ref())
                    .filter_map(|a| self.rows.iter().find(|r| r.id == a.id && !matches!(r.status, Status::NotInstalled)).cloned())
                    .collect()
            });
            for r in others {
                if ui.button(format!("Open in {}", r.name)).clicked() {
                    self.run(format!("Open {}", r.name), Command::Launch(r.id.clone(), vec![f.path.clone()]));
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Show in folder").clicked() {
                if let Some(dir) = f.path.parent() {
                    let _ = open::that_detached(dir);
                }
                ui.close();
            }
            if ui.button("Copy path").clicked() {
                ui.ctx().copy_text(f.path.display().to_string());
                ui.close();
            }
        });
    }

    fn open_project(&mut self, f: &ProjectFile, app: Option<&Row>) {
        let Some(r) = app else { return };
        let cmd = if matches!(r.status, Status::NotInstalled) {
            match r.kind {
                creativecraft_catalog::Kind::Release => Command::Install(r.id.clone()),
                creativecraft_catalog::Kind::External => Command::OpenHomepage(r.id.clone()),
            }
        } else {
            Command::Launch(r.id.clone(), vec![f.path.clone()])
        };
        let label = if matches!(r.status, Status::NotInstalled) { format!("Install {}", r.name) } else { format!("Open {}", r.name) };
        self.run(label, cmd);
    }
}

/// Decode a preview off the UI thread, scaled so its longest side is at most `THUMB_PX`.
fn decode(t: Option<Thumbnail>) -> Decoded {
    let img = match t {
        Some(Thumbnail::Encoded(bytes)) => match image::load_from_memory(&bytes) {
            Ok(i) => i,
            Err(_) => return Decoded::Nothing,
        },
        Some(Thumbnail::Rgba { width, height, pixels }) => match image::RgbaImage::from_raw(width, height, pixels) {
            Some(i) => image::DynamicImage::ImageRgba8(i),
            None => return Decoded::Nothing,
        },
        Some(Thumbnail::Text(s)) => return Decoded::Text(s),
        Some(Thumbnail::Svg(bytes)) => return assets::rasterize_svg(&bytes, THUMB_PX).map_or(Decoded::Nothing, Decoded::Image),
        None => return Decoded::Nothing,
    };
    let img = if img.width() > THUMB_PX || img.height() > THUMB_PX { img.thumbnail(THUMB_PX, THUMB_PX) } else { img };
    let rgba = img.to_rgba8();
    Decoded::Image(egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
}

/// "just now", "5 min ago", "3 hours ago", "yesterday", "4 days ago", then a date.
fn ago(t: SystemTime) -> String {
    let now = SystemTime::now();
    let secs = now.duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..7200 => "1 hour ago".into(),
        7200..86_400 => format!("{} hours ago", secs / 3600),
        86_400..172_800 => "yesterday".into(),
        172_800..604_800 => format!("{} days ago", secs / 86_400),
        _ => {
            let days = t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() / 86_400).unwrap_or(0) as i64;
            let (y, m, d) = civil_from_days(days);
            const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
            let this_year = civil_from_days(now.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() / 86_400).unwrap_or(0) as i64).0;
            let month = MONTHS[(m as usize).saturating_sub(1).min(11)];
            if y == this_year { format!("{month} {d}") } else { format!("{month} {d}, {y}") }
        }
    }
}

/// Days since 1970-01-01 → (year, month 1-12, day 1-31), proleptic Gregorian (H. Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_370), (2025, 10, 9));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn relative_times() {
        let now = SystemTime::now();
        assert_eq!(ago(now), "just now");
        assert_eq!(ago(now - Duration::from_secs(5 * 60)), "5 min ago");
        assert_eq!(ago(now - Duration::from_secs(3 * 3600)), "3 hours ago");
        assert_eq!(ago(now - Duration::from_secs(30 * 3600)), "yesterday");
        assert_eq!(ago(now - Duration::from_secs(4 * 86_400)), "4 days ago");
    }

    #[test]
    fn rgba_previews_decode() {
        let d = decode(Some(Thumbnail::Rgba { width: 2, height: 1, pixels: vec![255, 0, 0, 255, 0, 255, 0, 255] }));
        assert!(matches!(d, Decoded::Image(ref i) if i.size == [2, 1]));
        assert!(matches!(decode(Some(Thumbnail::Encoded(b"not an image".to_vec()))), Decoded::Nothing));
    }
}
