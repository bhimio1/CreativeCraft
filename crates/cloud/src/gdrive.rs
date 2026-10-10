//! Google Drive (REST v3) backend.
//!
//! Files live under one folder, "CreativeCraft Projects", mirroring the local tree with real Drive
//! folders so they look right in the Drive web UI. Every file and folder the hub creates is tagged
//! with private `appProperties` (Drive caps each key+value at 124 bytes, so they stay short):
//!
//! - `cc_kind` = `root` | `dir` | `file`
//! - `cc_root` = the root folder's id
//!
//! One paginated query lists the whole tree; each item's relative path is rebuilt from its `name`
//! and `parents`, so paths of any length work and a rename in the Drive web UI is seen as a move.
//! With the `drive.file` scope the hub only ever sees files it created.

use crate::oauth::{GoogleClient, Token, pct};
use crate::{CloudError, CloudStore, RemoteFile};
use creativecraft_secrets::Secrets;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;

const API: &str = "https://www.googleapis.com/drive/v3/files";
const UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3/files";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
pub const ROOT_NAME: &str = "CreativeCraft Projects";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveFile {
    id: String,
    #[serde(default)]
    mime_type: String,
    #[serde(default)]
    md5_checksum: Option<String>,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    parents: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileList {
    #[serde(default)]
    files: Vec<DriveFile>,
    next_page_token: Option<String>,
}

pub struct GoogleDrive {
    client: GoogleClient,
    token: Token,
    secrets: Secrets,
    root_id: Option<String>,
    /// path → Drive id, for files and folders (folders keyed with a trailing `/`).
    ids: HashMap<String, String>,
}

impl GoogleDrive {
    /// Use a saved token (refreshing it when needed). `CloudError::SignedOut` if there is none.
    pub fn connect(client: GoogleClient, secrets: Secrets) -> Result<GoogleDrive, CloudError> {
        let token = Token::load(&secrets).ok_or(CloudError::SignedOut)?;
        Ok(GoogleDrive { client, token, secrets, root_id: None, ids: HashMap::new() })
    }

    /// Run the browser sign-in and save the token.
    pub fn sign_in(client: GoogleClient, secrets: Secrets) -> Result<GoogleDrive, CloudError> {
        let token = client.sign_in()?;
        token.save(&secrets)?;
        Ok(GoogleDrive { client, token, secrets, root_id: None, ids: HashMap::new() })
    }

    pub fn sign_out(secrets: &Secrets) -> Result<(), CloudError> {
        Token::forget(secrets)
    }

    fn bearer(&mut self) -> Result<String, CloudError> {
        if !self.token.is_fresh() {
            self.token = self.client.refresh(&self.token)?;
            self.token.save(&self.secrets)?;
        }
        Ok(format!("Bearer {}", self.token.access_token))
    }

    fn query(&mut self, q: &str) -> Result<Vec<DriveFile>, CloudError> {
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let auth = self.bearer()?;
            let mut req = ureq::get(API)
                .set("Authorization", &auth)
                .query("q", q)
                .query("spaces", "drive")
                .query("pageSize", "1000")
                .query("fields", "nextPageToken,files(id,name,parents,mimeType,md5Checksum,size)");
            if let Some(p) = &page {
                req = req.query("pageToken", p);
            }
            let list: FileList = req.call()?.into_json()?;
            out.extend(list.files);
            match list.next_page_token {
                Some(p) => page = Some(p),
                None => return Ok(out),
            }
        }
    }

    /// Find or create the "CreativeCraft Projects" folder.
    fn root(&mut self) -> Result<String, CloudError> {
        if let Some(id) = &self.root_id {
            return Ok(id.clone());
        }
        let found = self.query("appProperties has { key='cc_kind' and value='root' } and trashed = false")?;
        let id = match found.into_iter().next() {
            Some(f) => f.id,
            None => {
                let auth = self.bearer()?;
                let f: DriveFile = ureq::post(API)
                    .set("Authorization", &auth)
                    .query("fields", "id")
                    .send_json(json!({
                        "name": ROOT_NAME,
                        "mimeType": FOLDER_MIME,
                        "appProperties": { "cc_kind": "root" },
                    }))?
                    .into_json()?;
                f.id
            }
        };
        self.root_id = Some(id.clone());
        Ok(id)
    }

    /// Drive id of the folder holding `path` (creating folders as needed).
    fn parent_of(&mut self, path: &str) -> Result<String, CloudError> {
        let root = self.root()?;
        let Some((dir, _)) = path.rsplit_once('/') else { return Ok(root) };
        let mut parent = root.clone();
        let mut so_far = String::new();
        for seg in dir.split('/') {
            so_far.push_str(seg);
            so_far.push('/');
            if let Some(id) = self.ids.get(&so_far) {
                parent = id.clone();
                continue;
            }
            let auth = self.bearer()?;
            let f: DriveFile = ureq::post(API)
                .set("Authorization", &auth)
                .query("fields", "id")
                .send_json(json!({
                    "name": seg,
                    "mimeType": FOLDER_MIME,
                    "parents": [parent],
                    "appProperties": { "cc_kind": "dir", "cc_root": root },
                }))?
                .into_json()?;
            self.ids.insert(so_far.clone(), f.id.clone());
            parent = f.id;
        }
        Ok(parent)
    }

    /// Resumable upload (works for any size; Drive requires it above 5 MB).
    fn put_content(&mut self, session_url: &str, src: &Path) -> Result<DriveFile, CloudError> {
        let len = std::fs::metadata(src)?.len();
        let file = std::fs::File::open(src)?;
        let auth = self.bearer()?;
        let f: DriveFile =
            ureq::put(session_url).set("Authorization", &auth).set("Content-Length", &len.to_string()).send(std::io::BufReader::new(file))?.into_json()?;
        Ok(f)
    }
}

impl CloudStore for GoogleDrive {
    fn label(&self) -> String {
        format!("Google Drive › {ROOT_NAME}")
    }

    fn list(&mut self) -> Result<Vec<RemoteFile>, CloudError> {
        let root = self.root()?;
        let q = format!("appProperties has {{ key='cc_root' and value='{}' }} and trashed = false", root.replace('\'', "\\'"));
        let files = self.query(&q)?;
        let paths = build_paths(&files, &root);
        self.ids.clear();
        let mut out = Vec::new();
        for f in files {
            let Some(path) = paths.get(&f.id).cloned() else { continue };
            if f.mime_type == FOLDER_MIME {
                self.ids.insert(format!("{path}/"), f.id);
                continue;
            }
            // Google-native docs have no md5 and no binary content: not ours to sync
            let Some(md5) = f.md5_checksum else { continue };
            if self.ids.insert(path.clone(), f.id).is_some() {
                log::warn!("two Drive files claim {path}; using the last one listed");
            }
            out.push(RemoteFile { path, fingerprint: md5.clone(), md5: Some(md5), size: f.size.and_then(|s| s.parse().ok()).unwrap_or(0) });
        }
        Ok(out)
    }

    fn download(&mut self, path: &str, dest: &Path) -> Result<(), CloudError> {
        let id = self.ids.get(path).cloned().ok_or_else(|| CloudError::Missing(path.into()))?;
        let auth = self.bearer()?;
        let resp = ureq::get(&format!("{API}/{}", pct(&id))).set("Authorization", &auth).query("alt", "media").call()?;
        let mut tmp_name = dest.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(".creativecraft.part");
        let tmp = dest.with_file_name(tmp_name);
        {
            let mut out = std::fs::File::create(&tmp)?;
            std::io::copy(&mut resp.into_reader(), &mut out)?;
            out.sync_all()?;
        }
        std::fs::rename(tmp, dest)?;
        Ok(())
    }

    fn upload(&mut self, path: &str, src: &Path) -> Result<Option<String>, CloudError> {
        let len = std::fs::metadata(src)?.len().to_string();
        let auth = self.bearer()?;
        let session = match self.ids.get(path).cloned() {
            Some(id) => ureq::request("PATCH", &format!("{UPLOAD}/{}", pct(&id)))
                .set("Authorization", &auth)
                .set("X-Upload-Content-Length", &len)
                .query("uploadType", "resumable")
                .query("fields", "id,md5Checksum")
                .send_json(json!({}))?,
            None => {
                let parent = self.parent_of(path)?;
                let root = self.root()?;
                let name = path.rsplit('/').next().unwrap_or(path);
                let auth = self.bearer()?;
                ureq::post(UPLOAD)
                    .set("Authorization", &auth)
                    .set("X-Upload-Content-Length", &len)
                    .query("uploadType", "resumable")
                    .query("fields", "id,md5Checksum")
                    .send_json(json!({
                        "name": name,
                        "parents": [parent],
                        "appProperties": { "cc_kind": "file", "cc_root": root },
                    }))?
            }
        };
        let url = session.header("Location").map(str::to_string).ok_or_else(|| CloudError::Net("Drive gave no upload session".into()))?;
        let f = self.put_content(&url, src)?;
        self.ids.insert(path.to_string(), f.id);
        Ok(None)
    }

    fn trash(&mut self, path: &str) -> Result<(), CloudError> {
        let id = self.ids.get(path).cloned().ok_or_else(|| CloudError::Missing(path.into()))?;
        let auth = self.bearer()?;
        ureq::request("PATCH", &format!("{API}/{}", pct(&id))).set("Authorization", &auth).send_json(json!({ "trashed": true }))?;
        self.ids.remove(path);
        Ok(())
    }
}

/// Relative path of every item reachable from `root` through `parents`. Items whose chain leaves
/// the tree (moved out in the web UI) or loops are skipped.
fn build_paths(items: &[DriveFile], root: &str) -> HashMap<String, String> {
    let by_id: HashMap<&str, &DriveFile> = items.iter().map(|f| (f.id.as_str(), f)).collect();
    let mut out = HashMap::new();
    for f in items {
        let mut segs = vec![f.name.as_str()];
        let mut cur = f;
        let mut ok = false;
        for _ in 0..64 {
            let Some(parent) = cur.parents.first() else { break };
            if parent == root {
                ok = true;
                break;
            }
            let Some(p) = by_id.get(parent.as_str()) else { break };
            segs.push(p.name.as_str());
            cur = p;
        }
        let bad = |s: &&str| s.is_empty() || *s == "." || *s == ".." || s.contains('/');
        if ok && !segs.iter().any(bad) {
            segs.reverse();
            out.insert(f.id.clone(), segs.join("/"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, name: &str, parent: &str) -> DriveFile {
        DriveFile { id: id.into(), mime_type: String::new(), md5_checksum: None, size: None, name: name.into(), parents: vec![parent.into()] }
    }

    #[test]
    fn paths_from_parents() {
        let long = "x".repeat(300);
        let items = vec![
            item("d1", "art", "R"),
            item("d2", &long, "d1"),
            item("f1", "poster.pcraft", "d2"),
            item("f2", "top.png", "R"),
            item("f3", "lost.png", "elsewhere"),
            item("f4", "..", "R"),
        ];
        let p = build_paths(&items, "R");
        assert_eq!(p["f1"], format!("art/{long}/poster.pcraft"));
        assert_eq!(p["f2"], "top.png");
        assert!(!p.contains_key("f3"));
        assert!(!p.contains_key("f4"));
    }
}
