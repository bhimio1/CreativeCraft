use crate::{CloudError, CloudStore, RemoteFile, md5_file};
use std::path::{Path, PathBuf};

/// Cloud saves into a plain folder (often one a desktop sync client mirrors to the cloud).
/// Trashed files go to `<root>/.atelier-trash/<timestamp>/…`.
pub struct FolderStore {
    root: PathBuf,
}

impl FolderStore {
    pub fn new(root: impl Into<PathBuf>) -> FolderStore {
        FolderStore { root: root.into() }
    }

    fn abs(&self, rel: &str) -> PathBuf {
        rel.split('/').filter(|s| !s.is_empty() && *s != "." && *s != "..").fold(self.root.clone(), |p, s| p.join(s))
    }
}

impl CloudStore for FolderStore {
    fn label(&self) -> String {
        format!("Folder: {}", self.root.display())
    }

    fn list(&mut self) -> Result<Vec<RemoteFile>, CloudError> {
        let mut out = Vec::new();
        if self.root.is_dir() {
            walk(&self.root, &self.root, &mut out)?;
        }
        Ok(out)
    }

    fn download(&mut self, path: &str, dest: &Path) -> Result<(), CloudError> {
        let src = self.abs(path);
        if !src.is_file() {
            return Err(CloudError::Missing(path.to_string()));
        }
        std::fs::copy(src, dest)?;
        Ok(())
    }

    fn upload(&mut self, path: &str, src: &Path) -> Result<(), CloudError> {
        let dest = self.abs(path);
        if let Some(d) = dest.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut tmp_name = dest.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(".atelier.part");
        let tmp = dest.with_file_name(tmp_name);
        std::fs::copy(src, &tmp)?;
        std::fs::rename(tmp, dest)?;
        Ok(())
    }

    fn trash(&mut self, path: &str) -> Result<(), CloudError> {
        let src = self.abs(path);
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let dest = self.root.join(".atelier-trash").join(stamp.to_string()).join(path);
        if let Some(d) = dest.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::rename(src, dest)?;
        Ok(())
    }
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<RemoteFile>) -> Result<(), CloudError> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let p = e.path();
        let ft = e.file_type()?;
        let rel = rel_path(root, &p);
        if rel.starts_with(".atelier-trash") || atelier_sync_plan::is_ignored(&rel) {
            continue;
        }
        if ft.is_dir() {
            walk(root, &p, out)?;
        } else if ft.is_file() {
            out.push(RemoteFile { md5: md5_file(&p)?, size: e.metadata()?.len(), path: rel });
        }
    }
    Ok(())
}

/// `root/a/b.txt` → `a/b.txt`, always with `/`.
pub(crate) fn rel_path(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}
