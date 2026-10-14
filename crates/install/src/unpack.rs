use crate::InstallError;
use creativecraft_platform::ArchiveKind;
use std::path::{Path, PathBuf};

/// Unpack a downloaded archive into `dest`. Entries that would escape `dest` are refused (both
/// `tar::Archive::unpack` and `ZipArchive::extract` sanitise paths; we keep it that way).
pub fn unpack(archive: &Path, name: &str, dest: &Path) -> Result<(), InstallError> {
    let file = std::fs::File::open(archive)?;
    match ArchiveKind::of(name) {
        Some(ArchiveKind::TarGz) => {
            let mut a = tar::Archive::new(flate2::read::GzDecoder::new(std::io::BufReader::new(file)));
            a.set_preserve_permissions(true);
            a.set_overwrite(true);
            a.unpack(dest)?;
        }
        Some(ArchiveKind::Zip) => {
            let mut z = zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| InstallError::Archive(e.to_string()))?;
            z.extract(dest).map_err(|e| InstallError::Archive(e.to_string()))?;
            #[cfg(unix)]
            restore_zip_modes(&mut z, dest)?;
        }
        Some(ArchiveKind::Dmg) => {
            drop(file);
            unpack_dmg(archive, dest)?;
        }
        None => return Err(InstallError::Archive(format!("{name} is not a .tar.gz, .zip or .dmg"))),
    }
    Ok(())
}

/// Copy every `*.app` on a macOS disk image into `dest`: attach it read-only and hidden (no Finder
/// window), copy with `ditto` (keeps code signatures, extended attributes and symlinks), detach.
/// The image is detached on every path out, errors included.
#[cfg(target_os = "macos")]
fn unpack_dmg(dmg: &Path, dest: &Path) -> Result<(), InstallError> {
    use std::process::{Command, Stdio};

    struct Mounted(PathBuf);
    impl Drop for Mounted {
        fn drop(&mut self) {
            let detach = |force: bool| {
                let mut c = Command::new("/usr/bin/hdiutil");
                c.arg("detach").arg(&self.0).arg("-quiet");
                if force {
                    c.arg("-force");
                }
                c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
            };
            // Spotlight or Finder can hold the volume for a moment; then insist
            if !detach(false) {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if !detach(false) && !detach(true) {
                    log::warn!("couldn't detach the disk image mounted at {}", self.0.display());
                }
            }
            let _ = std::fs::remove_dir(&self.0);
        }
    }

    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mnt = std::env::temp_dir().join(format!("creativecraft-dmg-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&mnt)?;
    let out = Command::new("/usr/bin/hdiutil")
        .args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"])
        .arg(&mnt)
        .arg(dmg)
        .stdin(Stdio::null())
        .output()?;
    if !out.status.success() {
        let _ = std::fs::remove_dir(&mnt);
        return Err(InstallError::Archive(format!("couldn't open the disk image: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    let mounted = Mounted(mnt);

    std::fs::create_dir_all(dest)?;
    let mut copied = 0;
    let mut entries: Vec<_> = std::fs::read_dir(&mounted.0)?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name();
        // a real bundle folder, not the "Applications" shortcut drag-to-install images carry
        let is_app = e.file_type().is_ok_and(|t| t.is_dir()) && Path::new(&name).extension().is_some_and(|x| x.eq_ignore_ascii_case("app"));
        if !is_app {
            continue;
        }
        let out = Command::new("/usr/bin/ditto").arg(e.path()).arg(dest.join(&name)).stdin(Stdio::null()).output()?;
        if !out.status.success() {
            return Err(InstallError::Archive(format!(
                "couldn't copy {} out of the disk image: {}",
                name.to_string_lossy(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        copied += 1;
    }
    drop(mounted);
    if copied == 0 {
        return Err(InstallError::Archive("the disk image has no app in it".into()));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn unpack_dmg(_dmg: &Path, _dest: &Path) -> Result<(), InstallError> {
    Err(InstallError::Archive("disk images can only be installed on macOS".into()))
}

/// macOS zips carry Unix modes; make sure executables stay executable.
#[cfg(unix)]
fn restore_zip_modes<R: std::io::Read + std::io::Seek>(z: &mut zip::ZipArchive<R>, dest: &Path) -> Result<(), InstallError> {
    use std::os::unix::fs::PermissionsExt;
    for i in 0..z.len() {
        let f = z.by_index(i).map_err(|e| InstallError::Archive(e.to_string()))?;
        if let (Some(mode), Some(rel)) = (f.unix_mode(), f.enclosed_name())
            && mode & 0o111 != 0
            && !f.is_dir()
        {
            let p = dest.join(rel);
            if p.exists() {
                std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode & 0o7777))?;
            }
        }
    }
    Ok(())
}

/// Find `exe` (a file name) under `root`, preferring the shallowest match. Looks inside macOS
/// bundles (`X.app/Contents/MacOS/`) too. Bounded depth: archives are a few levels deep at most.
pub fn find_executable(root: &Path, exe: &str) -> Option<PathBuf> {
    let mut frontier = vec![root.to_path_buf()];
    for _depth in 0..6 {
        let mut next = Vec::new();
        for dir in &frontier {
            let Ok(entries) = std::fs::read_dir(dir) else { continue };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let p = e.path();
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_file() && e.file_name().to_string_lossy().eq_ignore_ascii_case(exe) {
                    return Some(p);
                }
                if ft.is_dir() {
                    next.push(p);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::find_executable;

    /// A drag-to-install disk image like the Craft apps': an app bundle next to an Applications
    /// shortcut. The app is copied out intact, the shortcut isn't, and the image is detached.
    #[cfg(target_os = "macos")]
    #[test]
    fn installs_the_app_from_a_disk_image() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("creativecraft-dmg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let stage = root.join("stage");
        let exe = stage.join("FigCraft.app/Contents/MacOS/figcraft");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"#!/bin/sh\necho hi\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(stage.join("FigCraft.app/Contents/Info.plist"), b"<plist/>").unwrap();
        std::os::unix::fs::symlink("/Applications", stage.join("Applications")).unwrap();
        let dmg = root.join("figcraft-0.5.1-macos-universal.dmg");
        let st = std::process::Command::new("/usr/bin/hdiutil")
            .args(["create", "-quiet", "-volname", "FigCraft", "-fs", "HFS+", "-format", "UDZO", "-srcfolder"])
            .arg(&stage)
            .arg(&dmg)
            .status()
            .unwrap();
        assert!(st.success(), "hdiutil create failed");

        let dest = root.join("installed");
        super::unpack(&dmg, "figcraft-0.5.1-macos-universal.dmg", &dest).unwrap();
        let found = find_executable(&dest, "figcraft").unwrap();
        assert!(found.ends_with("FigCraft.app/Contents/MacOS/figcraft"));
        assert_eq!(std::fs::metadata(&found).unwrap().permissions().mode() & 0o111, 0o111, "still executable");
        assert!(!dest.join("Applications").exists(), "the Applications shortcut isn't copied");
        // nothing of ours is left mounted
        let info = std::process::Command::new("/usr/bin/hdiutil").arg("info").output().unwrap();
        assert!(!String::from_utf8_lossy(&info.stdout).contains("creativecraft-dmg-"), "disk image still attached");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn disk_images_are_refused_off_macos() {
        let dir = std::env::temp_dir().join(format!("creativecraft-dmg-refuse-{}", std::process::id()));
        let err = super::unpack(&dir.join("x.dmg"), "x-1.0-macos-universal.dmg", &dir).unwrap_err();
        assert!(err.to_string().contains("only be installed on macOS"), "{err}");
    }

    #[test]
    fn finds_nested_and_bundled_executables() {
        let root = std::env::temp_dir().join(format!("creativecraft-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("photocraft-0.5.0/bin")).unwrap();
        std::fs::write(root.join("photocraft-0.5.0/bin/photocraft-cli"), b"").unwrap();
        std::fs::create_dir_all(root.join("PhotoCraft.app/Contents/MacOS")).unwrap();
        std::fs::write(root.join("PhotoCraft.app/Contents/MacOS/photocraft"), b"").unwrap();
        assert!(find_executable(&root, "photocraft-cli").unwrap().ends_with("bin/photocraft-cli"));
        assert!(find_executable(&root, "photocraft").unwrap().ends_with("MacOS/photocraft"));
        assert!(find_executable(&root, "nope").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
