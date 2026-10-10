//! WebDAV: Nextcloud, ownCloud, NAS boxes (Synology, QNAP, TrueNAS, OpenMediaVault…), pCloud,
//! Koofr, Yandex Disk and other servers that speak RFC 4918.
//!
//! Files live under one folder URL. Listing walks it one level at a time (`Depth: 1`, which every
//! server allows; many refuse `infinity`). WebDAV has no MD5s, so files are identified by ETag and
//! the sync remembers which MD5 each ETag stands for. Deletions are `MOVE`d into
//! `.creativecraft-trash/<time>/…` inside the folder, never deleted.

// the retry closures return ureq's own (large) error type, as ureq does
#![allow(clippy::result_large_err)]

use crate::http::{decode, encode_path, retry};
use crate::{CloudError, CloudStore, RemoteFile, xml};
use base64::Engine as _;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

const TRASH: &str = ".creativecraft-trash";
const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getetag/><d:getcontentlength/><d:getlastmodified/></d:prop></d:propfind>"#;

pub struct WebDavStore {
    /// The folder's URL, ending in `/`.
    base: String,
    /// Its path part, decoded, ending in `/` (hrefs in replies are matched against it).
    base_path: String,
    auth: String,
    agent: ureq::Agent,
    /// Whether the sync folder itself is known to exist (it's created before the first write).
    base_ready: bool,
}

struct Entry {
    rel: String,
    dir: bool,
    etag: Option<String>,
    size: u64,
    modified: String,
}

impl WebDavStore {
    /// `url` is the folder to sync into, e.g. `https://cloud.example.com/remote.php/dav/files/me/CreativeCraft`.
    /// Plain `http://` is accepted for servers on the local network.
    pub fn new(url: &str, username: &str, password: &str) -> Result<WebDavStore, CloudError> {
        let url = url.trim();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(CloudError::Config("a WebDAV address starts with https:// (or http:// on your own network)".into()));
        }
        let base = if url.ends_with('/') { url.to_string() } else { format!("{url}/") };
        let after_scheme = &base[base.find("://").map_or(0, |i| i + 3)..];
        let base_path = decode(&after_scheme[after_scheme.find('/').unwrap_or(after_scheme.len())..]);
        let auth = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}")));
        let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(20)).timeout_read(Duration::from_secs(120)).build();
        Ok(WebDavStore { base, base_path, auth, agent, base_ready: false })
    }

    fn url(&self, rel: &str) -> String {
        format!("{}{}", self.base, encode_path(rel))
    }

    fn request(&self, method: &str, url: &str) -> ureq::Request {
        self.agent.request(method, url).set("Authorization", &self.auth)
    }

    /// The entries in folder `dir` (relative, "" for the root, ending in `/` otherwise) with
    /// `depth` "1", or the properties of the file `dir` itself with depth "0".
    fn propfind(&self, dir: &str, depth: &str) -> Result<Vec<Entry>, CloudError> {
        let skip_self = depth != "0";
        let url = self.url(dir);
        let resp =
            retry(|| self.request("PROPFIND", &url).set("Depth", depth).set("Content-Type", "application/xml; charset=utf-8").send_string(PROPFIND_BODY))?;
        let body = resp.into_string().map_err(|e| CloudError::Net(e.to_string()))?;
        let mut out = Vec::new();
        for r in xml::elements(&body, "response") {
            let Some(href) = xml::text(r, "href") else { continue };
            // hrefs are a path, or a whole URL
            let href_path = match href.find("://") {
                Some(i) => {
                    let rest = &href[i + 3..];
                    rest[rest.find('/').unwrap_or(rest.len())..].to_string()
                }
                None => href,
            };
            let path = decode(&href_path);
            let Some(rel) = path.strip_prefix(&self.base_path) else { continue };
            let rel = rel.trim_end_matches('/').to_string();
            if rel.is_empty() || (skip_self && rel == dir.trim_end_matches('/')) {
                continue; // the folder itself
            }
            out.push(Entry {
                dir: xml::has(r, "collection"),
                etag: xml::text(r, "getetag").map(|e| e.trim_matches('"').trim_start_matches("W/").trim_matches('"').to_string()).filter(|e| !e.is_empty()),
                size: xml::text(r, "getcontentlength").and_then(|s| s.parse().ok()).unwrap_or(0),
                modified: xml::text(r, "getlastmodified").unwrap_or_default(),
                rel,
            });
        }
        Ok(out)
    }

    fn fingerprint(e: &Entry) -> String {
        e.etag.clone().unwrap_or_else(|| format!("{}:{}", e.size, e.modified))
    }

    /// Create the sync folder itself if it isn't there yet (its own parents must exist).
    fn ensure_base(&mut self) -> Result<(), CloudError> {
        if self.base_ready {
            return Ok(());
        }
        match self.request("MKCOL", &self.base).call() {
            Ok(_) | Err(ureq::Error::Status(405 | 301, _)) => {}
            Err(ureq::Error::Status(409, _)) => {
                return Err(CloudError::Config(format!("the folder above {} doesn't exist on the server; check the address", self.base)));
            }
            Err(e) => return Err(e.into()),
        }
        self.base_ready = true;
        Ok(())
    }

    /// Create `dir` and its parents (relative, `/`-separated) inside the sync folder. Existing
    /// folders are fine.
    fn mkdirs(&mut self, dir: &str) -> Result<(), CloudError> {
        self.ensure_base()?;
        let mut acc = String::new();
        for seg in dir.split('/').filter(|s| !s.is_empty()) {
            acc.push_str(seg);
            acc.push('/');
            let url = self.url(&acc);
            match self.request("MKCOL", &url).call() {
                Ok(_) => {}
                // 405: already there (301 from some servers)
                Err(ureq::Error::Status(405 | 301, _)) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

fn parent(rel: &str) -> &str {
    rel.rfind('/').map_or("", |i| &rel[..i])
}

impl CloudStore for WebDavStore {
    fn label(&self) -> String {
        format!("WebDAV: {}", self.base)
    }

    fn list(&mut self) -> Result<Vec<RemoteFile>, CloudError> {
        let mut out = Vec::new();
        let mut dirs = vec![String::new()];
        while let Some(dir) = dirs.pop() {
            let entries = match self.propfind(&dir, "1") {
                Ok(e) => e,
                // a folder that doesn't exist yet is an empty one
                Err(CloudError::Http { status: 404, .. }) if dir.is_empty() => return Ok(out),
                Err(e) => return Err(e),
            };
            for e in entries {
                if e.rel == TRASH || e.rel.starts_with(&format!("{TRASH}/")) {
                    continue;
                }
                if e.dir {
                    dirs.push(format!("{}/", e.rel));
                } else {
                    out.push(RemoteFile { fingerprint: Self::fingerprint(&e), md5: None, size: e.size, path: e.rel });
                }
            }
        }
        Ok(out)
    }

    fn download(&mut self, path: &str, dest: &Path) -> Result<(), CloudError> {
        let url = self.url(path);
        let resp = match retry(|| self.request("GET", &url).call()) {
            Err(CloudError::Http { status: 404, .. }) => return Err(CloudError::Missing(path.into())),
            other => other?,
        };
        let mut f = std::fs::File::create(dest)?;
        std::io::copy(&mut resp.into_reader(), &mut f)?;
        f.flush()?;
        Ok(())
    }

    fn upload(&mut self, path: &str, src: &Path) -> Result<Option<String>, CloudError> {
        self.mkdirs(parent(path))?;
        let url = self.url(path);
        let len = std::fs::metadata(src)?.len();
        let resp = retry(|| {
            let file = std::fs::File::open(src).map_err(ureq::Error::from)?;
            self.request("PUT", &url)
                .set("Content-Length", &len.to_string())
                .set("Content-Type", "application/octet-stream")
                .send(std::io::BufReader::new(file))
        })?;
        let etag = resp.header("ETag").or_else(|| resp.header("OC-ETag")).map(|e| e.trim_start_matches("W/").trim_matches('"').to_string());
        if let Some(e) = etag.filter(|e| !e.is_empty()) {
            return Ok(Some(e));
        }
        // no ETag in the reply: ask for the file's properties
        Ok(self.propfind(path, "0")?.into_iter().find(|e| e.rel == path).map(|e| Self::fingerprint(&e)))
    }

    fn trash(&mut self, path: &str) -> Result<(), CloudError> {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let dest_rel = format!("{TRASH}/{stamp}/{path}");
        self.mkdirs(parent(&dest_rel))?;
        let (src, dest) = (self.url(path), self.url(&dest_rel));
        retry(|| self.request("MOVE", &src).set("Destination", &dest).set("Overwrite", "F").call())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_paths() {
        let s = WebDavStore::new("https://cloud.example.com/remote.php/dav/files/me/Creative%20Craft", "me", "pw").unwrap();
        assert_eq!(s.base, "https://cloud.example.com/remote.php/dav/files/me/Creative%20Craft/");
        assert_eq!(s.base_path, "/remote.php/dav/files/me/Creative Craft/");
        assert_eq!(s.url("Art/a b.png"), "https://cloud.example.com/remote.php/dav/files/me/Creative%20Craft/Art/a%20b.png");
        assert!(WebDavStore::new("ftp://x", "", "").is_err());
        assert_eq!(parent("a/b/c.png"), "a/b");
        assert_eq!(parent("c.png"), "");
    }
}
