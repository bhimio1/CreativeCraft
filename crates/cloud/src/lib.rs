//! Cloud saves (L2).
//!
//! A [`CloudStore`] is anything that can list, fetch, store and trash files by relative path.
//! Two backends ship:
//!
//! - [`GoogleDrive`]: Drive REST v3 with the narrow `drive.file` scope (the hub can only see files
//!   it created, never the rest of the user's Drive), files kept under an "Atelier Projects" folder
//!   that mirrors the local folder tree.
//! - [`FolderStore`]: any folder, including one kept in sync by a desktop client (Google Drive for
//!   desktop, Dropbox, OneDrive, Nextcloud, Syncthing…). Also what the tests use.
//!
//! [`sync`] runs one sync pass using the planner in `atelier-sync-plan`.
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod folder;
mod gdrive;
mod oauth;
pub mod sync;

pub use atelier_sync_plan::Action;
pub use folder::FolderStore;
pub use gdrive::GoogleDrive;
pub use oauth::{GoogleClient, Token};

use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("network: {0}")]
    Net(String),
    #[error("cloud storage answered {status}: {body}")]
    Http { status: u16, body: String },
    #[error("not signed in to cloud storage")]
    SignedOut,
    #[error("sign-in failed: {0}")]
    Auth(String),
    #[error("{0} is not in cloud storage")]
    Missing(String),
}

impl From<ureq::Error> for CloudError {
    fn from(e: ureq::Error) -> CloudError {
        match e {
            ureq::Error::Status(status, resp) => {
                let body = resp.into_string().unwrap_or_default();
                CloudError::Http { status, body: body.chars().take(500).collect() }
            }
            other => CloudError::Net(other.to_string()),
        }
    }
}

/// A file in cloud storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFile {
    /// `/`-separated path relative to the projects root.
    pub path: String,
    /// Lowercase hex MD5 of the content.
    pub md5: String,
    pub size: u64,
}

/// A place project files are saved to. Paths are relative and `/`-separated.
pub trait CloudStore {
    /// Human-readable name, for the UI ("Google Drive", "Folder: ~/Dropbox/Art").
    fn label(&self) -> String;
    fn list(&mut self) -> Result<Vec<RemoteFile>, CloudError>;
    /// Write the remote file at `path` to the local file `dest`.
    fn download(&mut self, path: &str, dest: &Path) -> Result<(), CloudError>;
    /// Create or replace the remote file at `path` with the contents of the local `src`.
    fn upload(&mut self, path: &str, src: &Path) -> Result<(), CloudError>;
    /// Move the remote file to the trash (recoverable), never a hard delete.
    fn trash(&mut self, path: &str) -> Result<(), CloudError>;
}

/// Lowercase hex MD5 of a file, streamed.
pub fn md5_file(path: &Path) -> Result<String, CloudError> {
    use md5::Digest;
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = md5::Md5::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
