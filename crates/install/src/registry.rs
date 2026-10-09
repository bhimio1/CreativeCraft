use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One installed app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub version: String,
    pub dir: PathBuf,
    pub gui: Option<PathBuf>,
    pub cli: Option<PathBuf>,
}

/// `apps/installed.json`: app id → what is installed.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Registry {
    apps: BTreeMap<String, Installed>,
}

impl Registry {
    pub fn load(path: &Path) -> Result<Registry, crate::InstallError> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Written to a temp file and renamed, so a crash never leaves a half-written registry.
    pub fn save(&self, path: &Path) -> Result<(), crate::InstallError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&Installed> {
        self.apps.get(id)
    }
    pub fn set(&mut self, id: &str, i: Installed) -> Option<Installed> {
        self.apps.insert(id.to_string(), i)
    }
    pub fn remove(&mut self, id: &str) -> Option<Installed> {
        self.apps.remove(id)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Installed)> {
        self.apps.iter()
    }
}
