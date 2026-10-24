//! Updating CreativeCraft itself from its own GitHub releases.
//!
//! How depends on how this copy was installed ([`HubInstall`]):
//!
//! - **Flatpak bundle**: download the release's `.flatpak` for this machine and install it on the
//!   host (`flatpak install --bundle`, through `flatpak-spawn --host`). Flatpak keeps the running
//!   copy until it exits, so the update applies on the next start.
//! - **Portable folder** (the `.tar.gz` / `.zip`): unpack the new archive and swap the two
//!   executables in place (a rename, so the running program keeps its old file).
//! - **macOS app** (`CreativeCraft.app`): unpack the macOS zip and swap the whole bundle.
//! - **Windows installer** (Program Files): download the `.msi`; it runs when CreativeCraft
//!   restarts, and starts CreativeCraft again when it's done.
//! - **Package manager** (AUR, Nix), a Flatpak from another remote, or a source build: nothing is
//!   touched; the hub says how to update instead.
//!
//! Every download must match the release's published SHA-256, or nothing is changed.

use crate::{HubDirs, InstallError, Progress, github, unpack};
use creativecraft_platform::{Arch, Host, HostCommand, Os};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where downloads for a self-update wait (removed at the next start).
const STAGING: &str = "creativecraft-update";

/// How this copy of CreativeCraft was installed, which decides how it can update itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubInstall {
    /// The release's Flatpak bundle, installed per user or system-wide.
    Flatpak { app_id: String, user: bool },
    /// A folder holding `creativecraft` and `creativecraft-cli` from a release archive.
    Portable { dir: PathBuf },
    /// `CreativeCraft.app` on macOS.
    MacApp { bundle: PathBuf },
    /// Installed by the `.msi` into Program Files.
    WindowsMsi { dir: PathBuf },
    /// CreativeCraft can't update this copy; `how` tells the person what to do instead.
    Managed { how: String },
}

impl HubInstall {
    /// Look at this process: Flatpak sandbox, executable path, OS.
    pub fn detect() -> HubInstall {
        let flatpak = std::env::var("FLATPAK_ID").ok().filter(|s| !s.is_empty());
        let info = flatpak.as_ref().and_then(|_| std::fs::read_to_string("/.flatpak-info").ok());
        let exe = std::env::current_exe().ok().map(|p| p.canonicalize().unwrap_or(p));
        let program_files: Vec<PathBuf> =
            ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"].iter().filter_map(std::env::var_os).map(PathBuf::from).collect();
        classify(Host::current(), exe.as_deref(), flatpak.as_deref(), info.as_deref(), &program_files)
    }

    /// `Err(how to update instead)` when CreativeCraft can't update this copy itself.
    pub fn can_update(&self) -> Result<(), String> {
        match self {
            HubInstall::Managed { how } => Err(how.clone()),
            _ => Ok(()),
        }
    }

    /// A few words for Settings: "Flatpak", "Portable folder", …
    pub fn label(&self) -> &'static str {
        match self {
            HubInstall::Flatpak { .. } => "Flatpak",
            HubInstall::Portable { .. } => "Portable folder",
            HubInstall::MacApp { .. } => "macOS app",
            HubInstall::WindowsMsi { .. } => "Windows installer",
            HubInstall::Managed { .. } => "Managed elsewhere",
        }
    }

    /// The installer for this kind of install runs when CreativeCraft restarts (Windows .msi),
    /// rather than during the update.
    pub fn installs_on_restart(&self) -> bool {
        matches!(self, HubInstall::WindowsMsi { .. })
    }
}

/// [`HubInstall::detect`] without touching the process or the file system.
pub(crate) fn classify(host: Option<Host>, exe: Option<&Path>, flatpak_id: Option<&str>, flatpak_info: Option<&str>, program_files: &[PathBuf]) -> HubInstall {
    if let Some(id) = flatpak_id {
        // a per-user installation lives in ~/.local/share/flatpak; anything else is system-wide
        let user = flatpak_info.and_then(|i| ini_value(i, "Instance", "app-path")).is_none_or(|p| p.contains("/.local/share/flatpak/"));
        return HubInstall::Flatpak { app_id: id.to_string(), user };
    }
    let managed = |how: &str| HubInstall::Managed { how: how.to_string() };
    let (Some(host), Some(exe)) = (host, exe) else {
        return managed("CreativeCraft couldn't tell where it's installed. Download the new version from its release page.");
    };
    let Some(dir) = exe.parent() else {
        return managed("CreativeCraft couldn't tell where it's installed. Download the new version from its release page.");
    };
    // cargo run / cargo build: target/<profile>/ or target/<triple>/<profile>/
    if exe.ancestors().skip(2).take(2).any(|a| a.file_name().is_some_and(|n| n == "target")) && dir.file_name().is_some_and(|n| n == "debug" || n == "release")
    {
        return managed("This is a build from source: update it with git pull and cargo build.");
    }
    match host.os {
        Os::MacOs => match exe.ancestors().find(|a| a.extension().is_some_and(|e| e == "app")) {
            Some(bundle) => HubInstall::MacApp { bundle: bundle.to_path_buf() },
            None => HubInstall::Portable { dir: dir.to_path_buf() },
        },
        Os::Windows => {
            let lower = |p: &Path| p.to_string_lossy().to_lowercase();
            if program_files.iter().any(|pf| !pf.as_os_str().is_empty() && lower(exe).starts_with(&lower(pf))) {
                HubInstall::WindowsMsi { dir: dir.to_path_buf() }
            } else {
                HubInstall::Portable { dir: dir.to_path_buf() }
            }
        }
        _ => {
            if exe.starts_with("/nix/store") {
                managed("Installed with Nix: update it there.")
            } else if exe.starts_with("/usr") && !exe.starts_with("/usr/local") {
                managed("Installed by your package manager (on Arch, the AUR's creativecraft-bin): update it there.")
            } else {
                HubInstall::Portable { dir: dir.to_path_buf() }
            }
        }
    }
}

/// `key=` under `[section]` in an ini-style file (`/.flatpak-info`).
fn ini_value<'a>(text: &'a str, section: &str, key: &str) -> Option<&'a str> {
    let mut inside = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            inside = line == format!("[{section}]");
        } else if inside
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim());
        }
    }
    None
}

/// The end of the release asset's name this install needs (`creativecraft-<version><suffix>`).
pub(crate) fn asset_suffix(install: &HubInstall, host: Host) -> Option<String> {
    let arch = match host.arch {
        Arch::X86_64 => "x86_64",
        Arch::Aarch64 => "aarch64",
        Arch::X86 => return None,
    };
    match (install, host.os) {
        (HubInstall::Flatpak { .. }, Os::Linux) => Some(format!("-linux-{arch}.flatpak")),
        (HubInstall::Portable { .. }, Os::Linux) => Some(format!("-linux-{arch}.tar.gz")),
        (HubInstall::Portable { .. } | HubInstall::MacApp { .. }, Os::MacOs) => Some("-macos-universal.zip".into()),
        (HubInstall::Portable { .. }, Os::Windows) if host.arch == Arch::X86_64 => Some("-windows-x64.zip".into()),
        (HubInstall::WindowsMsi { .. }, Os::Windows) if host.arch == Arch::X86_64 => Some("-windows-x64.msi".into()),
        _ => None,
    }
}

/// Update this copy of CreativeCraft to the newest release of `repo`, if it's newer than
/// `current`. Returns the version installed. The running program carries on; [`restart`] starts
/// the new one (and, for the Windows installer, installs it first).
pub fn update_hub(install: &HubInstall, repo: &str, current: &str, dirs: &HubDirs, progress: &mut dyn FnMut(Progress)) -> Result<String, InstallError> {
    install.can_update().map_err(InstallError::Source)?;
    let host = Host::current().ok_or(InstallError::UnsupportedHost)?;
    progress(Progress::Resolving);
    let release = github::newest_release(repo)?;
    let version = release.tag.trim_start_matches('v').to_string();
    if !creativecraft_platform::cmp_versions(&release.tag, current).is_gt() {
        return Err(InstallError::Source(format!("CreativeCraft {current} is already the newest version")));
    }
    let no_asset = || InstallError::NoAsset { app: "CreativeCraft".into(), tag: release.tag.clone() };
    let suffix = asset_suffix(install, host).ok_or_else(no_asset)?;
    let asset = release.assets.iter().find(|a| a.name.starts_with("creativecraft-") && a.name.ends_with(&suffix)).ok_or_else(no_asset)?;

    let staging = dirs.downloads().join(STAGING);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let file = staging.join(&asset.name);
    let digest = github::download(&asset.url, &file, &mut |done, total| progress(Progress::Downloading { done, total }))?;
    progress(Progress::Verifying);
    // CreativeCraft's releases always publish SHA256SUMS: refuse anything that doesn't match it
    match github::published_sha256(&release, &asset.name)? {
        Some(expected) if expected.eq_ignore_ascii_case(&digest) => {}
        Some(_) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(InstallError::Checksum(asset.name.clone()));
        }
        None => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(InstallError::Source(format!("release {} publishes no checksum for {}, so it wasn't installed", release.tag, asset.name)));
        }
    }

    progress(Progress::Unpacking);
    let result = match install {
        HubInstall::Flatpak { app_id, user } => install_flatpak_bundle(app_id, *user, &file),
        HubInstall::Portable { dir } => swap_executables(&file, &asset.name, &staging, dir, host),
        HubInstall::MacApp { bundle } => swap_bundle(&file, &asset.name, &staging, bundle),
        // the installer runs from `restart`, once this program is out of the way
        HubInstall::WindowsMsi { .. } => return Ok(version),
        HubInstall::Managed { .. } => Ok(()),
    };
    let _ = std::fs::remove_dir_all(&staging);
    result?;
    progress(Progress::Done);
    Ok(version)
}

/// `flatpak install --bundle` on the host. Installing a newer bundle over one installed from a
/// bundle is an update; a copy from a store or other remote is left alone.
fn install_flatpak_bundle(app_id: &str, user: bool, bundle: &Path) -> Result<(), InstallError> {
    let scope = if user { "--user" } else { "--system" };
    let mut info = HostCommand::new("flatpak");
    info.args(["info", scope, "--show-origin", app_id]);
    let out = info.into_std().stdin(Stdio::null()).output()?;
    let origin = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !origin.ends_with("-origin") {
        return Err(InstallError::Source(format!(
            "this Flatpak comes from the `{origin}` remote, so update it there: flatpak update {app_id} (or your software center)"
        )));
    }
    let mut c = HostCommand::new("flatpak");
    c.args(["install", scope, "--noninteractive", "-y", "--bundle"]).arg(bundle);
    let out = c.into_std().stdin(Stdio::null()).output()?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        let why = why.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("flatpak install failed");
        return Err(InstallError::Build(format!("flatpak couldn't install the update: {why}")));
    }
    Ok(())
}

/// Replace `creativecraft` and `creativecraft-cli` in `dir` with the ones from `archive`.
fn swap_executables(archive: &Path, name: &str, staging: &Path, dir: &Path, host: Host) -> Result<(), InstallError> {
    let unpacked = staging.join("unpacked");
    unpack::unpack(archive, name, &unpacked)?;
    // find every new file before replacing any, so a broken archive changes nothing
    let mut pairs = Vec::new();
    for stem in ["creativecraft", "creativecraft-cli"] {
        let exe = host.exe_name(stem);
        let target = dir.join(&exe);
        if stem == "creativecraft-cli" && !target.exists() {
            continue;
        }
        let new = unpack::find_executable(&unpacked, &exe).ok_or_else(|| InstallError::MissingExe { app: "CreativeCraft".into(), exe: exe.clone() })?;
        pairs.push((new, target));
    }
    for (new, target) in pairs {
        replace_file(&new, &target).map_err(|e| unwritable(dir, e))?;
    }
    Ok(())
}

/// Put `new` at `target` without disturbing a running `target`: copy beside it, then rename over
/// it. Windows won't replace a running program, but lets it be renamed aside first.
fn replace_file(new: &Path, target: &Path) -> std::io::Result<()> {
    let fresh = with_suffix(target, ".new");
    std::fs::copy(new, &fresh)?;
    if cfg!(windows) && target.exists() {
        let mut old = with_suffix(target, ".old");
        if old.exists() && std::fs::remove_file(&old).is_err() {
            // still running from an earlier update: step aside under another name
            old = with_suffix(target, &format!(".{}.old", std::process::id()));
        }
        if let Err(e) = std::fs::rename(target, &old) {
            let _ = std::fs::remove_file(&fresh);
            return Err(e);
        }
    }
    std::fs::rename(&fresh, target)
}

/// Replace `CreativeCraft.app` with the one in the macOS zip.
fn swap_bundle(archive: &Path, name: &str, staging: &Path, bundle: &Path) -> Result<(), InstallError> {
    let unpacked = staging.join("unpacked");
    unpack::unpack(archive, name, &unpacked)?;
    let new = find_bundle(&unpacked).ok_or_else(|| InstallError::MissingExe { app: "CreativeCraft".into(), exe: "CreativeCraft.app".into() })?;
    let parent = bundle.parent().ok_or_else(|| InstallError::Source("CreativeCraft.app has no folder".into()))?;
    let file = bundle.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "CreativeCraft.app".into());
    let fresh = parent.join(format!(".{file}.new"));
    let old = parent.join(format!(".{file}.old"));
    let _ = std::fs::remove_dir_all(&fresh);
    let _ = std::fs::remove_dir_all(&old);
    // ditto keeps the bundle's code signature, symlinks and extended attributes
    let out = Command::new("/usr/bin/ditto").arg(&new).arg(&fresh).stdin(Stdio::null()).output().map_err(|e| unwritable(parent, e))?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&fresh);
        return Err(unwritable(parent, std::io::Error::other(String::from_utf8_lossy(&out.stderr).trim().to_string())));
    }
    std::fs::rename(bundle, &old).map_err(|e| unwritable(parent, e))?;
    if let Err(e) = std::fs::rename(&fresh, bundle) {
        let _ = std::fs::rename(&old, bundle);
        return Err(unwritable(parent, e));
    }
    // the running program keeps its open files; the old bundle goes at the next start
    Ok(())
}

fn find_bundle(root: &Path) -> Option<PathBuf> {
    let mut frontier = vec![root.to_path_buf()];
    for _ in 0..3 {
        let mut next = Vec::new();
        for dir in frontier {
            let mut entries: Vec<_> = std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
            entries.sort();
            for p in entries {
                if p.extension().is_some_and(|e| e == "app") && p.join("Contents").join("MacOS").is_dir() {
                    return Some(p);
                }
                next.push(p);
            }
        }
        frontier = next;
    }
    None
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

fn unwritable(dir: &Path, e: std::io::Error) -> InstallError {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        InstallError::Source(format!("CreativeCraft can't write to {}; update it the way you installed it", dir.display()))
    } else {
        InstallError::Io(e)
    }
}

/// Start the updated CreativeCraft (for the Windows installer: install it, then start it). The
/// caller then quits.
pub fn restart(install: &HubInstall, dirs: &HubDirs) -> Result<(), InstallError> {
    match install {
        HubInstall::Flatpak { app_id, .. } => {
            let mut c = HostCommand::new("flatpak");
            c.args(["run", app_id.as_str()]).detached();
            c.into_std().stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
        }
        HubInstall::Portable { dir } => {
            let exe = dir.join(Host::current().ok_or(InstallError::UnsupportedHost)?.exe_name("creativecraft"));
            Command::new(&exe).current_dir(dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
        }
        HubInstall::MacApp { bundle } => {
            Command::new("/usr/bin/open").arg("-n").arg(bundle).spawn()?;
        }
        HubInstall::WindowsMsi { dir } => {
            let msi = downloaded_msi(dirs).ok_or_else(|| InstallError::Source("the update isn't downloaded yet".into()))?;
            let script = msi_script(&msi, &dir.join("creativecraft.exe"));
            let mut c = Command::new("powershell.exe");
            c.args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &script]);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                c.creation_flags(CREATE_NO_WINDOW);
            }
            c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
        }
        HubInstall::Managed { how } => return Err(InstallError::Source(how.clone())),
    }
    Ok(())
}

/// The `.msi` [`update_hub`] left for [`restart`].
fn downloaded_msi(dirs: &HubDirs) -> Option<PathBuf> {
    let dir = dirs.downloads().join(STAGING);
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("msi")))
}

/// PowerShell: wait for msiexec (it asks for administrator rights), then start CreativeCraft
/// whether the install worked or was cancelled.
pub(crate) fn msi_script(msi: &Path, exe: &Path) -> String {
    let quote = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "''"));
    format!(
        "Start-Sleep -Seconds 2; Start-Process -FilePath msiexec.exe -ArgumentList @('/i', {}, '/passive', '/norestart') -Wait; Start-Process -FilePath {}",
        quote(&with_quotes(msi)),
        quote(exe)
    )
}

/// msiexec reads its own command line, so a path with spaces needs double quotes inside it.
fn with_quotes(p: &Path) -> PathBuf {
    PathBuf::from(format!("\"{}\"", p.display()))
}

/// Clear what an earlier update left behind: the executables and bundle it replaced, and its
/// downloads. Call once at start.
pub fn clean_up_after_update(install: &HubInstall, dirs: &HubDirs) {
    let staging = dirs.downloads().join(STAGING);
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    match install {
        HubInstall::Portable { dir } | HubInstall::WindowsMsi { dir } => {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for e in entries.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n.starts_with("creativecraft") && (n.ends_with(".old") || n.ends_with(".new")) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        HubInstall::MacApp { bundle } => {
            if let (Some(parent), Some(file)) = (bundle.parent(), bundle.file_name()) {
                let _ = std::fs::remove_dir_all(parent.join(format!(".{}.old", file.to_string_lossy())));
            }
        }
        HubInstall::Flatpak { .. } | HubInstall::Managed { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINUX: Host = Host { os: Os::Linux, arch: Arch::X86_64 };
    const MAC: Host = Host { os: Os::MacOs, arch: Arch::Aarch64 };
    const WIN: Host = Host { os: Os::Windows, arch: Arch::X86_64 };

    fn classify_exe(host: Host, exe: &str) -> HubInstall {
        classify(Some(host), Some(Path::new(exe)), None, None, &[PathBuf::from(r"C:\Program Files")])
    }

    #[test]
    fn flatpak_installs_by_scope() {
        let user = "[Application]\nname=org.creativecraft.CreativeCraft\n\n[Instance]\napp-path=/home/a/.local/share/flatpak/app/org.creativecraft.CreativeCraft/x86_64/master/abc/files\n";
        let system = "[Instance]\napp-path=/var/lib/flatpak/app/org.creativecraft.CreativeCraft/x86_64/master/abc/files\n";
        let app_id = "org.creativecraft.CreativeCraft".to_string();
        let id = Some(app_id.as_str());
        let exe = Some(Path::new("/app/bin/creativecraft"));
        assert_eq!(classify(Some(LINUX), exe, id, Some(user), &[]), HubInstall::Flatpak { app_id: app_id.clone(), user: true });
        assert_eq!(classify(Some(LINUX), exe, id, Some(system), &[]), HubInstall::Flatpak { app_id: app_id.clone(), user: false });
        assert_eq!(classify(Some(LINUX), exe, id, None, &[]), HubInstall::Flatpak { app_id: app_id.clone(), user: true });
    }

    #[test]
    fn package_managers_and_source_builds_update_elsewhere() {
        assert!(classify_exe(LINUX, "/usr/bin/creativecraft").can_update().unwrap_err().contains("AUR"));
        assert!(classify_exe(LINUX, "/nix/store/abc-creativecraft/bin/creativecraft").can_update().is_err());
        assert!(classify_exe(LINUX, "/home/a/CreativeCraft/target/debug/creativecraft").can_update().unwrap_err().contains("cargo"));
        assert!(classify_exe(LINUX, "/home/a/cc/target/x86_64-unknown-linux-gnu/release/creativecraft").can_update().is_err());
    }

    #[test]
    fn portable_folders_and_bundles() {
        assert_eq!(
            classify_exe(LINUX, "/home/a/Apps/creativecraft-0.3.0/creativecraft"),
            HubInstall::Portable { dir: "/home/a/Apps/creativecraft-0.3.0".into() }
        );
        assert_eq!(classify_exe(LINUX, "/usr/local/bin/creativecraft"), HubInstall::Portable { dir: "/usr/local/bin".into() });
        assert_eq!(classify_exe(LINUX, "/opt/release/creativecraft"), HubInstall::Portable { dir: "/opt/release".into() });
        assert_eq!(
            classify_exe(MAC, "/Applications/CreativeCraft.app/Contents/MacOS/creativecraft-cli"),
            HubInstall::MacApp { bundle: "/Applications/CreativeCraft.app".into() }
        );
        assert_eq!(classify_exe(MAC, "/Users/a/cc/creativecraft-cli"), HubInstall::Portable { dir: "/Users/a/cc".into() });
    }

    #[test]
    fn windows_program_files_means_the_msi() {
        let exe = Path::new(r"C:\Program Files\CreativeCraft\creativecraft.exe");
        let pf = [PathBuf::from(r"C:\Program Files")];
        assert!(matches!(classify(Some(WIN), Some(exe), None, None, &pf), HubInstall::WindowsMsi { .. }));
        let exe = Path::new(r"D:\Tools\creativecraft\creativecraft.exe");
        assert!(matches!(classify(Some(WIN), Some(exe), None, None, &pf), HubInstall::Portable { .. }));
    }

    #[test]
    fn each_install_takes_its_own_release_file() {
        let fp = HubInstall::Flatpak { app_id: "x".into(), user: true };
        let dir = HubInstall::Portable { dir: "/x".into() };
        assert_eq!(asset_suffix(&fp, LINUX).as_deref(), Some("-linux-x86_64.flatpak"));
        assert_eq!(asset_suffix(&fp, Host { os: Os::Linux, arch: Arch::Aarch64 }).as_deref(), Some("-linux-aarch64.flatpak"));
        assert_eq!(asset_suffix(&dir, LINUX).as_deref(), Some("-linux-x86_64.tar.gz"));
        assert_eq!(asset_suffix(&dir, MAC).as_deref(), Some("-macos-universal.zip"));
        assert_eq!(asset_suffix(&HubInstall::MacApp { bundle: "/A.app".into() }, MAC).as_deref(), Some("-macos-universal.zip"));
        assert_eq!(asset_suffix(&dir, WIN).as_deref(), Some("-windows-x64.zip"));
        assert_eq!(asset_suffix(&HubInstall::WindowsMsi { dir: "C:/x".into() }, WIN).as_deref(), Some("-windows-x64.msi"));
        assert_eq!(asset_suffix(&dir, Host { os: Os::Windows, arch: Arch::Aarch64 }), None);
        assert_eq!(asset_suffix(&HubInstall::Managed { how: String::new() }, LINUX), None);
    }

    #[test]
    fn msi_script_quotes_paths() {
        let s = msi_script(Path::new(r"C:\Users\O'Neil\AppData\x.msi"), Path::new(r"C:\Program Files\CreativeCraft\creativecraft.exe"));
        assert!(s.contains(r#"'"C:\Users\O''Neil\AppData\x.msi"'"#), "{s}");
        assert!(s.contains(r"Start-Process -FilePath 'C:\Program Files\CreativeCraft\creativecraft.exe'"), "{s}");
        assert!(s.contains("/passive"));
    }

    #[test]
    fn replacing_an_executable_keeps_a_working_file() {
        let dir = std::env::temp_dir().join(format!("cc-selfupdate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("new")).unwrap();
        std::fs::write(dir.join("creativecraft"), b"old").unwrap();
        std::fs::write(dir.join("new").join("creativecraft"), b"new").unwrap();
        replace_file(&dir.join("new").join("creativecraft"), &dir.join("creativecraft")).unwrap();
        assert_eq!(std::fs::read(dir.join("creativecraft")).unwrap(), b"new");
        assert!(!dir.join("creativecraft.new").exists());
        let dirs = HubDirs::under(&dir.join("home"));
        clean_up_after_update(&HubInstall::Portable { dir: dir.clone() }, &dirs);
        assert!(!dir.join("creativecraft.old").exists());
        assert!(dir.join("creativecraft").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
