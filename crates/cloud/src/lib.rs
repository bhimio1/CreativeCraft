//! Cloud saves (L2).
//!
//! A [`CloudStore`] is anything that can list, fetch, store and trash files by relative path.
//! Backends:
//!
//! - [`GoogleDrive`]: Drive REST v3 with the narrow `drive.file` scope (the hub can only see files
//!   it created, never the rest of the user's Drive), files kept under an "CreativeCraft Projects" folder
//!   that mirrors the local folder tree.
//! - [`FolderStore`]: any folder, including one kept in sync by a desktop client (Google Drive for
//!   desktop, Dropbox, OneDrive, Nextcloud, Syncthing…) or a mounted NAS share (SMB, NFS).
//! - [`WebDavStore`]: WebDAV servers: Nextcloud, ownCloud, NAS boxes (Synology, QNAP, TrueNAS…),
//!   pCloud, Koofr and others.
//! - [`S3Store`]: S3-compatible object storage: AWS S3, Backblaze B2, Cloudflare R2, Wasabi, MinIO,
//!   Garage (also self-hosted on a NAS).
//! - [`RcloneStore`]: any remote the user configured in their own `rclone` (70+ services: Dropbox,
//!   OneDrive, Box, SFTP, SMB, Mega, Proton Drive, …); rclone keeps those sign-ins, not the hub.
//!
//! [`sync`] runs one sync pass using the planner in `creativecraft-sync-plan`.
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod folder;
mod gdrive;
mod http;
mod oauth;
mod rclone;
mod s3;
pub mod sync;
mod watch;
mod webdav;
mod xml;

pub use creativecraft_sync_plan::Action;
pub use folder::FolderStore;
pub use gdrive::GoogleDrive;
pub use oauth::{GoogleClient, TOKEN_NAME as GOOGLE_TOKEN, Token};
pub use rclone::{RcloneStore, rclone_remotes};
pub use s3::{S3Config, S3Store};
pub use watch::ChangeWatcher;
pub use webdav::WebDavStore;

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
    #[error("{0}")]
    Config(String),
    #[error("{path} arrived damaged (checksum mismatch); it will be fetched again next sync")]
    Corrupt { path: String },
}

impl From<ureq::Error> for CloudError {
    fn from(e: ureq::Error) -> CloudError {
        match e {
            ureq::Error::Status(status, resp) => {
                let body = resp.into_string().unwrap_or_default();
                CloudError::Http { status, body: explain(status, &body) }
            }
            other => CloudError::Net(other.to_string()),
        }
    }
}

/// A server's error reply in words: S3's `<Code>`/`<Message>`, with a hint for the usual
/// set-up mistakes, or the start of the body.
fn explain(status: u16, body: &str) -> String {
    let code = xml::text(body, "Code");
    let message = xml::text(body, "Message");
    let hint = match (status, code.as_deref()) {
        (_, Some("SignatureDoesNotMatch")) => "the secret key is wrong (or has extra spaces)",
        (_, Some("InvalidAccessKeyId")) => "the access key ID isn't known to this service",
        (_, Some("NoSuchBucket")) => "there's no bucket with that name at this endpoint",
        (_, Some("AccessDenied")) => "the key isn't allowed to use this bucket",
        (_, Some("AuthorizationHeaderMalformed")) => "the region doesn't match the bucket's",
        (_, Some("PermanentRedirect")) => "the bucket is in another region; use that region's endpoint",
        (401, _) => "the server didn't accept the user name or password",
        (403, _) => "the server refused access to this folder",
        (404, _) => "nothing at that address; check it",
        (507, _) => "the server is out of space",
        _ => "",
    };
    let detail = match (code, message) {
        (Some(c), Some(m)) => format!("{c}: {m}"),
        _ => body.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(300).collect(),
    };
    match (hint.is_empty(), detail.is_empty()) {
        (true, _) => detail,
        (false, true) => hint.to_string(),
        (false, false) => format!("{hint} ({detail})"),
    }
}

/// A file in cloud storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFile {
    /// `/`-separated path relative to the projects root.
    pub path: String,
    /// Lowercase hex MD5 of the content, when the store knows it.
    pub md5: Option<String>,
    /// Changes whenever the content does (an ETag, or size and modification time). For stores
    /// without MD5s the sync remembers which MD5 each fingerprint stands for.
    pub fingerprint: String,
    pub size: u64,
}

/// A place project files are saved to. Paths are relative and `/`-separated.
pub trait CloudStore {
    /// Human-readable name, for the UI ("Google Drive", "Folder: ~/Dropbox/Art").
    fn label(&self) -> String;
    fn list(&mut self) -> Result<Vec<RemoteFile>, CloudError>;
    /// Write the remote file at `path` to the local file `dest`.
    fn download(&mut self, path: &str, dest: &Path) -> Result<(), CloudError>;
    /// Create or replace the remote file at `path` with the contents of the local `src`. Returns
    /// the file's new fingerprint when the store reports it (see [`RemoteFile::fingerprint`]).
    fn upload(&mut self, path: &str, src: &Path) -> Result<Option<String>, CloudError>;
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
