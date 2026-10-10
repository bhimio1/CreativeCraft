//! Sync when files change: a watcher on the projects folder that says when something worth
//! syncing changed and has then been quiet for a while (a burst of saves makes one sync). Changes
//! sync skips anyway (CreativeCraft's own folder, temp files, `.creativecraftignore` rules) don't
//! count, and the caller mutes it while a sync is writing files, so syncs don't set each other off.

use crate::CloudError;
use creativecraft_sync_plan::{IgnoreRules, is_ignored};
use notify::{RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

pub struct ChangeWatcher {
    root: PathBuf,
    _watcher: notify::RecommendedWatcher,
    rx: Receiver<PathBuf>,
    rules: IgnoreRules,
    /// When the last change worth syncing was seen (none pending: `None`).
    last_change: Option<Instant>,
    muted_until: Option<Instant>,
}

impl ChangeWatcher {
    /// Watch `root` and everything under it. `wake` is called on every file event (e.g. to wake a
    /// window so it can call [`ChangeWatcher::poll`]).
    pub fn new(root: &Path, wake: impl Fn() + Send + 'static) -> Result<ChangeWatcher, CloudError> {
        let (tx, rx) = channel();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                // reads and metadata-only touches change nothing worth syncing
                if matches!(ev.kind, notify::EventKind::Access(_)) {
                    return;
                }
                for p in ev.paths {
                    let _ = tx.send(p);
                }
                wake();
            }
        })
        .map_err(|e| CloudError::Config(format!("can't watch the projects folder: {e}")))?;
        watcher.watch(root, RecursiveMode::Recursive).map_err(|e| CloudError::Config(format!("can't watch the projects folder: {e}")))?;
        Ok(ChangeWatcher { root: root.to_path_buf(), _watcher: watcher, rx, rules: crate::sync::ignore_rules(root), last_change: None, muted_until: None })
    }

    /// Ignore changes for `d` from now (a sync is writing, or just wrote, files).
    pub fn mute(&mut self, d: Duration) {
        let until = Instant::now() + d;
        self.muted_until = Some(self.muted_until.map_or(until, |m| m.max(until)));
        self.drain(Instant::now());
        self.last_change = None;
    }

    fn drain(&mut self, now: Instant) {
        let muted = self.muted_until.is_some_and(|m| now < m);
        while let Ok(path) = self.rx.try_recv() {
            let Ok(rel) = path.strip_prefix(&self.root) else { continue };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if rel.is_empty() {
                continue;
            }
            if rel == IgnoreRules::FILE {
                self.rules = crate::sync::ignore_rules(&self.root);
            }
            if muted || rel == ".creativecraft" || rel.starts_with(".creativecraft/") || is_ignored(&rel) || self.rules.matches(&rel) {
                continue;
            }
            self.last_change = Some(now);
        }
    }

    /// True once (then false until the next change) when something worth syncing changed and
    /// nothing has changed since for `quiet`.
    pub fn poll(&mut self, quiet: Duration) -> bool {
        let now = Instant::now();
        self.drain(now);
        match self.last_change {
            Some(t) if now.duration_since(t) >= quiet => {
                self.last_change = None;
                true
            }
            _ => false,
        }
    }

    /// A change is waiting for the quiet period to pass.
    pub fn pending(&self) -> bool {
        self.last_change.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_until(mut f: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn real_changes_trigger_once_after_quiet_and_ours_dont() {
        let root = std::env::temp_dir().join(format!("cc-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".creativecraft")).unwrap();
        std::fs::write(root.join(".creativecraftignore"), "renders/\n").unwrap();
        std::fs::create_dir_all(root.join("renders")).unwrap();
        let mut w = ChangeWatcher::new(&root, || {}).unwrap();
        let quiet = Duration::from_millis(200);

        // our own state, temp files and ignored folders: nothing to sync
        std::fs::write(root.join(".creativecraft/sync-base.json"), b"{}").unwrap();
        std::fs::write(root.join("renders/frame1.png"), b"x").unwrap();
        std::fs::write(root.join(".~lock.poster.odt#"), b"x").unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(!w.poll(quiet) && !w.pending(), "nothing worth syncing changed");

        // a real save: pending at once, due after the quiet period, then only once
        std::fs::write(root.join("poster.pcraft"), b"v1").unwrap();
        assert!(wait_until(|| {
            let _ = w.poll(Duration::from_secs(3600));
            w.pending()
        }));
        assert!(wait_until(|| w.poll(quiet)));
        assert!(!w.poll(quiet), "once per burst");

        // muted while a sync writes
        w.mute(Duration::from_secs(60));
        std::fs::write(root.join("downloaded.pcraft"), b"from the cloud").unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(!w.poll(quiet) && !w.pending());
        let _ = std::fs::remove_dir_all(&root);
    }
}
