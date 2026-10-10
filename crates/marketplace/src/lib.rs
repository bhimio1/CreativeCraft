//! The plugin index (L1): pure data describing the plugins the Marketplace lists.
//!
//! The built-in index is `marketplace/plugins.toml`, compiled in. A newer copy fetched from the
//! CreativeCraft repository replaces it when its `version` is at least the built-in one's (see the
//! engine). Plugins are MCP servers run from their publishers' own packages; nothing is hosted here.
#![cfg_attr(test, allow(clippy::unwrap_used))]

use serde::Deserialize;

const BUILTIN: &str = include_str!("../../../marketplace/plugins.toml");

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("plugin index is not valid TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("plugin `{0}`: {1}")]
    Invalid(String, &'static str),
}

/// A program run on the user's machine.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// A program that must be on the user's `PATH` before the plugin can work.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    /// What to look for on `PATH` (`uvx`).
    pub program: String,
    /// What the user installs to get it (`uv`).
    pub name: String,
    /// Where to get it.
    pub url: String,
}

/// A one-time step run when the plugin is installed, shown to the user first.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub label: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plugin {
    pub id: String,
    pub name: String,
    /// The catalog app it works with.
    #[serde(default)]
    pub for_app: Option<String>,
    pub category: String,
    pub summary: String,
    pub author: String,
    pub license: String,
    pub homepage: String,
    /// Starts the MCP server on stdio.
    pub run: Run,
    #[serde(default)]
    pub requires: Vec<Requirement>,
    #[serde(default)]
    pub setup: Vec<Step>,
    #[serde(default)]
    pub notes: String,
}

impl Plugin {
    fn validate(&self) -> Result<(), IndexError> {
        let bad = |why| Err(IndexError::Invalid(self.id.clone(), why));
        if self.id.is_empty() || !self.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return bad("id must be lowercase ascii letters, digits or '-'");
        }
        if !self.homepage.starts_with("https://") {
            return bad("homepage must be an https:// address");
        }
        let commands = std::iter::once(&self.run.command).chain(self.setup.iter().map(|s| &s.command));
        for c in commands {
            // a program name, never a shell line or a path: it is looked up on the user's PATH
            if c.is_empty() || c.contains(|ch: char| ch.is_whitespace() || r#"/\;&|$`'"<>"#.contains(ch)) || c.starts_with('-') {
                return bad("commands are plain program names (run without a shell)");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    /// Raised whenever the file changes; a fetched copy is used only when it's at least as new.
    pub version: u32,
    #[serde(default, rename = "plugin")]
    pub plugins: Vec<Plugin>,
}

impl Index {
    pub fn builtin() -> Result<Index, IndexError> {
        Index::parse(BUILTIN)
    }

    pub fn parse(text: &str) -> Result<Index, IndexError> {
        let index: Index = toml::from_str(text)?;
        let mut seen = std::collections::BTreeSet::new();
        for p in &index.plugins {
            p.validate()?;
            if !seen.insert(&p.id) {
                return Err(IndexError::Invalid(p.id.clone(), "duplicate id"));
            }
        }
        Ok(index)
    }

    pub fn get(&self, id: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|p| p.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_index_is_valid() {
        let i = Index::builtin().unwrap();
        assert!(i.version >= 1);
        assert!(i.get("blender-mcp").is_some_and(|p| p.run.command == "uvx" && !p.setup.is_empty()));
        // ids namespace tools as <id>__<tool>: never an underscore
        assert!(i.plugins.iter().all(|p| !p.id.contains('_')));
    }

    #[test]
    fn rejects_shell_lines_and_bad_ids() {
        let base = |id: &str, cmd: &str| {
            format!(
                "version = 1\n[[plugin]]\nid = \"{id}\"\nname = \"x\"\ncategory = \"c\"\nsummary = \"s\"\nauthor = \"a\"\nlicense = \"MIT\"\nhomepage = \"https://x\"\nrun = {{ command = \"{cmd}\" }}\n"
            )
        };
        assert!(Index::parse(&base("ok", "uvx")).is_ok());
        assert!(Index::parse(&base("Bad_Id", "uvx")).is_err());
        assert!(Index::parse(&base("ok", "sh -c evil")).is_err());
        assert!(Index::parse(&base("ok", "/tmp/evil")).is_err());
        assert!(Index::parse(&base("ok", "--flag")).is_err());
        let twice = format!("{}{}", base("ok", "uvx"), base("ok", "uvx").replace("version = 1\n", ""));
        assert!(Index::parse(&twice).is_err());
    }
}
