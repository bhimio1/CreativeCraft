//! Three-way sync planning. **Pure logic, std only** (L0): no I/O, no clocks, fully deterministic.
//!
//! Inputs are three snapshots of the projects folder, each a map of relative path → content hash:
//!
//! - **local**: what is on disk now,
//! - **remote**: what is in cloud storage now,
//! - **base**: what both sides held after the last successful sync.
//!
//! Comparing each side against the base tells us *who changed what*, which a two-way diff cannot.
//! The hash only has to be the same function on both sides (the Google Drive backend uses MD5,
//! because Drive reports `md5Checksum` for every file).
//!
//! Safety rules, in priority order:
//! 1. **Never lose work.** When both sides changed a file differently, nothing is overwritten:
//!    the cloud copy is downloaded beside the local one under a conflict name.
//! 2. **An edit beats a delete.** If one side deleted a file the other side edited, the edit is
//!    restored rather than the deletion propagated.
//! 3. Deletions are only propagated for files the base proves the other side had already seen;
//!    executors move them to the trash / OS recycle bin, they never hard-delete.
#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::collections::{BTreeMap, BTreeSet};

/// Relative path (always `/`-separated) → content hash.
pub type Snapshot = BTreeMap<String, String>;

/// One step of a sync plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Local file is new or changed: upload it.
    Upload(String),
    /// Remote file is new or changed: download it.
    Download(String),
    /// File was deleted locally and untouched remotely: move the remote copy to the cloud trash.
    DeleteRemote(String),
    /// File was deleted remotely and untouched locally: move the local copy to the recycle bin.
    DeleteLocal(String),
    /// Both sides changed it differently: keep local as is, download remote as `conflict_path`.
    Conflict { path: String, conflict_path: String },
}

impl Action {
    pub fn path(&self) -> &str {
        match self {
            Action::Upload(p) | Action::Download(p) | Action::DeleteRemote(p) | Action::DeleteLocal(p) => p,
            Action::Conflict { path, .. } => path,
        }
    }
}

/// Plan the actions that bring local and remote into agreement.
///
/// `device` names this machine; it goes into conflict file names so a user can tell copies apart.
/// Actions come out sorted by path, so plans are reproducible and easy to show in a UI.
pub fn plan(local: &Snapshot, remote: &Snapshot, base: &Snapshot, device: &str) -> Vec<Action> {
    let paths: BTreeSet<&String> = local.keys().chain(remote.keys()).chain(base.keys()).collect();
    let mut out = Vec::new();
    for path in paths {
        let (l, r, b) = (local.get(path), remote.get(path), base.get(path));
        let action = match (l, r) {
            // identical on both sides (or gone from both): nothing to move
            (Some(l), Some(r)) if l == r => None,
            (None, None) => None,

            (Some(l), Some(r)) => match b {
                Some(b) if b == l => Some(Action::Download(path.clone())), // only remote changed
                Some(b) if b == r => Some(Action::Upload(path.clone())),   // only local changed
                // both changed (or both created) with different contents
                _ => Some(Action::Conflict { path: path.clone(), conflict_path: conflict_name(path, device, local, remote) }),
            },

            (Some(l), None) => match b {
                None => Some(Action::Upload(path.clone())),                   // new locally
                Some(b) if b == l => Some(Action::DeleteLocal(path.clone())), // deleted in cloud, untouched here
                Some(_) => Some(Action::Upload(path.clone())),                // deleted in cloud but edited here: keep the edit
            },

            (None, Some(r)) => match b {
                None => Some(Action::Download(path.clone())),                  // new in cloud
                Some(b) if b == r => Some(Action::DeleteRemote(path.clone())), // deleted here, untouched in cloud
                Some(_) => Some(Action::Download(path.clone())),               // deleted here but edited in cloud: keep the edit
            },
        };
        out.extend(action);
    }
    out
}

/// The base to store after a plan has been fully applied: the agreed state of every file.
///
/// A conflict is applied by the executor as: download remote `path` → local `conflict_path`,
/// upload `conflict_path`, upload local `path` over the remote one. Afterwards both sides hold the
/// local hash at `path` and the old remote hash at `conflict_path`.
pub fn next_base(local: &Snapshot, remote: &Snapshot, actions: &[Action]) -> Snapshot {
    let mut next = Snapshot::new();
    // unchanged, identical files
    for (p, h) in local {
        if remote.get(p) == Some(h) {
            next.insert(p.clone(), h.clone());
        }
    }
    for a in actions {
        match a {
            Action::Upload(p) => {
                if let Some(h) = local.get(p) {
                    next.insert(p.clone(), h.clone());
                }
            }
            Action::Download(p) => {
                if let Some(h) = remote.get(p) {
                    next.insert(p.clone(), h.clone());
                }
            }
            Action::DeleteLocal(p) | Action::DeleteRemote(p) => {
                next.remove(p);
            }
            Action::Conflict { path, conflict_path } => {
                if let Some(h) = local.get(path) {
                    next.insert(path.clone(), h.clone());
                }
                if let Some(h) = remote.get(path) {
                    next.insert(conflict_path.clone(), h.clone());
                }
            }
        }
    }
    next
}

/// `art/poster.pcraft` → `art/poster (cloud copy from laptop).pcraft`, numbered if taken.
pub fn conflict_name(path: &str, device: &str, local: &Snapshot, remote: &Snapshot) -> String {
    let (dir, file) = match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    };
    // keep the extension (including double ones like .tar.gz would be overkill; one is enough)
    let (stem, ext) = match file.rfind('.') {
        Some(i) if i > 0 => (&file[..i], &file[i..]),
        _ => (file, ""),
    };
    let device: String = device.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == ' ' { c } else { '_' }).collect();
    let mut n = 1;
    loop {
        let suffix = if n == 1 { String::new() } else { format!(" {n}") };
        let candidate = format!("{dir}{stem} (cloud copy for {device}{suffix}){ext}");
        if !local.contains_key(&candidate) && !remote.contains_key(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Paths that should never be synced: OS litter, editor temp files, the hub's own state.
pub fn is_ignored(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    rel.split('/').any(|seg| seg == ".atelier" || seg == ".git")
        || matches!(name, ".DS_Store" | "Thumbs.db" | "desktop.ini")
        || name.starts_with("~$")
        || name.starts_with(".~lock.")
        || name.ends_with(".tmp")
        || name.ends_with(".part")
        || name.ends_with(".swp")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(items: &[(&str, &str)]) -> Snapshot {
        items.iter().map(|(p, h)| (p.to_string(), h.to_string())).collect()
    }

    #[test]
    fn first_sync_merges_both_sides() {
        let local = snap(&[("a.pcraft", "1")]);
        let remote = snap(&[("b.vcraft", "2")]);
        let p = plan(&local, &remote, &Snapshot::new(), "laptop");
        assert_eq!(p, vec![Action::Upload("a.pcraft".into()), Action::Download("b.vcraft".into())]);
    }

    #[test]
    fn one_sided_edits() {
        let base = snap(&[("a", "1"), ("b", "1")]);
        let local = snap(&[("a", "2"), ("b", "1")]);
        let remote = snap(&[("a", "1"), ("b", "3")]);
        assert_eq!(plan(&local, &remote, &base, "d"), vec![Action::Upload("a".into()), Action::Download("b".into())]);
    }

    #[test]
    fn both_edited_is_a_conflict_never_an_overwrite() {
        let base = snap(&[("art/poster.pcraft", "1")]);
        let local = snap(&[("art/poster.pcraft", "2")]);
        let remote = snap(&[("art/poster.pcraft", "3")]);
        let p = plan(&local, &remote, &base, "laptop");
        assert_eq!(p, vec![Action::Conflict { path: "art/poster.pcraft".into(), conflict_path: "art/poster (cloud copy for laptop).pcraft".into() }]);
    }

    #[test]
    fn both_created_differently_is_a_conflict() {
        let p = plan(&snap(&[("x", "1")]), &snap(&[("x", "2")]), &Snapshot::new(), "d");
        assert!(matches!(p[0], Action::Conflict { .. }));
    }

    #[test]
    fn same_edit_on_both_sides_is_a_no_op() {
        let p = plan(&snap(&[("x", "2")]), &snap(&[("x", "2")]), &snap(&[("x", "1")]), "d");
        assert!(p.is_empty());
    }

    #[test]
    fn deletions_propagate_only_when_the_other_side_is_unchanged() {
        let base = snap(&[("gone-here", "1"), ("gone-there", "1")]);
        let local = snap(&[("gone-there", "1")]);
        let remote = snap(&[("gone-here", "1")]);
        assert_eq!(plan(&local, &remote, &base, "d"), vec![Action::DeleteRemote("gone-here".into()), Action::DeleteLocal("gone-there".into())]);
    }

    #[test]
    fn edit_beats_delete() {
        let base = snap(&[("a", "1"), ("b", "1")]);
        let local = snap(&[("a", "2")]); // a edited here, b deleted here
        let remote = snap(&[("b", "9")]); // a deleted there, b edited there
        assert_eq!(plan(&local, &remote, &base, "d"), vec![Action::Upload("a".into()), Action::Download("b".into())]);
    }

    #[test]
    fn conflict_names_do_not_collide() {
        let local = snap(&[("p.png", "1"), ("p (cloud copy for d).png", "x")]);
        assert_eq!(conflict_name("p.png", "d", &local, &Snapshot::new()), "p (cloud copy for d 2).png");
        assert_eq!(conflict_name("README", "my/pc", &Snapshot::new(), &Snapshot::new()), "README (cloud copy for my_pc)");
        assert_eq!(conflict_name(".hidden", "d", &Snapshot::new(), &Snapshot::new()), ".hidden (cloud copy for d)");
    }

    #[test]
    fn next_base_after_a_full_apply_matches_both_sides() {
        let base = snap(&[("a", "1"), ("c", "1"), ("k", "1")]);
        let local = snap(&[("a", "2"), ("k", "5"), ("n", "7")]);
        let remote = snap(&[("a", "1"), ("c", "1"), ("k", "6")]);
        let actions = plan(&local, &remote, &base, "d");
        let next = next_base(&local, &remote, &actions);
        assert_eq!(next, snap(&[("a", "2"), ("k", "5"), ("k (cloud copy for d)", "6"), ("n", "7")]));
        // and planning again from the applied state does nothing
        assert!(plan(&next, &next, &next, "d").is_empty());
    }

    #[test]
    fn ignored_paths() {
        assert!(is_ignored("x/.DS_Store"));
        assert!(is_ignored(".atelier/state.json"));
        assert!(is_ignored("doc/~$report.docx"));
        assert!(is_ignored("big.psd.part"));
        assert!(!is_ignored("art/poster.pcraft"));
    }
}
