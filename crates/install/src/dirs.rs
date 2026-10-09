use std::path::{Path, PathBuf};

/// The hub's folders. `CREATIVECRAFT_HOME` puts everything under one folder (tests, portable installs).
#[derive(Debug, Clone)]
pub struct HubDirs {
    data: PathBuf,
    config: PathBuf,
}

impl HubDirs {
    pub fn discover() -> Option<HubDirs> {
        if let Some(home) = std::env::var_os("CREATIVECRAFT_HOME") {
            return Some(HubDirs::under(Path::new(&home)));
        }
        let p = directories::ProjectDirs::from("org", "CreativeCraft", "CreativeCraft")?;
        Some(HubDirs { data: p.data_dir().to_path_buf(), config: p.config_dir().to_path_buf() })
    }

    pub fn under(root: &Path) -> HubDirs {
        HubDirs { data: root.join("data"), config: root.join("config") }
    }

    pub fn apps(&self) -> PathBuf {
        self.data.join("apps")
    }
    pub fn downloads(&self) -> PathBuf {
        self.data.join("downloads")
    }
    pub fn registry_file(&self) -> PathBuf {
        self.apps().join("installed.json")
    }
    pub fn config(&self) -> &Path {
        &self.config
    }
    pub fn data(&self) -> &Path {
        &self.data
    }
    /// Optional user catalog overlay.
    pub fn catalog_overlay(&self) -> PathBuf {
        self.config.join("apps.toml")
    }
    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }
}
