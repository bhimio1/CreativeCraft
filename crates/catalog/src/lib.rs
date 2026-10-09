//! The app catalog (L1): pure data describing every app the hub knows about.
//!
//! The built-in catalog is `catalog/apps.toml`, compiled in. A user or organisation can drop an
//! `apps.toml` into the config folder to add apps; entries with the same `id` replace built-ins.
#![cfg_attr(test, allow(clippy::unwrap_used))]

use serde::Deserialize;

const BUILTIN: &str = include_str!("../../../catalog/apps.toml");

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("catalog is not valid TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("catalog entry `{0}`: {1}")]
    Invalid(String, &'static str),
}

/// How the hub obtains an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Portable archive from the project's GitHub releases, installed and updated by the hub.
    Release,
    /// Installed by the user from the official site; the hub detects and launches it.
    External,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSpec {
    pub id: String,
    pub name: String,
    pub category: String,
    pub summary: String,
    pub kind: Kind,
    pub license: String,
    pub homepage: String,
    /// Where to report a bug. Release apps default to their GitHub issues; external apps name
    /// their project's own tracker.
    #[serde(default)]
    pub bugs: Option<String>,
    /// File extensions (lowercase, no dot) this app opens; the first is its own project format.
    /// The Projects page uses them to pick an app and an icon for each file.
    #[serde(default)]
    pub opens: Vec<String>,

    // kind = "release"
    /// `owner/name` on GitHub.
    #[serde(default)]
    pub repo: Option<String>,
    /// Executable stem of the desktop app inside the archive.
    #[serde(default)]
    pub gui: Option<String>,
    /// Executable stem of the headless CLI inside the archive.
    #[serde(default)]
    pub cli: Option<String>,
    /// Arguments that start the CLI as an MCP server on stdio.
    #[serde(default)]
    pub mcp: Option<Vec<String>>,

    // kind = "external"
    #[serde(default)]
    pub path_names: Vec<String>,
    #[serde(default)]
    pub mac_bundle: Option<String>,
    #[serde(default)]
    pub windows_dirs: Vec<String>,
}

impl AppSpec {
    pub fn has_mcp(&self) -> bool {
        self.cli.is_some() && self.mcp.is_some()
    }

    /// The `owner/name` of the GitHub repository issues go to, if bugs are reported on GitHub.
    pub fn github_issues_repo(&self) -> Option<String> {
        if let Some(url) = &self.bugs {
            let rest = url.strip_prefix("https://github.com/")?;
            let mut parts = rest.split('/');
            let (owner, name) = (parts.next()?, parts.next()?);
            return (parts.next() == Some("issues")).then(|| format!("{owner}/{name}"));
        }
        self.repo.clone()
    }

    fn validate(&self) -> Result<(), CatalogError> {
        let bad = |why| Err(CatalogError::Invalid(self.id.clone(), why));
        if self.id.is_empty() || !self.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return bad("id must be lowercase ascii letters, digits or '-'");
        }
        match self.kind {
            Kind::Release => {
                if self.repo.as_deref().is_none_or(|r| r.split('/').count() != 2) {
                    return bad("release apps need repo = \"owner/name\"");
                }
                if self.gui.is_none() && self.cli.is_none() {
                    return bad("release apps need a gui or cli executable");
                }
            }
            Kind::External => {
                if self.path_names.is_empty() && self.mac_bundle.is_none() && self.windows_dirs.is_empty() {
                    return bad("external apps need at least one way to be detected");
                }
            }
        }
        if self.opens.iter().any(|e| e.is_empty() || e.starts_with('.') || e.chars().any(|c| c.is_ascii_uppercase())) {
            return bad("opens lists lowercase extensions without the dot");
        }
        if self.mcp.is_some() && self.cli.is_none() {
            return bad("mcp needs a cli executable to run");
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct File {
    #[serde(default)]
    app: Vec<AppSpec>,
}

#[derive(Debug, Clone)]
pub struct Catalog {
    pub apps: Vec<AppSpec>,
}

impl Catalog {
    /// The compiled-in catalog.
    pub fn builtin() -> Result<Catalog, CatalogError> {
        Catalog::parse(BUILTIN)
    }

    pub fn parse(text: &str) -> Result<Catalog, CatalogError> {
        let file: File = toml::from_str(text)?;
        for a in &file.app {
            a.validate()?;
        }
        Ok(Catalog { apps: file.app })
    }

    /// Add or replace entries from `overlay` (matched by id); new ids are appended.
    pub fn merge(&mut self, overlay: Catalog) {
        for app in overlay.apps {
            match self.apps.iter_mut().find(|a| a.id == app.id) {
                Some(slot) => *slot = app,
                None => self.apps.push(app),
            }
        }
    }

    pub fn get(&self, id: &str) -> Option<&AppSpec> {
        self.apps.iter().find(|a| a.id == id)
    }

    /// Apps that open files with extension `ext` (any case), in catalog order: an app whose own
    /// project format it is comes first.
    pub fn apps_for_extension(&self, ext: &str) -> Vec<&AppSpec> {
        let ext = ext.to_ascii_lowercase();
        let mut apps: Vec<&AppSpec> = self.apps.iter().filter(|a| a.opens.contains(&ext)).collect();
        apps.sort_by_key(|a| a.opens.first() != Some(&ext));
        apps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_catalog_is_valid() {
        let c = Catalog::builtin().unwrap();
        assert!(c.get("photocraft").unwrap().has_mcp());
        assert_eq!(c.get("blender").unwrap().kind, Kind::External);
        let mut ids: Vec<_> = c.apps.iter().map(|a| &a.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), c.apps.len(), "duplicate ids");
    }

    #[test]
    fn overlay_replaces_and_appends() {
        let mut c = Catalog::builtin().unwrap();
        let n = c.apps.len();
        let o = Catalog::parse(
            r#"
            [[app]]
            id = "blender"
            name = "Blender LTS"
            category = "3D"
            summary = "s"
            kind = "external"
            path_names = ["blender-lts"]
            license = "GPL-2.0-or-later"
            homepage = "https://example.org"

            [[app]]
            id = "my-tool"
            name = "My Tool"
            category = "Misc"
            summary = "s"
            kind = "release"
            repo = "me/my-tool"
            cli = "my-tool"
            mcp = ["serve", "--stdio"]
            license = "MIT"
            homepage = "https://example.org"
            "#,
        )
        .unwrap();
        c.merge(o);
        assert_eq!(c.apps.len(), n + 1);
        assert_eq!(c.get("blender").unwrap().name, "Blender LTS");
    }

    #[test]
    fn rejects_bad_entries() {
        let e = Catalog::parse(
            r#"
            [[app]]
            id = "Bad Id"
            name = "x"
            category = "x"
            summary = "x"
            kind = "external"
            path_names = ["x"]
            license = "MIT"
            homepage = "x"
            "#,
        );
        assert!(e.is_err());
    }
}
