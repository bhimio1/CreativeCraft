//! Any remote the user set up in their own [rclone](https://rclone.org): Dropbox, OneDrive, Box,
//! pCloud, Mega, Proton Drive, SFTP and SMB shares on a NAS, and 70-odd more. rclone keeps those
//! accounts and their sign-ins (`rclone config`); the hub only runs `rclone` with a remote name and
//! never sees credentials.
//!
//! Like `git` and the agent CLIs, `rclone` is an external program on the user's machine (run on the
//! host when CreativeCraft is a Flatpak). Remotes whose backend has no MD5 are identified by size
//! and modification time; the sync remembers which MD5 each stands for.

use crate::{CloudError, CloudStore, RemoteFile};
use creativecraft_platform::{HostCommand, find_on_path};
use serde::Deserialize;
use std::path::{Path, PathBuf};

const TRASH: &str = ".creativecraft-trash";

pub struct RcloneStore {
    rclone: PathBuf,
    /// `remote:` (with the colon)
    remote: String,
    /// Folder inside the remote, without slashes at the ends ("" for the remote's root).
    folder: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Item {
    path: String,
    #[serde(default)]
    size: i64,
    #[serde(default)]
    mod_time: String,
    #[serde(default)]
    is_dir: bool,
    #[serde(default)]
    hashes: std::collections::BTreeMap<String, String>,
}

fn rclone_path() -> Result<PathBuf, CloudError> {
    find_on_path("rclone")
        .ok_or_else(|| CloudError::Config("rclone isn't installed; get it from https://rclone.org/install/ and set up a remote with `rclone config`".into()))
}

/// The remotes configured in the user's rclone (`name:`), for the settings menu.
pub fn rclone_remotes() -> Result<Vec<String>, CloudError> {
    let rclone = rclone_path()?;
    let out = run(&rclone, &["listremotes"])?;
    Ok(out.lines().map(str::trim).filter(|l| l.ends_with(':')).map(String::from).collect())
}

/// Run rclone; stdout on success, its error message otherwise.
fn run(rclone: &Path, args: &[&str]) -> Result<String, CloudError> {
    let mut c = HostCommand::new(rclone);
    // never stop to ask for a config password on a terminal nobody is watching
    c.args(args).env("RCLONE_ASK_PASSWORD", "false");
    let out = c.into_std().output()?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let last: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).collect();
    let msg = last.iter().rev().take(3).rev().copied().collect::<Vec<_>>().join("\n");
    // exit code 3: directory not found
    if out.status.code() == Some(3) {
        return Err(CloudError::Missing(msg));
    }
    Err(CloudError::Net(format!("rclone: {msg}")))
}

impl RcloneStore {
    /// `remote` is a configured remote (`dropbox` or `dropbox:`); `folder` a folder inside it.
    pub fn new(remote: &str, folder: &str) -> Result<RcloneStore, CloudError> {
        let remote = remote.trim().trim_end_matches(':');
        if remote.is_empty() || remote.starts_with('-') || remote.contains(['/', '\\']) {
            return Err(CloudError::Config("choose one of your rclone remotes".into()));
        }
        Ok(RcloneStore { rclone: rclone_path()?, remote: format!("{remote}:"), folder: folder.trim().trim_matches('/').to_string() })
    }

    /// `remote:folder/rel`
    fn target(&self, rel: &str) -> String {
        match (self.folder.is_empty(), rel.is_empty()) {
            (true, _) => format!("{}{rel}", self.remote),
            (false, true) => format!("{}{}", self.remote, self.folder),
            (false, false) => format!("{}{}/{rel}", self.remote, self.folder),
        }
    }

    fn item_file(i: Item) -> RemoteFile {
        let md5 = i.hashes.get("md5").or_else(|| i.hashes.get("MD5")).map(|h| h.to_ascii_lowercase()).filter(|h| h.len() == 32);
        let fingerprint = md5.clone().unwrap_or_else(|| format!("{}:{}", i.size, i.mod_time));
        RemoteFile { path: i.path, md5, size: i.size.max(0) as u64, fingerprint }
    }
}

impl CloudStore for RcloneStore {
    fn label(&self) -> String {
        format!("rclone: {}", self.target(""))
    }

    fn list(&mut self) -> Result<Vec<RemoteFile>, CloudError> {
        let root = self.target("");
        let out = match run(
            &self.rclone,
            &["lsjson", "--recursive", "--files-only", "--hash", "--no-mimetype", "--exclude", "/.creativecraft-trash/**", "--", &root],
        ) {
            Ok(o) => o,
            // a folder that doesn't exist yet is an empty one
            Err(CloudError::Missing(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let items: Vec<Item> = serde_json::from_str(&out)?;
        Ok(items.into_iter().filter(|i| !i.is_dir && !i.path.starts_with(TRASH)).map(Self::item_file).collect())
    }

    fn download(&mut self, path: &str, dest: &Path) -> Result<(), CloudError> {
        let dest = dest.display().to_string();
        // --ignore-times: always copy (by default rclone skips a file whose size and modification
        // time look the same, which would silently keep the old content)
        match run(&self.rclone, &["copyto", "--ignore-times", "--", &self.target(path), &dest]) {
            Err(CloudError::Missing(_)) => Err(CloudError::Missing(path.into())),
            other => other.map(|_| ()),
        }
    }

    fn upload(&mut self, path: &str, src: &Path) -> Result<Option<String>, CloudError> {
        let local_md5 = crate::md5_file(src)?;
        let (src, dest) = (src.display().to_string(), self.target(path));
        // --ignore-times: always copy, even when size and modification time look unchanged (two
        // edits of the same length in the same second would otherwise be skipped)
        run(&self.rclone, &["copyto", "--ignore-times", "--", &src, &dest])?;
        // the uploaded file as list() will report it; where the remote has MD5s, check them
        let out = run(&self.rclone, &["lsjson", "--files-only", "--hash", "--no-mimetype", "--", &dest])?;
        let items: Vec<Item> = serde_json::from_str(&out)?;
        let Some(file) = items.into_iter().next().map(Self::item_file) else {
            return Err(CloudError::Net(format!("rclone: {path} isn't there after uploading it")));
        };
        if file.md5.as_ref().is_some_and(|m| *m != local_md5) {
            return Err(CloudError::Corrupt { path: path.to_string() });
        }
        Ok(Some(file.fingerprint))
    }

    fn trash(&mut self, path: &str) -> Result<(), CloudError> {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let dest = self.target(&format!("{TRASH}/{stamp}/{path}"));
        run(&self.rclone, &["moveto", "--", &self.target(path), &dest])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(remote: &str, folder: &str) -> RcloneStore {
        RcloneStore { rclone: PathBuf::from("rclone"), remote: format!("{}:", remote.trim_end_matches(':')), folder: folder.trim_matches('/').into() }
    }

    #[test]
    fn targets() {
        assert_eq!(store("dropbox", "CreativeCraft").target("a/b.png"), "dropbox:CreativeCraft/a/b.png");
        assert_eq!(store("dropbox:", "/Art/").target(""), "dropbox:Art");
        assert_eq!(store("nas", "").target("x.png"), "nas:x.png");
    }

    #[test]
    fn lsjson_items() {
        let items: Vec<Item> = serde_json::from_str(
            r#"[{"Path":"a.png","Name":"a.png","Size":3,"ModTime":"2025-01-02T03:04:05.000000000Z","IsDir":false,"Hashes":{"md5":"0CC175B9C0F1B6A831C399E269772661"}},
                {"Path":"b.png","Name":"b.png","Size":5,"ModTime":"2025-01-02T03:04:06Z","IsDir":false,"Hashes":{"quickxor":"abc"}}]"#,
        )
        .unwrap();
        let files: Vec<RemoteFile> = items.into_iter().map(RcloneStore::item_file).collect();
        assert_eq!(files[0].md5.as_deref(), Some("0cc175b9c0f1b6a831c399e269772661"));
        assert_eq!(files[0].fingerprint, "0cc175b9c0f1b6a831c399e269772661");
        assert_eq!(files[1].md5, None);
        assert_eq!(files[1].fingerprint, "5:2025-01-02T03:04:06Z");
    }

    #[test]
    fn remote_names_are_checked() {
        assert!(RcloneStore::new("--config=/tmp/x", "").is_err());
        assert!(RcloneStore::new("", "").is_err());
    }
}
