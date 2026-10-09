//! Images compiled into the binary and uploaded as textures once.
//!
//! - `assets/brand/`: the ArtCraft wordmark and mark, used unmodified (see
//!   `assets/brand/LICENSE-brand.txt`; this build uses them pending the ArtCraft Team's permission).
//! - `assets/apps/`: each Craft app's own app icon, from its repository (MIT OR Apache-2.0).

use std::collections::HashMap;

const LOGO_WHITE: &[u8] = include_bytes!("../../../assets/brand/artcraft-logo-white.png");
const LOGO_DARK: &[u8] = include_bytes!("../../../assets/brand/artcraft-logo.png");
const MARK: &[u8] = include_bytes!("../../../assets/brand/artcraft-mark.png");
const APPS: &[(&str, &[u8])] = &[
    ("photocraft", include_bytes!("../../../assets/apps/photocraft.png")),
    ("vectorcraft", include_bytes!("../../../assets/apps/vectorcraft.png")),
    ("filmcraft", include_bytes!("../../../assets/apps/filmcraft.png")),
    ("lightcraft", include_bytes!("../../../assets/apps/lightcraft.png")),
    ("printcraft", include_bytes!("../../../assets/apps/printcraft.png")),
    ("effectcraft", include_bytes!("../../../assets/apps/effectcraft.png")),
    ("designcraft", include_bytes!("../../../assets/apps/designcraft.png")),
    ("cadcraft", include_bytes!("../../../assets/apps/cadcraft.png")),
    ("soundcraft", include_bytes!("../../../assets/apps/soundcraft.png")),
    ("wordcraft", include_bytes!("../../../assets/apps/wordcraft.png")),
    ("gridcraft", include_bytes!("../../../assets/apps/gridcraft.png")),
    ("deckcraft", include_bytes!("../../../assets/apps/deckcraft.png")),
];

pub struct Textures {
    logo_white: Option<egui::TextureHandle>,
    logo_dark: Option<egui::TextureHandle>,
    pub mark: Option<egui::TextureHandle>,
    apps: HashMap<&'static str, egui::TextureHandle>,
}

fn load(ctx: &egui::Context, name: &str, png: &[u8]) -> Option<egui::TextureHandle> {
    let img = match image::load_from_memory_with_format(png, image::ImageFormat::Png) {
        // the GPU's texture limit can be as low as 2048 (the wordmark is wider): scale down to fit
        Ok(i) => {
            let max = ctx.input(|i| i.max_texture_side) as u32;
            if i.width() > max || i.height() > max { i.resize(max, max, image::imageops::FilterType::Lanczos3).to_rgba8() } else { i.to_rgba8() }
        }
        Err(e) => {
            log::warn!("bundled image {name} didn't decode: {e}");
            return None;
        }
    };
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    Some(ctx.load_texture(name, color, egui::TextureOptions::LINEAR))
}

impl Textures {
    pub fn load(ctx: &egui::Context) -> Textures {
        Textures {
            logo_white: load(ctx, "brand/logo-white", LOGO_WHITE),
            logo_dark: load(ctx, "brand/logo", LOGO_DARK),
            mark: load(ctx, "brand/mark", MARK),
            apps: APPS.iter().filter_map(|(id, png)| load(ctx, id, png).map(|t| (*id, t))).collect(),
        }
    }

    /// The wordmark that reads on the current background.
    pub fn logo(&self) -> Option<&egui::TextureHandle> {
        if crate::theme::is_dark() { self.logo_white.as_ref() } else { self.logo_dark.as_ref() }
    }

    /// Every bundled Craft app icon, in catalog order.
    pub fn app_icons(&self) -> Vec<&egui::TextureHandle> {
        APPS.iter().filter_map(|(id, _)| self.apps.get(id)).collect()
    }

    pub fn app(&self, id: &str) -> Option<&egui::TextureHandle> {
        self.apps.get(id)
    }
}

/// Draw `tex` to fill `rect` exactly as supplied (no tint, no crop).
pub fn paint(ui: &egui::Ui, tex: &egui::TextureHandle, rect: egui::Rect) {
    paint_faded(ui.painter(), tex, rect, 1.0);
}

/// Draw `tex` into `rect` at `opacity` (0..=1); shape and colours stay as supplied.
pub fn paint_faded(painter: &egui::Painter, tex: &egui::TextureHandle, rect: egui::Rect, opacity: f32) {
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    painter.image(tex.id(), rect, uv, egui::Color32::WHITE.gamma_multiply(opacity.clamp(0.0, 1.0)));
}

/// Fallback fonts for glyphs egui's built-in fonts lack (symbols agent CLIs draw, CJK text). They
/// are read from the operating system's own font folders at start-up, never bundled; whichever
/// exist are added after the built-in fonts, so they only fill gaps.
const SYSTEM_FALLBACKS: &[&[&str]] = &[
    // monospace with wide symbol coverage
    &[
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
        "/System/Library/Fonts/Menlo.ttc",
        "C:\\Windows\\Fonts\\consola.ttf",
    ],
    // symbols and dingbats (✓ ⏺ ✻ ⎿ …)
    &[
        "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/google-noto/NotoSansSymbols2-Regular.ttf",
        "/System/Library/Fonts/Apple Symbols.ttf",
        "C:\\Windows\\Fonts\\seguisym.ttf",
    ],
    &["/usr/share/fonts/TTF/DejaVuSans.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", "/usr/share/fonts/dejavu/DejaVuSans.ttf"],
    // CJK
    &[
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        "/System/Library/Fonts/PingFang.ttc",
        "C:\\Windows\\Fonts\\msyh.ttc",
    ],
];

pub fn install_fallback_fonts(ctx: &egui::Context) {
    use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
    for (i, group) in SYSTEM_FALLBACKS.iter().enumerate() {
        let Some(bytes) = group.iter().find_map(|p| std::fs::read(p).ok()) else { continue };
        let families = [egui::FontFamily::Monospace, egui::FontFamily::Proportional]
            .into_iter()
            .map(|family| InsertFontFamily { family, priority: FontPriority::Lowest })
            .collect();
        ctx.add_font(FontInsert::new(&format!("system-fallback-{i}"), egui::FontData::from_owned(bytes), families));
    }
}
