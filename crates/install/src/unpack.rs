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
        None => return Err(InstallError::Archive(format!("{name} is not a .tar.gz or .zip"))),
    }
    Ok(())
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
