//! Marketplace plugins: MCP servers from the plugin index that the user installed. Installing
//! checks the programs a plugin needs and runs its one-time setup steps (on the host, through
//! `HostCommand`); the hub then starts an enabled plugin on demand like an app's MCP server, its
//! tools named `<plugin id>__<tool>`.

use crate::{EngineError, Session};
use creativecraft_marketplace::{AudioIndex, Index, Plugin};
use creativecraft_platform::{HostCommand, find_on_path};
use serde::{Deserialize, Serialize};
use std::process::{Child, Stdio};

/// An installed plugin, as saved in the settings. Fields added after the first release have
/// defaults, so older `settings.json` files still load.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginState {
    /// Off: kept installed, but agents don't get its tools. Always off for audio plugins, which
    /// have no tools.
    pub enabled: bool,
    /// Audio plugins: the release tag installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Audio plugins: exactly what was copied into the plugin folders (what Remove deletes).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<std::path::PathBuf>,
}

/// Where a fresh copy of an index file is fetched from: this project's repository.
fn index_url(file: &str) -> Option<String> {
    let repo = env!("CARGO_PKG_REPOSITORY").strip_prefix("https://github.com/")?.trim_end_matches('/');
    Some(format!("https://raw.githubusercontent.com/{repo}/main/marketplace/{file}"))
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

    fn audio_index_cache(&self) -> std::path::PathBuf {
        self.dirs.data().join("marketplace").join("audio-plugins.toml")
    }

    /// Fetch the newest plugin indexes (MCP plugins and audio plugins) from the CreativeCraft
    /// repository. Each is used only when at least as new as the one in use, and the two must not
    /// list the same id.
    pub(crate) fn refresh_marketplace(&mut self) -> Result<String, EngineError> {
        let fetch = |file: &str| -> Result<Option<String>, EngineError> {
            let url = index_url(file).ok_or_else(|| EngineError::Plugin("this build doesn't know where its repository is".into()))?;
            Ok(creativecraft_install::fetch_text(&url)?)
        };
        let mcp_text = fetch("plugins.toml")?;
        let audio_text = fetch("audio-plugins.toml")?;
        let mcp = match &mcp_text {
            Some(t) => Some(Index::parse(t).map_err(|e| EngineError::Plugin(format!("the published plugin list isn't valid: {e}")))?),
            None => None,
        };
        let audio = match &audio_text {
            Some(t) => Some(AudioIndex::parse(t).map_err(|e| EngineError::Plugin(format!("the published audio plugin list isn't valid: {e}")))?),
            None => None,
        };
        let mcp_newer = mcp.as_ref().is_some_and(|m| m.version >= self.marketplace.version);
        let audio_newer = audio.as_ref().is_some_and(|a| a.version >= self.audio_plugins.version);
        let next_mcp = mcp.filter(|_| mcp_newer).unwrap_or_else(|| self.marketplace.clone());
        let next_audio = audio.filter(|_| audio_newer).unwrap_or_else(|| self.audio_plugins.clone());
        creativecraft_marketplace::check_ids_unique(&next_mcp, &next_audio)
            .map_err(|e| EngineError::Plugin(format!("the published plugin lists don't fit together: {e}")))?;
        let write = |path: std::path::PathBuf, text: &str| -> Result<(), EngineError> {
            if let Some(d) = path.parent() {
                std::fs::create_dir_all(d)?;
            }
            Ok(std::fs::write(path, text)?)
        };
        if let (true, Some(t)) = (mcp_newer, &mcp_text) {
            write(self.index_cache(), t)?;
        }
        if let (true, Some(t)) = (audio_newer, &audio_text) {
            write(self.audio_index_cache(), t)?;
        }
        self.marketplace = next_mcp;
        self.audio_plugins = next_audio;
        let listed = format!("{} plugins and {} audio plugins listed", self.marketplace.plugins.len(), self.audio_plugins.plugins.len());
        Ok(match (mcp_text.is_some() || audio_text.is_some(), mcp_newer || audio_newer) {
            (false, _) => format!("Using the built-in lists ({listed}); no newer ones are published yet"),
            (true, false) => format!("{listed}; this build's lists are newer than the published ones"),
            (true, true) => listed,
        })
    }

    pub fn plugin(&self, id: &str) -> Result<Plugin, EngineError> {
        self.marketplace.get(id).cloned().ok_or_else(|| EngineError::Plugin(format!("no plugin called {id}")))
    }

    /// Programs `plugin` needs that aren't on this computer: `(name, where to get it)`.
    pub fn missing_requirements(plugin: &Plugin) -> Vec<(String, String)> {
        plugin.requires.iter().filter(|r| find_on_path(&r.program).is_none()).map(|r| (r.name.clone(), r.url.clone())).collect()
    }

    pub(crate) fn install_plugin(&mut self, id: &str, events: &mut dyn FnMut(crate::Event)) -> Result<String, EngineError> {
        if self.audio_plugins.get(id).is_some() {
            return self.install_audio_plugin(id, events);
        }
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
        self.settings.plugins.insert(id.to_string(), PluginState { enabled: true, ..PluginState::default() });
        self.save_settings()?;
        let notes = if plugin.notes.is_empty() { String::new() } else { format!(" {}", plugin.notes) };
        Ok(format!("{} installed.{notes}", plugin.name))
    }

    pub(crate) fn remove_plugin(&mut self, id: &str) -> Result<String, EngineError> {
        if self.is_audio_plugin(id) {
            return self.remove_audio_plugin(id);
        }
        let name = self.marketplace.get(id).map_or(id.to_string(), |p| p.name.clone());
        if self.settings.plugins.remove(id).is_none() {
            return Err(EngineError::Plugin(format!("{name} isn't installed")));
        }
        self.save_settings()?;
        Ok(format!("{name} removed. Anything its setup added to an app (an add-on) stays there until you remove it in that app."))
    }

    pub(crate) fn set_plugin_enabled(&mut self, id: &str, enabled: bool) -> Result<String, EngineError> {
        if self.is_audio_plugin(id) {
            let name = self.audio_plugins.get(id).map_or(id.to_string(), |p| p.name.clone());
            return Err(EngineError::Plugin(format!("{name} is an audio plugin for music apps; it has no tools for agents to turn on or off")));
        }
        let name = self.marketplace.get(id).map_or(id.to_string(), |p| p.name.clone());
        let state = self.settings.plugins.get_mut(id).ok_or_else(|| EngineError::Plugin(format!("{name} isn't installed")))?;
        state.enabled = enabled;
        self.save_settings()?;
        Ok(format!("{name} {}", if enabled { "turned on" } else { "turned off" }))
    }

    /// Start an installed, enabled plugin's MCP server, if `id` is one.
    pub(crate) fn spawn_plugin(&self, id: &str) -> Option<Result<Child, EngineError>> {
        if !self.settings.plugins.get(id).is_some_and(|s| s.enabled) || self.is_audio_plugin(id) {
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
        let url = index_url("plugins.toml").unwrap();
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

    #[test]
    fn old_plugin_settings_still_load() {
        let old: PluginState = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert_eq!(old, PluginState { enabled: true, version: None, files: Vec::new() });
        // and an MCP plugin's state is written as before
        assert_eq!(serde_json::to_string(&old).unwrap(), r#"{"enabled":true}"#);
        let audio = PluginState { enabled: false, version: Some("v0.3.0".into()), files: vec!["/home/me/.clap/X.clap".into()] };
        assert_eq!(serde_json::from_str::<PluginState>(&serde_json::to_string(&audio).unwrap()).unwrap(), audio);
        let settings: crate::Settings = serde_json::from_str(r#"{"plugins":{"blender-mcp":{"enabled":false}}}"#).unwrap();
        assert_eq!(settings.plugins["blender-mcp"], PluginState::default());
    }
}
