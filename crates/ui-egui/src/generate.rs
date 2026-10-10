//! The Generate page: make images and short videos from a prompt with the user's own AI service
//! (an API key they bring, a custom endpoint, or ComfyUI / Stable Diffusion WebUI on this
//! computer), and browse what was made in the projects folder's `Generated/`. Also the Settings
//! section where those services are set up.
//!
//! A generation runs on its own thread from a job copied out of the settings, so a long video
//! never holds the session (installs and syncs carry on).

use crate::{CreativeCraftApp, Page, settings::Section, theme};
use creativecraft_engine::generate::{GENERATED_DIR, GenerationJob, sidecar_path};
use creativecraft_engine::generation::{Aspect, Kind, Preset, Provider, Request, presets};
use creativecraft_engine::{Command, Event};
use egui::{Color32, Rect, RichText, vec2};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::SystemTime;

const TILE: f32 = 168.0;
const GAP: f32 = 10.0;
const THUMB_PX: u32 = 384;
/// The gallery shows this many of the newest results.
const RECENT: usize = 120;

/// One generated file and how it was made.
#[derive(Debug, Clone)]
struct Item {
    path: PathBuf,
    video: bool,
    prompt: String,
    detail: String,
    modified: SystemTime,
}

enum Reply {
    Status(String),
    Done(Result<Vec<PathBuf>, String>),
    Scan(Vec<Item>),
    Thumb(PathBuf, Option<egui::ColorImage>),
}

enum Thumb {
    Loading,
    Ready(egui::TextureHandle),
    Failed,
}

pub(crate) struct GenerateState {
    /// Whether ffmpeg is installed (looked up once), for the video-preview hint.
    has_ffmpeg: Option<bool>,
    /// Debug builds: open a provider's details for screenshots.
    #[cfg(debug_assertions)]
    pub(crate) debug_open: Option<String>,
    kind: Kind,
    prompt: String,
    negative: String,
    show_negative: bool,
    /// Provider id ("" = the default for the kind).
    provider: String,
    model: String,
    aspect: Aspect,
    count: u32,
    seconds: u32,
    seed: String,
    /// A picture to start from (edit, restyle, or a video's first frame), and how much to change it.
    start: Option<PathBuf>,
    strength: u8,
    /// A mask painted over the starting picture (only that part changes).
    mask: Option<crate::mask::MaskPaint>,
    /// While a generation runs: what the service says, and when it started.
    running: Option<(String, f64)>,
    /// Stops the running generation.
    cancel: creativecraft_engine::generation::Cancel,
    error: Option<String>,
    /// The files the last generation made, highlighted in the gallery.
    fresh: Vec<PathBuf>,
    items: Vec<Item>,
    scanned: bool,
    thumbs: HashMap<PathBuf, Thumb>,
    selected: Option<PathBuf>,
    /// Settings: the preset chosen in "Add a service", and the service whose details are open.
    add_preset: String,
    open_provider: Option<String>,
    tx: Sender<Reply>,
    rx: Receiver<Reply>,
}

impl Default for GenerateState {
    fn default() -> GenerateState {
        let (tx, rx) = channel();
        GenerateState {
            has_ffmpeg: None,
            #[cfg(debug_assertions)]
            debug_open: None,
            kind: Kind::Image,
            prompt: String::new(),
            negative: String::new(),
            show_negative: false,
            provider: String::new(),
            model: String::new(),
            aspect: Aspect::Square,
            count: 1,
            seconds: 8,
            seed: String::new(),
            start: None,
            strength: 60,
            mask: None,
            running: None,
            cancel: Default::default(),
            error: None,
            fresh: Vec::new(),
            items: Vec::new(),
            scanned: false,
            thumbs: HashMap::new(),
            selected: None,
            add_preset: "comfyui".into(),
            open_provider: None,
            tx,
            rx,
        }
    }
}

/// Every generated file under `dir` (two levels: `Generated/<date>/<file>`), newest first.
fn scan(dir: &Path) -> Vec<Item> {
    let mut out = Vec::new();
    let days = std::fs::read_dir(dir).into_iter().flatten().flatten().filter(|e| e.path().is_dir());
    for day in days {
        for e in std::fs::read_dir(day.path()).into_iter().flatten().flatten() {
            let path = e.path();
            let ext = path.extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            let video = matches!(ext.as_str(), "mp4" | "webm" | "mov" | "mkv");
            if !video && !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif") {
                continue;
            }
            let side: serde_json::Value = std::fs::read(sidecar_path(&path)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
            let s = |k: &str| side.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let detail = [s("provider"), s("model")].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ");
            let modified = e.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
            out.push(Item { path, video, prompt: s("prompt"), detail, modified });
        }
    }
    out.sort_by_key(|i| std::cmp::Reverse(i.modified));
    out.truncate(RECENT);
    out
}

fn decode(path: &Path) -> Option<egui::ColorImage> {
    let img = image::open(path).ok()?;
    let img = if img.width() > THUMB_PX || img.height() > THUMB_PX { img.thumbnail(THUMB_PX, THUMB_PX) } else { img };
    let rgba = img.to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
}

/// The preset a provider was made from (`comfyui-2` → `comfyui`), for its key page and notes.
fn preset_for(p: &Provider) -> Option<Preset> {
    presets().into_iter().find(|x| p.id == x.id || p.id.strip_prefix(&x.id).is_some_and(|rest| rest.starts_with('-')))
}

/// "Local", "Key set" or "Needs a key".
fn key_state(p: &Provider) -> (&'static str, bool) {
    if p.kind.is_local() {
        ("On this computer", true)
    } else if p.api_key.trim().is_empty() {
        ("Needs a key", false)
    } else {
        ("Key set", true)
    }
}

impl CreativeCraftApp {
    fn generate_pump(&mut self) {
        while let Ok(r) = self.generate.rx.try_recv() {
            match r {
                Reply::Status(s) => {
                    if let Some((status, _)) = &mut self.generate.running {
                        *status = s;
                    }
                }
                Reply::Done(result) => {
                    self.generate.running = None;
                    match result {
                        Ok(files) => {
                            self.generate.error = None;
                            self.generate.selected = files.first().cloned();
                            self.generate.fresh = files;
                        }
                        // stopping is what was asked for, not an error
                        Err(e) => self.generate.error = (e != "Stopped").then_some(e),
                    }
                    self.generate.scanned = false;
                    self.projects.mark_stale();
                }
                Reply::Scan(items) => self.generate.items = items,
                Reply::Thumb(path, img) => {
                    let thumb = match img {
                        Some(i) => Thumb::Ready(self.ctx.load_texture(format!("gen:{}", path.display()), i, egui::TextureOptions::LINEAR)),
                        None => Thumb::Failed,
                    };
                    self.generate.thumbs.insert(path, thumb);
                }
            }
        }
        if !self.generate.scanned {
            self.generate.scanned = true;
            let (dir, tx, ctx) = (self.draft.projects_dir.join(GENERATED_DIR), self.generate.tx.clone(), self.ctx.clone());
            std::thread::spawn(move || {
                let _ = tx.send(Reply::Scan(scan(&dir)));
                ctx.request_repaint();
            });
        }
    }

    /// Look at Generated/ again (after a command changed it).
    pub(crate) fn generate_mark_stale(&mut self) {
        self.generate.scanned = false;
    }

    fn request_thumb(&mut self, path: &Path) {
        if self.generate.thumbs.contains_key(path) {
            return;
        }
        self.generate.thumbs.insert(path.to_path_buf(), Thumb::Loading);
        let (path, tx, ctx, projects) = (path.to_path_buf(), self.generate.tx.clone(), self.ctx.clone(), self.draft.projects_dir.clone());
        std::thread::spawn(move || {
            let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            // videos: a still taken by ffmpeg when it's installed
            let img = if matches!(ext.as_str(), "mp4" | "webm" | "mov" | "mkv") {
                creativecraft_engine::generate::video_still(&projects, &path).and_then(|still| decode(&still))
            } else {
                decode(&path)
            };
            let _ = tx.send(Reply::Thumb(path, img));
            ctx.request_repaint();
        });
    }

    fn start_generation(&mut self) {
        let g = &self.generate;
        let request = Request {
            kind: g.kind,
            prompt: g.prompt.trim().to_string(),
            negative: g.negative.trim().to_string(),
            model: g.model.trim().to_string(),
            aspect: g.aspect,
            count: if g.kind == Kind::Image { g.count } else { 1 },
            seconds: (g.kind == Kind::Video).then_some(g.seconds),
            seed: g.seed.trim().parse().ok(),
            images: g.start.iter().cloned().collect(),
            strength: g.start.as_ref().map(|_| g.strength),
            mask: if g.kind == Kind::Image { self.current_mask() } else { None },
        };
        let provider = (!g.provider.is_empty()).then(|| g.provider.clone());
        let job = match GenerationJob::new(&self.draft, &request, provider.as_deref()) {
            Ok(j) => j,
            Err(e) => {
                self.generate.error = Some(e.to_string());
                return;
            }
        };
        let now = self.ctx.input(|i| i.time);
        self.generate.running = Some((format!("asking {}…", job.provider.name), now));
        self.generate.error = None;
        let cancel = creativecraft_engine::generation::Cancel::default();
        self.generate.cancel = cancel.clone();
        let (tx, ctx) = (self.generate.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let result = job.run_cancellable(
                &mut |e| {
                    if let Event::Generate(s) = e {
                        let _ = tx.send(Reply::Status(s));
                        ctx.request_repaint();
                    }
                },
                &cancel,
            );
            let _ = tx.send(Reply::Done(result.map(|g| g.files).map_err(|e| e.to_string())));
            ctx.request_repaint();
        });
    }

    pub(crate) fn generate_page(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        self.generate_pump();
        // a picture dropped on the page becomes the one to start from
        if let Some(path) = ui.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty())) {
            self.generate.start = Some(path);
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("Generate").size(22.0).strong().color(t.ink));
        });
        ui.label(
            RichText::new("Make images and short videos from a description, with your own AI service or one running on this computer. Results go to Generated/ in your projects folder.")
                .size(13.0)
                .color(t.muted),
        );
        ui.add_space(14.0);
        if self.draft.generation.providers.is_empty() {
            self.generate_setup_card(ui);
            return;
        }
        let avail = ui.available_size();
        let form_w = 340.0_f32.min(avail.x * 0.45);
        ui.horizontal_top(|ui| {
            let down = egui::Layout::top_down(egui::Align::Min);
            ui.allocate_ui_with_layout(vec2(form_w, avail.y), down, |ui| {
                egui::ScrollArea::vertical().id_salt("gen_form").auto_shrink(false).show(ui, |ui| self.generate_form(ui));
            });
            ui.add_space(20.0);
            ui.allocate_ui_with_layout(vec2(ui.available_width(), avail.y), down, |ui| {
                egui::ScrollArea::vertical().id_salt("gen_gallery").auto_shrink(false).show(ui, |ui| self.generate_gallery(ui));
            });
        });
    }

    /// No services yet: offer the common ones, local first.
    fn generate_setup_card(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        theme::card(ui, |ui| {
            ui.add_space(12.0);
            ui.label(RichText::new("Choose a service to generate with").size(15.0).color(t.ink));
            ui.label(
                RichText::new("Run Stable Diffusion or FLUX on this computer with ComfyUI or Stable Diffusion WebUI (free, private), or bring your own API key for a cloud service. Keys stay on this computer.")
                    .size(12.0)
                    .color(t.muted),
            );
            ui.add_space(10.0);
            let mut chosen = None;
            let all = presets();
            for (local, title) in [(true, "On this computer: free and private"), (false, "With your own API key")] {
                ui.label(RichText::new(title).size(12.0).color(t.muted));
                ui.horizontal_wrapped(|ui| {
                    for p in all.iter().filter(|p| p.kind.is_local() == local) {
                        if ui.add(theme::secondary(&p.name)).on_hover_text(&p.note).clicked() {
                            chosen = Some(p.id.clone());
                        }
                    }
                });
                ui.add_space(8.0);
            }
            ui.add_space(12.0);
            if let Some(id) = chosen
                && let Ok(p) = self.draft.generation.add_preset(&id)
            {
                self.generate.open_provider = Some(p.id.clone());
                self.settings_section = Section::Generation;
                self.page = Page::Settings;
            }
        });
    }

    fn generate_form(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        let busy = self.generate.running.is_some();
        let mut kind = self.generate.kind;
        if theme::segmented(ui, &mut kind, &[(Kind::Image, "Image"), (Kind::Video, "Video")]) {
            self.generate.kind = kind;
            self.generate.model.clear();
            self.generate.aspect = if kind == Kind::Video { Aspect::Landscape } else { Aspect::Square };
        }
        ui.add_space(12.0);
        ui.label(RichText::new("Describe it").size(13.0).color(t.muted));
        let hint = if self.generate.kind == Kind::Image {
            "A lighthouse on a cliff at dusk, oil painting"
        } else {
            "Slow drone shot over a misty pine forest at sunrise"
        };
        let resp = ui.add(egui::TextEdit::multiline(&mut self.generate.prompt).hint_text(hint).desired_rows(5).desired_width(f32::INFINITY));
        let submit = resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
        ui.add_space(6.0);
        if self.generate.show_negative {
            ui.label(RichText::new("What to avoid").size(13.0).color(t.muted));
            ui.add(egui::TextEdit::singleline(&mut self.generate.negative).hint_text("blurry, text, watermark").desired_width(f32::INFINITY));
        } else if ui.add(egui::Button::new(RichText::new("+ What to avoid").size(12.0).color(t.muted)).frame(false)).clicked() {
            self.generate.show_negative = true;
        }
        ui.add_space(10.0);
        self.generate_start_picture(ui);
        ui.add_space(12.0);

        // the service, among those that can make this kind
        let kind = self.generate.kind;
        let usable: Vec<Provider> = self.draft.generation.providers.iter().filter(|p| kind == Kind::Image || p.makes_video()).cloned().collect();
        let current = self.draft.generation.provider((!self.generate.provider.is_empty()).then_some(self.generate.provider.as_str()), kind).ok().cloned();
        ui.label(RichText::new("Service").size(13.0).color(t.muted));
        egui::ComboBox::from_id_salt("gen_provider")
            .selected_text(current.as_ref().map_or("Choose…".to_string(), |p| p.name.clone()))
            .width(ui.available_width() - 8.0)
            .show_ui(ui, |ui| {
                for p in &usable {
                    let (state, _) = key_state(p);
                    if ui.selectable_label(current.as_ref().is_some_and(|c| c.id == p.id), format!("{}  ·  {state}", p.name)).clicked() {
                        self.generate.provider = p.id.clone();
                        self.generate.model.clear();
                    }
                }
            });
        if usable.is_empty() && kind == Kind::Video {
            ui.label(
                RichText::new("None of your services make video. Add OpenAI, Google, xAI, Replicate, fal or a ComfyUI video workflow in Settings.")
                    .size(12.0)
                    .color(t.danger),
            );
        }
        ui.add_space(8.0);
        let default_model = current.as_ref().map(|p| if kind == Kind::Image { p.image_model.clone() } else { p.video_model.clone() }).unwrap_or_default();
        ui.label(RichText::new("Model").size(13.0).color(t.muted));
        let model_hint = if default_model.is_empty() { "the service's default".to_string() } else { default_model };
        ui.add(egui::TextEdit::singleline(&mut self.generate.model).hint_text(model_hint).desired_width(f32::INFINITY));
        ui.add_space(12.0);

        ui.label(RichText::new("Shape").size(13.0).color(t.muted));
        theme::segmented(ui, &mut self.generate.aspect, &[(Aspect::Square, "Square"), (Aspect::Landscape, "Wide"), (Aspect::Portrait, "Tall")]);
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if kind == Kind::Image {
                ui.label(RichText::new("Images").size(13.0).color(t.muted));
                theme::segmented(ui, &mut self.generate.count, &[(1, "1"), (2, "2"), (3, "3"), (4, "4")]);
            } else {
                ui.label(RichText::new("Seconds").size(13.0).color(t.muted));
                ui.add(egui::DragValue::new(&mut self.generate.seconds).range(2..=20));
            }
            ui.add_space(10.0);
            ui.label(RichText::new("Seed").size(13.0).color(t.muted));
            ui.add(egui::TextEdit::singleline(&mut self.generate.seed).hint_text("random").desired_width(80.0));
        });
        ui.add_space(14.0);

        // who pays: say it plainly before the button
        if let Some(p) = &current {
            let note = if p.kind.is_local() {
                format!("Runs on this computer with {}.", p.name)
            } else if p.api_key.trim().is_empty() && !matches!(p.kind, creativecraft_engine::generation::ProviderKind::OpenaiCompatible) {
                format!("{} needs an API key: add it in Settings → AI generation.", p.name)
            } else {
                format!("Uses your {} account, which may charge for each {}.", p.name, if kind == Kind::Image { "image" } else { "video" })
            };
            ui.label(RichText::new(note).size(12.0).color(t.muted));
            ui.add_space(8.0);
        }
        let ready = !busy && current.is_some() && !self.generate.prompt.trim().is_empty();
        let label = if kind == Kind::Image { "Generate" } else { "Generate video" };
        if (ui.add_enabled(ready, theme::primary(label).min_size(vec2(ui.available_width() - 8.0, 36.0))).clicked() || (submit && ready)) && !busy {
            self.start_generation();
        }
        if let Some((status, since)) = &self.generate.running {
            let secs = (ui.input(|i| i.time) - since).max(0.0) as u64;
            ui.add_space(8.0);
            let stopping = self.generate.cancel.is_cancelled();
            ui.horizontal(|ui| {
                ui.spinner();
                let status = if stopping { "stopping…" } else { status.as_str() };
                ui.label(RichText::new(format!("{status}  ·  {}:{:02}", secs / 60, secs % 60)).size(12.0).color(t.muted));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!stopping, theme::secondary("Stop"))
                        .on_hover_text("Stop waiting, and stop the job on the service where it allows that")
                        .clicked()
                    {
                        self.generate.cancel.cancel();
                    }
                });
            });
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        }
        if let Some(e) = &self.generate.error {
            ui.add_space(8.0);
            ui.label(RichText::new(e).size(12.0).color(t.danger));
        }
    }

    /// Open the Generate page starting from `path` (from Stock or elsewhere).
    pub(crate) fn generate_from(&mut self, path: &Path) {
        self.generate.start = Some(path.to_path_buf());
        self.generate.kind = Kind::Image;
        self.page = Page::Generate;
    }

    /// "Start from a picture": the chosen one as a thumbnail with a way to clear it, and how much
    /// to change it; or how to choose one.
    fn generate_start_picture(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        let video = self.generate.kind == Kind::Video;
        let Some(path) = self.generate.start.clone() else {
            let hint = if video {
                "Drop a picture here to use as the first frame, or right-click a result → Start from this."
            } else {
                "Drop a picture here to edit or restyle it, or right-click a result → Start from this."
            };
            ui.label(RichText::new(hint).size(12.0).color(t.muted));
            return;
        };
        ui.label(RichText::new(if video { "First frame" } else { "Start from" }).size(13.0).color(t.muted));
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(56.0, 56.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 6.0, t.bar);
            self.request_thumb(&path);
            if let Some(Thumb::Ready(tex)) = self.generate.thumbs.get(&path) {
                let s = tex.size_vec2();
                let img = Rect::from_center_size(rect.center(), s * (rect.width() / s.x).min(rect.height() / s.y));
                ui.painter().image(tex.id(), img, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            }
            ui.vertical(|ui| {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                ui.label(RichText::new(name).size(12.0).color(t.ink)).on_hover_text(path.display().to_string());
                if ui.add(egui::Button::new(RichText::new("Remove").size(12.0).color(t.muted)).frame(false)).clicked() {
                    self.generate.start = None;
                }
            });
        });
        if !video {
            self.generate_mask_row(ui, &path);
            ui.horizontal(|ui| {
                ui.label(RichText::new("How much to change").size(12.0).color(t.muted));
                ui.add(egui::Slider::new(&mut self.generate.strength, 1..=100).suffix("%")).on_hover_text(
                    "Where the service lets you choose: low keeps the picture close, high changes more. Editing models follow your words instead.",
                );
            });
        }
    }

    /// Debug builds: open the Generate page on `picture` with the mask editor open and a stroke
    /// painted, for screenshots.
    #[cfg(debug_assertions)]
    pub(crate) fn debug_mask(&mut self, picture: &Path) {
        self.generate_from(picture);
        self.generate.mask = crate::mask::MaskPaint::new(picture).map(|mut m| {
            m.debug_stroke();
            m
        });
    }

    /// The saved mask, if it belongs to the current starting picture.
    fn current_mask(&self) -> Option<PathBuf> {
        let m = self.generate.mask.as_ref()?;
        (Some(&m.source) == self.generate.start.as_ref()).then(|| m.saved.clone()).flatten()
    }

    /// "Paint area to change", or the painted mask with Edit and Remove; and the editor window.
    fn generate_mask_row(&mut self, ui: &mut egui::Ui, picture: &Path) {
        let t = theme::Tokens::get();
        if self.generate.mask.as_ref().is_some_and(|m| m.source != picture) {
            self.generate.mask = None;
        }
        ui.horizontal(|ui| match &mut self.generate.mask {
            Some(m) if m.saved.is_some() => {
                ui.label(RichText::new("Only the painted area changes").size(12.0).color(t.ink));
                if ui.add(egui::Button::new(RichText::new("Edit").size(12.0).color(t.muted)).frame(false)).clicked() {
                    m.editing = true;
                }
                if ui.add(egui::Button::new(RichText::new("Remove").size(12.0).color(t.muted)).frame(false)).clicked() {
                    self.generate.mask = None;
                }
            }
            _ => {
                if ui
                    .add(egui::Button::new(RichText::new("+ Paint area to change").size(12.0).color(t.muted)).frame(false))
                    .on_hover_text("Inpainting: change only part of the picture")
                    .clicked()
                {
                    match crate::mask::MaskPaint::new(picture) {
                        Some(m) => self.generate.mask = Some(m),
                        None => self.generate.error = Some("can't read that picture to paint on".into()),
                    }
                }
            }
        });
        let masks = self.draft.projects_dir.join(".creativecraft").join("masks");
        if let Some(m) = &mut self.generate.mask
            && m.editing
            && m.editor(ui.ctx())
        {
            match m.save(&masks) {
                Ok(path) => m.saved = Some(path),
                Err(e) => self.generate.error = Some(format!("couldn't save the mask: {e}")),
            }
        }
        // closed without Done and never saved: no mask
        if self.generate.mask.as_ref().is_some_and(|m| !m.editing && m.saved.is_none()) {
            self.generate.mask = None;
        }
    }

    fn generate_gallery(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Recent").size(15.0).color(t.ink));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(theme::secondary("Show folder")).clicked() {
                    let dir = self.draft.projects_dir.join(GENERATED_DIR);
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = open::that_detached(dir);
                }
            });
        });
        // a video without a preview: say how to get them
        let unpreviewed = self.generate.items.iter().any(|i| i.video && matches!(self.generate.thumbs.get(&i.path), Some(Thumb::Failed)));
        if unpreviewed && !*self.generate.has_ffmpeg.get_or_insert_with(|| creativecraft_platform::find_on_path("ffmpeg").is_some()) {
            let how = if cfg!(windows) {
                "winget install ffmpeg"
            } else if cfg!(target_os = "macos") {
                "brew install ffmpeg"
            } else {
                "your package manager"
            };
            ui.label(RichText::new(format!("Install ffmpeg to see previews of videos ({how}).")).size(12.0).color(t.muted));
        }
        ui.add_space(8.0);
        if self.generate.items.is_empty() {
            ui.label(RichText::new("What you make appears here, and in Generated/ in your projects folder.").color(t.muted));
            return;
        }
        let items = self.generate.items.clone();
        // as many columns as fit with tiles at least 3/4 of their size, then fill the row (but never
        // grow a tile past half again its size)
        let per_row = ((ui.available_width() + GAP) / (TILE * 0.75 + GAP)).floor().max(1.0) as usize;
        let w = ((ui.available_width() - GAP * (per_row as f32 - 1.0)) / per_row as f32).min(TILE * 1.5);
        for row in items.chunks(per_row) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for item in row {
                    self.generate_tile(ui, item, w);
                }
            });
            ui.add_space(GAP);
        }
    }

    fn generate_tile(&mut self, ui: &mut egui::Ui, item: &Item, w: f32) {
        let t = theme::Tokens::get();
        let (rect, resp) = ui.allocate_exact_size(vec2(w, w), egui::Sense::click());
        let fresh = self.generate.fresh.contains(&item.path);
        ui.painter().rect_filled(rect, 10.0, if resp.hovered() { t.row_hover } else { t.row });
        if ui.is_rect_visible(rect) {
            self.request_thumb(&item.path);
            match self.generate.thumbs.get(&item.path) {
                Some(Thumb::Ready(tex)) => {
                    let s = tex.size_vec2();
                    let inner = rect.shrink(4.0);
                    let img = Rect::from_center_size(inner.center(), s * (inner.width() / s.x).min(inner.height() / s.y));
                    ui.painter().image(tex.id(), img, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                }
                // a video without a still (no ffmpeg) is just the play badge below
                Some(Thumb::Failed) if !item.video => {
                    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "can't preview", egui::FontId::proportional(12.0), t.muted);
                }
                _ => {}
            }
            if item.video {
                ui.painter().circle_filled(rect.center(), 22.0, t.bar.gamma_multiply(0.85));
                ui.painter().text(rect.center() + vec2(2.0, 0.0), egui::Align2::CENTER_CENTER, "▶", egui::FontId::proportional(20.0), t.ink);
            }
        }
        if fresh || self.generate.selected.as_ref() == Some(&item.path) {
            ui.painter().rect_stroke(rect, 10.0, egui::Stroke::new(2.0, t.ink), egui::StrokeKind::Inside);
        }
        let name = item.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let tip = if item.prompt.is_empty() { name } else { format!("{}\n{}", item.prompt, item.detail) };
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip);
        if resp.clicked() {
            self.generate.selected = Some(item.path.clone());
            self.open_media_file(&item.path);
        }
        resp.context_menu(|ui| {
            if !item.video && ui.button("Start from this").clicked() {
                self.generate.start = Some(item.path.clone());
                ui.close();
            }
            if !item.prompt.is_empty() && ui.button("Use this prompt").clicked() {
                self.generate.prompt = item.prompt.clone();
                ui.close();
            }
            if ui.button("Copy path").clicked() {
                ui.ctx().copy_text(item.path.display().to_string());
                ui.close();
            }
            if ui.button("Move to trash").clicked() {
                self.run("Move to trash", Command::TrashGenerated(item.path.clone()));
                ui.close();
            }
            if ui.button("Show in folder").clicked() {
                if let Some(d) = item.path.parent() {
                    let _ = open::that_detached(d);
                }
                ui.close();
            }
        });
    }

    /// Settings → AI generation.
    pub(crate) fn settings_generation(&mut self, ui: &mut egui::Ui) {
        #[cfg(debug_assertions)]
        if let Some(id) = self.generate.debug_open.take() {
            self.generate.open_provider = Some(id);
        }
        let t = theme::Tokens::get();
        let idle = self.busy.is_none();
        let mut action = None;
        let mut remove = None;
        let ids: Vec<String> = self.draft.generation.providers.iter().map(|p| p.id.clone()).collect();
        if ids.is_empty() {
            theme::card(ui, |ui| {
                ui.add_space(10.0);
                ui.label(RichText::new("No services yet. Add one below: ComfyUI or Stable Diffusion WebUI run on this computer for free; the others use an API key from your own account.").size(12.0).color(t.muted));
                ui.add_space(10.0);
            });
            ui.add_space(12.0);
        }
        for id in ids {
            let open = self.generate.open_provider.as_deref() == Some(id.as_str());
            let Some(p) = self.draft.generation.providers.iter_mut().find(|p| p.id == id) else { continue };
            let preset = preset_for(p);
            theme::card(ui, |ui| {
                // header: name, state, and a toggle to show the details
                let (state, good) = key_state(p);
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), egui::Sense::click());
                ui.painter().text(rect.left_center(), egui::Align2::LEFT_CENTER, &p.name, egui::FontId::proportional(14.0), t.ink);
                ui.painter().text(
                    rect.right_center(),
                    egui::Align2::RIGHT_CENTER,
                    format!("{state}   {}", if open { "▾" } else { "▸" }),
                    egui::FontId::proportional(12.0),
                    if good { t.muted } else { t.danger },
                );
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.generate.open_provider = if open { None } else { Some(id.clone()) };
                }
                if !open {
                    return;
                }
                theme::row_divider(ui);
                if let Some(note) = preset.as_ref().map(|x| x.note.clone()).filter(|n| !n.is_empty()) {
                    ui.add_space(8.0);
                    ui.label(RichText::new(note).size(12.0).color(t.muted));
                }
                theme::setting_row(ui, "Name", "", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut p.name).desired_width(260.0));
                });
                theme::row_divider(ui);
                theme::setting_row(ui, "Address", "The service's API address. Change it for a proxy, a self-hosted server or another region.", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut p.base_url).desired_width(260.0));
                });
                if !p.kind.is_local() {
                    theme::row_divider(ui);
                    let optional = matches!(p.kind, creativecraft_engine::generation::ProviderKind::OpenaiCompatible);
                    let desc = if optional {
                        "If the server asks for one. Kept in secrets.json, readable only by you."
                    } else {
                        "From your own account. Kept in secrets.json, readable only by you; never shown to agents."
                    };
                    theme::setting_row(ui, "API key", desc, |ui| {
                        if let Some(url) = preset.as_ref().map(|x| x.key_url.clone()).filter(|u| !u.is_empty())
                            && ui.add(theme::secondary("Get a key")).clicked()
                        {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                        }
                        ui.add(egui::TextEdit::singleline(&mut p.api_key).hint_text("Paste your key").password(true).desired_width(220.0));
                    });
                }
                theme::row_divider(ui);
                let local_hint = if p.kind.is_local() { "Empty uses the first checkpoint it has." } else { "The model id as the service names it." };
                theme::setting_row(ui, "Image model", local_hint, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut p.image_model).desired_width(260.0));
                });
                if p.kind.does_video() && p.kind != creativecraft_engine::generation::ProviderKind::Comfyui {
                    theme::row_divider(ui);
                    theme::setting_row(ui, "Video model", "", |ui| {
                        ui.add(egui::TextEdit::singleline(&mut p.video_model).desired_width(260.0));
                    });
                }
                if p.kind == creativecraft_engine::generation::ProviderKind::Comfyui {
                    theme::row_divider(ui);
                    theme::setting_row(
                        ui,
                        "Your workflow",
                        "Optional: a workflow saved with \"Export (API)\", for FLUX, video and anything else. Put {{prompt}}, {{negative}}, {{seed}}, {{width}}, {{height}} where the values go.",
                        |ui| {
                            ui.add(egui::TextEdit::singleline(&mut p.workflow).hint_text("/path/to/workflow_api.json").desired_width(260.0));
                        },
                    );
                }
                if !matches!(p.kind, creativecraft_engine::generation::ProviderKind::Comfyui | creativecraft_engine::generation::ProviderKind::Google) {
                    theme::row_divider(ui);
                    theme::setting_row(ui, "Extra options", "Advanced: a JSON object added to every request, for model-specific settings.", |ui| {
                        ui.add(egui::TextEdit::singleline(&mut p.extra).hint_text("{\"steps\": 30}").desired_width(260.0).font(egui::TextStyle::Monospace));
                    });
                }
                theme::row_divider(ui);
                theme::setting_row(ui, "Check it", "Reaches the service and checks the key, without generating (or paying for) anything.", |ui| {
                    if ui.add(theme::secondary("Remove")).clicked() {
                        remove = Some(id.clone());
                    }
                    if ui.add_enabled(idle, theme::secondary("Test")).clicked() {
                        action = Some(id.clone());
                    }
                });
            });
            ui.add_space(10.0);
        }

        // add a service
        theme::card(ui, |ui| {
            let presets = presets();
            let chosen = presets.iter().find(|p| p.id == self.generate.add_preset).cloned();
            theme::setting_row(ui, "Add a service", chosen.as_ref().map_or("", |p| p.note.as_str()), |ui| {
                if ui.add(theme::secondary("Add")).clicked()
                    && let Ok(p) = self.draft.generation.add_preset(&self.generate.add_preset)
                {
                    self.generate.open_provider = Some(p.id.clone());
                }
                egui::ComboBox::from_id_salt("gen_add").selected_text(chosen.as_ref().map_or("Choose…", |p| p.name.as_str())).width(220.0).show_ui(ui, |ui| {
                    for p in &presets {
                        let label = p.name.clone();
                        ui.selectable_value(&mut self.generate.add_preset, p.id.clone(), label);
                    }
                });
            });
        });
        ui.add_space(12.0);

        // defaults and agents
        theme::card(ui, |ui| {
            let providers = self.draft.generation.providers.clone();
            let pick = |ui: &mut egui::Ui, salt: &str, value: &mut String, video: bool| {
                let name = providers.iter().find(|p| p.id == *value).map_or("First that can".to_string(), |p| p.name.clone());
                egui::ComboBox::from_id_salt(salt).selected_text(name).width(220.0).show_ui(ui, |ui| {
                    ui.selectable_value(value, String::new(), "First that can");
                    for p in providers.iter().filter(|p| !video || p.makes_video()) {
                        ui.selectable_value(value, p.id.clone(), p.name.as_str());
                    }
                });
            };
            theme::setting_row(ui, "Images with", "Used when you or an agent don't choose a service.", |ui| {
                pick(ui, "gen_default_image", &mut self.draft.generation.image_provider, false);
            });
            theme::row_divider(ui);
            theme::setting_row(ui, "Videos with", "", |ui| {
                pick(ui, "gen_default_video", &mut self.draft.generation.video_provider, true);
            });
            theme::row_divider(ui);
            theme::setting_row(
                ui,
                "Agents can generate",
                "Lets the Assistant and other agents make images and videos through CreativeCraft's tools. Cloud services bill your account.",
                |ui| {
                    theme::toggle(ui, &mut self.draft.generation.agents_can_generate);
                },
            );
        });

        if let Some(id) = remove {
            self.draft.generation.providers.retain(|p| p.id != id);
            for d in [&mut self.draft.generation.image_provider, &mut self.draft.generation.video_provider] {
                if *d == id {
                    d.clear();
                }
            }
        }
        if let Some(id) = action {
            // settings first, so the test uses what's on screen
            self.save_draft();
            self.run("Test", Command::TestGenerator(id));
        }
    }
}
