//! Running programs on the user's machine, also from inside a Flatpak sandbox.
//!
//! Agent CLIs, external apps, `git` and `cargo` live on the host, outside the sandbox. When the
//! hub runs as a Flatpak (`FLATPAK_ID` is set), every program it starts goes through
//! `flatpak-spawn --host`, and the environment and working directory travel as its `--env=` and
//! `--directory=` flags: set on the outer command they would only reach `flatpak-spawn` itself.
//! Outside Flatpak a [`HostCommand`] is exactly the `std::process::Command` it describes.
//!
//! Binaries are looked up on the host's `PATH` the same way ([`find_on_path`]).

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

/// Is the hub running inside a Flatpak sandbox?
pub fn in_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some_and(|v| !v.is_empty())
}

/// A program to run on the host: what, with which arguments, extra environment and directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    pub cwd: Option<PathBuf>,
    /// Keep running after the hub quits (app windows the user opened). Inside Flatpak this drops
    /// `--watch-bus`, which otherwise ends the host process along with the hub.
    pub detached: bool,
}

impl HostCommand {
    pub fn new(program: impl AsRef<OsStr>) -> HostCommand {
        HostCommand { program: program.as_ref().to_os_string(), ..HostCommand::default() }
    }

    pub fn arg(&mut self, a: impl AsRef<OsStr>) -> &mut HostCommand {
        self.args.push(a.as_ref().to_os_string());
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut HostCommand
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args.extend(args.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }

    pub fn env(&mut self, k: impl AsRef<OsStr>, v: impl AsRef<OsStr>) -> &mut HostCommand {
        self.env.push((k.as_ref().to_os_string(), v.as_ref().to_os_string()));
        self
    }

    pub fn current_dir(&mut self, dir: impl AsRef<Path>) -> &mut HostCommand {
        self.cwd = Some(dir.as_ref().to_path_buf());
        self
    }

    /// See [`HostCommand::detached`].
    pub fn detached(&mut self) -> &mut HostCommand {
        self.detached = true;
        self
    }

    /// A `std::process::Command` that runs this on the host (stdio, if piped, is forwarded).
    pub fn into_std(self) -> Command {
        self.to_std(in_flatpak())
    }

    fn to_std(&self, sandboxed: bool) -> Command {
        let (program, args) = self.argv_os(sandboxed);
        let mut c = Command::new(program);
        c.args(args);
        if !sandboxed {
            c.envs(self.env.iter().map(|(k, v)| (k, v)));
            if let Some(d) = &self.cwd {
                c.current_dir(d);
            }
        }
        c
    }

    /// Program and arguments to execute, for callers that build the process themselves (a PTY).
    /// Inside Flatpak this is the `flatpak-spawn` line, with env and directory in it; outside,
    /// apply [`HostCommand::outer_env`] and [`HostCommand::outer_cwd`] yourself.
    pub fn argv(&self) -> (String, Vec<String>) {
        let (p, a) = self.argv_os(in_flatpak());
        (p.to_string_lossy().into_owned(), a.iter().map(|s| s.to_string_lossy().into_owned()).collect())
    }

    /// Environment to set on the process [`HostCommand::argv`] starts (none inside Flatpak,
    /// where it's already in the arguments).
    pub fn outer_env(&self) -> &[(OsString, OsString)] {
        if in_flatpak() { &[] } else { &self.env }
    }

    /// Working directory for the process [`HostCommand::argv`] starts (none inside Flatpak).
    pub fn outer_cwd(&self) -> Option<&Path> {
        if in_flatpak() { None } else { self.cwd.as_deref() }
    }

    fn argv_os(&self, sandboxed: bool) -> (OsString, Vec<OsString>) {
        if !sandboxed {
            return (self.program.clone(), self.args.clone());
        }
        // --watch-bus: the host process ends when the sandboxed hub does (not for detached apps)
        let mut a: Vec<OsString> = vec!["--host".into()];
        if !self.detached {
            a.push("--watch-bus".into());
        }
        if let Some(d) = &self.cwd {
            let mut f = OsString::from("--directory=");
            f.push(d);
            a.push(f);
        }
        for (k, v) in &self.env {
            let mut f = OsString::from("--env=");
            f.push(k);
            f.push("=");
            f.push(v);
            a.push(f);
        }
        a.push(self.program.clone());
        a.extend(self.args.iter().cloned());
        ("flatpak-spawn".into(), a)
    }
}

/// Stop a child started from a [`HostCommand`]. Inside Flatpak the child is `flatpak-spawn`,
/// which forwards SIGTERM to the host process but can't forward SIGKILL, so it gets SIGTERM
/// first; outside Flatpak this is `Child::kill`.
pub fn stop(child: &mut std::process::Child) {
    if cfg!(unix) && in_flatpak() {
        let _ = Command::new("kill").args(["-TERM", &child.id().to_string()]).status();
        for _ in 0..20 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    let _ = child.kill();
}

/// Find `name` the way a shell on the host would: a path is used as given if it's a file;
/// otherwise each `PATH` entry (with `.exe`/`.cmd`/`.bat` on Windows). Inside Flatpak the host's
/// `PATH` is asked through `flatpak-spawn` (answers are cached for the session).
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    let p = Path::new(name);
    if p.components().count() > 1 {
        return if in_flatpak() { host_is_file(p).then(|| p.to_path_buf()) } else { p.is_file().then(|| p.to_path_buf()) };
    }
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    let on_path = if in_flatpak() {
        host_command_v(name)
    } else {
        std::env::var_os("PATH")
            .and_then(|path| std::env::split_paths(&path).flat_map(|d| exts.iter().map(move |e| d.join(format!("{name}{e}")))).find(|c| c.is_file()))
    };
    on_path.or_else(|| {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
        user_bin_dirs(Path::new(&home))
            .into_iter()
            .flat_map(|d| exts.iter().map(move |e| d.join(format!("{name}{e}"))))
            .find(|c| if in_flatpak() { host_is_file(c) } else { c.is_file() })
    })
}

/// Where per-user installers put CLIs. Apps started from a desktop menu don't read `~/.bashrc`,
/// so these are often missing from their `PATH` even though a terminal finds the program
/// (Claude Code's installer uses `~/.local/bin`, nvm a folder per Node version).
fn user_bin_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        [".local/bin", ".claude/local", ".npm-global/bin", ".bun/bin", ".volta/bin", ".opencode/bin", ".cargo/bin"].iter().map(|d| home.join(d)).collect();
    if let Ok(versions) = std::fs::read_dir(home.join(".nvm/versions/node")) {
        let mut nvm: Vec<PathBuf> = versions.flatten().map(|v| v.path().join("bin")).collect();
        nvm.sort();
        dirs.extend(nvm.into_iter().rev()); // newest version first
    }
    if cfg!(unix) {
        dirs.push(PathBuf::from("/usr/local/bin"));
    }
    dirs
}

fn cache() -> &'static Mutex<HashMap<String, Option<PathBuf>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `command -v NAME` in a shell on the host. The name is a positional argument, never part of
/// the script, so it can't be interpreted by the shell.
fn host_command_v(name: &str) -> Option<PathBuf> {
    if let Ok(c) = cache().lock()
        && let Some(hit) = c.get(name)
    {
        return hit.clone();
    }
    let out = Command::new("flatpak-spawn").args(host_lookup_args(name)).output().ok();
    let found =
        out.filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| s.starts_with('/')).map(PathBuf::from);
    // misses aren't cached: a CLI installed while the hub runs is found on the next look
    if let Some(f) = &found
        && let Ok(mut c) = cache().lock()
    {
        c.insert(name.to_string(), Some(f.clone()));
    }
    found
}

fn host_lookup_args(name: &str) -> Vec<String> {
    ["--host", "sh", "-c", r#"command -v -- "$1""#, "sh", name].iter().map(|s| s.to_string()).collect()
}

/// `--filesystem=home` shares the home folder, but system paths (`/usr/bin`) are the sandbox's;
/// ask the host for anything outside home.
fn host_is_file(p: &Path) -> bool {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if home.as_ref().is_some_and(|h| p.starts_with(h)) {
        return p.is_file();
    }
    Command::new("flatpak-spawn").args(["--host", "test", "-f"]).arg(p).status().is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> HostCommand {
        let mut c = HostCommand::new("claude");
        c.args(["--model", "opus"]).env("TERM", "xterm-256color").env("OPENCODE_CONFIG", "/home/a/x.json").current_dir("/home/a/Projects");
        c
    }

    #[test]
    fn outside_flatpak_runs_as_is() {
        let (p, a) = sample().argv_os(false);
        assert_eq!(p, "claude");
        assert_eq!(a, ["--model", "opus"]);
        let std = sample().to_std(false);
        assert_eq!(std.get_program(), "claude");
        assert_eq!(std.get_current_dir(), Some(Path::new("/home/a/Projects")));
        assert_eq!(std.get_envs().count(), 2);
    }

    #[test]
    fn inside_flatpak_goes_through_flatpak_spawn() {
        let (p, a) = sample().argv_os(true);
        assert_eq!(p, "flatpak-spawn");
        assert_eq!(
            a,
            [
                "--host",
                "--watch-bus",
                "--directory=/home/a/Projects",
                "--env=TERM=xterm-256color",
                "--env=OPENCODE_CONFIG=/home/a/x.json",
                "claude",
                "--model",
                "opus"
            ]
        );
        // env and directory travel as flags only, never on the outer process
        let std = sample().to_std(true);
        assert_eq!(std.get_program(), "flatpak-spawn");
        assert_eq!(std.get_envs().count(), 0);
        assert_eq!(std.get_current_dir(), None);
    }

    #[test]
    fn inside_flatpak_without_env_or_dir() {
        let (p, a) = HostCommand::new("git").argv_os(true);
        assert_eq!(p, "flatpak-spawn");
        assert_eq!(a, ["--host", "--watch-bus", "git"]);
    }

    #[test]
    fn detached_apps_outlive_the_hub() {
        let mut c = HostCommand::new("/usr/bin/blender");
        c.arg("scene.blend").current_dir("/home/a").detached();
        let (p, a) = c.argv_os(true);
        assert_eq!(p, "flatpak-spawn");
        assert_eq!(a, ["--host", "--directory=/home/a", "/usr/bin/blender", "scene.blend"]);
        assert_eq!(c.argv_os(false), ("/usr/bin/blender".into(), vec!["scene.blend".into()]));
    }

    #[test]
    fn host_lookup_keeps_the_name_out_of_the_script() {
        assert_eq!(host_lookup_args("x; rm -rf ~"), ["--host", "sh", "-c", r#"command -v -- "$1""#, "sh", "x; rm -rf ~"]);
    }

    #[test]
    fn path_lookup_outside_flatpak() {
        assert_eq!(find_on_path(""), None);
        let dir = std::env::temp_dir().join(format!("cc-host-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("tool");
        std::fs::write(&f, b"").unwrap();
        if !in_flatpak() {
            assert_eq!(find_on_path(&f.display().to_string()), Some(f.clone()));
            assert_eq!(find_on_path(&dir.join("missing").display().to_string()), None);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn user_bin_dirs_cover_desktop_launches() {
        let home = std::env::temp_dir().join(format!("cc-home-{}", std::process::id()));
        for v in ["v18.0.0", "v22.1.0"] {
            std::fs::create_dir_all(home.join(".nvm/versions/node").join(v).join("bin")).unwrap();
        }
        let dirs = user_bin_dirs(&home);
        assert_eq!(dirs[0], home.join(".local/bin"));
        let nvm22 = dirs.iter().position(|d| d.ends_with("v22.1.0/bin")).unwrap();
        let nvm18 = dirs.iter().position(|d| d.ends_with("v18.0.0/bin")).unwrap();
        assert!(nvm22 < nvm18);
        let _ = std::fs::remove_dir_all(&home);
    }
}
