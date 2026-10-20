//! Install, update, detect and launch catalog apps (L2).
//!
//! Layout under the data folder (see [`HubDirs`]):
//!
//! ```text
//! apps/<id>/<version>/…        unpacked release archive or git build (one folder per installed version)
//! apps/installed.json          which version of each app is current, and where its executables are
//! downloads/                   in-flight downloads (`*.part`), removed after unpacking
//! src/<id>/…, build/<id>/…     git checkouts and their build caches (see [`Source`])
//! ```
//!
//! Everything is per-user and needs no admin rights. Nothing is ever re-hosted: archives come from
//! the app's own GitHub releases, verified against the release's SHA-256 checksums when it
//! publishes them.
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod dirs;
mod external;
mod github;
mod icons;
mod registry;
mod source;
mod unpack;
mod updates;

pub use dirs::HubDirs;
pub use external::detect_external;
pub use github::{Asset, Release, fetch_text, latest_release, newest_release};
pub use icons::IconData;
pub use registry::{Installed, Registry};
pub use source::Source;
pub use updates::{UpdateCheck, check_update};

use creativecraft_catalog::{AppSpec, Kind};
use creativecraft_platform::Host;
use creativecraft_platform::HostCommand;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};

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
    #[error("couldn't find the `{exe}` executable inside {app}'s download or build")]
    MissingExe { app: String, exe: String },
    #[error("{0}")]
    Source(String),
    #[error("building from git needs `{tool}`: {hint}")]
    MissingTool { tool: String, hint: &'static str },
    #[error("{0}")]
    Build(String),
}

/// Progress of a long operation, for the UI.
#[derive(Debug, Clone)]
pub enum Progress {
    Resolving,
    Downloading {
        done: u64,
        total: Option<u64>,
    },
    Verifying,
    Unpacking,
    /// Cloning or updating a git checkout.
    Fetching,
    /// Compiling: crates compiled so far and the one just started.
    Building {
        crates: usize,
        current: String,
    },
    /// A line of git or cargo output, for a log view.
    Log(String),
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

    /// The icon an installed external app ships with (read from its own installation).
    pub fn external_icon(&self, app: &AppSpec) -> Option<IconData> {
        let exe = detect_external(app, self.host)?;
        icons::installed_icon(app, &exe, self.host)
    }

    /// The newest release of a release-kind app.
    pub fn latest(&self, app: &AppSpec) -> Result<Release, InstallError> {
        let repo = app.repo.as_deref().ok_or_else(|| InstallError::External(app.name.clone()))?;
        github::latest_release(repo)
    }

    /// Install (or update) from `source`: official releases, a fork's releases, or a git build.
    pub fn install_from(&mut self, app: &AppSpec, source: &Source, progress: &mut dyn FnMut(Progress)) -> Result<Installed, InstallError> {
        if app.kind == Kind::External {
            return Err(InstallError::External(app.name.clone()));
        }
        source.validate()?;
        match source {
            Source::Release => self.install(app, progress),
            Source::Fork { repo } => self.install_release(app, repo, Some(format!("fork {repo}")), progress),
            Source::Git { url, branch } => self.install_git(app, url, branch, progress),
        }
    }

    /// Install (or update to) the latest release. Older versions are removed after the new one is
    /// in place, so a failed update leaves the working version untouched.
    pub fn install(&mut self, app: &AppSpec, progress: &mut dyn FnMut(Progress)) -> Result<Installed, InstallError> {
        let repo = app.repo.clone().ok_or_else(|| InstallError::External(app.name.clone()))?;
        self.install_release(app, &repo, None, progress)
    }

    fn install_release(&mut self, app: &AppSpec, repo: &str, source: Option<String>, progress: &mut dyn FnMut(Progress)) -> Result<Installed, InstallError> {
        if app.kind == Kind::External {
            return Err(InstallError::External(app.name.clone()));
        }
        progress(Progress::Resolving);
        let release = github::latest_release(repo)?;
        let names: Vec<&str> = release.assets.iter().map(|a| a.name.as_str()).collect();
        let picked =
            creativecraft_platform::pick_asset(&names, self.host).ok_or_else(|| InstallError::NoAsset { app: app.name.clone(), tag: release.tag.clone() })?;

        let version_dir = self.dirs.apps().join(&app.id).join(sanitize(&release.tag));
        let staging = self.dirs.apps().join(&app.id).join(format!(".staging-{}", sanitize(&release.tag)));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;
        self.fetch_into(app, &release, picked, &staging, progress)?;

        // The app's own download may not carry its CLI (the MCP server): on macOS the Craft apps
        // ship App.app on a .dmg and the CLI as a separate <app>-cli-<version>-macos-<arch>.zip.
        if let Some(cli) = &app.cli
            && unpack::find_executable(&staging, &self.host.exe_name(cli)).is_none()
            && let Some(extra) = creativecraft_platform::pick_cli_asset(&names, self.host, picked)
        {
            self.fetch_into(app, &release, extra, &staging, progress)?;
        }

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
        let gui = find(&app.gui)?;
        // a release that doesn't carry the CLI yet (MusicCraft before 0.4.0) still installs the
        // desktop app; agents get it once an update brings the CLI
        let cli = match find(&app.cli) {
            Err(e) if gui.is_some() => {
                log::warn!("{} {}: {e}; installed without its CLI", app.name, release.tag);
                None
            }
            other => other?,
        };
        let installed = Installed { version: release.tag.clone(), dir: version_dir.clone(), gui, cli, source };

        let previous = self.registry.set(&app.id, installed.clone());
        self.registry.save(&self.dirs.registry_file())?;
        if let Some(old) = previous.filter(|o| o.dir != version_dir) {
            let _ = std::fs::remove_dir_all(old.dir);
        }
        progress(Progress::Done);
        Ok(installed)
    }

    /// Download one release asset, check it against the release's published checksums, and unpack
    /// it into `dest`.
    fn fetch_into(&self, app: &AppSpec, release: &Release, name: &str, dest: &Path, progress: &mut dyn FnMut(Progress)) -> Result<(), InstallError> {
        fetch_asset(&self.dirs, &app.name, release, name, dest, progress)
    }

    pub fn uninstall(&mut self, app: &AppSpec) -> Result<(), InstallError> {
        let gone = self.registry.remove(&app.id).ok_or_else(|| InstallError::NotInstalled(app.name.clone()))?;
        self.registry.save(&self.dirs.registry_file())?;
        let _ = std::fs::remove_dir_all(self.dirs.apps().join(&app.id));
        self.remove_git_caches(&app.id);
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
                    let mut c = HostCommand::new("open");
                    c.arg("-a").arg(&path).detached();
                    if !files.is_empty() {
                        c.arg("--args").args(files);
                    }
                    c.into_std().spawn()?;
                    Ok(())
                } else {
                    spawn_detached(&path, files)
                }
            }
        }
    }

    /// Start `<cli> <args…>` (the app's MCP arguments, placeholders already filled in) with piped
    /// stdio, ready for an MCP client.
    pub fn spawn_mcp(&self, app: &AppSpec, args: &[String]) -> Result<Child, InstallError> {
        let i = self.registry.get(&app.id).ok_or_else(|| InstallError::NotInstalled(app.name.clone()))?;
        let cli = i.cli.as_ref().ok_or_else(|| InstallError::MissingExe { app: app.name.clone(), exe: app.cli.clone().unwrap_or_else(|| "cli".into()) })?;
        let mut c = HostCommand::new(cli);
        c.args(args);
        Ok(c.into_std().stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn()?)
    }

    /// Start the desktop app with `args` (e.g. a control port for live mode). Unlike [`Self::launch`]
    /// the process is returned, so the caller can tell whether it exited during start-up. The
    /// window stays open after the hub quits.
    pub fn launch_with_args(&self, app: &AppSpec, args: &[String]) -> Result<Child, InstallError> {
        let i = self.registry.get(&app.id).ok_or_else(|| InstallError::NotInstalled(app.name.clone()))?;
        let gui = i.gui.as_ref().ok_or_else(|| InstallError::MissingExe { app: app.name.clone(), exe: "gui".into() })?;
        let mut c = HostCommand::new(gui);
        c.args(args).detached();
        if let Some(dir) = gui.parent() {
            c.current_dir(dir);
        }
        Ok(c.into_std().stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?)
    }

    /// Open the official download page of an external app in the browser.
    pub fn open_homepage(app: &AppSpec) -> Result<(), InstallError> {
        Self::open_url(&app.homepage)
    }

    /// Open a web page in the default browser.
    pub fn open_url(url: &str) -> Result<(), InstallError> {
        open::that_detached(url)?;
        Ok(())
    }
}

/// Download the asset `name` of `release` into the downloads folder, check it against the
/// release's published SHA-256 sums (a mismatch is an error; no sums is a logged warning), and
/// unpack it into `dest`. `what` names the download in logs. The download is removed afterwards.
pub fn fetch_asset(dirs: &HubDirs, what: &str, release: &Release, name: &str, dest: &Path, progress: &mut dyn FnMut(Progress)) -> Result<(), InstallError> {
    let asset = release.assets.iter().find(|a| a.name == name).ok_or_else(|| InstallError::Archive(format!("{name} vanished from the release")))?;
    std::fs::create_dir_all(dirs.downloads())?;
    let part = dirs.downloads().join(format!("{}.part", asset.name));
    let digest = github::download(&asset.url, &part, &mut |done, total| progress(Progress::Downloading { done, total }))?;

    progress(Progress::Verifying);
    if let Some(expected) = github::published_sha256(release, &asset.name)? {
        if !expected.eq_ignore_ascii_case(&digest) {
            let _ = std::fs::remove_file(&part);
            return Err(InstallError::Checksum(asset.name.clone()));
        }
    } else {
        log::warn!("{what} {} publishes no checksum for {}; installed without verification", release.tag, asset.name);
    }

    progress(Progress::Unpacking);
    let result = unpack::unpack(&part, &asset.name, dest);
    let _ = std::fs::remove_file(&part);
    result
}

/// Start an app window that keeps running on its own (also after the hub quits).
fn spawn_detached(exe: &Path, files: &[PathBuf]) -> Result<(), InstallError> {
    let mut c = HostCommand::new(exe);
    c.args(files).detached();
    if let Some(dir) = exe.parent() {
        c.current_dir(dir);
    }
    let mut child = c.into_std().stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    // reap it whenever the user closes it, so it doesn't linger as a zombie while the hub runs
    std::thread::spawn(move || {
        let _ = child.wait();
    });
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
