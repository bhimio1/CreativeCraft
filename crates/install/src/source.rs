//! Where an app comes from: its official releases, a fork's releases, or a git checkout (the
//! official repository or any fork) built on this computer with `cargo`.
//!
//! Git builds shell out to the user's own `git` and `cargo`. The checkout and the build cache are
//! kept per source, so updating only fetches and rebuilds what changed:
//!
//! ```text
//! src/<id>/<source-key>/        shallow git checkout (owned by the hub; reset on each update)
//! build/<id>/<source-key>/      CARGO_TARGET_DIR for that checkout
//! apps/<id>/git-<rev>/          the built executables, installed like a release
//! ```

use crate::{InstallError, Installed, Installer, Progress, sanitize};
use creativecraft_catalog::AppSpec;
use creativecraft_platform::HostCommand;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// How an app is installed. The default is the catalog's own releases.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// Prebuilt releases from GitHub: the catalog's repository, or a fork's (`owner/name`).
    #[default]
    Release,
    Fork {
        repo: String,
    },
    /// Built from a git checkout. `url` empty = the catalog repository; `branch` empty = its
    /// default branch.
    Git {
        #[serde(default)]
        url: String,
        #[serde(default)]
        branch: String,
    },
}

impl Source {
    /// A short label for the UI: "Official releases", "Fork alice/photocraft", "Git main", …
    pub fn label(&self) -> String {
        match self {
            Source::Release => "Official releases".into(),
            Source::Fork { repo } => format!("Releases from {repo}"),
            Source::Git { url, branch } => {
                let from = if url.trim().is_empty() { "official repository".to_string() } else { short_url(url) };
                let branch = if branch.trim().is_empty() { "default branch".to_string() } else { branch.clone() };
                format!("Git, {branch} of {from}")
            }
        }
    }

    /// Reject values that can't work before touching the network.
    pub fn validate(&self) -> Result<(), InstallError> {
        let bad = |why: &str| Err(InstallError::Source(why.to_string()));
        match self {
            Source::Release => Ok(()),
            Source::Fork { repo } => {
                let parts: Vec<&str> = repo.split('/').collect();
                if parts.len() != 2 || parts.iter().any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))) {
                    return bad("a fork is written owner/name, e.g. alice/photocraft");
                }
                Ok(())
            }
            Source::Git { url, branch } => {
                let url = url.trim();
                if !url.is_empty() && !(url.starts_with("https://") || url.starts_with("ssh://") || url.starts_with("git@") || Path::new(url).is_absolute()) {
                    return bad("a git source is an https://, ssh:// or git@ address, or a local folder");
                }
                // a branch is passed to git as an argument: no leading dash, no spaces
                if branch.starts_with('-') || branch.contains(char::is_whitespace) || url.starts_with('-') {
                    return bad("that branch name isn't valid");
                }
                Ok(())
            }
        }
    }
}

fn short_url(url: &str) -> String {
    let u = url.trim().trim_end_matches('/').trim_end_matches(".git");
    u.strip_prefix("https://github.com/").or_else(|| u.strip_prefix("git@github.com:")).unwrap_or(u).to_string()
}

/// The git URL for a source: the given one, or the catalog repository on GitHub.
fn git_url(app: &AppSpec, url: &str) -> Result<String, InstallError> {
    if !url.trim().is_empty() {
        return Ok(url.trim().to_string());
    }
    let repo = app.repo.as_deref().ok_or_else(|| InstallError::External(app.name.clone()))?;
    Ok(format!("https://github.com/{repo}.git"))
}

/// A folder name that is stable for a URL and branch.
fn source_key(url: &str, branch: &str) -> String {
    // FNV-1a: stable across runs and platforms, unlike std's hasher
    let mut h: u64 = 0xcbf29ce484222325;
    for b in url.bytes().chain([0]).chain(branch.bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{}-{h:016x}", sanitize(&short_url(url).replace(['/', ':'], "-")))
}

fn tool(name: &str, hint: &'static str) -> Result<PathBuf, InstallError> {
    if creativecraft_platform::in_flatpak() {
        // git and cargo live on the host, not in the sandbox
        return creativecraft_platform::find_on_path(name).ok_or(InstallError::MissingTool { tool: name.to_string(), hint });
    }
    let exts: &[&str] = if cfg!(windows) { &[".exe", ""] } else { &[""] };
    let path = std::env::var_os("PATH").ok_or(InstallError::MissingTool { tool: name.to_string(), hint })?;
    std::env::split_paths(&path)
        .flat_map(|d| exts.iter().map(move |e| d.join(format!("{name}{e}"))))
        .find(|p| p.is_file())
        .ok_or(InstallError::MissingTool { tool: name.to_string(), hint })
}

const GIT_HINT: &str = "install Git from https://git-scm.com";
const CARGO_HINT: &str = "install Rust from https://rustup.rs";

/// Run a command, sending each output line to `line`; fail with the last lines if it fails.
fn run(mut cmd: HostCommand, what: &str, line: &mut dyn FnMut(&str)) -> Result<(), InstallError> {
    // never stop to ask for credentials: a private fork needs the user's own git setup
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let mut cmd = cmd.into_std();
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take();
    let out_thread = stdout.map(|o| std::thread::spawn(move || BufReader::new(o).lines().map_while(Result::ok).collect::<Vec<_>>()));
    let mut tail: Vec<String> = Vec::new();
    if let Some(err) = child.stderr.take() {
        for l in BufReader::new(err).lines().map_while(Result::ok) {
            line(&l);
            tail.push(l);
            if tail.len() > 12 {
                tail.remove(0);
            }
        }
    }
    if let Some(t) = out_thread
        && let Ok(lines) = t.join()
    {
        for l in lines.iter().rev().take(4).rev() {
            tail.push(l.clone());
        }
    }
    let status = child.wait()?;
    if status.success() { Ok(()) } else { Err(InstallError::Build(format!("{what} failed ({status}):\n{}", tail.join("\n")))) }
}

fn git_output(git: &Path, dir: &Path, args: &[&str]) -> Result<String, InstallError> {
    let mut c = HostCommand::new(git);
    c.arg("-C").arg(dir).args(args).env("GIT_TERMINAL_PROMPT", "0");
    let out = c.into_std().output()?;
    if !out.status.success() {
        return Err(InstallError::Build(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The commit `branch` (empty: the default branch) points at in the repository at `url`, asked
/// with `git ls-remote` (nothing is downloaded).
pub(crate) fn remote_head(app: &AppSpec, url: &str, branch: &str) -> Result<String, InstallError> {
    let git = tool("git", GIT_HINT)?;
    let url = git_url(app, url)?;
    let branch = branch.trim();
    let wanted = if branch.is_empty() { "HEAD".to_string() } else { format!("refs/heads/{branch}") };
    let mut c = HostCommand::new(&git);
    c.args(["ls-remote", "--"]).arg(&url).arg(&wanted).env("GIT_TERMINAL_PROMPT", "0");
    let out = c.into_std().output()?;
    if !out.status.success() {
        return Err(InstallError::Source(format!("couldn't reach {}: {}", short_url(&url), String::from_utf8_lossy(&out.stderr).trim())));
    }
    head_from_ls_remote(&String::from_utf8_lossy(&out.stdout), &wanted)
        .ok_or_else(|| InstallError::Source(format!("{} has no branch {}", short_url(&url), if branch.is_empty() { "HEAD" } else { branch })))
}

/// The commit for `wanted` in `git ls-remote` output (`<sha>\t<ref>` lines).
pub(crate) fn head_from_ls_remote(out: &str, wanted: &str) -> Option<String> {
    out.lines().find_map(|l| {
        let (sha, r) = l.split_once('\t')?;
        (r.trim() == wanted && sha.len() >= 10).then(|| sha.trim().to_string())
    })
}

impl Installer {
    /// Clone or update the checkout for `url`/`branch`, build the app's executables with cargo
    /// and install them as `apps/<id>/git-<rev>`. The previous install stays until this succeeds.
    pub(crate) fn install_git(&mut self, app: &AppSpec, url: &str, branch: &str, progress: &mut dyn FnMut(Progress)) -> Result<Installed, InstallError> {
        let git = tool("git", GIT_HINT)?;
        let cargo = tool("cargo", CARGO_HINT)?;
        let url = git_url(app, url)?;
        let branch = branch.trim();
        let key = source_key(&url, branch);
        let checkout = self.dirs.data().join("src").join(&app.id).join(&key);
        let target = self.dirs.data().join("build").join(&app.id).join(&key);

        // ── fetch
        progress(Progress::Fetching);
        if checkout.join(".git").is_dir() {
            let mut c = HostCommand::new(&git);
            c.arg("-C").arg(&checkout).args(["fetch", "--depth", "1", "origin"]);
            c.arg(if branch.is_empty() { "HEAD" } else { branch });
            run(c, "git fetch", &mut |l: &str| progress(Progress::Log(l.to_string())))?;
            // the checkout is the hub's own copy: reset it to exactly what was fetched
            let mut c = HostCommand::new(&git);
            c.arg("-C").arg(&checkout).args(["reset", "--hard", "FETCH_HEAD"]);
            run(c, "git reset", &mut |l: &str| progress(Progress::Log(l.to_string())))?;
        } else {
            let _ = std::fs::remove_dir_all(&checkout);
            if let Some(parent) = checkout.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut c = HostCommand::new(&git);
            c.args(["clone", "--depth", "1"]);
            if !branch.is_empty() {
                c.args(["--branch", branch]);
            }
            c.arg("--").arg(&url).arg(&checkout);
            run(c, "git clone", &mut |l: &str| progress(Progress::Log(l.to_string())))?;
        }
        let rev = git_output(&git, &checkout, &["rev-parse", "--short=10", "HEAD"])?;

        // ── build the GUI and CLI packages (named like their executables in every Craft repo)
        let packages: Vec<&String> = [&app.gui, &app.cli].into_iter().flatten().collect();
        let build = |pkgs: &[&String], progress: &mut dyn FnMut(Progress)| -> Result<(), InstallError> {
            let mut c = HostCommand::new(&cargo);
            c.current_dir(&checkout).args(["build", "--release"]).env("CARGO_TARGET_DIR", &target).env("CARGO_TERM_COLOR", "never");
            for p in pkgs {
                c.arg("-p").arg(p.as_str());
            }
            let mut crates = 0usize;
            run(c, "cargo build", &mut |l: &str| {
                let l = l.trim_start();
                if let Some(rest) = l.strip_prefix("Compiling ") {
                    crates += 1;
                    progress(Progress::Building { crates, current: rest.split_whitespace().next().unwrap_or("").to_string() });
                } else {
                    progress(Progress::Log(l.to_string()));
                }
            })
        };
        progress(Progress::Building { crates: 0, current: String::new() });
        if let Err(e) = build(&packages, progress) {
            // a fork may name its packages differently: fall back to the workspace's default build
            if matches!(&e, InstallError::Build(m) if m.contains("did not match any packages") || m.contains("package ID specification")) {
                build(&[], progress)?;
            } else {
                return Err(e);
            }
        }

        // ── install the executables like a release
        progress(Progress::Unpacking);
        let version = format!("git-{rev}");
        let version_dir = self.dirs.apps().join(&app.id).join(&version);
        let staging = self.dirs.apps().join(&app.id).join(format!(".staging-{version}"));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;
        let copy = |stem: &Option<String>| -> Result<Option<PathBuf>, InstallError> {
            let Some(stem) = stem else { return Ok(None) };
            let exe = self.host.exe_name(stem);
            let built = target.join("release").join(&exe);
            if !built.is_file() {
                return Err(InstallError::MissingExe { app: app.name.clone(), exe });
            }
            std::fs::copy(&built, staging.join(&exe))?;
            Ok(Some(version_dir.join(&exe)))
        };
        let (gui, cli) = (copy(&app.gui)?, copy(&app.cli)?);
        let _ = std::fs::remove_dir_all(&version_dir);
        std::fs::rename(&staging, &version_dir)?;

        let source = if branch.is_empty() { format!("git {}", short_url(&url)) } else { format!("git {} ({branch})", short_url(&url)) };
        let installed = Installed { version, dir: version_dir.clone(), gui, cli, source: Some(source) };
        let previous = self.registry.set(&app.id, installed.clone());
        self.registry.save(&self.dirs.registry_file())?;
        if let Some(old) = previous.filter(|o| o.dir != version_dir) {
            let _ = std::fs::remove_dir_all(old.dir);
        }
        progress(Progress::Done);
        Ok(installed)
    }

    /// Remove the checkouts and build caches of git installs of `id`.
    pub(crate) fn remove_git_caches(&self, id: &str) {
        let _ = std::fs::remove_dir_all(self.dirs.data().join("src").join(id));
        let _ = std::fs::remove_dir_all(self.dirs.data().join("build").join(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation() {
        assert!(Source::Release.validate().is_ok());
        assert!(Source::Fork { repo: "alice/photocraft".into() }.validate().is_ok());
        assert!(Source::Fork { repo: "alice".into() }.validate().is_err());
        assert!(Source::Fork { repo: "alice/photo craft".into() }.validate().is_err());
        assert!(Source::Git { url: String::new(), branch: String::new() }.validate().is_ok());
        assert!(Source::Git { url: "https://github.com/alice/photocraft".into(), branch: "dev".into() }.validate().is_ok());
        assert!(Source::Git { url: "git@github.com:alice/photocraft.git".into(), branch: String::new() }.validate().is_ok());
        assert!(Source::Git { url: "--upload-pack=evil".into(), branch: String::new() }.validate().is_err());
        assert!(Source::Git { url: String::new(), branch: "--force".into() }.validate().is_err());
        assert!(Source::Git { url: "file.txt".into(), branch: String::new() }.validate().is_err());
    }

    #[test]
    fn labels_and_keys() {
        assert_eq!(Source::Fork { repo: "alice/photocraft".into() }.label(), "Releases from alice/photocraft");
        assert_eq!(Source::Git { url: "https://github.com/alice/photocraft.git".into(), branch: "dev".into() }.label(), "Git, dev of alice/photocraft");
        assert_eq!(Source::Git { url: String::new(), branch: String::new() }.label(), "Git, default branch of official repository");
        let a = source_key("https://github.com/alice/photocraft.git", "dev");
        assert_eq!(a, source_key("https://github.com/alice/photocraft.git", "dev"));
        assert_ne!(a, source_key("https://github.com/alice/photocraft.git", "main"));
        assert!(a.starts_with("alice-photocraft-"), "{a}");
    }

    #[test]
    fn serde_shape() {
        let s: Source = serde_json::from_str(r#"{"kind":"git","branch":"main"}"#).unwrap();
        assert_eq!(s, Source::Git { url: String::new(), branch: "main".into() });
        assert_eq!(serde_json::to_string(&Source::Fork { repo: "a/b".into() }).unwrap(), r#"{"kind":"fork","repo":"a/b"}"#);
    }
}
