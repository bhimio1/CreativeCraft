//! Recent projects: the files in the projects folder that some app in the catalog opens, newest
//! first, and a small preview for each where the file carries one.
//!
//! Previews come only from the file itself, never from running an app:
//! - image files are their own preview;
//! - ZIP-based documents are searched for an embedded preview (`thumb.png` in PhotoCraft's
//!   `.pcraft`, `Thumbnails/thumbnail.png` in OpenDocument and OpenRaster, `docProps/thumbnail.*`
//!   in Office files, `preview.png` in Krita's `.kra`), as are directory bundles;
//! - Blender `.blend` files carry an RGBA thumbnail block;
//! - plain-text formats (Markdown, CSV, …) show their first lines.
//!
//! Everything is read with size limits, so a large or hostile file can't stall or exhaust the hub.

use creativecraft_catalog::Catalog;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A file the Projects page lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFile {
    pub path: PathBuf,
    /// Lowercase extension without the dot.
    pub ext: String,
    pub modified: SystemTime,
    pub size: u64,
    /// The catalog app that opens it (its own format first), if any.
    pub app: Option<String>,
}

/// A preview, ready for the UI to decode or upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Thumbnail {
    /// A PNG/JPEG/WebP/GIF/BMP file's bytes.
    Encoded(Vec<u8>),
    /// Straight RGBA pixels, top row first.
    Rgba { width: u32, height: u32, pixels: Vec<u8> },
    /// The first lines of a text document.
    Text(String),
    /// An SVG document, for the UI to rasterise.
    Svg(Vec<u8>),
}

/// Folders CreativeCraft or agents keep for themselves; never listed.
const SKIP_DIRS: &[&str] = &[".creativecraft", ".agents", ".git", "node_modules", "target"];
/// How deep to look below the projects folder, and how many files to look at at most.
const MAX_DEPTH: usize = 6;
const MAX_VISITED: usize = 20_000;
/// Largest image or embedded preview read for a thumbnail.
const MAX_THUMB_BYTES: u64 = 24 * 1024 * 1024;
/// Largest `.blend` scanned for its thumbnail block.
const MAX_BLEND_BYTES: u64 = 512 * 1024 * 1024;

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];
const TEXT_EXTS: &[&str] = &["md", "txt", "csv", "tsv"];
const ZIP_PREVIEWS: &[&str] = &[
    "thumb.png",
    "composite/preview.png",
    "Thumbnails/thumbnail.png",
    "docProps/thumbnail.png",
    "docProps/thumbnail.jpeg",
    "docProps/thumbnail.jpg",
    "preview.png",
    "thumbnail.png",
    "mergedimage.png",
];

/// The newest `limit` files under `root` that an app in `catalog` opens.
pub fn scan(root: &Path, catalog: &Catalog, limit: usize) -> Vec<ProjectFile> {
    let mut found = Vec::new();
    let mut visited = 0;
    walk(root, 0, catalog, &mut found, &mut visited);
    found.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.path.cmp(&b.path)));
    found.truncate(limit);
    found
}

fn walk(dir: &Path, depth: usize, catalog: &Catalog, out: &mut Vec<ProjectFile>, visited: &mut usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        *visited += 1;
        if *visited > MAX_VISITED {
            return;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // hidden files, conflict/temporary files and our own folders stay out of the list
        if name.starts_with('.') || name.ends_with(".part") || name.ends_with('~') || SKIP_DIRS.contains(&name.as_ref()) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let app = catalog.apps_for_extension(&ext).first().map(|a| a.id.clone());
        if meta.is_dir() {
            // a directory bundle (e.g. a `.pcraft` saved as a folder) is one project
            if app.is_some() {
                out.push(ProjectFile { path, ext, modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), size: 0, app });
            } else if depth < MAX_DEPTH {
                walk(&path, depth + 1, catalog, out, visited);
            }
        } else if meta.is_file() && app.is_some() {
            out.push(ProjectFile { path, ext, modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), size: meta.len(), app });
        }
    }
}

/// The best preview `path` carries, if any.
pub fn thumbnail(path: &Path) -> Option<Thumbnail> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if path.is_dir() {
        return ZIP_PREVIEWS.iter().map(|p| path.join(p)).find_map(|p| read_limited(&p).map(Thumbnail::Encoded));
    }
    if IMAGE_EXTS.contains(&ext.as_str()) {
        return read_limited(path).map(Thumbnail::Encoded);
    }
    if TEXT_EXTS.contains(&ext.as_str()) {
        return text_preview(path).map(Thumbnail::Text);
    }
    if ext == "svg" {
        return read_limited(path).map(Thumbnail::Svg);
    }
    if ext == "blend" {
        return blend_thumbnail(path);
    }
    zip_preview(path).map(Thumbnail::Encoded)
}

fn read_limited(path: &Path) -> Option<Vec<u8>> {
    let f = std::fs::File::open(path).ok()?;
    if f.metadata().ok()?.len() > MAX_THUMB_BYTES {
        return None;
    }
    let mut buf = Vec::new();
    f.take(MAX_THUMB_BYTES).read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn text_preview(path: &Path) -> Option<String> {
    let mut buf = vec![0u8; 2048];
    let n = std::fs::File::open(path).ok()?.read(&mut buf).ok()?;
    buf.truncate(n);
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text.lines().take(14).collect();
    let s = lines.join("\n");
    (!s.trim().is_empty()).then_some(s)
}

fn zip_preview(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    // cheap check before parsing: ZIP files start with "PK"
    let mut magic = [0u8; 2];
    (&file).read_exact(&mut magic).ok()?;
    if &magic != b"PK" {
        return None;
    }
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(std::fs::File::open(path).ok()?)).ok()?;
    for name in ZIP_PREVIEWS {
        let Ok(entry) = zip.by_name(name) else { continue };
        if entry.size() > MAX_THUMB_BYTES {
            continue;
        }
        let mut buf = Vec::new();
        if entry.take(MAX_THUMB_BYTES).read_to_end(&mut buf).is_ok() && !buf.is_empty() {
            return Some(buf);
        }
    }
    None
}

/// The `TEST` block of an uncompressed `.blend`: width, height, then RGBA rows bottom-up.
/// (Compressed `.blend` files are skipped and get the app icon instead.)
fn blend_thumbnail(path: &Path) -> Option<Thumbnail> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_BLEND_BYTES {
        return None;
    }
    let mut r = std::io::BufReader::new(file);
    let mut header = [0u8; 12];
    r.read_exact(&mut header).ok()?;
    // the classic 12-byte header only ("BLENDER" + pointer size + endianness + version)
    if &header[..7] != b"BLENDER" || !matches!(header[7], b'-' | b'_') || !matches!(header[8], b'v' | b'V') {
        return None;
    }
    let ptr = if header[7] == b'-' { 8 } else { 4 };
    let le = header[8] == b'v';
    let u32_at = |b: &[u8]| -> u32 {
        let a = [b[0], b[1], b[2], b[3]];
        if le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) }
    };
    let mut block = vec![0u8; 16 + ptr];
    // the thumbnail block comes before the scene data, so only the first blocks are looked at
    for _ in 0..64 {
        r.read_exact(&mut block[..8 + ptr + 8]).ok()?;
        let code = &block[..4];
        let size = u32_at(&block[4..8]) as usize;
        if code == b"ENDB" {
            return None;
        }
        if code == b"TEST" {
            let mut data = vec![0u8; size];
            r.read_exact(&mut data).ok()?;
            let (w, h) = (u32_at(&data[0..4]), u32_at(&data[4..8]));
            let len = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
            if w == 0 || h == 0 || w > 1024 || h > 1024 || data.len() < 8 + len {
                return None;
            }
            let src = &data[8..8 + len];
            let row = w as usize * 4;
            let pixels: Vec<u8> = src.chunks_exact(row).rev().flatten().copied().collect();
            return Some(Thumbnail::Rgba { width: w, height: h, pixels });
        }
        std::io::copy(&mut (&mut r).take(size as u64), &mut std::io::sink()).ok()?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cc-projects-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn scan_lists_known_files_newest_first_and_skips_ours() {
        let root = tmp("scan");
        let cat = Catalog::builtin().unwrap();
        std::fs::write(root.join("old.pcraft"), b"x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/new.fcproj"), b"{}").unwrap();
        std::fs::write(root.join("notes.unknownext"), b"x").unwrap();
        std::fs::create_dir_all(root.join(".creativecraft/trash")).unwrap();
        std::fs::write(root.join(".creativecraft/trash/gone.pcraft"), b"x").unwrap();
        std::fs::write(root.join("half.pcraft.part"), b"x").unwrap();
        let found = scan(&root, &cat, 10);
        let names: Vec<_> = found.iter().map(|f| f.path.file_name().unwrap().to_string_lossy().to_string()).collect();
        assert_eq!(names, ["new.fcproj", "old.pcraft"]);
        assert_eq!(found[0].app.as_deref(), Some("filmcraft"));
        assert_eq!(found[1].app.as_deref(), Some("photocraft"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bundles_are_one_project() {
        let root = tmp("bundle");
        std::fs::create_dir_all(root.join("poster.pcraft/tiles")).unwrap();
        std::fs::write(root.join("poster.pcraft/thumb.png"), b"\x89PNGfake").unwrap();
        let found = scan(&root, &Catalog::builtin().unwrap(), 10);
        assert_eq!(found.len(), 1);
        assert_eq!(thumbnail(&found[0].path), Some(Thumbnail::Encoded(b"\x89PNGfake".to_vec())));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn zip_documents_give_their_embedded_preview() {
        let root = tmp("zip");
        let path = root.join("doc.pcraft");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        z.start_file("manifest.json", opts).unwrap();
        z.write_all(b"{}").unwrap();
        z.start_file("thumb.png", opts).unwrap();
        z.write_all(b"PNGDATA").unwrap();
        z.finish().unwrap();
        assert_eq!(thumbnail(&path), Some(Thumbnail::Encoded(b"PNGDATA".to_vec())));
        // not a zip: no preview, no error
        std::fs::write(root.join("plain.fcproj"), b"{\"version\":1}").unwrap();
        assert_eq!(thumbnail(&root.join("plain.fcproj")), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn blend_thumbnail_is_flipped_upright() {
        let root = tmp("blend");
        let mut b = b"BLENDER-v400".to_vec();
        // a REND block to skip, then TEST: 1x2 image, bottom row red, top row blue
        b.extend_from_slice(b"REND");
        b.extend_from_slice(&4u32.to_le_bytes());
        b.extend_from_slice(&[0u8; 8 + 8]);
        b.extend_from_slice(&[1, 2, 3, 4]);
        let pixels = [255u8, 0, 0, 255, 0, 0, 255, 255];
        b.extend_from_slice(b"TEST");
        b.extend_from_slice(&((8 + pixels.len()) as u32).to_le_bytes());
        b.extend_from_slice(&[0u8; 8 + 8]);
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&pixels);
        std::fs::write(root.join("scene.blend"), &b).unwrap();
        assert_eq!(thumbnail(&root.join("scene.blend")), Some(Thumbnail::Rgba { width: 1, height: 2, pixels: vec![0, 0, 255, 255, 255, 0, 0, 255] }));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn text_files_preview_their_first_lines() {
        let root = tmp("text");
        std::fs::write(root.join("plan.md"), "# Plan\n\n- one\n- two\n").unwrap();
        assert_eq!(thumbnail(&root.join("plan.md")), Some(Thumbnail::Text("# Plan\n\n- one\n- two".into())));
        let _ = std::fs::remove_dir_all(&root);
    }
}
