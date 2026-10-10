//! Stock media in the projects folder: search StockCraft's public-domain sources and download into
//! `<projects>/Stock`, where it syncs with the rest of the user's work and apps can open it.

use crate::{EngineError, Session};
use creativecraft_stock::{Asset, MediaKind, Pick, StockLibrary, rendition_labels};
use serde_json::{Value, json};

/// The folder downloads go to, inside the projects folder.
pub const STOCK_DIR: &str = "Stock";

/// A short JSON summary of an asset, for agents.
pub fn asset_json(a: &Asset) -> Value {
    json!({
        "source": a.source, "id": a.id, "kind": a.kind.slug(), "title": a.title,
        "creator": a.creator, "date": a.date, "width": a.width, "height": a.height,
        "rights": a.rights.kind.short_label(), "credit": a.credit_line(),
        "preview": a.preview,
        "sizes": rendition_labels(a).into_iter().map(|(l, d)| json!({ "label": l, "detail": d })).collect::<Vec<_>>(),
    })
}

impl Session {
    pub fn stock_dir(&self) -> std::path::PathBuf {
        self.settings.projects_dir.join(STOCK_DIR)
    }

    /// The stock library in the projects folder.
    pub fn stock(&self) -> Result<StockLibrary, EngineError> {
        Ok(StockLibrary::open(self.stock_dir())?)
    }

    /// Download `source`/`id` at the size named `size` (a rendition label), or the largest no wider
    /// than `max_width`, or (neither) at most 3000 px wide so originals of hundreds of MB aren't
    /// fetched by accident. Returns a one-line result with where it went and its credit line.
    pub(crate) fn stock_download(&self, source: &str, id: &str, size: Option<&str>, max_width: Option<u32>) -> Result<String, EngineError> {
        let pick = match (size, max_width) {
            (Some(label), _) if !label.trim().is_empty() => Pick::Label(label.trim().to_string()),
            (_, Some(w)) => Pick::FitWidth(w),
            _ => Pick::FitWidth(3000),
        };
        let lib = self.stock()?;
        let (item, path) = lib.download(source, id, pick)?;
        let rel = path.strip_prefix(&self.settings.projects_dir).unwrap_or(&path).display().to_string();
        Ok(format!("Saved {rel} ({}). Credit: {}", item.asset.rights.kind.short_label(), item.asset.credit_line()))
    }

    /// Search for agents: a JSON array of assets.
    pub fn stock_search_json(&self, text: &str, kind: Option<&str>, page: u32) -> Result<Value, EngineError> {
        let kind = kind.and_then(MediaKind::from_slug);
        let page = self.stock()?.search(text, kind, page);
        let failures: Vec<Value> = page.failures.iter().map(|f| json!({ "source": f.source, "message": f.message })).collect();
        Ok(json!({ "results": page.assets.iter().map(asset_json).collect::<Vec<_>>(), "has_more": page.has_more, "unavailable_sources": failures }))
    }
}
