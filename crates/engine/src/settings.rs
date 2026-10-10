use creativecraft_agent::AgentSettings;
use creativecraft_install::Source;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where cloud saves go.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CloudChoice {
    #[default]
    Off,
    /// Google Drive through the REST API. The OAuth client is the publisher's "Desktop app" client.
    GoogleDrive { client_id: String, client_secret: String },
    /// Any folder, typically one a desktop sync client (Drive for desktop, Dropbox, …) mirrors.
    Folder { path: PathBuf },
}

/// Window colour theme.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    /// Follow the operating system's light/dark setting.
    #[default]
    System,
    Dark,
    Light,
}

/// An extra MCP server the user wants the hub to front, e.g. a Blender add-on bridge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomMcp {
    /// Lowercase id, no `_` (it namespaces the tools: `<id>__<tool>`).
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The folder that is synced. Apps save projects anywhere inside it.
    pub projects_dir: PathBuf,
    pub cloud: CloudChoice,
    /// Goes into conflict-copy names, so the user can tell machines apart.
    pub device_name: String,
    /// Sync automatically every N minutes while the hub is open (0 = only on demand).
    pub auto_sync_minutes: u32,
    pub custom_mcp: Vec<CustomMcp>,
    /// The assistant: which agent CLI runs it, and how.
    pub agent: AgentSettings,
    pub appearance: Appearance,
    /// Where each app installs from, when not its official releases (app id → source).
    pub app_sources: BTreeMap<String, Source>,
    /// Play the short start-up animation when the window opens.
    pub intro_animation: bool,
    /// Agents work in apps' visible windows (live mode) rather than headless, so the user can
    /// watch and take over.
    pub show_app_windows: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        let device = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "this computer".into());
        Settings {
            projects_dir: home.join("CreativeCraft Projects"),
            cloud: CloudChoice::Off,
            device_name: device,
            auto_sync_minutes: 10,
            custom_mcp: Vec::new(),
            agent: AgentSettings::default(),
            appearance: Appearance::System,
            app_sources: BTreeMap::new(),
            intro_animation: true,
            show_app_windows: true,
        }
    }
}

impl Settings {
    /// Missing file → defaults. Environment overrides for the Google client make CI and packaging
    /// easy: `CREATIVECRAFT_GOOGLE_CLIENT_ID` / `CREATIVECRAFT_GOOGLE_CLIENT_SECRET`.
    pub fn load(path: &Path) -> Settings {
        let mut s: Settings = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if let (Ok(id), Ok(secret)) = (std::env::var("CREATIVECRAFT_GOOGLE_CLIENT_ID"), std::env::var("CREATIVECRAFT_GOOGLE_CLIENT_SECRET"))
            && matches!(s.cloud, CloudChoice::Off)
        {
            s.cloud = CloudChoice::GoogleDrive { client_id: id, client_secret: secret };
        }
        s
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, path)
    }
}
