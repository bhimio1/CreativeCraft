//! The icon an installed external app ships with, read from its own installation on this
//! computer, the same way a desktop launcher shows it. CreativeCraft bundles no other project's
//! icons (see docs/clean-room.md); an app that isn't installed gets a plain letter tile instead.
//!
//! - Linux/BSD: the app's `.desktop` entry names an icon, looked up in the hicolor icon theme
//!   (largest PNG, else the scalable SVG) or `pixmaps`.
//! - macOS: the largest PNG inside the bundle's `.icns`.
//! - Windows: not read yet (icons live in the executable's resources).

use creativecraft_catalog::AppSpec;
use creativecraft_platform::{Host, Os};
use std::io::Read;
use std::path::{Path, PathBuf};

/// An icon file's bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IconData {
    Png(Vec<u8>),
    Svg(Vec<u8>),
}

const MAX_ICON_BYTES: u64 = 4 * 1024 * 1024;

/// The icon of `app` as installed at `exe` (what [`crate::detect_external`] found).
pub fn installed_icon(app: &AppSpec, exe: &Path, host: Host) -> Option<IconData> {
    match host.os {
        Os::Linux | Os::FreeBsd => desktop_icon(app, exe),
        Os::MacOs => mac_icon(exe),
        Os::Windows => None,
    }
}

fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match std::env::var_os("XDG_DATA_HOME") {
        Some(d) => dirs.push(PathBuf::from(d)),
        None => dirs.extend(home.as_ref().map(|h| h.join(".local/share"))),
    }
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    // Flatpak and Snap installs export their desktop entries and icons here
    dirs.extend(home.map(|h| h.join(".local/share/flatpak/exports/share")));
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share"));
    dirs.push(PathBuf::from("/var/lib/snapd/desktop"));
    dirs
}

/// Find the `.desktop` entry that runs `exe` (or one of the app's command names) and resolve
/// its `Icon=` key.
fn desktop_icon(app: &AppSpec, exe: &Path) -> Option<IconData> {
    let exe_name = exe.file_name()?.to_string_lossy().to_string();
    let names: Vec<String> = std::iter::once(exe_name).chain(app.path_names.iter().cloned()).collect();
    let dirs = data_dirs();
    for dir in &dirs {
        let Ok(entries) = std::fs::read_dir(dir.join("applications")) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "desktop") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let (exec, icon) = desktop_keys(&text);
            let Some(exec) = exec else { continue };
            // the program is the first word of Exec, maybe with a path (and `env VAR=…` before it)
            let program = exec.split_whitespace().find(|w| w != &"env" && !w.contains('=')).unwrap_or("");
            let program = Path::new(program).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let matches = names.iter().any(|n| program == *n || program.starts_with(&format!("{n}-")));
            if matches && let Some(icon) = icon.and_then(|i| resolve_icon(&i, &dirs)) {
                return Some(icon);
            }
        }
    }
    None
}

/// `Exec` and `Icon` from the `[Desktop Entry]` group.
fn desktop_keys(text: &str) -> (Option<String>, Option<String>) {
    let (mut exec, mut icon, mut in_entry) = (None, None, false);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some(v) = line.strip_prefix("Exec=") {
            exec = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("Icon=") {
            icon = Some(v.trim().to_string());
        }
    }
    (exec, icon)
}

/// An icon name (or absolute path) → the best file for it.
fn resolve_icon(name: &str, dirs: &[PathBuf]) -> Option<IconData> {
    let p = Path::new(name);
    if p.is_absolute() {
        return read_icon(p);
    }
    const SIZES: [&str; 7] = ["512x512", "256x256", "192x192", "128x128", "96x96", "64x64", "48x48"];
    for dir in dirs {
        let theme = dir.join("icons/hicolor");
        for size in SIZES {
            if let Some(i) = read_icon(&theme.join(size).join("apps").join(format!("{name}.png"))) {
                return Some(i);
            }
        }
    }
    for dir in dirs {
        if let Some(i) = read_icon(&dir.join("icons/hicolor/scalable/apps").join(format!("{name}.svg"))) {
            return Some(i);
        }
        for ext in ["png", "svg"] {
            if let Some(i) = read_icon(&dir.join("pixmaps").join(format!("{name}.{ext}"))) {
                return Some(i);
            }
        }
    }
    None
}

fn read_limited(p: &Path) -> Option<Vec<u8>> {
    let f = std::fs::File::open(p).ok()?;
    if !f.metadata().ok()?.is_file() || f.metadata().ok()?.len() > MAX_ICON_BYTES {
        return None;
    }
    let mut buf = Vec::new();
    f.take(MAX_ICON_BYTES).read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn read_icon(p: &Path) -> Option<IconData> {
    let bytes = read_limited(p)?;
    if bytes.starts_with(b"\x89PNG") {
        Some(IconData::Png(bytes))
    } else if p.extension().is_some_and(|e| e == "svg") {
        Some(IconData::Svg(bytes))
    } else {
        None
    }
}

/// The largest PNG image inside a bundle's `.icns` (modern `.icns` store PNGs for 128 px and up).
fn mac_icon(bundle: &Path) -> Option<IconData> {
    let resources = bundle.join("Contents/Resources");
    let mut icns: Vec<PathBuf> =
        std::fs::read_dir(&resources).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "icns")).collect();
    // prefer the file named like the bundle, then the biggest
    let stem = bundle.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    icns.sort_by_key(|p| {
        (p.file_stem().map(|s| s.to_string_lossy().to_lowercase()) != Some(stem.clone()), std::cmp::Reverse(p.metadata().map(|m| m.len()).unwrap_or(0)))
    });
    icns.iter().find_map(|p| read_limited(p).and_then(|b| largest_png_in_icns(&b))).map(IconData::Png)
}

fn largest_png_in_icns(b: &[u8]) -> Option<Vec<u8>> {
    if b.len() < 8 || &b[..4] != b"icns" {
        return None;
    }
    let mut best: Option<&[u8]> = None;
    let mut i = 8;
    while i + 8 <= b.len() {
        let len = u32::from_be_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]]) as usize;
        if len < 8 || i + len > b.len() {
            break;
        }
        let data = &b[i + 8..i + len];
        if data.starts_with(b"\x89PNG") && best.is_none_or(|x| data.len() > x.len()) {
            best = Some(data);
        }
        i += len;
    }
    best.map(<[u8]>::to_vec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_entry_keys() {
        let text = "[Desktop Entry]\nName=Thing\nExec=env FOO=1 /opt/thing/bin/thing %F\nIcon=thing\n[Desktop Action new]\nExec=thing --new\nIcon=other\n";
        assert_eq!(desktop_keys(text), (Some("env FOO=1 /opt/thing/bin/thing %F".into()), Some("thing".into())));
    }

    #[test]
    fn resolves_hicolor_png_before_svg() {
        let root = std::env::temp_dir().join(format!("cc-icons-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("icons/hicolor/48x48/apps")).unwrap();
        std::fs::create_dir_all(root.join("icons/hicolor/scalable/apps")).unwrap();
        std::fs::write(root.join("icons/hicolor/48x48/apps/thing.png"), b"\x89PNGsmall").unwrap();
        std::fs::write(root.join("icons/hicolor/scalable/apps/thing.svg"), b"<svg/>").unwrap();
        assert_eq!(resolve_icon("thing", std::slice::from_ref(&root)), Some(IconData::Png(b"\x89PNGsmall".to_vec())));
        std::fs::remove_file(root.join("icons/hicolor/48x48/apps/thing.png")).unwrap();
        assert_eq!(resolve_icon("thing", std::slice::from_ref(&root)), Some(IconData::Svg(b"<svg/>".to_vec())));
        assert_eq!(resolve_icon("missing", std::slice::from_ref(&root)), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn icns_picks_largest_png() {
        let mut b = b"icns\0\0\0\0".to_vec();
        for data in [&b"\x89PNGaa"[..], &b"\x89PNGaaaa"[..], &b"notpng"[..]] {
            b.extend_from_slice(b"ic08");
            b.extend_from_slice(&((data.len() + 8) as u32).to_be_bytes());
            b.extend_from_slice(data);
        }
        assert_eq!(largest_png_in_icns(&b), Some(b"\x89PNGaaaa".to_vec()));
    }
}
