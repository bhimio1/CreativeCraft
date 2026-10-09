//! Google OAuth 2.0 for installed apps: authorization code + PKCE, loopback redirect.
//!
//! The user's browser opens Google's consent page; Google redirects to `http://127.0.0.1:<port>`
//! where a one-shot listener in this process receives the code. No embedded webview, no secrets
//! in the binary: the OAuth client id/secret come from the hub's settings (a "Desktop app" client
//! the publisher creates in Google Cloud; for desktop clients the secret is not confidential).

use crate::CloudError;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Only files this app creates or the user opens with it. Never the whole Drive.
pub const SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleClient {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds.
    pub expires_at: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Token {
    pub fn is_fresh(&self) -> bool {
        self.expires_at > now() + 60
    }

    pub fn load(path: &Path) -> Option<Token> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    /// Saved readable by the current user only. (OS keychain storage is on the roadmap.)
    pub fn save(&self, path: &Path) -> Result<(), CloudError> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}

fn random_url_safe(bytes: usize) -> Result<String, CloudError> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| CloudError::Auth(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// RFC 3986 unreserved characters pass; everything else is %-encoded.
pub(crate) fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let hexval = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match (hexval(bytes[i + 1]), hexval(bytes[i + 2])) {
                (Some(hi), Some(lo)) => {
                    out.push(hi << 4 | lo);
                    i += 3;
                    continue;
                }
                _ => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == key).map(|(_, v)| pct_decode(v))
}

impl GoogleClient {
    /// Interactive sign-in: opens the browser, waits (up to 5 minutes) for the redirect.
    pub fn sign_in(&self) -> Result<Token, CloudError> {
        let verifier = random_url_safe(48)?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = random_url_safe(16)?;

        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let redirect = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
        let url = format!(
            "{AUTH_URL}?client_id={}&redirect_uri={}&response_type=code&scope={}&code_challenge={}&code_challenge_method=S256&access_type=offline&prompt=consent&state={}",
            pct(&self.client_id),
            pct(&redirect),
            pct(SCOPE),
            challenge,
            state
        );
        open::that_detached(&url)?;
        log::info!("waiting for Google sign-in at {redirect}");

        let code = wait_for_code(&listener, &state, Duration::from_secs(300))?;
        let resp: TokenResponse = ureq::post(TOKEN_URL)
            .send_form(&[
                ("code", code.as_str()),
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("redirect_uri", redirect.as_str()),
                ("grant_type", "authorization_code"),
                ("code_verifier", verifier.as_str()),
            ])?
            .into_json()?;
        Ok(Token { access_token: resp.access_token, refresh_token: resp.refresh_token, expires_at: now() + resp.expires_in })
    }

    /// New access token from the refresh token; keeps the refresh token.
    pub fn refresh(&self, token: &Token) -> Result<Token, CloudError> {
        let rt = token.refresh_token.as_deref().ok_or(CloudError::SignedOut)?;
        let resp: TokenResponse = ureq::post(TOKEN_URL)
            .send_form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("refresh_token", rt),
                ("grant_type", "refresh_token"),
            ])?
            .into_json()?;
        Ok(Token {
            access_token: resp.access_token,
            refresh_token: resp.refresh_token.or_else(|| token.refresh_token.clone()),
            expires_at: now() + resp.expires_in,
        })
    }
}

/// Accept connections until one carries `?code=…&state=<state>`; answer each with a small page.
fn wait_for_code(listener: &TcpListener, state: &str, timeout: Duration) -> Result<String, CloudError> {
    listener.set_nonblocking(true)?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if std::time::Instant::now() > deadline {
            return Err(CloudError::Auth("timed out waiting for the browser".into()));
        }
        let (mut stream, _) = match listener.accept() {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line)?;
        // "GET /?code=…&state=… HTTP/1.1"
        let target = line.split_whitespace().nth(1).unwrap_or("");
        let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
        let (ok, result) = match (query_param(query, "code"), query_param(query, "state"), query_param(query, "error")) {
            (Some(code), Some(s), _) if s == state => (true, Ok(code)),
            (_, _, Some(err)) => (false, Err(CloudError::Auth(err))),
            _ => (false, Err(CloudError::Auth(String::new()))), // favicon or stray request: keep waiting
        };
        let body = if ok {
            "<h2>Signed in.</h2><p>You can close this tab and return to Atelier.</p>"
        } else {
            "<h2>Sign-in did not complete.</h2><p>Return to Atelier and try again.</p>"
        };
        let _ =
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        match result {
            Ok(code) => return Ok(code),
            Err(CloudError::Auth(e)) if e.is_empty() => continue,
            Err(e) => return Err(e),
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_round_trip() {
        assert_eq!(pct("http://127.0.0.1:8080"), "http%3A%2F%2F127.0.0.1%3A8080");
        assert_eq!(pct_decode("4%2F0Ab-x_y"), "4/0Ab-x_y");
        assert_eq!(query_param("state=abc&code=4%2Fxyz", "code").as_deref(), Some("4/xyz"));
    }

    #[test]
    fn loopback_receives_the_code() {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            // a stray request first, then the real redirect
            for path in ["/favicon.ico", "/?state=s1&code=4%2Fabc"] {
                let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
                write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
                let mut resp = String::new();
                let _ = std::io::Read::read_to_string(&mut s, &mut resp);
            }
        });
        assert_eq!(wait_for_code(&l, "s1", Duration::from_secs(5)).unwrap(), "4/abc");
        t.join().unwrap();
    }
}
