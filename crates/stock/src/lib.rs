//! Public-domain stock media for CreativeCraft (L2), through StockCraft's embeddable library
//! (`stockcraft-local`): photos, artworks, illustrations, videos and audio from open collections
//! (Openverse, the Art Institute of Chicago, the Cleveland Museum of Art, the Met, NASA), CC0,
//! public domain or US government work only.
//!
//! [`StockLibrary`] wraps the async library in a blocking API with its own small runtime, so the
//! engine and the window can call it from worker threads like everything else. Downloads go into
//! one folder (CreativeCraft uses `<projects>/Stock`), with an offline catalog of what was saved,
//! its rights and a credit line.
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub use stockcraft_local::{Asset, LocalFile, LocalItem, LocalQuery, MediaKind, Pick, Rendition, RightsKind, SearchPage, SearchQuery};

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum StockError {
    #[error("{0}")]
    Stock(#[from] stockcraft_local::LocalError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

pub struct StockLibrary {
    stock: stockcraft_local::Stock,
    rt: tokio::runtime::Runtime,
}

impl StockLibrary {
    /// The library in `root`, created if needed.
    pub fn open(root: impl Into<PathBuf>) -> Result<StockLibrary, StockError> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let stock = stockcraft_local::Stock::open(root)?;
        Ok(StockLibrary { stock, rt })
    }

    pub fn root(&self) -> &Path {
        self.stock.root()
    }

    /// One page of results from every source. Sources that fail are listed in `failures`.
    pub fn search(&self, text: &str, kind: Option<MediaKind>, page: u32) -> SearchPage {
        let mut q = SearchQuery::new(text);
        q.kind = kind;
        q.page = page.max(1);
        self.rt.block_on(self.stock.search(&q))
    }

    pub fn get(&self, source: &str, id: &str) -> Result<Asset, StockError> {
        Ok(self.rt.block_on(self.stock.get(source, id))?)
    }

    pub fn similar(&self, asset: &Asset, limit: usize) -> Vec<Asset> {
        self.rt.block_on(self.stock.similar(asset, limit))
    }

    /// Download one rendition; returns the catalog entry and the file just saved.
    pub fn download(&self, source: &str, id: &str, pick: Pick) -> Result<(LocalItem, PathBuf), StockError> {
        let item = self.rt.block_on(self.stock.download(source, id, pick))?;
        // files are newest first
        let path = item.files.first().and_then(|f| self.stock.store().path(&item, f)).ok_or(stockcraft_local::LocalError::NotFound)?;
        Ok((item, path))
    }

    /// What has been downloaded, matching `text` ("" for everything), with no network.
    pub fn local(&self, text: &str) -> Vec<LocalItem> {
        let mut items = self.stock.store().search(&LocalQuery::new(text));
        items.sort_by_key(|i| std::cmp::Reverse(i.added_at));
        items
    }

    /// Where a downloaded file is.
    pub fn path(&self, item: &LocalItem, file: &LocalFile) -> Option<PathBuf> {
        self.stock.store().path(item, file)
    }
}

/// The sizes worth offering for an asset, as `(label, description)`, e.g. ("Large", "2048 × 1365 · JPEG · 1.2 MB").
pub fn rendition_labels(asset: &Asset) -> Vec<(String, String)> {
    asset
        .renditions
        .iter()
        .map(|r| {
            let mut parts = Vec::new();
            if let (Some(w), Some(h)) = (r.width, r.height) {
                parts.push(format!("{w} × {h}"));
            }
            if let Some(f) = &r.format {
                parts.push(f.to_uppercase());
            }
            if let Some(b) = r.bytes {
                parts.push(if b >= 1_000_000 { format!("{:.1} MB", b as f64 / 1e6) } else { format!("{} KB", (b / 1000).max(1)) });
            }
            (r.label.clone(), parts.join(" · "))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_library_opens_empty() {
        let dir = std::env::temp_dir().join(format!("cc-stock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lib = StockLibrary::open(&dir).unwrap();
        assert!(lib.local("").is_empty());
        assert_eq!(lib.root(), dir.as_path());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
