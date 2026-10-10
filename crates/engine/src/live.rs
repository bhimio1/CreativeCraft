//! Starting an app's MCP server for an agent: headless, or "live" in the app's visible window.
//!
//! Live mode starts the desktop app with a localhost control channel on a free port (and, for
//! apps that authenticate it, a private token file the app creates), waits until the window
//! listens, then starts the CLI's MCP server connected to it. The user watches the agent work
//! and can take over; the window stays open when the agent is done.

use crate::{EngineError, Session};
use creativecraft_catalog::{AppSpec, Live};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

/// How long a window may take to open its control channel.
const START_TIMEOUT: Duration = Duration::from_secs(60);

/// `args` with `{port}`, `{token}` and `{projects}` filled in.
pub(crate) fn fill(args: &[String], subs: &[(&str, String)]) -> Vec<String> {
    args.iter().map(|a| subs.iter().fold(a.clone(), |acc, (k, v)| acc.replace(k, v))).collect()
}

fn free_port() -> Result<u16, EngineError> {
    Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?.local_addr()?.port())
}

/// `<config>/control`, readable only by the user: control-channel tokens are bearer credentials.
fn private_dir(config: &Path) -> Result<PathBuf, EngineError> {
    let dir = config.join("control");
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    // tokens of windows from earlier days are of no use any more
    let week = Duration::from_secs(7 * 24 * 3600);
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|age| age > week)) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    Ok(dir)
}

impl Session {
    /// Start `id`'s MCP server. `window`: work in the app's visible window (live mode) when the
    /// app supports it; `None` follows the user's setting.
    pub fn spawn_mcp(&self, id: &str, window: Option<bool>) -> Result<Child, EngineError> {
        if let Some(c) = self.settings.custom_mcp.iter().find(|c| c.id == id) {
            let mut cmd = creativecraft_platform::HostCommand::new(&c.command);
            cmd.args(&c.args);
            return Ok(cmd
                .into_std()
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::inherit())
                .spawn()?);
        }
        if let Some(plugin) = self.spawn_plugin(id) {
            return plugin;
        }
        let app = self.app(id)?;
        std::fs::create_dir_all(&self.settings.projects_dir)?;
        let projects = self.settings.projects_dir.display().to_string();
        if window.unwrap_or(self.settings.show_app_windows)
            && let Some(live) = &app.live
        {
            return self.spawn_live(&app, live, projects);
        }
        let args = fill(app.mcp.as_deref().unwrap_or_default(), &[("{projects}", projects)]);
        Ok(self.installer.spawn_mcp(&app, &args)?)
    }

    fn spawn_live(&self, app: &AppSpec, live: &Live, projects: String) -> Result<Child, EngineError> {
        let port = free_port()?;
        let token = private_dir(self.dirs.config())?.join(format!("{}-{port}.token", app.id));
        let _ = std::fs::remove_file(&token);
        let subs = [("{port}", port.to_string()), ("{token}", token.display().to_string()), ("{projects}", projects)];
        let mut gui = self.installer.launch_with_args(app, &fill(&live.gui, &subs))?;

        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let started = Instant::now();
        let needs_token = live.gui.iter().any(|a| a.contains("{token}"));
        loop {
            let listening = TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok();
            if listening && (!needs_token || token.is_file()) {
                break;
            }
            if let Ok(Some(status)) = gui.try_wait() {
                return Err(EngineError::Live(format!("{} closed before its window was ready ({status})", app.name)));
            }
            if started.elapsed() > START_TIMEOUT {
                return Err(EngineError::Live(format!("{}'s window didn't open its control channel within a minute", app.name)));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        // the window is the user's now; reap it whenever they close it
        std::thread::spawn(move || {
            let _ = gui.wait();
        });
        Ok(self.installer.spawn_mcp(app, &fill(&live.mcp, &subs))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders() {
        let args: Vec<String> = ["mcp", "--bridge", "127.0.0.1:{port}", "--control-token-file", "{token}", "--root={projects}"].map(String::from).to_vec();
        let subs = [("{port}", "4242".to_string()), ("{token}", "/c/t.token".to_string()), ("{projects}", "/home/a/Projects".to_string())];
        assert_eq!(fill(&args, &subs), ["mcp", "--bridge", "127.0.0.1:4242", "--control-token-file", "/c/t.token", "--root=/home/a/Projects"]);
    }

    #[test]
    fn catalog_live_specs_fill_cleanly() {
        let cat = creativecraft_catalog::Catalog::builtin().unwrap();
        let subs = [("{port}", "1".to_string()), ("{token}", "t".to_string()), ("{projects}", "p".to_string())];
        for app in &cat.apps {
            for args in app.live.iter().flat_map(|l| [&l.gui, &l.mcp]).chain(app.mcp.as_ref()) {
                assert!(fill(args, &subs).iter().all(|a| !a.contains('{')), "{}: unfilled placeholder in {args:?}", app.id);
            }
        }
    }
}
