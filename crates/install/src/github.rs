//! GitHub releases: resolve the latest release, download an asset, read published checksums.
//!
//! Unauthenticated API calls are limited to 60/hour per IP; set `GITHUB_TOKEN` to raise that.

use crate::InstallError;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;

const UA: &str = concat!("creativecraft-hub/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone)]
pub struct Release {
    pub tag: String,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

fn get(url: &str) -> ureq::Request {
    let mut r = ureq::get(url).set("User-Agent", UA);
    if url.starts_with("https://api.github.com/")
        && let Ok(tok) = std::env::var("GITHUB_TOKEN")
        && !tok.is_empty()
    {
        r = r.set("Authorization", &format!("Bearer {tok}"));
    }
    r
}

fn net(e: impl std::fmt::Display) -> InstallError {
    InstallError::Net(e.to_string())
}

/// A small text file over HTTPS (e.g. the plugin index); `None` if there's nothing at `url`.
pub fn fetch_text(url: &str) -> Result<Option<String>, InstallError> {
    match get(url).call() {
        Ok(r) => r.into_string().map(Some).map_err(net),
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(net(e)),
    }
}

pub fn latest_release(repo: &str) -> Result<Release, InstallError> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let r: ApiRelease = match get(&url).set("Accept", "application/vnd.github+json").call() {
        Ok(resp) => resp.into_json().map_err(net)?,
        // no repository, or one without releases (common for forks): say so plainly
        Err(ureq::Error::Status(404, _)) => return Err(InstallError::Source(format!("{repo} doesn't exist or has published no releases"))),
        Err(e) => return Err(net(e)),
    };
    Ok(Release { tag: r.tag_name, assets: r.assets.into_iter().map(|a| Asset { name: a.name, url: a.browser_download_url, size: a.size }).collect() })
}

/// Stream `url` into `dest`, hashing as it goes. Returns the lowercase hex SHA-256.
pub fn download(url: &str, dest: &Path, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<String, InstallError> {
    let resp = get(url).call().map_err(net)?;
    let total = resp.header("Content-Length").and_then(|v| v.parse().ok());
    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(dest)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        done += n as u64;
        progress(done, total);
    }
    file.sync_all()?;
    Ok(hex(&hasher.finalize()))
}

/// The SHA-256 a release publishes for `asset`, from `<asset>.sha256` or a `SHA256SUMS`-style file.
pub fn published_sha256(release: &Release, asset: &str) -> Result<Option<String>, InstallError> {
    let candidates = release.assets.iter().filter(|a| {
        let n = a.name.to_ascii_lowercase();
        n == format!("{}.sha256", asset.to_ascii_lowercase()) || n.contains("sha256sums") || n == "checksums.txt"
    });
    for c in candidates {
        if c.size > 1024 * 1024 {
            continue; // not a checksum file
        }
        let text = get(&c.url).call().map_err(net)?.into_string().map_err(net)?;
        if let Some(h) = find_hash(&text, asset) {
            return Ok(Some(h));
        }
    }
    Ok(None)
}

/// Parse `sha256sum` output (`<hex>  <name>` or `<hex> *<name>`), or a bare hash file.
pub(crate) fn find_hash(text: &str, asset: &str) -> Option<String> {
    let is_hex = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    for l in &lines {
        let mut parts = l.split_whitespace();
        if let (Some(h), Some(name)) = (parts.next(), parts.next())
            && is_hex(h)
            && name.trim_start_matches('*').rsplit('/').next() == Some(asset)
        {
            return Some(h.to_ascii_lowercase());
        }
    }
    match lines.as_slice() {
        [only] if is_hex(only.split_whitespace().next().unwrap_or("")) => only.split_whitespace().next().map(str::to_ascii_lowercase),
        _ => None,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::find_hash;

    const H: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    #[test]
    fn parses_checksum_files() {
        let sums = format!("{H}  app-linux-x86_64.tar.gz\n{}  app-macos.zip\n", "0".repeat(64));
        assert_eq!(find_hash(&sums, "app-linux-x86_64.tar.gz").as_deref(), Some(H));
        assert_eq!(find_hash(&format!("{H} *dist/app.zip"), "app.zip").as_deref(), Some(H));
        assert_eq!(find_hash(H, "anything").as_deref(), Some(H));
        assert_eq!(find_hash(&sums, "missing.zip"), None);
    }
}
