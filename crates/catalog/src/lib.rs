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

/// Live mode: the desktop app runs with a localhost control channel and the CLI's MCP server
/// drives that window, so the user sees the agent work and can take over. Placeholders:
/// `{port}` (a free local port), `{token}` (a private token file for apps that authenticate the
/// channel) and `{projects}` (the projects folder).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Live {
    /// Arguments for the desktop app (`gui`).
    pub gui: Vec<String>,
    /// Arguments for the CLI that serve MCP against that window.
    pub mcp: Vec<String>,
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
    /// Arguments that start the CLI as an MCP server on stdio, headless (no window).
    /// `{projects}` becomes the projects folder.
    #[serde(default)]
    pub mcp: Option<Vec<String>>,
    /// How to let an agent work in the app's visible window instead (see [`Live`]).
    #[serde(default)]
    pub live: Option<Live>,

    // kind = "external"
    #[serde(default)]
    pub path_names: Vec<String>,
    #[serde(default)]
    pub mac_bundle: Option<String>,
    #[serde(default)]
    pub windows_dirs: Vec<String>,
    /// Its Flathub app id, to find a Flatpak install (`exports/bin/<id>`).
    #[serde(default)]
    pub flatpak: Option<String>,
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
        if let Some(live) = &self.live {
            if self.gui.is_none() || self.cli.is_none() {
                return bad("live needs both a gui and a cli executable");
            }
            if !live.gui.iter().any(|a| a.contains("{port}")) || !live.mcp.iter().any(|a| a.contains("{port}")) {
                return bad("live.gui and live.mcp must both use {port}");
            }
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

    /// MusicCraft's MCP server is its own program: no subcommand, and live mode both authenticates
    /// the window's control channel and lands relative paths in the projects folder.
    #[test]
    fn musiccraft_is_agent_ready() {
        let c = Catalog::builtin().unwrap();
        let m = c.get("musiccraft").unwrap();
        assert!(m.has_mcp());
        assert_eq!(m.cli.as_deref(), Some("musiccraft-mcp"));
        assert_eq!(m.mcp.as_deref().unwrap(), ["--dir", "{projects}"]);
        let live = m.live.as_ref().unwrap();
        assert!(live.gui.windows(2).any(|w| w == ["--control-token-file", "{token}"]));
        assert!(live.gui.windows(2).any(|w| w == ["--dir", "{projects}"]));
        assert_eq!(live.mcp, ["--bridge", "127.0.0.1:{port}", "--control-token-file", "{token}"]);
        assert_eq!(m.opens.first().map(String::as_str), Some("mcproj"));
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

    #[test]
    fn office_files_prefer_their_own_apps() {
        let c = Catalog::builtin().unwrap();
        let first = |ext: &str| c.apps_for_extension(ext).first().map(|a| a.id.clone());
        // LibreOffice's own format opens in it; Office formats stay with the Craft apps, with
        // LibreOffice offered as another app that opens them
        assert_eq!(first("odt").as_deref(), Some("libreoffice"));
        assert_eq!(first("ods").as_deref(), Some("libreoffice"));
        assert_eq!(first("docx").as_deref(), Some("wordcraft"));
        assert_eq!(first("xlsx").as_deref(), Some("gridcraft"));
        assert!(c.apps_for_extension("pptx").iter().any(|a| a.id == "libreoffice"));
    }
}
