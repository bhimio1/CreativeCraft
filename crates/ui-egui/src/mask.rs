//! Painting a mask over a starting picture: the part to change (inpainting). Painted on a grid at
//! most `GRID` cells on the long side; the generation service scales it to the picture.

use crate::theme;
use egui::{Color32, Rect, RichText, vec2};
use std::path::{Path, PathBuf};

const GRID: u32 = 512;

pub(crate) struct MaskPaint {
    /// The picture this mask belongs to.
    pub source: PathBuf,
    w: u32,
    h: u32,
    /// One byte per cell: 255 = change.
    cells: Vec<u8>,
    brush: f32,
    erase: bool,
    overlay: Option<egui::TextureHandle>,
    /// The picture at editing size, loaded when the editor first draws.
    picture: Option<Option<egui::TextureHandle>>,
    dirty: bool,
    /// Where the finished mask was saved (set by "Done").
    pub saved: Option<PathBuf>,
    /// The editor window is open.
    pub editing: bool,
}

impl MaskPaint {
    /// A blank mask for `source`, sized like the picture.
    pub fn new(source: &Path) -> Option<MaskPaint> {
        let (pw, ph) = image::image_dimensions(source).ok()?;
        let k = (GRID as f32 / pw.max(ph) as f32).min(1.0);
        let (w, h) = (((pw as f32 * k).round() as u32).max(1), ((ph as f32 * k).round() as u32).max(1));
        Some(MaskPaint {
            source: source.to_path_buf(),
            w,
            h,
            cells: vec![0; (w * h) as usize],
            brush: (w.max(h) as f32 / 16.0).max(4.0),
            erase: false,
            overlay: None,
            picture: None,
            dirty: true,
            saved: None,
            editing: true,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.cells.iter().all(|c| *c == 0)
    }

    /// Paint (or erase) a disc at grid position (`cx`, `cy`).
    fn dab(&mut self, cx: f32, cy: f32) {
        let r = self.brush;
        let value = if self.erase { 0 } else { 255 };
        let (x0, x1) = ((cx - r).floor().max(0.0) as u32, (cx + r).ceil().min(self.w as f32 - 1.0).max(0.0) as u32);
        let (y0, y1) = ((cy - r).floor().max(0.0) as u32, (cy + r).ceil().min(self.h as f32 - 1.0).max(0.0) as u32);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                if dx * dx + dy * dy <= r * r {
                    self.cells[(y * self.w + x) as usize] = value;
                }
            }
        }
        self.dirty = true;
    }

    /// Paint a stroke from `a` to `b` (grid coordinates), so fast drags leave no gaps.
    fn stroke(&mut self, a: egui::Pos2, b: egui::Pos2) {
        let steps = ((b - a).length() / (self.brush * 0.4).max(1.0)).ceil().max(1.0) as usize;
        for i in 0..=steps {
            let p = a + (b - a) * (i as f32 / steps as f32);
            self.dab(p.x, p.y);
        }
    }

    /// Debug builds: a stroke across the middle, for screenshots.
    #[cfg(debug_assertions)]
    pub fn debug_stroke(&mut self) {
        let (w, h) = (self.w as f32, self.h as f32);
        self.stroke(egui::pos2(w * 0.3, h * 0.45), egui::pos2(w * 0.7, h * 0.55));
    }

    fn overlay(&mut self, ctx: &egui::Context) -> egui::TextureId {
        if self.dirty || self.overlay.is_none() {
            let tint = Color32::from_rgba_unmultiplied(232, 72, 85, 140);
            let pixels = self.cells.iter().map(|c| if *c > 0 { tint } else { Color32::TRANSPARENT }).collect();
            let img = egui::ColorImage { size: [self.w as usize, self.h as usize], pixels, source_size: vec2(self.w as f32, self.h as f32) };
            match &mut self.overlay {
                Some(t) => t.set(img, egui::TextureOptions::NEAREST),
                None => self.overlay = Some(ctx.load_texture("mask-overlay", img, egui::TextureOptions::NEAREST)),
            }
            self.dirty = false;
        }
        self.overlay.as_ref().map_or(egui::TextureId::default(), egui::TextureHandle::id)
    }

    /// Save as a white-on-black PNG in `dir`; returns its path.
    pub fn save(&self, dir: &Path) -> Result<PathBuf, String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let stem = self.source.file_stem().map_or_else(|| "mask".into(), |s| s.to_string_lossy().into_owned());
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let path = dir.join(format!("{stem}-mask-{nanos:x}.png"));
        let img = image::GrayImage::from_raw(self.w, self.h, self.cells.clone()).ok_or("the mask is the wrong size")?;
        let tmp = path.with_extension("part");
        img.save_with_format(&tmp, image::ImageFormat::Png).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// The editor: the picture with the mask over it, a brush, and Done. Returns true when Done
    /// was pressed.
    pub fn editor(&mut self, ctx: &egui::Context) -> bool {
        let t = theme::Tokens::get();
        if self.picture.is_none() {
            let img = image::open(&self.source).ok().map(|i| i.thumbnail(1024, 1024).to_rgba8());
            self.picture = Some(img.map(|i| {
                let ci = egui::ColorImage::from_rgba_unmultiplied([i.width() as usize, i.height() as usize], i.as_raw());
                ctx.load_texture("mask-picture", ci, egui::TextureOptions::LINEAR)
            }));
        }
        let picture = self.picture.clone().flatten();
        let mut done = false;
        let mut open = self.editing;
        egui::Window::new("Paint the area to change").open(&mut open).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(
            ctx,
            |ui| {
                ui.label(RichText::new("Paint over what should change; the rest stays as it is.").size(12.0).color(t.muted));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    theme::segmented(ui, &mut self.erase, &[(false, "Paint"), (true, "Erase")]);
                    ui.add_space(8.0);
                    ui.label(RichText::new("Brush").size(12.0).color(t.muted));
                    let max = self.w.max(self.h) as f32 / 4.0;
                    ui.add(egui::Slider::new(&mut self.brush, 2.0..=max.max(8.0)).show_value(false));
                    if ui.add(theme::secondary("Clear")).clicked() {
                        self.cells.iter_mut().for_each(|c| *c = 0);
                        self.dirty = true;
                    }
                });
                ui.add_space(8.0);
                // the picture, fitted into the window
                let avail = vec2(600.0, 460.0);
                let scale = (avail.x / self.w as f32).min(avail.y / self.h as f32);
                let size = vec2(self.w as f32, self.h as f32) * scale;
                let (row, _) = ui.allocate_exact_size(vec2(avail.x, size.y), egui::Sense::hover());
                let rect = Rect::from_center_size(row.center(), size);
                let resp = ui.interact(rect, ui.id().with("mask-canvas"), egui::Sense::click_and_drag());
                let full = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                ui.painter().rect_filled(rect, 0.0, t.bar);
                if let Some(tex) = &picture {
                    ui.painter().image(tex.id(), rect, full, Color32::WHITE);
                }
                let to_grid = |p: egui::Pos2| ((p - rect.min) / scale).to_pos2();
                if (resp.dragged() || resp.clicked())
                    && let Some(now) = resp.interact_pointer_pos()
                {
                    let prev = now - resp.drag_delta();
                    self.stroke(to_grid(prev), to_grid(now));
                }
                let overlay = self.overlay(ui.ctx());
                ui.painter().image(overlay, rect, full, Color32::WHITE);
                // the brush outline under the pointer
                if let Some(p) = resp.hover_pos() {
                    ui.painter().circle_stroke(p, self.brush * scale, egui::Stroke::new(1.5, t.ink));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(!self.is_empty(), theme::primary("Done")).clicked() {
                            done = true;
                        }
                    });
                });
            },
        );
        self.editing = open && !done;
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strokes_paint_and_erase_then_save_white_on_black() {
        let dir = std::env::temp_dir().join(format!("cc-maskpaint-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pic = dir.join("pic.png");
        image::RgbImage::new(1024, 512).save(&pic).unwrap();
        let mut m = MaskPaint::new(&pic).unwrap();
        assert_eq!((m.w, m.h), (512, 256), "scaled to the grid");
        assert!(m.is_empty());
        m.brush = 10.0;
        m.stroke(egui::pos2(20.0, 20.0), egui::pos2(200.0, 20.0));
        assert_eq!(m.cells[(20 * m.w + 100) as usize], 255, "no gaps along the stroke");
        m.erase = true;
        m.dab(100.0, 20.0);
        assert_eq!(m.cells[(20 * m.w + 100) as usize], 0);
        let saved = m.save(&dir.join("masks")).unwrap();
        let back = image::open(&saved).unwrap().to_luma8();
        assert_eq!(back.dimensions(), (512, 256));
        assert_eq!(back.get_pixel(30, 20).0[0], 255);
        assert_eq!(back.get_pixel(300, 200).0[0], 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
