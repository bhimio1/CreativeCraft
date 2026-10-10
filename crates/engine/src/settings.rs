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
    /// Any folder, typically one a desktop sync client (Drive for desktop, Dropbox, …) mirrors, or a
    /// mounted NAS share (SMB, NFS).
    Folder { path: PathBuf },
    /// A WebDAV folder: Nextcloud, ownCloud, a NAS, pCloud, Koofr… The password lives in
    /// `secrets.json`, never in `settings.json`.
    WebDav {
        url: String,
        username: String,
        #[serde(default, skip_serializing)]
        password: String,
    },
    /// S3-compatible storage: AWS, Backblaze B2, Cloudflare R2, Wasabi, MinIO, Garage… The secret
    /// key lives in `secrets.json`.
    S3 {
        endpoint: String,
        region: String,
        bucket: String,
        #[serde(default)]
        prefix: String,
        access_key: String,
        #[serde(default, skip_serializing)]
        secret_key: String,
        #[serde(default)]
        path_style: bool,
    },
    /// A remote configured in the user's own rclone (Dropbox, OneDrive, SFTP, SMB, …).
    Rclone {
        remote: String,
        #[serde(default)]
        folder: String,
    },
}

impl CloudChoice {
    /// Which secret belongs to this setup, and its current value: keyed by server and account, so
    /// switching between setups keeps each one's password.
    fn secret_slot(&mut self) -> Option<(String, &mut String)> {
        match self {
            CloudChoice::WebDav { url, username, password } => Some((format!("webdav {url} {username}"), password)),
            CloudChoice::S3 { endpoint, bucket, access_key, secret_key, .. } => Some((format!("s3 {endpoint} {bucket} {access_key}"), secret_key)),
            _ => None,
        }
    }
}

/// `secrets.json` beside `settings.json`, readable only by the user.
fn secrets_path(settings: &Path) -> PathBuf {
    settings.with_file_name("secrets.json")
}

fn load_secrets(settings: &Path) -> BTreeMap<String, String> {
    std::fs::read(secrets_path(settings)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_secrets(settings: &Path, secrets: &BTreeMap<String, String>) -> std::io::Result<()> {
    let path = secrets_path(settings);
    let tmp = path.with_extension("json.tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    std::io::Write::write_all(&mut f, &serde_json::to_vec_pretty(secrets).map_err(std::io::Error::other)?)?;
    drop(f);
    std::fs::rename(tmp, path)
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
    /// Installed Marketplace plugins (plugin id → state).
    pub plugins: BTreeMap<String, crate::PluginState>,
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
            plugins: BTreeMap::new(),
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
        if let Some((key, value)) = s.cloud.secret_slot()
            && let Some(secret) = load_secrets(path).remove(&key)
        {
            *value = secret;
        }
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
        // the password or secret key goes to secrets.json; settings.json never contains it
        let mut cloud = self.cloud.clone();
        if let Some((key, value)) = cloud.secret_slot()
            && !value.is_empty()
        {
            let mut secrets = load_secrets(path);
            if secrets.get(&key) != Some(value) {
                secrets.insert(key, value.clone());
                save_secrets(path, &secrets)?;
            }
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_stay_out_of_settings_json() {
        let dir = std::env::temp_dir().join(format!("cc-secrets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let mut s = Settings {
            cloud: CloudChoice::WebDav { url: "https://nas.local/dav".into(), username: "me".into(), password: "hunter2".into() },
            ..Settings::default()
        };
        s.save(&path).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("hunter2"));
        assert!(std::fs::read_to_string(dir.join("secrets.json")).unwrap().contains("hunter2"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join("secrets.json")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(Settings::load(&path).cloud, s.cloud);
        // another setup keeps its own secret; switching back finds the first one again
        s.cloud = CloudChoice::S3 {
            endpoint: "https://s3.example.com".into(),
            region: "us-east-1".into(),
            bucket: "b".into(),
            prefix: String::new(),
            access_key: "AK".into(),
            secret_key: "sk".into(),
            path_style: true,
        };
        s.save(&path).unwrap();
        s.cloud = CloudChoice::WebDav { url: "https://nas.local/dav".into(), username: "me".into(), password: String::new() };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).cloud, CloudChoice::WebDav { url: "https://nas.local/dav".into(), username: "me".into(), password: "hunter2".into() });
        let _ = std::fs::remove_dir_all(&dir);
    }
}
