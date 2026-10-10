//! S3-compatible object storage: AWS S3, Backblaze B2, Cloudflare R2, Wasabi, MinIO, Garage,
//! Storj and others, including ones self-hosted on a NAS.
//!
//! Requests are signed with AWS Signature Version 4 (implemented here on `sha2`). Objects live
//! under an optional key prefix in one bucket. A single-request upload's ETag is the MD5 of the
//! content, so it doubles as the sync's hash; uploads send `Content-MD5`, so the server rejects a
//! damaged transfer. Deletions copy the object to `<prefix>.creativecraft-trash/<time>/…` and then
//! remove the original. Files over 5 GB would need multipart uploads, which aren't done yet.

// the retry closures return ureq's own (large) error type, as ureq does
#![allow(clippy::result_large_err)]

use crate::http::{encode_path, encode_segment, retry};
use crate::{CloudError, CloudStore, RemoteFile, xml};
use base64::Engine as _;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TRASH: &str = ".creativecraft-trash";
/// The largest object a single PUT may create.
const MAX_SINGLE_PUT: u64 = 5 * 1024 * 1024 * 1024;

/// Where and how to reach the bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Config {
    /// e.g. `https://s3.eu-west-1.amazonaws.com`, `https://<account>.r2.cloudflarestorage.com`,
    /// `https://s3.us-west-004.backblazeb2.com`, `http://nas.local:9000`.
    pub endpoint: String,
    /// e.g. `eu-west-1`; `auto` for R2; MinIO and most NAS servers accept `us-east-1`.
    pub region: String,
    pub bucket: String,
    /// Folder inside the bucket ("" for the bucket root).
    pub prefix: String,
    pub access_key: String,
    pub secret_key: String,
    /// `https://endpoint/bucket/key` instead of `https://bucket.endpoint/key`. Self-hosted servers
    /// usually need it; AWS prefers virtual-hosted style.
    pub path_style: bool,
}

pub struct S3Store {
    cfg: S3Config,
    /// `https` or `http`
    scheme: String,
    /// Host (and port) requests go to.
    host: String,
    /// Path before the key: `/bucket/` (path style) or `/`.
    key_base: String,
    /// The prefix, normalised to "" or "folder/".
    prefix: String,
    agent: ureq::Agent,
}

impl S3Store {
    pub fn new(cfg: S3Config) -> Result<S3Store, CloudError> {
        let bad = |m: &str| Err(CloudError::Config(m.to_string()));
        let endpoint = cfg.endpoint.trim().trim_end_matches('/').to_string();
        let Some((scheme, host)) = endpoint.split_once("://") else { return bad("the S3 endpoint starts with https:// (or http:// on your own network)") };
        if !matches!(scheme, "https" | "http") || host.is_empty() || host.contains('/') {
            return bad("the S3 endpoint is a server address like https://s3.eu-west-1.amazonaws.com, without a path");
        }
        if cfg.bucket.trim().is_empty() || cfg.access_key.trim().is_empty() || cfg.secret_key.is_empty() {
            return bad("S3 needs a bucket, an access key and a secret key");
        }
        let bucket = cfg.bucket.trim();
        let (host, key_base) =
            if cfg.path_style { (host.to_string(), format!("/{}/", encode_segment(bucket))) } else { (format!("{bucket}.{host}"), "/".to_string()) };
        let prefix = cfg.prefix.trim().trim_matches('/');
        let prefix = if prefix.is_empty() { String::new() } else { format!("{prefix}/") };
        let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(20)).timeout_read(Duration::from_secs(120)).build();
        Ok(S3Store { scheme: scheme.to_string(), host, key_base, prefix, cfg, agent })
    }

    fn key(&self, rel: &str) -> String {
        format!("{}{rel}", self.prefix)
    }

    /// A signed request for `key` ("" for the bucket) with `query` pairs and extra headers.
    fn signed(&self, method: &str, key: &str, query: &[(&str, String)], extra: &[(&str, String)]) -> ureq::Request {
        let uri = format!("{}{}", self.key_base, encode_path(key));
        let amz_date = amz_date(SystemTime::now());
        let mut headers: Vec<(String, String)> =
            vec![("host".into(), self.host.clone()), ("x-amz-content-sha256".into(), "UNSIGNED-PAYLOAD".into()), ("x-amz-date".into(), amz_date.clone())];
        headers.extend(extra.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.clone())));
        let auth = sign(&SignInput {
            method,
            uri: &uri,
            query,
            headers: &headers,
            payload_hash: "UNSIGNED-PAYLOAD",
            amz_date: &amz_date,
            region: &self.cfg.region,
            access_key: &self.cfg.access_key,
            secret_key: &self.cfg.secret_key,
        });
        let qs = canonical_query(query);
        let url = if qs.is_empty() { format!("{}://{}{uri}", self.scheme, self.host) } else { format!("{}://{}{uri}?{qs}", self.scheme, self.host) };
        let mut r = self.agent.request(method, &url).set("Authorization", &auth);
        for (k, v) in &headers {
            if k != "host" {
                r = r.set(k, v);
            }
        }
        r
    }
}

impl CloudStore for S3Store {
    fn label(&self) -> String {
        format!("S3: {}://{}{}{}", self.scheme, self.host, self.key_base, self.prefix)
    }

    fn list(&mut self) -> Result<Vec<RemoteFile>, CloudError> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        let trash = format!("{}{TRASH}/", self.prefix);
        loop {
            let mut q: Vec<(&str, String)> = vec![("list-type", "2".into()), ("prefix", self.prefix.clone())];
            if let Some(t) = &token {
                q.push(("continuation-token", t.clone()));
            }
            let body = retry(|| self.signed("GET", "", &q, &[]).call())?.into_string().map_err(|e| CloudError::Net(e.to_string()))?;
            for c in xml::elements(&body, "Contents") {
                let Some(key) = xml::text(c, "Key") else { continue };
                if key.ends_with('/') || key.starts_with(&trash) {
                    continue; // folder markers and our trash
                }
                let Some(rel) = key.strip_prefix(&self.prefix) else { continue };
                let etag = xml::text(c, "ETag").unwrap_or_default().trim_matches('"').to_ascii_lowercase();
                out.push(RemoteFile {
                    path: rel.to_string(),
                    md5: md5_from_etag(&etag),
                    size: xml::text(c, "Size").and_then(|s| s.parse().ok()).unwrap_or(0),
                    fingerprint: etag,
                });
            }
            token = xml::text(&body, "NextContinuationToken").filter(|_| xml::text(&body, "IsTruncated").as_deref() == Some("true"));
            if token.is_none() {
                return Ok(out);
            }
        }
    }

    fn download(&mut self, path: &str, dest: &Path) -> Result<(), CloudError> {
        let key = self.key(path);
        let resp = match retry(|| self.signed("GET", &key, &[], &[]).call()) {
            Err(CloudError::Http { status: 404, .. }) => return Err(CloudError::Missing(path.into())),
            other => other?,
        };
        let mut f = std::fs::File::create(dest)?;
        std::io::copy(&mut resp.into_reader(), &mut f)?;
        f.flush()?;
        Ok(())
    }

    fn upload(&mut self, path: &str, src: &Path) -> Result<Option<String>, CloudError> {
        let len = std::fs::metadata(src)?.len();
        if len > MAX_SINGLE_PUT {
            return Err(CloudError::Config(format!("{path} is over 5 GB; S3 uploads that large aren't supported yet")));
        }
        let md5_hex = crate::md5_file(src)?;
        let md5_b64 = base64::engine::general_purpose::STANDARD.encode(hex_bytes(&md5_hex));
        let key = self.key(path);
        let resp = retry(|| {
            let file = std::fs::File::open(src).map_err(ureq::Error::from)?;
            self.signed("PUT", &key, &[], &[("content-md5", md5_b64.clone()), ("content-length", len.to_string())]).send(std::io::BufReader::new(file))
        })?;
        Ok(resp.header("ETag").map(|e| e.trim_matches('"').to_ascii_lowercase()))
    }

    fn trash(&mut self, path: &str) -> Result<(), CloudError> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let (src, dest) = (self.key(path), self.key(&format!("{TRASH}/{stamp}/{path}")));
        let source = format!("/{}/{}", encode_segment(self.cfg.bucket.trim()), encode_path(&src));
        retry(|| self.signed("PUT", &dest, &[], &[("x-amz-copy-source", source.clone())]).call())?;
        retry(|| self.signed("DELETE", &src, &[], &[]).call())?;
        Ok(())
    }
}

/// A single-part upload's ETag is the content's MD5; a multipart one has a `-N` suffix.
fn md5_from_etag(etag: &str) -> Option<String> {
    (etag.len() == 32 && etag.chars().all(|c| c.is_ascii_hexdigit())).then(|| etag.to_string())
}

fn hex_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len() / 2).filter_map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()).collect()
}

// ─────────────────────────── Signature Version 4 ───────────────────────────

struct SignInput<'a> {
    method: &'a str,
    /// Already percent-encoded path.
    uri: &'a str,
    query: &'a [(&'a str, String)],
    /// Lowercase names.
    headers: &'a [(String, String)],
    payload_hash: &'a str,
    amz_date: &'a str,
    region: &'a str,
    access_key: &'a str,
    secret_key: &'a str,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut k = if key.len() > 64 { Sha256::digest(key).to_vec() } else { key.to_vec() };
    k.resize(64, 0);
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::new().chain_update(&ipad).chain_update(msg).finalize();
    Sha256::new().chain_update(&opad).chain_update(inner).finalize().to_vec()
}

fn canonical_query(q: &[(&str, String)]) -> String {
    let mut pairs: Vec<(String, String)> = q.iter().map(|(k, v)| (encode_segment(k), encode_segment(v))).collect();
    pairs.sort();
    pairs.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
}

/// The `Authorization` header value.
fn sign(i: &SignInput) -> String {
    let mut headers: Vec<(String, String)> = i.headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string())).collect();
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let canonical = format!("{}\n{}\n{}\n{canonical_headers}\n{signed_headers}\n{}", i.method, i.uri, canonical_query(i.query), i.payload_hash);
    let date = &i.amz_date[..8];
    let scope = format!("{date}/{}/s3/aws4_request", i.region);
    let to_sign = format!("AWS4-HMAC-SHA256\n{}\n{scope}\n{}", i.amz_date, hex(&Sha256::digest(canonical.as_bytes())));
    let k = hmac(format!("AWS4{}", i.secret_key).as_bytes(), date.as_bytes());
    let k = hmac(&k, i.region.as_bytes());
    let k = hmac(&k, b"s3");
    let k = hmac(&k, b"aws4_request");
    let signature = hex(&hmac(&k, to_sign.as_bytes()));
    format!("AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}", i.access_key)
}

/// `20250102T030405Z`
fn amz_date(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let s = secs % 86_400;
    format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", s / 3600, (s / 60) % 60, s % 60)
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AK: &str = "AKIAIOSFODNN7EXAMPLE";
    const SK: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// The "GET Object" example from AWS's Signature Version 4 documentation.
    #[test]
    fn aws_example_get_object() {
        let headers =
            h(&[("host", "examplebucket.s3.amazonaws.com"), ("range", "bytes=0-9"), ("x-amz-content-sha256", EMPTY), ("x-amz-date", "20130524T000000Z")]);
        let a = sign(&SignInput {
            method: "GET",
            uri: "/test.txt",
            query: &[],
            headers: &headers,
            payload_hash: EMPTY,
            amz_date: "20130524T000000Z",
            region: "us-east-1",
            access_key: AK,
            secret_key: SK,
        });
        assert!(a.ends_with("Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"), "{a}");
        assert!(a.contains("SignedHeaders=host;range;x-amz-content-sha256;x-amz-date"));
    }

    /// The "GET Bucket (List Objects)" example from the same documentation.
    #[test]
    fn aws_example_list_objects() {
        let headers = h(&[("host", "examplebucket.s3.amazonaws.com"), ("x-amz-content-sha256", EMPTY), ("x-amz-date", "20130524T000000Z")]);
        let q = [("max-keys", "2".to_string()), ("prefix", "J".to_string())];
        let a = sign(&SignInput {
            method: "GET",
            uri: "/",
            query: &q,
            headers: &headers,
            payload_hash: EMPTY,
            amz_date: "20130524T000000Z",
            region: "us-east-1",
            access_key: AK,
            secret_key: SK,
        });
        assert!(a.ends_with("Signature=34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"), "{a}");
    }

    #[test]
    fn hmac_rfc4231_case_2() {
        assert_eq!(hex(&hmac(b"Jefe", b"what do ya want for nothing?")), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }

    #[test]
    fn dates_etags_and_urls() {
        assert_eq!(amz_date(UNIX_EPOCH + Duration::from_secs(1_369_353_600)), "20130524T000000Z");
        assert_eq!(md5_from_etag("0cc175b9c0f1b6a831c399e269772661").as_deref(), Some("0cc175b9c0f1b6a831c399e269772661"));
        assert_eq!(md5_from_etag("0cc175b9c0f1b6a831c399e269772661-3"), None);
        let cfg = S3Config {
            endpoint: "https://s3.eu-west-1.amazonaws.com/".into(),
            region: "eu-west-1".into(),
            bucket: "art".into(),
            prefix: "/CreativeCraft/".into(),
            access_key: "a".into(),
            secret_key: "s".into(),
            path_style: false,
        };
        let s = S3Store::new(cfg.clone()).unwrap();
        assert_eq!(s.host, "art.s3.eu-west-1.amazonaws.com");
        assert_eq!(s.key("a b.png"), "CreativeCraft/a b.png");
        let s = S3Store::new(S3Config { endpoint: "http://nas.local:9000".into(), path_style: true, ..cfg.clone() }).unwrap();
        assert_eq!((s.host.as_str(), s.key_base.as_str()), ("nas.local:9000", "/art/"));
        assert!(S3Store::new(S3Config { endpoint: "s3.amazonaws.com".into(), ..cfg }).is_err());
    }
}
