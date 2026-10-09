//! One sync pass between a local projects folder and a [`CloudStore`].
//!
//! The base snapshot lives in `<projects>/.creativecraft/sync-base.json` (never synced itself), tagged
//! with the store's label: pointing the folder at a different store starts from an empty base,
//! which is safe (differences become conflict copies, never overwrites).

use crate::folder::rel_path;
use crate::{CloudError, CloudStore, md5_file};
use creativecraft_sync_plan::{Action, Snapshot, is_ignored, next_base, plan};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Default)]
struct BaseFile {
    store: String,
    base: Snapshot,
}

/// What a sync pass did.
#[derive(Debug, Default, Clone)]
pub struct Report {
    pub uploaded: usize,
    pub downloaded: usize,
    pub trashed_remote: usize,
    pub trashed_local: usize,
    /// `(original, cloud copy)` pairs kept side by side.
    pub conflicts: Vec<(String, String)>,
    /// `(path, why)` for actions that failed; they are retried next pass.
    pub errors: Vec<(String, String)>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.errors.is_empty()
    }
    pub fn summary(&self) -> String {
        let mut parts = vec![];
        for (n, what) in [(self.uploaded, "uploaded"), (self.downloaded, "downloaded"), (self.trashed_remote + self.trashed_local, "moved to trash")] {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        }
        if !self.conflicts.is_empty() {
            parts.push(format!("{} conflict copies kept", self.conflicts.len()));
        }
        if !self.errors.is_empty() {
            parts.push(format!("{} failed", self.errors.len()));
        }
        if parts.is_empty() { "Everything is up to date".into() } else { parts.join(", ") }
    }
}

fn state_dir(root: &Path) -> PathBuf {
    root.join(".creativecraft")
}

/// Hash every syncable file under `root`.
pub fn local_snapshot(root: &Path) -> Result<Snapshot, CloudError> {
    let mut snap = Snapshot::new();
    fn walk(root: &Path, dir: &Path, snap: &mut Snapshot) -> Result<(), CloudError> {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            let p = e.path();
            let rel = rel_path(root, &p);
            if is_ignored(&rel) {
                continue;
            }
            let ft = e.file_type()?;
            if ft.is_dir() {
                walk(root, &p, snap)?;
            } else if ft.is_file() {
                snap.insert(rel, md5_file(&p)?);
            }
        }
        Ok(())
    }
    if root.is_dir() {
        walk(root, root, &mut snap)?;
    }
    Ok(snap)
}

fn local_path(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').filter(|s| !s.is_empty() && *s != "." && *s != "..").fold(root.to_path_buf(), |p, s| p.join(s))
}

/// The plan a sync would run now, without changing anything (for a "preview" in the UI).
pub fn preview(root: &Path, store: &mut dyn CloudStore, device: &str) -> Result<Vec<Action>, CloudError> {
    let local = local_snapshot(root)?;
    let remote: Snapshot = store.list()?.into_iter().map(|f| (f.path, f.md5)).collect();
    let base = load_base(root, &store.label());
    Ok(plan(&local, &remote, &base, device))
}

fn load_base(root: &Path, label: &str) -> Snapshot {
    std::fs::read(state_dir(root).join("sync-base.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<BaseFile>(&b).ok())
        .filter(|f| f.store == label)
        .map(|f| f.base)
        .unwrap_or_default()
}

fn save_base(root: &Path, label: &str, base: Snapshot) -> Result<(), CloudError> {
    let dir = state_dir(root);
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join("sync-base.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&BaseFile { store: label.to_string(), base })?)?;
    std::fs::rename(tmp, dir.join("sync-base.json"))?;
    Ok(())
}

/// Run one full sync pass. Individual failures don't stop the pass; they are reported and retried
/// next time (their base entries are left as they were).
pub fn run(root: &Path, store: &mut dyn CloudStore, device: &str, progress: &mut dyn FnMut(usize, usize, &str)) -> Result<Report, CloudError> {
    std::fs::create_dir_all(root)?;
    // the window, the CLI and the agent's MCP server can all start a sync: one at a time
    let _lock = SyncLock::take(root)?;
    let label = store.label();
    let local = local_snapshot(root)?;
    let remote: Snapshot = store.list()?.into_iter().map(|f| (f.path, f.md5)).collect();
    let base = load_base(root, &label);
    let actions = plan(&local, &remote, &base, device);

    let mut report = Report::default();
    let mut applied = Vec::new();
    let mut failed = Vec::new();
    let total = actions.len();
    for (i, a) in actions.into_iter().enumerate() {
        progress(i, total, a.path());
        match apply(root, store, &local, &a, &mut report) {
            Ok(()) => applied.push(a),
            Err(e) => {
                report.errors.push((a.path().to_string(), e.to_string()));
                failed.push(a);
            }
        }
    }
    progress(total, total, "");

    let mut nb = next_base(&local, &remote, &applied);
    for a in &failed {
        match base.get(a.path()) {
            Some(h) => nb.insert(a.path().to_string(), h.clone()),
            None => nb.remove(a.path()),
        };
        if let Action::Conflict { conflict_path, .. } = a {
            nb.remove(conflict_path);
        }
    }
    save_base(root, &label, nb)?;
    Ok(report)
}

/// A lock file in the state folder; one older than 15 minutes is treated as left over from a crash.
struct SyncLock(PathBuf);

impl SyncLock {
    fn take(root: &Path) -> Result<SyncLock, CloudError> {
        let dir = state_dir(root);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("sync.lock");
        for _ in 0..2 {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Ok(SyncLock(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > std::time::Duration::from_secs(15 * 60));
                    if !stale {
                        return Err(CloudError::Io(std::io::Error::other("a sync is already running")));
                    }
                    let _ = std::fs::remove_file(&path);
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(CloudError::Io(std::io::Error::other("a sync is already running")))
    }
}

impl Drop for SyncLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn apply(root: &Path, store: &mut dyn CloudStore, local: &Snapshot, a: &Action, report: &mut Report) -> Result<(), CloudError> {
    match a {
        Action::Upload(p) => {
            store.upload(p, &local_path(root, p))?;
            report.uploaded += 1;
        }
        Action::Download(p) => {
            let dest = local_path(root, p);
            // don't clobber an edit made while this pass was running
            if dest.exists() && local.get(p).is_some_and(|h| md5_file(&dest).is_ok_and(|now| &now != h)) {
                return Err(CloudError::Io(std::io::Error::other("changed locally during sync; will retry")));
            }
            if let Some(d) = dest.parent() {
                std::fs::create_dir_all(d)?;
            }
            store.download(p, &dest)?;
            report.downloaded += 1;
        }
        Action::DeleteRemote(p) => {
            store.trash(p)?;
            report.trashed_remote += 1;
        }
        Action::DeleteLocal(p) => {
            let src = local_path(root, p);
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let dest = local_path(&state_dir(root).join("trash").join(stamp.to_string()), p);
            if let Some(d) = dest.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::rename(src, dest)?;
            report.trashed_local += 1;
        }
        Action::Conflict { path, conflict_path } => {
            let copy = local_path(root, conflict_path);
            if let Some(d) = copy.parent() {
                std::fs::create_dir_all(d)?;
            }
            store.download(path, &copy)?;
            store.upload(conflict_path, &copy)?;
            store.upload(path, &local_path(root, path))?;
            report.conflicts.push((path.clone(), conflict_path.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FolderStore;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("creativecraft-sync-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Two devices sharing one store, end to end.
    #[test]
    fn two_devices_round_trip_and_conflict() {
        let cloud = tmp("cloud");
        let (a, b) = (tmp("a"), tmp("b"));
        let noop = &mut |_: usize, _: usize, _: &str| {};

        std::fs::create_dir_all(a.join("art")).unwrap();
        std::fs::write(a.join("art/poster.pcraft"), b"v1").unwrap();
        let r = run(&a, &mut FolderStore::new(&cloud), "a", noop).unwrap();
        assert_eq!(r.uploaded, 1);

        let r = run(&b, &mut FolderStore::new(&cloud), "b", noop).unwrap();
        assert_eq!(r.downloaded, 1);
        assert_eq!(std::fs::read(b.join("art/poster.pcraft")).unwrap(), b"v1");

        // both edit offline, then sync
        std::fs::write(a.join("art/poster.pcraft"), b"from a").unwrap();
        std::fs::write(b.join("art/poster.pcraft"), b"from b").unwrap();
        run(&a, &mut FolderStore::new(&cloud), "a", noop).unwrap();
        let r = run(&b, &mut FolderStore::new(&cloud), "b", noop).unwrap();
        assert_eq!(r.conflicts.len(), 1);
        // b keeps its edit, and a's edit sits beside it: nothing lost
        assert_eq!(std::fs::read(b.join("art/poster.pcraft")).unwrap(), b"from b");
        assert_eq!(std::fs::read(b.join("art/poster (cloud copy for b).pcraft")).unwrap(), b"from a");

        // a picks up both; then everything is quiet
        run(&a, &mut FolderStore::new(&cloud), "a", noop).unwrap();
        assert_eq!(std::fs::read(a.join("art/poster (cloud copy for b).pcraft")).unwrap(), b"from a");
        assert_eq!(preview(&a, &mut FolderStore::new(&cloud), "a").unwrap(), vec![]);
        assert_eq!(preview(&b, &mut FolderStore::new(&cloud), "b").unwrap(), vec![]);

        // a deletion travels, into the trash
        std::fs::remove_file(a.join("art/poster (cloud copy for b).pcraft")).unwrap();
        let r = run(&a, &mut FolderStore::new(&cloud), "a", noop).unwrap();
        assert_eq!(r.trashed_remote, 1);
        let r = run(&b, &mut FolderStore::new(&cloud), "b", noop).unwrap();
        assert_eq!(r.trashed_local, 1);
        assert!(!b.join("art/poster (cloud copy for b).pcraft").exists());

        for d in [cloud, a, b] {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
