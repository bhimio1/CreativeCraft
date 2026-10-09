//! Detect apps the user installed themselves (kind = "external").

use creativecraft_catalog::AppSpec;
use creativecraft_platform::{Host, Os};
use std::path::{Path, PathBuf};

/// Where an external app lives, if it is installed: an executable on `PATH`, a macOS `.app`
/// bundle, or an executable under a Windows Program Files folder.
pub fn detect_external(app: &AppSpec, host: Host) -> Option<PathBuf> {
    for name in &app.path_names {
        if let Some(p) = on_path(&host.exe_name(name)) {
            return Some(p);
        }
    }
    match host.os {
        Os::MacOs => {
            let bundle = app.mac_bundle.as_deref()?;
            let home_apps = std::env::var_os("HOME").map(|h| Path::new(&h).join("Applications"));
            [Some(PathBuf::from("/Applications")), home_apps].into_iter().flatten().map(|d| d.join(bundle)).find(|p| p.is_dir())
        }
        Os::Windows => {
            let roots = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"].iter().filter_map(std::env::var_os).map(PathBuf::from);
            for root in roots {
                for d in &app.windows_dirs {
                    let base = root.join(d);
                    if !base.is_dir() {
                        continue;
                    }
                    for name in &app.path_names {
                        let exe = host.exe_name(name);
                        if let Some(p) = crate::unpack::find_executable(&base, &exe) {
                            return Some(p);
                        }
                    }
                }
            }
            None
        }
        Os::Linux | Os::FreeBsd => flatpak_export(app),
    }
}

fn on_path(exe: &str) -> Option<PathBuf> {
    if creativecraft_platform::in_flatpak() {
        // the sandbox's PATH holds the runtime's tools, not the user's apps
        return creativecraft_platform::find_on_path(exe);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(exe)).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// Where Flatpak exports installed apps: per-user, then system-wide.
fn flatpak_exports() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(Path::new(&home).join(".local/share/flatpak/exports"));
    }
    roots.push(PathBuf::from("/var/lib/flatpak/exports"));
    roots
}

/// A Flatpak install of `app`: its Flathub id under `exports/bin`, or an exported desktop entry
/// that runs one of its commands (`flatpak run … --command=blender <id>`). The result is the
/// `exports/bin/<id>` launcher, which runs the app on the host.
fn flatpak_export(app: &AppSpec) -> Option<PathBuf> {
    let exists = |p: &Path| creativecraft_platform::find_on_path(&p.display().to_string()).is_some();
    let roots = flatpak_exports();
    if let Some(id) = &app.flatpak
        && let Some(bin) = roots.iter().map(|r| r.join("bin").join(id)).find(|b| exists(b))
    {
        return Some(bin);
    }
    for root in &roots {
        for (desktop, exec) in desktop_execs(&root.join("share/applications")) {
            let Some(id) = desktop.file_stem().map(|s| s.to_string_lossy().to_string()) else { continue };
            if exec_runs_command(&exec, &app.path_names) {
                let bin = root.join("bin").join(&id);
                if exists(&bin) {
                    return Some(bin);
                }
            }
        }
    }
    None
}

/// Does a Flatpak `Exec=` line run one of `names` (`--command=<name>`)?
fn exec_runs_command(exec: &str, names: &[String]) -> bool {
    exec.split_whitespace().filter_map(|w| w.strip_prefix("--command=")).any(|c| names.iter().any(|n| n == c))
}

/// `(desktop file, Exec value)` for every `.desktop` file in `dir`. Inside Flatpak, a folder
/// outside the home folder is read on the host (the sandbox can't see `/var/lib/flatpak`).
fn desktop_execs(dir: &Path) -> Vec<(PathBuf, String)> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let sandboxed = creativecraft_platform::in_flatpak() && !home.is_some_and(|h| dir.starts_with(h));
    if sandboxed {
        let mut c = creativecraft_platform::HostCommand::new("sh");
        c.args(["-c", r#"grep -H "^Exec=" "$1"/*.desktop 2>/dev/null"#, "sh"]).arg(dir);
        let Ok(out) = c.into_std().output() else { return Vec::new() };
        return String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.split_once(":Exec=").map(|(f, e)| (PathBuf::from(f), e.to_string()))).collect();
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "desktop"))
        .filter_map(|p| {
            let text = std::fs::read_to_string(&p).ok()?;
            let exec = text.lines().find_map(|l| l.strip_prefix("Exec="))?.to_string();
            Some((p, exec))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatpak_exec_lines() {
        let names = vec!["blender".to_string()];
        assert!(exec_runs_command(
            "/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=blender --file-forwarding org.blender.Blender @@ %f @@",
            &names
        ));
        assert!(!exec_runs_command("/usr/bin/flatpak run --command=entrypoint com.example.App", &names));
        assert!(!exec_runs_command("blender %f", &names));
    }

    #[test]
    fn desktop_entries_are_read() {
        let dir = std::env::temp_dir().join(format!("cc-flatpak-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("org.blender.Blender.desktop"), "[Desktop Entry]\nExec=/usr/bin/flatpak run --command=blender org.blender.Blender\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "Exec=nope").unwrap();
        if !creativecraft_platform::in_flatpak() {
            let found = desktop_execs(&dir);
            assert_eq!(found.len(), 1);
            assert!(found[0].0.ends_with("org.blender.Blender.desktop"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
