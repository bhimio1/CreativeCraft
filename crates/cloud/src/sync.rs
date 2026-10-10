//! One sync pass between a local projects folder and a [`CloudStore`].
//!
//! The base snapshot lives in `<projects>/.creativecraft/sync-base.json` (never synced itself), tagged
//! with the store's label: pointing the folder at a different store starts from an empty base,
//! which is safe (differences become conflict copies, never overwrites).
//!
//! Also in `.creativecraft/`:
//! - `hash-cache.json`: MD5s of local files by size and modification time, so unchanged files
//!   aren't hashed again every pass;
//! - `remote-hashes.json`: for stores that report no MD5 (WebDAV, some rclone remotes), which MD5
//!   each remote fingerprint stands for. An unknown file is downloaded once into `incoming/`,
//!   hashed, and that copy is what a later download uses;
//! - `incoming/`: downloads land here and are checked before they're renamed into place.
//!
//! `.creativecraftignore` in the projects folder adds the user's own ignore rules (both sides).

use crate::folder::rel_path;
use crate::{CloudError, CloudStore, RemoteFile, md5_file};
use creativecraft_sync_plan::{Action, IgnoreRules, Snapshot, is_ignored, next_base, plan};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

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

/// The user's ignore rules (`.creativecraftignore`), if any.
pub fn ignore_rules(root: &Path) -> IgnoreRules {
    std::fs::read_to_string(root.join(IgnoreRules::FILE)).map(|t| IgnoreRules::parse(&t)).unwrap_or_default()
}

fn skipped(rules: &IgnoreRules, rel: &str) -> bool {
    is_ignored(rel) || rules.matches(rel)
}

/// `path → (size, stamp, md5)`. The stamp is the modification time plus a time tools can't carry
/// over from another file: the inode change time on Unix, the creation time on Windows. Tools that
/// preserve modification times (`cp -p`, unzip, rclone, and `std::fs::copy` on Windows) can't set
/// those, so a file replaced by another of the same length and date is still hashed again. Files
/// a sync pass writes itself are dropped from the cache too (`forget_cached`), so they're always
/// re-hashed on the next pass whatever the file system's timestamp resolution.
#[derive(Serialize, Deserialize, Default)]
struct HashCache(BTreeMap<String, (u64, u128, String)>);

fn stamp(meta: &std::fs::Metadata) -> u128 {
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let ctime = (meta.ctime() as u128).wrapping_mul(1_000_000_000).wrapping_add(meta.ctime_nsec() as u128);
        // one number that changes when either does
        mtime ^ ctime.rotate_left(64)
    }
    #[cfg(not(unix))]
    {
        let created = meta.created().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
        mtime ^ created.rotate_left(64)
    }
}

/// Drop `paths` from the local hash cache, so the next snapshot hashes them from their contents.
fn forget_cached<'a>(root: &Path, paths: impl IntoIterator<Item = &'a str>) -> Result<(), CloudError> {
    let cache_file = state_dir(root).join("hash-cache.json");
    let Some(mut cache) = std::fs::read(&cache_file).ok().and_then(|b| serde_json::from_slice::<HashCache>(&b).ok()) else { return Ok(()) };
    let before = cache.0.len();
    for p in paths {
        cache.0.remove(p);
    }
    if cache.0.len() != before {
        write_json(&cache_file, &cache)?;
    }
    Ok(())
}

/// Hash every syncable file under `root` (re-using the hash of files whose size and modification
/// time haven't changed since last time).
pub fn local_snapshot(root: &Path) -> Result<Snapshot, CloudError> {
    let rules = ignore_rules(root);
    let cache_file = state_dir(root).join("hash-cache.json");
    let old: HashCache = std::fs::read(&cache_file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut new = HashCache::default();
    let mut snap = Snapshot::new();
    fn walk(root: &Path, dir: &Path, rules: &IgnoreRules, old: &HashCache, new: &mut HashCache, snap: &mut Snapshot) -> Result<(), CloudError> {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            let p = e.path();
            let rel = rel_path(root, &p);
            if skipped(rules, &rel) {
                continue;
            }
            let ft = e.file_type()?;
            if ft.is_dir() {
                walk(root, &p, rules, old, new, snap)?;
            } else if ft.is_file() {
                let meta = e.metadata()?;
                let mtime = stamp(&meta);
                let md5 = match old.0.get(&rel) {
                    Some((size, m, h)) if *size == meta.len() && *m == mtime && mtime != 0 => h.clone(),
                    _ => md5_file(&p)?,
                };
                new.0.insert(rel.clone(), (meta.len(), mtime, md5.clone()));
                snap.insert(rel, md5);
            }
        }
        Ok(())
    }
    if root.is_dir() {
        walk(root, root, &rules, &old, &mut new, &mut snap)?;
        if std::fs::create_dir_all(state_dir(root)).is_ok() {
            write_json(&cache_file, &new)?;
        }
    }
    Ok(snap)
}

fn write_json(path: &Path, v: &impl Serialize) -> Result<(), CloudError> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(v)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// For stores without MD5s: `store label → path → (fingerprint, md5)`.
#[derive(Serialize, Deserialize, Default)]
struct RemoteHashes(BTreeMap<String, BTreeMap<String, (String, String)>>);

impl RemoteHashes {
    fn load(root: &Path) -> RemoteHashes {
        std::fs::read(state_dir(root).join("remote-hashes.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }
    fn save(&self, root: &Path) -> Result<(), CloudError> {
        std::fs::create_dir_all(state_dir(root))?;
        write_json(&state_dir(root).join("remote-hashes.json"), self)
    }
}

fn incoming(root: &Path) -> PathBuf {
    state_dir(root).join("incoming")
}

/// Download `path` into `incoming/`, returning the temp file and its MD5.
fn fetch(root: &Path, store: &mut dyn CloudStore, path: &str) -> Result<(PathBuf, String), CloudError> {
    let dir = incoming(root);
    std::fs::create_dir_all(&dir)?;
    let stamp = std::time::SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let tmp = dir.join(format!("{stamp}.part"));
    if let Err(e) = store.download(path, &tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let md5 = md5_file(&tmp)?;
    Ok((tmp, md5))
}

/// The remote side as a snapshot (path → MD5), ignoring what the user ignores. MD5s the store
/// doesn't report come from `remote-hashes.json`; files not seen before are fetched once and
/// hashed (`fetch_unknown`), or, for a preview, stand in as their fingerprint.
fn remote_snapshot(
    root: &Path,
    store: &mut dyn CloudStore,
    files: Vec<RemoteFile>,
    hashes: &mut RemoteHashes,
    fetched: &mut BTreeMap<String, PathBuf>,
    fetch_unknown: bool,
) -> Result<Snapshot, CloudError> {
    let rules = ignore_rules(root);
    let label = store.label();
    let mut snap = Snapshot::new();
    let mut seen = BTreeMap::new();
    for f in files {
        if skipped(&rules, &f.path) {
            continue;
        }
        let known = hashes.0.get(&label).and_then(|m| m.get(&f.path)).filter(|(fp, _)| *fp == f.fingerprint).map(|(_, h)| h.clone());
        let md5 = match (f.md5.clone(), known) {
            (Some(h), _) | (None, Some(h)) => h,
            (None, None) if fetch_unknown => {
                let (tmp, h) = fetch(root, store, &f.path)?;
                fetched.insert(h.clone(), tmp);
                h
            }
            (None, None) => format!("fingerprint:{}", f.fingerprint),
        };
        if f.md5.is_none() && !md5.starts_with("fingerprint:") {
            seen.insert(f.path.clone(), (f.fingerprint.clone(), md5.clone()));
        }
        snap.insert(f.path, md5);
    }
    // forget files that are gone
    hashes.0.insert(label, seen);
    Ok(snap)
}

fn local_path(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').filter(|s| !s.is_empty() && *s != "." && *s != "..").fold(root.to_path_buf(), |p, s| p.join(s))
}

/// The plan a sync would run now, without changing anything (for a "preview" in the UI). Files a
/// store reports without an MD5 and that haven't been seen before count as changed.
pub fn preview(root: &Path, store: &mut dyn CloudStore, device: &str) -> Result<Vec<Action>, CloudError> {
    let local = local_snapshot(root)?;
    let files = store.list()?;
    let mut hashes = RemoteHashes::load(root);
    let remote = remote_snapshot(root, store, files, &mut hashes, &mut BTreeMap::new(), false)?;
    let base = load_base(root, &store.label());
    Ok(plan(&local, &remote, &base, device))
}

/// Check that a store can be reached and listed; returns a one-line summary for the settings page.
pub fn test_connection(store: &mut dyn CloudStore) -> Result<String, CloudError> {
    let files = store.list()?;
    let bytes: u64 = files.iter().map(|f| f.size).sum();
    Ok(match files.len() {
        0 => format!("Connected to {}. It's empty so far.", store.label()),
        n => format!("Connected to {}: {n} files, {:.1} MB.", store.label(), bytes as f64 / 1e6),
    })
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
    let files = store.list()?;
    let mut hashes = RemoteHashes::load(root);
    // remote files fetched while working out their hash: md5 → temp file, reused by downloads
    let mut fetched: BTreeMap<String, PathBuf> = BTreeMap::new();
    let remote = remote_snapshot(root, store, files, &mut hashes, &mut fetched, true)?;
    let base = load_base(root, &label);
    let actions = plan(&local, &remote, &base, device);

    let mut report = Report::default();
    let mut applied = Vec::new();
    let mut failed = Vec::new();
    let total = actions.len();
    for (i, a) in actions.into_iter().enumerate() {
        progress(i, total, a.path());
        match apply(root, store, &local, &remote, &label, &mut hashes, &mut fetched, &a, &mut report) {
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
    // never trust a cached hash for a file this pass touched (see `HashCache`)
    let touched = applied.iter().chain(&failed).flat_map(|a| match a {
        Action::Conflict { path, conflict_path } => vec![path.as_str(), conflict_path.as_str()],
        other => vec![other.path()],
    });
    forget_cached(root, touched)?;
    hashes.save(root)?;
    // fetched copies nothing needed
    for tmp in fetched.values() {
        let _ = std::fs::remove_file(tmp);
    }
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

/// Bring the remote file `path` (expected MD5 `md5`) to `dest`: from a copy fetched earlier, or
/// downloaded now into `incoming/`; checked, then renamed into place.
fn receive(root: &Path, store: &mut dyn CloudStore, path: &str, md5: &str, fetched: &mut BTreeMap<String, PathBuf>, dest: &Path) -> Result<(), CloudError> {
    let tmp = match fetched.remove(md5) {
        Some(tmp) if tmp.is_file() => tmp,
        _ => {
            let (tmp, got) = fetch(root, store, path)?;
            if got != md5 && !md5.starts_with("fingerprint:") {
                let _ = std::fs::remove_file(&tmp);
                return Err(CloudError::Corrupt { path: path.to_string() });
            }
            tmp
        }
    };
    if let Some(d) = dest.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::rename(&tmp, dest).or_else(|_| {
        // `incoming/` on another file system than `dest`: copy, then drop the temp file
        std::fs::copy(&tmp, dest).map(|_| ()).inspect(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    })?;
    Ok(())
}

/// Record the fingerprint a store reported for something just uploaded.
fn remember(hashes: &mut RemoteHashes, label: &str, path: &str, fingerprint: Option<String>, md5: &str) {
    if let Some(fp) = fingerprint {
        hashes.0.entry(label.to_string()).or_default().insert(path.to_string(), (fp, md5.to_string()));
    }
}

#[allow(clippy::too_many_arguments)]
fn apply(
    root: &Path,
    store: &mut dyn CloudStore,
    local: &Snapshot,
    remote: &Snapshot,
    label: &str,
    hashes: &mut RemoteHashes,
    fetched: &mut BTreeMap<String, PathBuf>,
    a: &Action,
    report: &mut Report,
) -> Result<(), CloudError> {
    match a {
        Action::Upload(p) => {
            let src = local_path(root, p);
            let fp = store.upload(p, &src)?;
            remember(hashes, label, p, fp, local.get(p).map_or("", String::as_str));
            report.uploaded += 1;
        }
        Action::Download(p) => {
            let dest = local_path(root, p);
            // don't clobber an edit made while this pass was running
            if dest.exists() && local.get(p).is_some_and(|h| md5_file(&dest).is_ok_and(|now| &now != h)) {
                return Err(CloudError::Io(std::io::Error::other("changed locally during sync; will retry")));
            }
            let md5 = remote.get(p).cloned().unwrap_or_default();
            receive(root, store, p, &md5, fetched, &dest)?;
            report.downloaded += 1;
        }
        Action::DeleteRemote(p) => {
            store.trash(p)?;
            if let Some(m) = hashes.0.get_mut(label) {
                m.remove(p);
            }
            report.trashed_remote += 1;
        }
        Action::DeleteLocal(p) => {
            let src = local_path(root, p);
            let stamp = std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let dest = local_path(&state_dir(root).join("trash").join(stamp.to_string()), p);
            if let Some(d) = dest.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::rename(src, dest)?;
            report.trashed_local += 1;
        }
        Action::Conflict { path, conflict_path } => {
            let copy = local_path(root, conflict_path);
            let remote_md5 = remote.get(path).cloned().unwrap_or_default();
            receive(root, store, path, &remote_md5, fetched, &copy)?;
            let copy_md5 = md5_file(&copy)?;
            let fp = store.upload(conflict_path, &copy)?;
            remember(hashes, label, conflict_path, fp, &copy_md5);
            let fp = store.upload(path, &local_path(root, path))?;
            remember(hashes, label, path, fp, local.get(path).map_or("", String::as_str));
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

    /// A store like WebDAV: no MD5s, an ETag-like fingerprint, counted downloads.
    struct NoMd5 {
        inner: FolderStore,
        downloads: usize,
    }

    impl NoMd5 {
        fn new(root: &Path) -> NoMd5 {
            NoMd5 { inner: FolderStore::new(root), downloads: 0 }
        }
    }

    impl CloudStore for NoMd5 {
        fn label(&self) -> String {
            format!("nomd5 {}", self.inner.label())
        }
        fn list(&mut self) -> Result<Vec<RemoteFile>, CloudError> {
            Ok(self.inner.list()?.into_iter().map(|f| RemoteFile { fingerprint: format!("etag-{}", f.fingerprint), md5: None, ..f }).collect())
        }
        fn download(&mut self, path: &str, dest: &Path) -> Result<(), CloudError> {
            self.downloads += 1;
            self.inner.download(path, dest)
        }
        fn upload(&mut self, path: &str, src: &Path) -> Result<Option<String>, CloudError> {
            self.inner.upload(path, src)?;
            Ok(Some(format!("etag-{}", md5_file(src)?)))
        }
        fn trash(&mut self, path: &str) -> Result<(), CloudError> {
            self.inner.trash(path)
        }
    }

    #[test]
    fn stores_without_md5s_sync_without_extra_downloads() {
        let cloud = tmp("nomd5-cloud");
        let (a, b) = (tmp("nomd5-a"), tmp("nomd5-b"));
        let noop = &mut |_: usize, _: usize, _: &str| {};
        std::fs::write(a.join("one.pcraft"), b"1").unwrap();
        std::fs::write(a.join("two.pcraft"), b"2").unwrap();
        let mut sa = NoMd5::new(&cloud);
        assert_eq!(run(&a, &mut sa, "a", noop).unwrap().uploaded, 2);
        // a knows the fingerprints of what it uploaded: a second pass downloads nothing
        assert_eq!(run(&a, &mut sa, "a", noop).unwrap().summary(), "Everything is up to date");
        assert_eq!(sa.downloads, 0);

        // b has never seen these: each is fetched exactly once, and that copy is used
        let mut sb = NoMd5::new(&cloud);
        let r = run(&b, &mut sb, "b", noop).unwrap();
        assert_eq!(r.downloaded, 2);
        assert_eq!(sb.downloads, 2);
        assert_eq!(std::fs::read(b.join("two.pcraft")).unwrap(), b"2");
        assert_eq!(run(&b, &mut sb, "b", noop).unwrap().summary(), "Everything is up to date");
        assert_eq!(sb.downloads, 2);
        assert!(std::fs::read_dir(b.join(".creativecraft/incoming")).unwrap().next().is_none(), "temp files cleaned up");

        // an edit from b reaches a
        std::fs::write(b.join("one.pcraft"), b"1 edited").unwrap();
        run(&b, &mut sb, "b", noop).unwrap();
        run(&a, &mut sa, "a", noop).unwrap();
        assert_eq!(std::fs::read(a.join("one.pcraft")).unwrap(), b"1 edited");
        for d in [cloud, a, b] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn damaged_downloads_never_replace_files() {
        let cloud = tmp("corrupt-cloud");
        let (a, b) = (tmp("corrupt-a"), tmp("corrupt-b"));
        let noop = &mut |_: usize, _: usize, _: &str| {};
        std::fs::write(a.join("x.pcraft"), b"v1").unwrap();
        run(&a, &mut FolderStore::new(&cloud), "a", noop).unwrap();
        run(&b, &mut FolderStore::new(&cloud), "b", noop).unwrap();
        std::fs::write(a.join("x.pcraft"), b"v2").unwrap();
        run(&a, &mut FolderStore::new(&cloud), "a", noop).unwrap();

        // a store whose downloads arrive damaged, but which does report MD5s
        struct Damaging(FolderStore);
        impl CloudStore for Damaging {
            fn label(&self) -> String {
                self.0.label()
            }
            fn list(&mut self) -> Result<Vec<RemoteFile>, CloudError> {
                self.0.list()
            }
            fn download(&mut self, path: &str, dest: &Path) -> Result<(), CloudError> {
                self.0.download(path, dest)?;
                std::fs::write(dest, b"garbage")?;
                Ok(())
            }
            fn upload(&mut self, path: &str, src: &Path) -> Result<Option<String>, CloudError> {
                self.0.upload(path, src)
            }
            fn trash(&mut self, path: &str) -> Result<(), CloudError> {
                self.0.trash(path)
            }
        }
        let r = run(&b, &mut Damaging(FolderStore::new(&cloud)), "b", noop).unwrap();
        assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
        assert_eq!(std::fs::read(b.join("x.pcraft")).unwrap(), b"v1", "the old version is untouched");
        // the next good pass gets it
        run(&b, &mut FolderStore::new(&cloud), "b", noop).unwrap();
        assert_eq!(std::fs::read(b.join("x.pcraft")).unwrap(), b"v2");
        for d in [cloud, a, b] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn ignore_file_applies_to_both_sides() {
        let cloud = tmp("ign-cloud");
        let a = tmp("ign-a");
        let noop = &mut |_: usize, _: usize, _: &str| {};
        std::fs::write(a.join(".creativecraftignore"), "renders/\n*.mov\n").unwrap();
        std::fs::create_dir_all(a.join("renders")).unwrap();
        std::fs::write(a.join("renders/frame.png"), b"big").unwrap();
        std::fs::write(a.join("clip.mov"), b"big").unwrap();
        std::fs::write(a.join("poster.pcraft"), b"p").unwrap();
        // something already in the cloud that matches the rules stays there, untouched
        std::fs::write(cloud.join("old.mov"), b"cloud").unwrap();
        let r = run(&a, &mut FolderStore::new(&cloud), "a", noop).unwrap();
        // the ignore file itself syncs (so other computers share the rules); renders and .mov don't
        assert_eq!(r.uploaded, 2);
        assert!(cloud.join("poster.pcraft").exists() && cloud.join(".creativecraftignore").exists());
        assert!(!cloud.join("renders").exists() && !cloud.join("clip.mov").exists());
        assert!(!a.join("old.mov").exists());
        for d in [cloud, a] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn hash_cache_is_reused_and_refreshed() {
        let a = tmp("hc");
        std::fs::write(a.join("f.txt"), b"one").unwrap();
        let first = local_snapshot(&a).unwrap();
        // a cached hash is used while size and time match…
        let cache = a.join(".creativecraft/hash-cache.json");
        let text = std::fs::read_to_string(&cache).unwrap();
        std::fs::write(&cache, text.replace(&first["f.txt"], "cachedhash")).unwrap();
        assert_eq!(local_snapshot(&a).unwrap()["f.txt"], "cachedhash");
        // …and recomputed when the file changes
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(a.join("f.txt"), b"two!").unwrap();
        assert_eq!(local_snapshot(&a).unwrap()["f.txt"], md5_file(&a.join("f.txt")).unwrap());
        let _ = std::fs::remove_dir_all(&a);
    }

    /// A download that keeps the cloud copy's date (fs::copy does on Windows) can replace a local
    /// file with one of the same size and, at coarse clock resolution, the same timestamps. Files a
    /// sync pass wrote must therefore be re-hashed from their contents, never served from the cache.
    #[test]
    fn files_a_sync_touched_are_rehashed() {
        let a = tmp("hc-touched");
        let f = a.join("p.pcraft");
        std::fs::write(&f, b"from a").unwrap();
        let first = local_snapshot(&a).unwrap()["p.pcraft"].clone();
        // poison the cache entry for the current size and stamp, as a same-size, same-date
        // replacement would
        let cache_file = a.join(".creativecraft/hash-cache.json");
        let mut cache: HashCache = serde_json::from_slice(&std::fs::read(&cache_file).unwrap()).unwrap();
        std::fs::write(&f, b"from b").unwrap();
        let meta = std::fs::metadata(&f).unwrap();
        cache.0.insert("p.pcraft".into(), (meta.len(), stamp(&meta), first.clone()));
        write_json(&cache_file, &cache).unwrap();
        assert_eq!(local_snapshot(&a).unwrap()["p.pcraft"], first, "the poisoned entry is what a stale cache returns");
        forget_cached(&a, ["p.pcraft"]).unwrap();
        assert_eq!(local_snapshot(&a).unwrap()["p.pcraft"], md5_file(&f).unwrap());
        let _ = std::fs::remove_dir_all(&a);
    }

    /// Tools that keep modification times (cp -p, unzip, rclone) can replace a file with another of
    /// the same size and date; the hash cache must still notice.
    #[cfg(unix)]
    #[test]
    fn hash_cache_sees_replacements_with_the_same_size_and_date() {
        let a = tmp("hc-same");
        let f = a.join("f.txt");
        std::fs::write(&f, b"aaaa").unwrap();
        let when = std::fs::metadata(&f).unwrap().modified().unwrap();
        assert_eq!(local_snapshot(&a).unwrap()["f.txt"], md5_file(&f).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(20));
        // replace it the way a download does: write elsewhere, keep the date, rename over it
        let tmp_file = a.join("new.part");
        std::fs::write(&tmp_file, b"bbbb").unwrap();
        std::fs::File::options().write(true).open(&tmp_file).unwrap().set_modified(when).unwrap();
        std::fs::rename(&tmp_file, &f).unwrap();
        assert_eq!(std::fs::metadata(&f).unwrap().modified().unwrap(), when);
        assert_eq!(local_snapshot(&a).unwrap()["f.txt"], md5_file(&f).unwrap());
        let _ = std::fs::remove_dir_all(&a);
    }
}
