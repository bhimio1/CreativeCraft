//! Marketplace plugins: MCP servers from the plugin index that the user installed. Installing
//! checks the programs a plugin needs and runs its one-time setup steps (on the host, through
//! `HostCommand`); the hub then starts an enabled plugin on demand like an app's MCP server, its
//! tools named `<plugin id>__<tool>`.

use crate::{EngineError, Session};
use creativecraft_marketplace::{Index, Plugin};
use creativecraft_platform::{HostCommand, find_on_path};
use serde::{Deserialize, Serialize};
use std::process::{Child, Stdio};

/// An installed plugin, as saved in the settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginState {
    /// Off: kept installed, but agents don't get its tools.
    pub enabled: bool,
}

/// Where a fresh copy of the index is fetched from: this project's repository.
fn index_url() -> Option<String> {
    let repo = env!("CARGO_PKG_REPOSITORY").strip_prefix("https://github.com/")?.trim_end_matches('/');
    Some(format!("https://raw.githubusercontent.com/{repo}/main/marketplace/plugins.toml"))
}

/// The built-in index, or a fetched copy at least as new.
pub(crate) fn load_index(cache: &std::path::Path) -> Index {
    let builtin = Index::builtin().unwrap_or(Index { version: 0, plugins: Vec::new() });
    match std::fs::read_to_string(cache).ok().and_then(|t| Index::parse(&t).ok()) {
        Some(fetched) if fetched.version >= builtin.version => fetched,
        _ => builtin,
    }
}

impl Session {
    fn index_cache(&self) -> std::path::PathBuf {
        self.dirs.data().join("marketplace").join("plugins.toml")
    }

    /// Fetch the newest plugin index from the CreativeCraft repository.
    pub(crate) fn refresh_marketplace(&mut self) -> Result<String, EngineError> {
        let url = index_url().ok_or_else(|| EngineError::Plugin("this build doesn't know where its repository is".into()))?;
        let Some(text) = creativecraft_install::fetch_text(&url)? else {
            return Ok(format!("Using the built-in list of {} plugins; no newer one is published yet", self.marketplace.plugins.len()));
        };
        let fetched = Index::parse(&text).map_err(|e| EngineError::Plugin(format!("the published plugin list isn't valid: {e}")))?;
        if fetched.version < self.marketplace.version {
            return Ok(format!("{} plugins listed; this build's list is newer than the published one", self.marketplace.plugins.len()));
        }
        let cache = self.index_cache();
        if let Some(d) = cache.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&cache, &text)?;
        self.marketplace = fetched;
        Ok(format!("{} plugins listed", self.marketplace.plugins.len()))
    }

    pub fn plugin(&self, id: &str) -> Result<Plugin, EngineError> {
        self.marketplace.get(id).cloned().ok_or_else(|| EngineError::Plugin(format!("no plugin called {id}")))
    }

    /// Programs `plugin` needs that aren't on this computer: `(name, where to get it)`.
    pub fn missing_requirements(plugin: &Plugin) -> Vec<(String, String)> {
        plugin.requires.iter().filter(|r| find_on_path(&r.program).is_none()).map(|r| (r.name.clone(), r.url.clone())).collect()
    }

    pub(crate) fn install_plugin(&mut self, id: &str) -> Result<String, EngineError> {
        let plugin = self.plugin(id)?;
        if let Some((name, url)) = Self::missing_requirements(&plugin).into_iter().next() {
            return Err(EngineError::Plugin(format!("{} needs {name} first: get it from {url}", plugin.name)));
        }
        for step in &plugin.setup {
            let program = find_on_path(&step.command).ok_or_else(|| EngineError::Plugin(format!("{}: {} isn't installed", plugin.name, step.command)))?;
            let mut c = HostCommand::new(program);
            c.args(&step.args);
            let out = c.into_std().stdin(Stdio::null()).output()?;
            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr);
                let tail: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).collect();
                let tail = tail[tail.len().saturating_sub(4)..].join("\n");
                return Err(EngineError::Plugin(format!("{}: \"{}\" failed: {tail}", plugin.name, step.label)));
            }
        }
        self.settings.plugins.insert(id.to_string(), PluginState { enabled: true });
        self.save_settings()?;
        let notes = if plugin.notes.is_empty() { String::new() } else { format!(" {}", plugin.notes) };
        Ok(format!("{} installed.{notes}", plugin.name))
    }

    pub(crate) fn remove_plugin(&mut self, id: &str) -> Result<String, EngineError> {
        let name = self.marketplace.get(id).map_or(id.to_string(), |p| p.name.clone());
        if self.settings.plugins.remove(id).is_none() {
            return Err(EngineError::Plugin(format!("{name} isn't installed")));
        }
        self.save_settings()?;
        Ok(format!("{name} removed. Anything its setup added to an app (an add-on) stays there until you remove it in that app."))
    }

    pub(crate) fn set_plugin_enabled(&mut self, id: &str, enabled: bool) -> Result<String, EngineError> {
        let name = self.marketplace.get(id).map_or(id.to_string(), |p| p.name.clone());
        let state = self.settings.plugins.get_mut(id).ok_or_else(|| EngineError::Plugin(format!("{name} isn't installed")))?;
        state.enabled = enabled;
        self.save_settings()?;
        Ok(format!("{name} {}", if enabled { "turned on" } else { "turned off" }))
    }

    /// Start an installed, enabled plugin's MCP server, if `id` is one.
    pub(crate) fn spawn_plugin(&self, id: &str) -> Option<Result<Child, EngineError>> {
        if !self.settings.plugins.get(id).is_some_and(|s| s.enabled) {
            return None;
        }
        let plugin = self.marketplace.get(id)?;
        Some((|| {
            let program = find_on_path(&plugin.run.command)
                .ok_or_else(|| EngineError::Plugin(format!("{} needs {} on this computer", plugin.name, plugin.run.command)))?;
            let mut c = HostCommand::new(program);
            c.args(&plugin.run.args);
            Ok(c.into_std().stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn()?)
        })())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_url_points_at_this_repository() {
        let url = index_url().unwrap();
        assert!(url.starts_with("https://raw.githubusercontent.com/") && url.ends_with("/main/marketplace/plugins.toml"), "{url}");
    }

    #[test]
    fn a_fetched_index_is_used_only_when_newer_or_equal() {
        let dir = std::env::temp_dir().join(format!("cc-index-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("plugins.toml");
        let builtin = Index::builtin().unwrap();
        std::fs::write(&cache, "version = 0\n").unwrap();
        assert_eq!(load_index(&cache).plugins.len(), builtin.plugins.len(), "an older copy is ignored");
        std::fs::write(&cache, format!("version = {}\n", builtin.version + 1)).unwrap();
        assert!(load_index(&cache).plugins.is_empty(), "a newer copy wins");
        std::fs::write(&cache, "not toml [").unwrap();
        assert_eq!(load_index(&cache).plugins.len(), builtin.plugins.len(), "a broken copy is ignored");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
