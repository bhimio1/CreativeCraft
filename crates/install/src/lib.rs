//! Install, update, detect and launch catalog apps (L2).
//!
//! Layout under the data folder (see [`HubDirs`]):
//!
//! ```text
//! apps/<id>/<version>/…        unpacked release archive (one folder per installed version)
//! apps/installed.json          which version of each app is current, and where its executables are
//! downloads/                   in-flight downloads (`*.part`), removed after unpacking
//! ```
//!
//! Everything is per-user and needs no admin rights. Nothing is ever re-hosted: archives come from
//! the app's own GitHub releases, verified against the release's SHA-256 checksums when it
//! publishes them.
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod dirs;
mod external;
mod github;
mod registry;
mod unpack;

pub use dirs::HubDirs;
pub use external::detect_external;
pub use github::{Asset, Release};
pub use registry::{Installed, Registry};

use creativecraft_catalog::{AppSpec, Kind};
use creativecraft_platform::Host;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("network: {0}")]
    Net(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("this computer's platform is not supported")]
    UnsupportedHost,
    #[error("{app} has no download for this computer in release {tag}")]
    NoAsset { app: String, tag: String },
    #[error("checksum mismatch for {0}: the download is corrupt or was tampered with")]
    Checksum(String),
    #[error("archive: {0}")]
    Archive(String),
    #[error("{0} is not installed")]
    NotInstalled(String),
    #[error("{0} can't be installed by the hub; get it from its official site")]
    External(String),
    #[error("couldn't find the `{exe}` executable inside {app}'s download")]
    MissingExe { app: String, exe: String },
}

/// Progress of a long operation, for the UI.
#[derive(Debug, Clone)]
pub enum Progress {
    Resolving,
    Downloading { done: u64, total: Option<u64> },
    Verifying,
    Unpacking,
    Done,
}

/// The installer: owns the folders and the registry of what is installed.
pub struct Installer {
    pub dirs: HubDirs,
    pub host: Host,
    pub registry: Registry,
}

/// Where an app stands on this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    NotInstalled,
    Installed {
        version: String,
    },
    /// Found outside the hub (kind = "external").
    Detected {
        path: PathBuf,
    },
}

impl Installer {
    pub fn open(dirs: HubDirs) -> Result<Installer, InstallError> {
        let host = Host::current().ok_or(InstallError::UnsupportedHost)?;
        let registry = Registry::load(&dirs.registry_file())?;
        Ok(Installer { dirs, host, registry })
    }

    pub fn status(&self, app: &AppSpec) -> Status {
        match app.kind {
            Kind::Release => match self.registry.get(&app.id) {
                Some(i) => Status::Installed { version: i.version.clone() },
                None => Status::NotInstalled,
            },
            Kind::External => match detect_external(app, self.host) {
                Some(path) => Status::Detected { path },
                None => Status::NotInstalled,
            },
        }
    }

    /// The newest release of a release-kind app.
    pub fn latest(&self, app: &AppSpec) -> Result<Release, InstallError> {
        let repo = app.repo.as_deref().ok_or_else(|| InstallError::External(app.name.clone()))?;
        github::latest_release(repo)
    }

    /// `Some(new_tag)` if a newer release than the installed one exists.
    pub fn update_available(&self, app: &AppSpec) -> Result<Option<String>, InstallError> {
        let Some(have) = self.registry.get(&app.id) else { return Ok(None) };
        let latest = self.latest(app)?;
        Ok((creativecraft_platform::cmp_versions(&latest.tag, &have.version).is_gt()).then_some(latest.tag))
    }

    /// Install (or update to) the latest release. Older versions are removed after the new one is
    /// in place, so a failed update leaves the working version untouched.
    pub fn install(&mut self, app: &AppSpec, progress: &mut dyn FnMut(Progress)) -> Result<Installed, InstallError> {
        if app.kind == Kind::External {
            return Err(InstallError::External(app.name.clone()));
        }
        progress(Progress::Resolving);
        let release = self.latest(app)?;
        let names: Vec<&str> = release.assets.iter().map(|a| a.name.as_str()).collect();
        let picked =
            creativecraft_platform::pick_asset(&names, self.host).ok_or_else(|| InstallError::NoAsset { app: app.name.clone(), tag: release.tag.clone() })?;
        let asset = release.assets.iter().find(|a| a.name == picked).ok_or_else(|| InstallError::Archive("asset vanished".into()))?;

        std::fs::create_dir_all(self.dirs.downloads())?;
        let part = self.dirs.downloads().join(format!("{}.part", asset.name));
        let digest = github::download(&asset.url, &part, &mut |done, total| progress(Progress::Downloading { done, total }))?;

        progress(Progress::Verifying);
        if let Some(expected) = github::published_sha256(&release, &asset.name)? {
            if !expected.eq_ignore_ascii_case(&digest) {
                let _ = std::fs::remove_file(&part);
                return Err(InstallError::Checksum(asset.name.clone()));
            }
        } else {
            log::warn!("{} {} publishes no checksums; installed without verification", app.name, release.tag);
        }

        progress(Progress::Unpacking);
        let version_dir = self.dirs.apps().join(&app.id).join(sanitize(&release.tag));
        let staging = self.dirs.apps().join(&app.id).join(format!(".staging-{}", sanitize(&release.tag)));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;
        unpack::unpack(&part, &asset.name, &staging)?;
        let _ = std::fs::remove_file(&part);
        let _ = std::fs::remove_dir_all(&version_dir);
        std::fs::rename(&staging, &version_dir)?;

        let find = |stem: &Option<String>| -> Result<Option<PathBuf>, InstallError> {
            match stem {
                None => Ok(None),
                Some(s) => {
                    let exe = self.host.exe_name(s);
                    unpack::find_executable(&version_dir, &exe).map(Some).ok_or_else(|| InstallError::MissingExe { app: app.name.clone(), exe })
                }
            }
        };
        let installed = Installed { version: release.tag.clone(), dir: version_dir.clone(), gui: find(&app.gui)?, cli: find(&app.cli)? };

        let previous = self.registry.set(&app.id, installed.clone());
        self.registry.save(&self.dirs.registry_file())?;
        if let Some(old) = previous.filter(|o| o.dir != version_dir) {
            let _ = std::fs::remove_dir_all(old.dir);
        }
        progress(Progress::Done);
        Ok(installed)
    }

    pub fn uninstall(&mut self, app: &AppSpec) -> Result<(), InstallError> {
        let gone = self.registry.remove(&app.id).ok_or_else(|| InstallError::NotInstalled(app.name.clone()))?;
        self.registry.save(&self.dirs.registry_file())?;
        let _ = std::fs::remove_dir_all(self.dirs.apps().join(&app.id));
        drop(gone);
        Ok(())
    }

    /// Start the desktop app, optionally opening `files`. The hub does not wait for it.
    pub fn launch(&self, app: &AppSpec, files: &[PathBuf]) -> Result<(), InstallError> {
        match app.kind {
            Kind::Release => {
                let i = self.registry.get(&app.id).ok_or_else(|| InstallError::NotInstalled(app.name.clone()))?;
                let exe = i.gui.as_ref().or(i.cli.as_ref()).ok_or_else(|| InstallError::NotInstalled(app.name.clone()))?;
                spawn_detached(exe, files)
            }
            Kind::External => {
                let path = detect_external(app, self.host).ok_or_else(|| InstallError::NotInstalled(app.name.clone()))?;
                if path.extension().is_some_and(|e| e == "app") {
                    // a macOS bundle: let LaunchServices start it
                    let mut c = Command::new("open");
                    c.arg("-a").arg(&path);
                    if !files.is_empty() {
                        c.arg("--args").args(files);
                    }
                    c.spawn()?;
                    Ok(())
                } else {
                    spawn_detached(&path, files)
                }
            }
        }
    }

    /// Start `<cli> <mcp args…>` with piped stdio, ready for an MCP client.
    pub fn spawn_mcp(&self, app: &AppSpec) -> Result<Child, InstallError> {
        let i = self.registry.get(&app.id).ok_or_else(|| InstallError::NotInstalled(app.name.clone()))?;
        let cli = i.cli.as_ref().ok_or_else(|| InstallError::MissingExe { app: app.name.clone(), exe: "cli".into() })?;
        let args = app.mcp.clone().unwrap_or_default();
        Ok(Command::new(cli).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn()?)
    }

    /// Open the official download page of an external app in the browser.
    pub fn open_homepage(app: &AppSpec) -> Result<(), InstallError> {
        open::that_detached(&app.homepage)?;
        Ok(())
    }
}

fn spawn_detached(exe: &Path, files: &[PathBuf]) -> Result<(), InstallError> {
    let mut c = Command::new(exe);
    c.args(files).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Some(dir) = exe.parent() {
        c.current_dir(dir);
    }
    c.spawn()?;
    Ok(())
}

/// Release tags become folder names: keep them boring.
fn sanitize(tag: &str) -> String {
    tag.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn sanitize_tags() {
        assert_eq!(super::sanitize("v0.5.0"), "v0.5.0");
        assert_eq!(super::sanitize("../../evil"), ".._.._evil");
    }
}
