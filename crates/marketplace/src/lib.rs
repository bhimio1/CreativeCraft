//! The plugin indexes (L1): pure data describing the plugins the Marketplace lists.
//!
//! Two kinds, in two files, both compiled in:
//!
//! - `marketplace/plugins.toml` ([`Index`]): MCP servers that give agents tools, run from their
//!   publishers' own packages.
//! - `marketplace/audio-plugins.toml` ([`AudioIndex`]): CLAP / VST3 audio plugins, downloaded from
//!   their publishers' own GitHub releases into the user's plugin folders. A separate file because
//!   released hubs read `plugins.toml` strictly (unknown fields are errors), so its schema is frozen.
//!
//! A newer copy of either fetched from the CreativeCraft repository replaces the built-in one when
//! its `version` is at least the built-in one's (see the engine). Ids are unique across both files:
//! installed plugins of both kinds share the settings and the Marketplace page. Nothing is hosted
//! here.
#![cfg_attr(test, allow(clippy::unwrap_used))]

use serde::Deserialize;

const BUILTIN: &str = include_str!("../../../marketplace/plugins.toml");
const BUILTIN_AUDIO: &str = include_str!("../../../marketplace/audio-plugins.toml");

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
        validate_common(&self.id, &self.homepage)?;
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

/// Rules every listing follows, whatever its kind.
fn validate_common(id: &str, homepage: &str) -> Result<(), IndexError> {
    let bad = |why| Err(IndexError::Invalid(id.to_string(), why));
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return bad("id must be lowercase ascii letters, digits or '-'");
    }
    if !homepage.starts_with("https://") {
        return bad("homepage must be an https:// address");
    }
    Ok(())
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

/// An audio plugin format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Clap,
    Vst3,
}

impl Format {
    pub const ALL: [Format; 2] = [Format::Clap, Format::Vst3];

    /// The bundle's extension (`clap`, `vst3`).
    pub fn extension(self) -> &'static str {
        match self {
            Format::Clap => "clap",
            Format::Vst3 => "vst3",
        }
    }

    /// How people write it (`CLAP`, `VST3`).
    pub fn label(self) -> &'static str {
        match self {
            Format::Clap => "CLAP",
            Format::Vst3 => "VST3",
        }
    }

    /// The format a bundle name (`X.clap`, `X.VST3`) is in.
    pub fn of(name: &str) -> Option<Format> {
        let ext = std::path::Path::new(name).extension()?.to_str()?;
        Format::ALL.into_iter().find(|f| f.extension().eq_ignore_ascii_case(ext))
    }
}

/// A CLAP / VST3 plugin downloaded from its publisher's GitHub releases.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioPlugin {
    pub id: String,
    pub name: String,
    /// The catalog app it comes from or works with.
    #[serde(default)]
    pub for_app: Option<String>,
    pub category: String,
    pub summary: String,
    pub author: String,
    pub license: String,
    pub homepage: String,
    /// `owner/name` of the GitHub repository whose latest release it downloads.
    pub repo: String,
    /// Release asset name prefix; the host's build is picked among the assets starting with it.
    pub asset: String,
    /// Bundle name in the archive, without extension.
    pub bundle: String,
    pub formats: Vec<Format>,
    #[serde(default)]
    pub notes: String,
}

impl AudioPlugin {
    /// `MusicCraft Effects.clap`.
    pub fn bundle_name(&self, format: Format) -> String {
        format!("{}.{}", self.bundle, format.extension())
    }

    /// `CLAP · VST3`.
    pub fn formats_label(&self) -> String {
        self.formats.iter().map(|f| f.label()).collect::<Vec<_>>().join(" · ")
    }

    fn validate(&self) -> Result<(), IndexError> {
        let bad = |why| Err(IndexError::Invalid(self.id.clone(), why));
        validate_common(&self.id, &self.homepage)?;
        let name_part = |s: &str| !s.is_empty() && s != "." && s != ".." && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
        match self.repo.split_once('/') {
            Some((owner, name)) if name_part(owner) && name_part(name) => {}
            _ => return bad("repo must be a GitHub owner/name"),
        }
        let plain = |s: &str| !s.trim().is_empty() && !s.contains(['/', '\\']) && !s.contains("..") && !s.chars().any(char::is_control);
        if !plain(&self.asset) {
            return bad("asset must be a plain file name prefix");
        }
        if !plain(&self.bundle) || self.bundle.starts_with('.') || self.bundle.trim() != self.bundle {
            return bad("bundle must be a plain name, without folders or '..'");
        }
        if self.formats.is_empty() {
            return bad("formats must name at least one of clap, vst3");
        }
        let mut seen = std::collections::BTreeSet::new();
        if !self.formats.iter().all(|f| seen.insert(*f)) {
            return bad("formats lists a format twice");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioIndex {
    /// Raised whenever the file changes; a fetched copy is used only when it's at least as new.
    pub version: u32,
    #[serde(default, rename = "plugin")]
    pub plugins: Vec<AudioPlugin>,
}

impl AudioIndex {
    pub fn builtin() -> Result<AudioIndex, IndexError> {
        AudioIndex::parse(BUILTIN_AUDIO)
    }

    pub fn parse(text: &str) -> Result<AudioIndex, IndexError> {
        let index: AudioIndex = toml::from_str(text)?;
        let mut seen = std::collections::BTreeSet::new();
        for p in &index.plugins {
            p.validate()?;
            if !seen.insert(&p.id) {
                return Err(IndexError::Invalid(p.id.clone(), "duplicate id"));
            }
        }
        Ok(index)
    }

    pub fn get(&self, id: &str) -> Option<&AudioPlugin> {
        self.plugins.iter().find(|p| p.id == id)
    }
}

/// Both kinds share settings and the Marketplace page, so an id may be listed only once overall.
pub fn check_ids_unique(mcp: &Index, audio: &AudioIndex) -> Result<(), IndexError> {
    match audio.plugins.iter().find(|a| mcp.get(&a.id).is_some()) {
        Some(a) => Err(IndexError::Invalid(a.id.clone(), "listed both as an MCP plugin and as an audio plugin")),
        None => Ok(()),
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

    #[test]
    fn builtin_audio_index_lists_musiccraft_plugins() {
        let a = AudioIndex::builtin().unwrap();
        assert!(a.version >= 1);
        for (id, bundle) in [("musiccraft-effects", "MusicCraft Effects"), ("musiccraft-instruments", "MusicCraft Instruments")] {
            let p = a.get(id).unwrap();
            assert_eq!(p.bundle, bundle);
            assert_eq!(p.repo, "bhimio1/musiccraft");
            assert_eq!(p.asset, "MusicCraft-Plugins");
            assert_eq!(p.formats, vec![Format::Clap, Format::Vst3]);
            assert_eq!(p.for_app.as_deref(), Some("musiccraft"));
            assert_eq!(p.bundle_name(Format::Vst3), format!("{bundle}.vst3"));
            assert_eq!(p.formats_label(), "CLAP · VST3");
        }
    }

    #[test]
    fn ids_are_unique_across_both_indexes() {
        let mcp = Index::builtin().unwrap();
        let audio = AudioIndex::builtin().unwrap();
        check_ids_unique(&mcp, &audio).unwrap();
        let clash = AudioIndex { version: 1, plugins: vec![AudioPlugin { id: "blender-mcp".into(), ..audio.plugins[0].clone() }] };
        assert!(check_ids_unique(&mcp, &clash).is_err());
    }

    #[test]
    fn audio_entries_are_checked() {
        let entry = |field: &str, value: &str| {
            let mut fields = vec![
                ("id", "\"x-fx\""),
                ("name", "\"X\""),
                ("category", "\"Effects\""),
                ("summary", "\"s\""),
                ("author", "\"a\""),
                ("license", "\"MIT\""),
                ("homepage", "\"https://x\""),
                ("repo", "\"owner/name\""),
                ("asset", "\"X-Plugins\""),
                ("bundle", "\"X Effects\""),
                ("formats", "[\"clap\", \"vst3\"]"),
            ];
            for f in &mut fields {
                if f.0 == field {
                    f.1 = value;
                }
            }
            let body: String = fields.iter().map(|(k, v)| format!("{k} = {v}\n")).collect();
            AudioIndex::parse(&format!("version = 1\n[[plugin]]\n{body}"))
        };
        assert!(entry("", "").is_ok());
        assert!(entry("id", "\"Bad_Id\"").is_err());
        assert!(entry("homepage", "\"http://x\"").is_err());
        assert!(entry("repo", "\"owner\"").is_err());
        assert!(entry("repo", "\"a/b/c\"").is_err());
        assert!(entry("repo", "\"../b\"").is_err());
        assert!(entry("bundle", "\"../../evil\"").is_err());
        assert!(entry("bundle", "\"sub/X\"").is_err());
        assert!(entry("bundle", "\"sub\\\\X\"").is_err());
        assert!(entry("bundle", "\"\"").is_err());
        assert!(entry("asset", "\"a/b\"").is_err());
        assert!(entry("formats", "[]").is_err());
        assert!(entry("formats", "[\"vst2\"]").is_err());
        assert!(entry("formats", "[\"clap\", \"clap\"]").is_err());
        assert!(entry("unknown", "1").is_ok(), "only listed fields are replaced");
        assert_eq!(Format::of("X.VST3"), Some(Format::Vst3));
        assert_eq!(Format::of("X.dll"), None);
    }
}
