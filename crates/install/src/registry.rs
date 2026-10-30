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
    /// Where it came from, when not the official releases: "fork alice/photocraft",
    /// "git alice/photocraft (dev)".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Catalog ids that changed: (former id, current id).
const RENAMED_IDS: &[(&str, &str)] = &[("solvecraft", "solidcraft")];

/// `apps/installed.json`: app id → what is installed.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Registry {
    apps: BTreeMap<String, Installed>,
}

impl Registry {
    pub fn load(path: &Path) -> Result<Registry, crate::InstallError> {
        let mut registry: Registry = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Registry::default()),
            Err(e) => return Err(e.into()),
        };
        registry.rename_ids();
        Ok(registry)
    }

    /// Installs made under an app's former catalog id carry over to its new one; the next update
    /// installs under the new id and removes the old version folder.
    fn rename_ids(&mut self) {
        for (old, new) in RENAMED_IDS {
            if !self.apps.contains_key(*new)
                && let Some(i) = self.apps.remove(*old)
            {
                self.apps.insert((*new).to_string(), i);
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(version: &str) -> Installed {
        Installed { version: version.into(), dir: PathBuf::from(version), gui: None, cli: None, source: None }
    }

    #[test]
    fn installs_under_a_former_id_carry_over() {
        let dir = std::env::temp_dir().join(format!("creativecraft-registry-{}", std::process::id()));
        let path = dir.join("installed.json");
        let mut r = Registry::default();
        r.set("solvecraft", installed("v0.4.0"));
        r.save(&path).unwrap();
        let r = Registry::load(&path).unwrap();
        assert_eq!(r.get("solidcraft"), Some(&installed("v0.4.0")));
        assert_eq!(r.get("solvecraft"), None);

        // an install under the new id wins over a leftover under the old one
        let mut both = Registry::default();
        both.set("solvecraft", installed("v0.4.0"));
        both.set("solidcraft", installed("v0.27.1"));
        both.rename_ids();
        assert_eq!(both.get("solidcraft"), Some(&installed("v0.27.1")));
        let _ = std::fs::remove_dir_all(dir);
    }
}
