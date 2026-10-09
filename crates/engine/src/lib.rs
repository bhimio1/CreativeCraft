//! The Session (L3): catalog + installer + settings + cloud, and the command registry.
//!
//! Same rule as the Craft apps: **every action is a command**. The egui shell, the CLI and the
//! MCP server all call [`Session::execute`], so anything a person can click, an agent can do, and
//! there is exactly one implementation of each action.
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod settings;

pub use creativecraft_agent as agent;
pub use creativecraft_cloud::Action as SyncAction;
pub use settings::{CloudChoice, CustomMcp, Settings};

use creativecraft_catalog::{AppSpec, Catalog, Kind};
use creativecraft_cloud::{CloudError, CloudStore, FolderStore, GoogleClient, GoogleDrive, sync};
use creativecraft_install::{HubDirs, InstallError, Installer, Progress, Status};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Child, Command as Proc, Stdio};

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
}

/// Everything the hub can do. Ids (`app.install`, …) are stable: the CLI and docs use them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Install(String),
    Uninstall(String),
    Launch(String, Vec<PathBuf>),
    OpenHomepage(String),
    Sync,
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
            Command::Sync => "cloud.sync",
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
    Sync { done: usize, total: usize, path: String },
}

pub struct Session {
    pub dirs: HubDirs,
    pub catalog: Catalog,
    pub installer: Installer,
    pub settings: Settings,
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
        Ok(Session { dirs, catalog, installer, settings })
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
        }
    }

    /// Run a command. `events` receives progress; the returned string is a one-line result.
    pub fn execute(&mut self, cmd: Command, events: &mut dyn FnMut(Event)) -> Result<String, EngineError> {
        log::info!("command {}", cmd.id());
        match cmd {
            Command::Install(id) => {
                let app = self.app(&id)?;
                let i = self.installer.install(&app, &mut |p| events(Event::Install(id.clone(), p)))?;
                Ok(format!("{} {} installed", app.name, i.version))
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
            Command::Sync => {
                let mut store = self.cloud_store()?;
                let root = self.settings.projects_dir.clone();
                let device = self.settings.device_name.clone();
                let report = sync::run(&root, store.as_mut(), &device, &mut |done, total, path| events(Event::Sync { done, total, path: path.to_string() }))?;
                for (path, why) in &report.errors {
                    log::warn!("sync: {path}: {why}");
                }
                Ok(report.summary())
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

    /// Start an MCP server for a catalog app or a user-configured one.
    pub fn spawn_mcp(&self, id: &str) -> Result<Child, EngineError> {
        if let Some(c) = self.settings.custom_mcp.iter().find(|c| c.id == id) {
            return Ok(Proc::new(&c.command).args(&c.args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn()?);
        }
        let app = self.app(id)?;
        Ok(self.installer.spawn_mcp(&app)?)
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
                    "installable_by_hub": a.kind == Kind::Release, "license": a.license,
                })
            })
            .collect();
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
    fn spawn_mcp(&mut self, id: &str) -> Result<Child, String> {
        Session::spawn_mcp(self, id).map_err(|e| e.to_string())
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
