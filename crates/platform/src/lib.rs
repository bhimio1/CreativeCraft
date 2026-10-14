//! Host platform detection and release-asset matching. **std only** (L0).
//!
//! The Craft apps publish per-platform archives on their GitHub releases, named like
//! `photocraft-0.5.0-linux-x86_64.tar.gz`. Spelling varies a little between projects and over time
//! (`x64` / `x86_64` / `amd64`, `macos` / `darwin`, `universal`), so instead of hard-coding names we
//! score every asset against the host and pick the best *portable archive*. Installer formats
//! (`.msi`, `.dmg`, `.deb`, `.rpm`) are deliberately not picked: the hub installs per-user, without
//! admin rights, into its own folder, so a plain archive is what it wants.
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod host;
pub use host::{HostCommand, find_on_path, in_flatpak, stop};

/// Operating systems the hub runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Os {
    Linux,
    MacOs,
    Windows,
    FreeBsd,
}

/// CPU architectures the hub runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arch {
    X86_64,
    Aarch64,
    X86,
}

/// The machine the hub is running on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Host {
    pub os: Os,
    pub arch: Arch,
}

impl Host {
    /// The host this binary was compiled for.
    pub fn current() -> Option<Host> {
        let os = match std::env::consts::OS {
            "linux" => Os::Linux,
            "macos" => Os::MacOs,
            "windows" => Os::Windows,
            "freebsd" => Os::FreeBsd,
            _ => return None,
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => Arch::X86_64,
            "aarch64" => Arch::Aarch64,
            "x86" => Arch::X86,
            _ => return None,
        };
        Some(Host { os, arch })
    }

    /// File name of an executable on this OS (`name.exe` on Windows).
    pub fn exe_name(&self, stem: &str) -> String {
        if self.os == Os::Windows { format!("{stem}.exe") } else { stem.to_string() }
    }
}

/// Archive formats the installer can unpack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    TarGz,
    Zip,
}

impl ArchiveKind {
    pub fn of(name: &str) -> Option<ArchiveKind> {
        let n = name.to_ascii_lowercase();
        if n.ends_with(".tar.gz") || n.ends_with(".tgz") {
            Some(ArchiveKind::TarGz)
        } else if n.ends_with(".zip") {
            Some(ArchiveKind::Zip)
        } else {
            None
        }
    }
}

fn os_aliases(os: Os) -> &'static [&'static str] {
    match os {
        Os::Linux => &["linux"],
        Os::MacOs => &["macos", "darwin", "mac", "osx", "apple"],
        Os::Windows => &["windows", "win64", "win32", "win"],
        Os::FreeBsd => &["freebsd"],
    }
}

fn arch_aliases(arch: Arch) -> &'static [&'static str] {
    match arch {
        Arch::X86_64 => &["x86_64", "x64", "amd64", "x86-64"],
        Arch::Aarch64 => &["aarch64", "arm64"],
        Arch::X86 => &["x86", "i686", "i386", "win32"],
    }
}

/// Split an asset name into lowercase tokens on `-`, `_` and `.`, but keep `x86_64` / `x86-64`
/// together (otherwise `x86` would match an x86_64 asset).
fn tokens(name: &str) -> Vec<String> {
    let lower = name.to_ascii_lowercase().replace("x86_64", "x86~64").replace("x86-64", "x86~64");
    lower.split(['-', '_', '.']).filter(|t| !t.is_empty()).map(|t| t.replace('~', "_")).collect()
}

/// How well `asset` fits `host`; `None` if it can't be used at all.
pub fn score_asset(asset: &str, host: Host) -> Option<i32> {
    let kind = ArchiveKind::of(asset)?;
    let toks = tokens(asset);
    let has = |aliases: &[&str]| toks.iter().any(|t| aliases.contains(&t.as_str()));

    // the asset must name our OS, and must not name another one
    if !has(os_aliases(host.os)) {
        return None;
    }
    for other in [Os::Linux, Os::MacOs, Os::Windows, Os::FreeBsd] {
        // "win32" is both an OS alias and an arch alias, so only reject on unambiguous names
        if other != host.os && has(&os_aliases(other)[..1]) {
            return None;
        }
    }
    // never pick source archives, debug symbols or checksums
    if has(&["src", "source", "debug", "dbg", "symbols", "pdb", "sha256", "sha256sum", "sig"]) {
        return None;
    }

    let mut score = 0;
    if has(arch_aliases(host.arch)) {
        score += 100;
    } else if host.os == Os::MacOs && has(&["universal", "universal2"]) {
        score += 90;
    } else {
        // names another arch → unusable; names none → maybe a single-arch project
        let others = [Arch::X86_64, Arch::Aarch64, Arch::X86].into_iter().filter(|a| *a != host.arch);
        for a in others {
            if has(arch_aliases(a)) {
                return None;
            }
        }
        score += 10;
    }
    // the platform's natural archive wins ties
    score += match (host.os, kind) {
        (Os::Windows, ArchiveKind::Zip) | (Os::MacOs, ArchiveKind::Zip) => 5,
        (Os::Linux | Os::FreeBsd, ArchiveKind::TarGz) => 5,
        _ => 0,
    };
    // the CLI-only archive (when a project ships one) is less useful than the full app
    if has(&["cli"]) {
        score -= 20;
    }
    // nor is a separate headless server build (FigCraft ships figcraft-server-<v>-<os>-<arch>)
    if has(&["server", "headless"]) {
        score -= 30;
    }
    // web (wasm) bundles are not desktop apps
    if has(&["web", "wasm"]) {
        return None;
    }
    Some(score)
}

/// The best asset for `host` among `assets`, if any.
pub fn pick_asset<'a>(assets: &[&'a str], host: Host) -> Option<&'a str> {
    assets.iter().filter_map(|a| score_asset(a, host).map(|s| (s, *a))).max_by_key(|(s, _)| *s).map(|(_, a)| a)
}

/// Compare two version strings like `v0.5.0` / `0.10.1-alpha.2` numerically, part by part.
/// A pre-release sorts below the same release.
pub fn cmp_versions(a: &str, b: &str) -> std::cmp::Ordering {
    fn split(v: &str) -> (Vec<u64>, Option<&str>) {
        let v = v.trim().trim_start_matches(['v', 'V']);
        let (core, pre) = match v.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (v, None),
        };
        (core.split('.').map(|p| p.parse().unwrap_or(0)).collect(), pre)
    }
    let (ca, pa) = split(a);
    let (cb, pb) = split(b);
    let n = ca.len().max(cb.len());
    for i in 0..n {
        let (x, y) = (ca.get(i).copied().unwrap_or(0), cb.get(i).copied().unwrap_or(0));
        if x != y {
            return x.cmp(&y);
        }
    }
    match (pa, pb) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(x), Some(y)) => x.cmp(y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    const LINUX: Host = Host { os: Os::Linux, arch: Arch::X86_64 };
    const LINUX_ARM: Host = Host { os: Os::Linux, arch: Arch::Aarch64 };
    const MAC_ARM: Host = Host { os: Os::MacOs, arch: Arch::Aarch64 };
    const WIN: Host = Host { os: Os::Windows, arch: Arch::X86_64 };
    const WIN32: Host = Host { os: Os::Windows, arch: Arch::X86 };

    const ASSETS: &[&str] = &[
        "photocraft-0.5.0-linux-x86_64.tar.gz",
        "photocraft-0.5.0-linux-aarch64.tar.gz",
        "photocraft-0.5.0-linux-x86_64.AppImage",
        "photocraft_0.5.0_amd64.deb",
        "photocraft-0.5.0-macos-universal.zip",
        "photocraft-0.5.0-macos-universal.dmg",
        "photocraft-0.5.0-windows-x64.zip",
        "photocraft-0.5.0-windows-x86.zip",
        "photocraft-0.5.0-windows-arm64.msi",
        "photocraft-0.5.0-freebsd-x86_64.tar.gz",
        "photocraft-0.5.0-web.zip",
        "SHA256SUMS",
    ];

    #[test]
    fn picks_the_right_archive_per_host() {
        assert_eq!(pick_asset(ASSETS, LINUX), Some("photocraft-0.5.0-linux-x86_64.tar.gz"));
        assert_eq!(pick_asset(ASSETS, LINUX_ARM), Some("photocraft-0.5.0-linux-aarch64.tar.gz"));
        assert_eq!(pick_asset(ASSETS, MAC_ARM), Some("photocraft-0.5.0-macos-universal.zip"));
        assert_eq!(pick_asset(ASSETS, WIN), Some("photocraft-0.5.0-windows-x64.zip"));
        assert_eq!(pick_asset(ASSETS, WIN32), Some("photocraft-0.5.0-windows-x86.zip"));
    }

    #[test]
    fn x86_does_not_match_x86_64() {
        assert_eq!(score_asset("app-1.0-windows-x86_64.zip", WIN32), None);
    }

    #[test]
    fn prefers_full_app_over_cli_archive() {
        let a = ["app-cli-1.0-linux-x86_64.tar.gz", "app-1.0-linux-x86_64.tar.gz"];
        assert_eq!(pick_asset(&a, LINUX), Some("app-1.0-linux-x86_64.tar.gz"));
    }

    /// FigCraft's release: the app, its CLI, a headless server and installers, side by side. The
    /// server must never win over the app, whichever order the release lists them in.
    #[test]
    fn prefers_the_app_over_a_server_build() {
        let mut a = vec![
            "figcraft-server-0.5.1-linux-x86_64.tar.gz",
            "figcraft-0.5.1-linux-x86_64.tar.gz",
            "figcraft-0.5.1-linux-x86_64.AppImage",
            "figcraft-0.5.1-windows-x64-portable.zip",
            "figcraft-0.5.1-windows-x64.msi",
            "figcraft-0.5.1-freebsd-x86_64.tar.gz",
        ];
        for _ in 0..2 {
            assert_eq!(pick_asset(&a, LINUX), Some("figcraft-0.5.1-linux-x86_64.tar.gz"));
            assert_eq!(pick_asset(&a, WIN), Some("figcraft-0.5.1-windows-x64-portable.zip"));
            a.reverse();
        }
    }

    #[test]
    fn nothing_usable() {
        assert_eq!(pick_asset(&["app.dmg", "app.msi", "app-linux-x86_64.AppImage"], LINUX), None);
    }

    #[test]
    fn versions() {
        assert_eq!(cmp_versions("v0.10.0", "0.9.9"), Ordering::Greater);
        assert_eq!(cmp_versions("0.5.0", "v0.5.0"), Ordering::Equal);
        assert_eq!(cmp_versions("0.5.0-alpha.1", "0.5.0"), Ordering::Less);
        assert_eq!(cmp_versions("1.2", "1.2.1"), Ordering::Less);
    }
}
