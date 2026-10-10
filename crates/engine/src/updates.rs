//! Update checks: is there a newer version of an installed app (from wherever it installs: its
//! releases, a fork's, or a git branch) or of CreativeCraft itself? Checking never installs.
//! The network part runs from an [`UpdateJob`] copied out of the session, so a slow check
//! doesn't hold it.

use crate::{EngineError, Session};
use creativecraft_catalog::{AppSpec, Kind};
use creativecraft_install::{Installed, Source, UpdateCheck};
use serde_json::{Value, json};

/// The GitHub repository CreativeCraft's releases are published from.
fn hub_repo() -> Option<String> {
    let url = env!("CARGO_PKG_REPOSITORY");
    url.strip_prefix("https://github.com/").map(|r| r.trim_end_matches('/').trim_end_matches(".git").to_string())
}

/// Everything a check needs, copied out of the session.
#[derive(Debug, Clone)]
pub struct UpdateJob {
    apps: Vec<(AppSpec, Source, Installed)>,
    /// Also look for a newer CreativeCraft.
    hub: bool,
}

/// A newer version of an installed app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUpdate {
    pub id: String,
    pub name: String,
    pub have: String,
    /// A release tag, or `git-<commit>` for a git build.
    pub latest: String,
}

/// What a check found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateReport {
    pub updates: Vec<AppUpdate>,
    /// Apps checked and up to date.
    pub current: Vec<String>,
    /// A newer CreativeCraft: (version, release page).
    pub hub: Option<(String, String)>,
    /// (app name, why the check failed).
    pub failed: Vec<(String, String)>,
}

impl UpdateJob {
    pub fn run(&self) -> UpdateReport {
        let mut r = UpdateReport::default();
        for (app, source, installed) in &self.apps {
            match creativecraft_install::check_update(app, source, installed) {
                Ok(UpdateCheck::Available { have, latest }) => r.updates.push(AppUpdate { id: app.id.clone(), name: app.name.clone(), have, latest }),
                Ok(UpdateCheck::UpToDate { .. }) => r.current.push(app.name.clone()),
                Err(e) => r.failed.push((app.name.clone(), e.to_string())),
            }
        }
        if self.hub
            && let Some(repo) = hub_repo()
        {
            match creativecraft_install::newest_release(&repo) {
                Ok(rel) if creativecraft_platform::cmp_versions(&rel.tag, env!("CARGO_PKG_VERSION")).is_gt() => {
                    r.hub = Some((rel.tag.trim_start_matches('v').to_string(), format!("https://github.com/{repo}/releases/tag/{}", rel.tag)));
                }
                Ok(_) => {}
                Err(e) => r.failed.push(("CreativeCraft".into(), e.to_string())),
            }
        }
        r
    }
}

/// `v1.2.0` → `1.2.0`; `git-0123456789` → `commit 0123456`.
pub fn show_version(v: &str) -> String {
    match v.strip_prefix("git-") {
        Some(rev) => format!("commit {}", &rev[..rev.len().min(7)]),
        None => v.trim_start_matches('v').to_string(),
    }
}

impl UpdateReport {
    /// One line per finding, for the window's status, the CLI and agents.
    pub fn summary(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        if let Some((v, _)) = &self.hub {
            lines.push(format!("CreativeCraft {v} is available"));
        }
        for u in &self.updates {
            lines.push(format!("{} {} → {} is available", u.name, show_version(&u.have), show_version(&u.latest)));
        }
        if lines.is_empty() && self.failed.is_empty() {
            return match self.current.as_slice() {
                [] => "No apps installed by CreativeCraft to check".into(),
                [one] => format!("{one} is up to date"),
                all => format!("All {} apps are up to date", all.len()),
            };
        }
        for (name, why) in &self.failed {
            lines.push(format!("{name}: couldn't check ({why})"));
        }
        lines.join("\n")
    }

    pub fn json(&self) -> Value {
        json!({
            "updates": self.updates.iter().map(|u| json!({ "id": u.id, "name": u.name, "installed": u.have, "latest": u.latest })).collect::<Vec<_>>(),
            "up_to_date": self.current,
            "creativecraft": self.hub.as_ref().map(|(v, url)| json!({ "version": v, "release_page": url })),
            "failed": self.failed.iter().map(|(n, w)| json!({ "name": n, "error": w })).collect::<Vec<_>>(),
        })
    }
}

impl Session {
    /// A check of `only` (an app id) or of every app the hub installed, and of CreativeCraft
    /// itself when checking everything.
    pub fn update_job(&self, only: Option<&str>) -> Result<UpdateJob, EngineError> {
        let mut apps = Vec::new();
        for app in &self.catalog.apps {
            if only.is_some_and(|id| id != app.id) || app.kind != Kind::Release {
                continue;
            }
            if let Some(installed) = self.installed(&app.id) {
                apps.push((app.clone(), self.source(&app.id), installed.clone()));
            }
        }
        if let Some(id) = only
            && apps.is_empty()
        {
            let app = self.app(id)?;
            return Err(EngineError::Plugin(format!("{} isn't installed by CreativeCraft, so there's nothing to check", app.name)));
        }
        Ok(UpdateJob { apps, hub: only.is_none() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_say_what_was_found() {
        let up = AppUpdate { id: "photocraft".into(), name: "PhotoCraft".into(), have: "v0.5.0".into(), latest: "v0.6.0".into() };
        let r = UpdateReport { updates: vec![up], current: vec!["VectorCraft".into()], hub: Some(("0.3.0".into(), "https://x".into())), failed: vec![] };
        assert_eq!(r.summary(), "CreativeCraft 0.3.0 is available\nPhotoCraft 0.5.0 → 0.6.0 is available");
        let git = AppUpdate { id: "photocraft".into(), name: "PhotoCraft".into(), have: "git-0123456789".into(), latest: "git-abcdef0123".into() };
        assert_eq!(UpdateReport { updates: vec![git], ..Default::default() }.summary(), "PhotoCraft commit 0123456 → commit abcdef0 is available");
        assert_eq!(UpdateReport { current: vec!["PhotoCraft".into()], ..Default::default() }.summary(), "PhotoCraft is up to date");
        assert_eq!(UpdateReport { current: vec!["A".into(), "B".into()], ..Default::default() }.summary(), "All 2 apps are up to date");
        assert!(UpdateReport { failed: vec![("PhotoCraft".into(), "offline".into())], ..Default::default() }.summary().contains("couldn't check (offline)"));
    }

    #[test]
    fn the_hub_repository_is_known() {
        assert_eq!(hub_repo().as_deref(), Some("bhimio1/CreativeCraft"));
    }
}
