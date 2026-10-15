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
    ("figcraft", include_bytes!("../../../assets/apps/figcraft.png")),
    ("musiccraft", include_bytes!("../../../assets/apps/musiccraft.png")),
];

pub struct Textures {
    logo_white: Option<egui::TextureHandle>,
    logo_dark: Option<egui::TextureHandle>,
    pub mark: Option<egui::TextureHandle>,
    apps: HashMap<String, egui::TextureHandle>,
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
            apps: APPS.iter().filter_map(|(id, png)| load(ctx, id, png).map(|t| ((*id).to_string(), t))).collect(),
        }
    }

    /// The wordmark that reads on the current background.
    pub fn logo(&self) -> Option<&egui::TextureHandle> {
        if crate::theme::is_dark() { self.logo_white.as_ref() } else { self.logo_dark.as_ref() }
    }

    /// Every bundled Craft app icon, in catalog order.
    pub fn app_icons(&self) -> Vec<&egui::TextureHandle> {
        APPS.iter().filter_map(|(id, _)| self.apps.get(*id)).collect()
    }

    pub fn has_app(&self, id: &str) -> bool {
        self.apps.contains_key(id)
    }

    /// Add an icon read from an installed app (see `Installer::external_icon`).
    pub fn add_app_icon(&mut self, ctx: &egui::Context, id: &str, icon: creativecraft_install::IconData) {
        let img = match icon {
            creativecraft_install::IconData::Png(bytes) => match image::load_from_memory_with_format(&bytes, image::ImageFormat::Png) {
                Ok(i) => {
                    let i = if i.width() > 256 { i.thumbnail(256, 256) } else { i };
                    let rgba = i.to_rgba8();
                    egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw())
                }
                Err(e) => {
                    log::warn!("{id}'s icon didn't decode: {e}");
                    return;
                }
            },
            creativecraft_install::IconData::Svg(bytes) => match rasterize_svg(&bytes, 256) {
                Some(i) => i,
                None => return,
            },
        };
        self.apps.insert(id.to_string(), ctx.load_texture(format!("app/{id}"), img, egui::TextureOptions::LINEAR));
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

/// Rasterise an SVG so its longest side is `px` pixels. System fonts are loaded once, on first
/// use, so text in project files renders too.
pub fn rasterize_svg(data: &[u8], px: u32) -> Option<egui::ColorImage> {
    use resvg::{tiny_skia, usvg};
    static OPTIONS: std::sync::OnceLock<usvg::Options<'static>> = std::sync::OnceLock::new();
    let opt = OPTIONS.get_or_init(|| {
        let mut o = usvg::Options::default();
        o.fontdb_mut().load_system_fonts();
        o
    });
    let tree = usvg::Tree::from_data(data, opt).ok()?;
    let size = tree.size();
    let scale = px as f32 / size.width().max(size.height()).max(1.0);
    let (w, h) = (((size.width() * scale).ceil() as u32).max(1), ((size.height() * scale).ceil() as u32).max(1));
    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    Some(egui::ColorImage::from_rgba_premultiplied([w as usize, h as usize], pixmap.data()))
}
