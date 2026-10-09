//! Images compiled into the binary and uploaded as textures once.
//!
//! - `assets/brand/`: the ArtCraft wordmark and mark, used unmodified (see
//!   `assets/brand/LICENSE-brand.txt`; this build uses them pending the ArtCraft Team's permission).
//! - `assets/apps/`: each Craft app's own app icon, from its repository (MIT OR Apache-2.0).

use std::collections::HashMap;

const LOGO_WHITE: &[u8] = include_bytes!("../../../assets/brand/artcraft-logo-white.png");
const APPS: &[(&str, &[u8])] = &[
    ("photocraft", include_bytes!("../../../assets/apps/photocraft.png")),
    ("vectorcraft", include_bytes!("../../../assets/apps/vectorcraft.png")),
    ("filmcraft", include_bytes!("../../../assets/apps/filmcraft.png")),
    ("lightcraft", include_bytes!("../../../assets/apps/lightcraft.png")),
    ("printcraft", include_bytes!("../../../assets/apps/printcraft.png")),
    ("effectcraft", include_bytes!("../../../assets/apps/effectcraft.png")),
    ("designcraft", include_bytes!("../../../assets/apps/designcraft.png")),
];

pub struct Textures {
    pub logo: Option<egui::TextureHandle>,
    apps: HashMap<&'static str, egui::TextureHandle>,
}

fn load(ctx: &egui::Context, name: &str, png: &[u8]) -> Option<egui::TextureHandle> {
    let img = match image::load_from_memory_with_format(png, image::ImageFormat::Png) {
        Ok(i) => i.to_rgba8(),
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
        Textures { logo: load(ctx, "brand/logo-white", LOGO_WHITE), apps: APPS.iter().filter_map(|(id, png)| load(ctx, id, png).map(|t| (*id, t))).collect() }
    }

    pub fn app(&self, id: &str) -> Option<&egui::TextureHandle> {
        self.apps.get(id)
    }
}

/// Draw `tex` to fill `rect` exactly as supplied (no tint, no crop).
pub fn paint(ui: &egui::Ui, tex: &egui::TextureHandle, rect: egui::Rect) {
    ui.painter().image(tex.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
}
