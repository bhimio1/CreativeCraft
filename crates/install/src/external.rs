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
            let roots = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"].iter().filter_map(|v| std::env::var_os(v)).map(PathBuf::from);
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
        Os::Linux | Os::FreeBsd => None,
    }
}

fn on_path(exe: &str) -> Option<PathBuf> {
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
