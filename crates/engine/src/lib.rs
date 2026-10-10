//! The Session (L3): catalog + installer + settings + cloud, and the command registry.
//!
//! Same rule as the Craft apps: **every action is a command**. The egui shell, the CLI and the
//! MCP server all call [`Session::execute`], so anything a person can click, an agent can do, and
//! there is exactly one implementation of each action.
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod generate;
mod live;
mod plugins;
pub mod projects;
mod report;
mod settings;
pub mod stock;

pub use creativecraft_agent as agent;
pub use creativecraft_cloud::Action as SyncAction;
pub use creativecraft_generate as generation;
pub use creativecraft_install::Source;
pub use creativecraft_marketplace::{Index as PluginIndex, Plugin};
pub use plugins::PluginState;
pub use settings::{Appearance, CloudChoice, CustomMcp, GenerationSettings, Settings};

use creativecraft_catalog::{AppSpec, Catalog, Kind};
use creativecraft_cloud::{CloudError, CloudStore, FolderStore, GoogleClient, GoogleDrive, sync};
use creativecraft_install::{HubDirs, InstallError, Installer, Progress, Status};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Child;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("{0}")]
    Install(#[from] InstallError),
    #[error("{0}")]
    Cloud(#[from] CloudError),
    #[error("{0}")]
    Catalog(#[from] creativecraft_catalog::CatalogError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("no app with id `{0}`")]
    UnknownApp(String),
    #[error("cloud saves are off; choose Google Drive or a folder in Settings")]
    CloudOff,
    #[error("couldn't find the hub's data folder on this computer")]
    NoHome,
    #[error("{0}")]
    Agent(#[from] creativecraft_agent::AgentError),
    #[error("{0}")]
    Live(String),
    #[error("{0}")]
    Plugin(String),
    #[error("{0}")]
    Stock(#[from] creativecraft_stock::StockError),
    #[error("{0}")]
    Generate(String),
}

/// Everything the hub can do. Ids (`app.install`, …) are stable: the CLI and docs use them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Install(String),
    Uninstall(String),
    Launch(String, Vec<PathBuf>),
    OpenHomepage(String),
    /// Open the issue tracker for an app (`Some(id)`) or for CreativeCraft itself (`None`).
    ReportBug(Option<String>),
    Sync,
    /// Check that the configured cloud storage can be reached (Settings → Cloud sync → Test).
    TestCloud,
    /// Install a Marketplace plugin: check what it needs, run its setup steps, turn it on.
    InstallPlugin(String),
    /// Forget an installed plugin.
    RemovePlugin(String),
    /// Turn an installed plugin's tools on or off for agents.
    SetPluginEnabled(String, bool),
    /// Fetch the newest plugin list from the CreativeCraft repository.
    RefreshMarketplace,
    /// Download a stock asset into `<projects>/Stock`: a size label, or the largest no wider than a
    /// width, or (neither) at most 3000 px wide.
    StockDownload {
        source: String,
        id: String,
        size: Option<String>,
        max_width: Option<u32>,
    },
    /// Make images or a video with an AI generation service (`None` = the default for the kind)
    /// and save them under `<projects>/Generated`.
    Generate {
        request: creativecraft_generate::Request,
        provider: Option<String>,
    },
    /// Check that a generation service can be reached and accepts its key.
    TestGenerator(String),
    /// Move a generated file (and its note) to the projects folder's trash.
    TrashGenerated(PathBuf),
    SignInGoogle,
    SignOutGoogle,
    /// One-time setup so the chat's agent CLI can reach the hub (Hermes Agent needs it).
    SetupAgent,
}

impl Command {
    pub fn id(&self) -> &'static str {
        match self {
            Command::Install(_) => "app.install",
            Command::Uninstall(_) => "app.uninstall",
            Command::Launch(..) => "app.launch",
            Command::OpenHomepage(_) => "app.homepage",
            Command::ReportBug(Some(_)) => "app.report_bug",
            Command::ReportBug(None) => "hub.report_bug",
            Command::Sync => "cloud.sync",
            Command::TestCloud => "cloud.test",
            Command::InstallPlugin(_) => "plugin.install",
            Command::RemovePlugin(_) => "plugin.remove",
            Command::SetPluginEnabled(..) => "plugin.enable",
            Command::RefreshMarketplace => "plugin.refresh",
            Command::StockDownload { .. } => "stock.download",
            Command::Generate { .. } => "generate.run",
            Command::TestGenerator(_) => "generate.test",
            Command::TrashGenerated(_) => "generate.trash",
            Command::SignInGoogle => "cloud.google.sign_in",
            Command::SignOutGoogle => "cloud.google.sign_out",
            Command::SetupAgent => "agent.setup",
        }
    }
}

/// Feedback while a command runs (install progress, sync progress).
#[derive(Debug, Clone)]
pub enum Event {
    Install(String, Progress),
    Sync {
        done: usize,
        total: usize,
        path: String,
    },
    /// What a generation service is doing ("queued", "running"…).
    Generate(String),
}

pub struct Session {
    pub dirs: HubDirs,
    pub catalog: Catalog,
    pub installer: Installer,
    pub settings: Settings,
    /// The plugins the Marketplace lists.
    pub marketplace: creativecraft_marketplace::Index,
}

impl Session {
    pub fn open() -> Result<Session, EngineError> {
        Session::open_at(HubDirs::discover().ok_or(EngineError::NoHome)?)
    }

    pub fn open_at(dirs: HubDirs) -> Result<Session, EngineError> {
        let mut catalog = Catalog::builtin()?;
        if let Ok(text) = std::fs::read_to_string(dirs.catalog_overlay()) {
            catalog.merge(Catalog::parse(&text)?);
        }
        let installer = Installer::open(dirs.clone())?;
        let settings = Settings::load(&dirs.settings_file());
        let marketplace = plugins::load_index(&dirs.data().join("marketplace").join("plugins.toml"));
        Ok(Session { dirs, catalog, installer, settings, marketplace })
    }

    pub fn save_settings(&self) -> Result<(), EngineError> {
        Ok(self.settings.save(&self.dirs.settings_file())?)
    }

    pub fn app(&self, id: &str) -> Result<AppSpec, EngineError> {
        self.catalog.get(id).cloned().ok_or_else(|| EngineError::UnknownApp(id.to_string()))
    }

    pub fn status(&self, app: &AppSpec) -> Status {
        self.installer.status(app)
    }

    /// Where `id` installs from (its official releases unless Settings say otherwise).
    pub fn source(&self, id: &str) -> creativecraft_install::Source {
        self.settings.app_sources.get(id).cloned().unwrap_or_default()
    }

    /// What is installed for `id`, if the hub installed it.
    pub fn installed(&self, id: &str) -> Option<&creativecraft_install::Installed> {
        self.installer.registry.get(id)
    }

    /// Where "Report a bug" goes for an app (`Some(id)`) or for CreativeCraft (`None`).
    pub fn bug_report_url(&self, id: Option<&str>) -> Result<String, EngineError> {
        let Some(id) = id else { return Ok(report::bug_report_url(None, None)) };
        let app = self.app(id)?;
        let version = match self.status(&app) {
            Status::Installed { version } => Some(version),
            _ => None,
        };
        Ok(report::bug_report_url(Some(&app), version.as_deref()))
    }

    fn token_file(&self) -> PathBuf {
        self.dirs.config().join("google-token.json")
    }

    pub fn google_signed_in(&self) -> bool {
        self.token_file().is_file()
    }

    /// The configured cloud store, connected.
    pub fn cloud_store(&self) -> Result<Box<dyn CloudStore>, EngineError> {
        match &self.settings.cloud {
            CloudChoice::Off => Err(EngineError::CloudOff),
            CloudChoice::Folder { path } => Ok(Box::new(FolderStore::new(path.clone()))),
            CloudChoice::GoogleDrive { client_id, client_secret } => {
                let client = GoogleClient { client_id: client_id.clone(), client_secret: client_secret.clone() };
                Ok(Box::new(GoogleDrive::connect(client, self.token_file())?))
            }
            CloudChoice::WebDav { url, username, password } => Ok(Box::new(creativecraft_cloud::WebDavStore::new(url, username, password)?)),
            CloudChoice::S3 { endpoint, region, bucket, prefix, access_key, secret_key, path_style } => {
                Ok(Box::new(creativecraft_cloud::S3Store::new(creativecraft_cloud::S3Config {
                    endpoint: endpoint.clone(),
                    region: region.clone(),
                    bucket: bucket.clone(),
                    prefix: prefix.clone(),
                    access_key: access_key.clone(),
                    secret_key: secret_key.clone(),
                    path_style: *path_style,
                })?))
            }
            CloudChoice::Rclone { remote, folder } => Ok(Box::new(creativecraft_cloud::RcloneStore::new(remote, folder)?)),
        }
    }

    /// The remotes in the user's rclone, for the settings menu (empty if rclone isn't installed).
    pub fn rclone_remotes() -> Vec<String> {
        creativecraft_cloud::rclone_remotes().unwrap_or_default()
    }

    /// Run a command. `events` receives progress; the returned string is a one-line result.
    pub fn execute(&mut self, cmd: Command, events: &mut dyn FnMut(Event)) -> Result<String, EngineError> {
        log::info!("command {}", cmd.id());
        match cmd {
            Command::Install(id) => {
                let app = self.app(&id)?;
                let source = self.source(&id);
                let i = self.installer.install_from(&app, &source, &mut |p| events(Event::Install(id.clone(), p)))?;
                let from = i.source.as_deref().map(|s| format!(" from {s}")).unwrap_or_default();
                Ok(format!("{} {} installed{from}", app.name, i.version))
            }
            Command::Uninstall(id) => {
                let app = self.app(&id)?;
                self.installer.uninstall(&app)?;
                Ok(format!("{} removed", app.name))
            }
            Command::Launch(id, files) => {
                let app = self.app(&id)?;
                self.installer.launch(&app, &files)?;
                Ok(format!("{} opened", app.name))
            }
            Command::OpenHomepage(id) => {
                let app = self.app(&id)?;
                Installer::open_homepage(&app)?;
                Ok(format!("opened {}", app.homepage))
            }
            Command::ReportBug(id) => {
                let url = self.bug_report_url(id.as_deref())?;
                Installer::open_url(&url)?;
                Ok("Opened the bug report page".into())
            }
            Command::Sync => {
                let mut store = self.cloud_store()?;
                let root = self.settings.projects_dir.clone();
                let device = self.settings.device_name.clone();
                let report = sync::run(&root, store.as_mut(), &device, &mut |done, total, path| events(Event::Sync { done, total, path: path.to_string() }))?;
                for (path, why) in &report.errors {
                    log::warn!("sync: {path}: {why}");
                }
                // say what went wrong, not just how often
                let mut out = report.summary();
                for (path, why) in report.errors.iter().take(3) {
                    out.push_str(&format!("\n  {path}: {why}"));
                }
                if report.errors.len() > 3 {
                    out.push_str(&format!("\n  …and {} more", report.errors.len() - 3));
                }
                Ok(out)
            }
            Command::InstallPlugin(id) => self.install_plugin(&id),
            Command::RemovePlugin(id) => self.remove_plugin(&id),
            Command::SetPluginEnabled(id, on) => self.set_plugin_enabled(&id, on),
            Command::RefreshMarketplace => self.refresh_marketplace(),
            Command::StockDownload { source, id, size, max_width } => self.stock_download(&source, &id, size.as_deref(), max_width),
            Command::Generate { request, provider } => self.generate(&request, provider.as_deref(), events),
            Command::TestGenerator(id) => self.test_generator(&id),
            Command::TrashGenerated(file) => generate::trash_generated(&self.settings.projects_dir, &file),
            Command::TestCloud => {
                let mut store = self.cloud_store()?;
                Ok(sync::test_connection(store.as_mut())?)
            }
            Command::SignInGoogle => {
                let CloudChoice::GoogleDrive { client_id, client_secret } = &self.settings.cloud else {
                    return Err(EngineError::CloudOff);
                };
                let client = GoogleClient { client_id: client_id.clone(), client_secret: client_secret.clone() };
                GoogleDrive::sign_in(client, self.token_file())?;
                Ok("Signed in to Google Drive".into())
            }
            Command::SignOutGoogle => {
                GoogleDrive::sign_out(&self.token_file())?;
                Ok("Signed out of Google Drive".into())
            }
            Command::SetupAgent => match self.settings.agent.backend {
                creativecraft_agent::Backend::Hermes => Ok(creativecraft_agent::setup_hermes(&creativecraft_agent::McpServer::hub())?),
                b => Ok(format!("{} needs no setup: CreativeCraft passes it the MCP config on every turn", b.name())),
            },
        }
    }

    /// The chat transcript (the model-side history lives in the backend's own session).
    pub fn chat_file(&self) -> PathBuf {
        self.dirs.data().join("chat.json")
    }

    /// Start one chat turn with the configured agent CLI, in the projects folder, with the hub as
    /// its MCP server.
    pub fn start_chat_turn(&self, prompt: &str, session: Option<&str>) -> Result<creativecraft_agent::Running, EngineError> {
        std::fs::create_dir_all(&self.settings.projects_dir)?;
        let hub = creativecraft_agent::McpServer::hub();
        let state = creativecraft_agent::state_dir(&self.settings.projects_dir);
        let req = creativecraft_agent::TurnRequest {
            settings: &self.settings.agent,
            workspace: &self.settings.projects_dir,
            state_dir: &state,
            hub: &hub,
            prompt,
            session,
        };
        Ok(creativecraft_agent::start_turn(&req)?)
    }

    /// What a sync would do now, changing nothing.
    pub fn preview_sync(&self) -> Result<Vec<SyncAction>, EngineError> {
        let mut store = self.cloud_store()?;
        Ok(sync::preview(&self.settings.projects_dir, store.as_mut(), &self.settings.device_name)?)
    }

    /// The catalog with live status, as JSON (for the agent and `creativecraft-cli list --json`).
    pub fn apps_json(&self) -> Value {
        let mut out: Vec<Value> = self
            .catalog
            .apps
            .iter()
            .map(|a| {
                let (status, version) = match self.status(a) {
                    Status::NotInstalled => ("not_installed", None),
                    Status::Installed { version } => ("installed", Some(version)),
                    Status::Detected { .. } => ("installed_externally", None),
                };
                json!({
                    "id": a.id, "name": a.name, "category": a.category, "summary": a.summary,
                    "status": status, "version": version,
                    "mcp": a.has_mcp() && matches!(a.kind, Kind::Release) && status == "installed",
                    "window": a.live.is_some(),
                    "installable_by_hub": a.kind == Kind::Release, "license": a.license,
                })
            })
            .collect();
        for (id, state) in &self.settings.plugins {
            if let Some(p) = self.marketplace.get(id) {
                out.push(json!({ "id": id, "name": p.name, "category": "Plugin", "summary": p.summary, "status": if state.enabled { "installed" } else { "turned_off" }, "mcp": state.enabled, "for_app": p.for_app, "installable_by_hub": false }));
            }
        }
        for c in &self.settings.custom_mcp {
            out.push(json!({ "id": c.id, "name": c.name, "category": "Custom", "status": "configured", "mcp": true, "installable_by_hub": false }));
        }
        Value::Array(out)
    }
}

/// The agent's view of the session.
impl creativecraft_mcp::HubBackend for Session {
    fn list_apps(&mut self) -> Value {
        self.apps_json()
    }
    fn install(&mut self, id: &str) -> Result<String, String> {
        self.execute(Command::Install(id.into()), &mut |_| {}).map_err(|e| e.to_string())
    }
    fn launch(&mut self, id: &str, files: &[String]) -> Result<String, String> {
        self.execute(Command::Launch(id.into(), files.iter().map(PathBuf::from).collect()), &mut |_| {}).map_err(|e| e.to_string())
    }
    fn sync(&mut self) -> Result<String, String> {
        self.execute(Command::Sync, &mut |_| {}).map_err(|e| e.to_string())
    }
    fn spawn_mcp(&mut self, id: &str, window: Option<bool>) -> Result<Child, String> {
        Session::spawn_mcp(self, id, window).map_err(|e| e.to_string())
    }
    fn stock_search(&mut self, query: &str, kind: Option<&str>, page: u32) -> Result<Value, String> {
        self.stock_search_json(query, kind, page).map_err(|e| e.to_string())
    }
    fn stock_download(&mut self, source: &str, id: &str, size: Option<&str>, max_width: Option<u32>) -> Result<String, String> {
        let cmd = Command::StockDownload { source: source.into(), id: id.into(), size: size.map(String::from), max_width };
        self.execute(cmd, &mut |_| {}).map_err(|e| e.to_string())
    }
    fn generation_providers(&mut self) -> Value {
        self.generation_providers_json()
    }
    fn generate(&mut self, request: Value, provider: Option<&str>) -> Result<String, String> {
        if !self.settings.generation.agents_can_generate {
            return Err("The user has turned off AI generation for agents (Settings → AI generation).".into());
        }
        let request: creativecraft_generate::Request = serde_json::from_value(request).map_err(|e| format!("bad arguments: {e}"))?;
        let pictures: Vec<PathBuf> = request.images.iter().chain(request.mask.as_ref()).cloned().collect();
        generate::check_agent_images(&self.settings.projects_dir, &pictures).map_err(|e| e.to_string())?;
        self.execute(Command::Generate { request, provider: provider.map(String::from) }, &mut |_| {}).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_session_lists_the_catalog_and_syncs_to_a_folder() {
        let root = std::env::temp_dir().join(format!("creativecraft-engine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut s = Session::open_at(HubDirs::under(&root)).unwrap();
        assert!(s.apps_json().as_array().unwrap().iter().any(|a| a["id"] == "photocraft" && a["status"] == "not_installed"));
        assert!(matches!(s.execute(Command::Sync, &mut |_| {}), Err(EngineError::CloudOff)));

        s.settings.projects_dir = root.join("projects");
        s.settings.cloud = CloudChoice::Folder { path: root.join("cloud") };
        std::fs::create_dir_all(&s.settings.projects_dir).unwrap();
        std::fs::write(s.settings.projects_dir.join("a.pcraft"), b"x").unwrap();
        assert_eq!(s.execute(Command::Sync, &mut |_| {}).unwrap(), "1 uploaded");
        assert!(root.join("cloud/a.pcraft").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }
}
