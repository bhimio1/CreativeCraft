//! HTTP helpers shared by the web backends: retries with backoff for transient failures.

use crate::CloudError;
use std::time::Duration;

/// Attempts per request, and the first backoff (doubled after each failure).
const ATTEMPTS: u32 = 5;
const FIRST_BACKOFF: Duration = Duration::from_millis(500);

/// Is this worth retrying? Rate limits, server errors and network failures are; anything else
/// (auth, not found, bad request) won't get better by asking again.
fn transient(e: &ureq::Error) -> Option<Option<Duration>> {
    match e {
        ureq::Error::Status(code, resp) if *code == 429 || *code >= 500 => {
            let after = resp.header("Retry-After").and_then(|v| v.trim().parse::<u64>().ok()).map(Duration::from_secs);
            Some(after)
        }
        ureq::Error::Status(..) => None,
        ureq::Error::Transport(_) => Some(None),
    }
}

/// Run `request` (which builds and sends one request) until it succeeds, fails for good, or runs
/// out of attempts. Waits honour `Retry-After` (capped at a minute).
pub(crate) fn retry(mut request: impl FnMut() -> Result<ureq::Response, ureq::Error>) -> Result<ureq::Response, CloudError> {
    let mut wait = FIRST_BACKOFF;
    for attempt in 1..=ATTEMPTS {
        match request() {
            Ok(r) => return Ok(r),
            Err(e) => match transient(&e) {
                Some(after) if attempt < ATTEMPTS => {
                    let pause = after.unwrap_or(wait).min(Duration::from_secs(60));
                    log::info!("cloud: retrying after {pause:?} ({e})");
                    std::thread::sleep(pause);
                    wait *= 2;
                }
                _ => return Err(e.into()),
            },
        }
    }
    Err(CloudError::Net("gave up after repeated failures".into()))
}

/// Percent-encode one path segment (RFC 3986 unreserved characters stay as they are).
pub(crate) fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Percent-encode a `/`-separated path, keeping the slashes.
pub(crate) fn encode_path(p: &str) -> String {
    p.split('/').map(encode_segment).collect::<Vec<_>>().join("/")
}

/// Undo percent-encoding (as found in WebDAV hrefs). Invalid escapes are kept as they are.
pub(crate) fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_round_trips() {
        assert_eq!(encode_path("Art/poster draft #2.pcraft"), "Art/poster%20draft%20%232.pcraft");
        assert_eq!(decode("Art/poster%20draft%20%232.pcraft"), "Art/poster draft #2.pcraft");
        assert_eq!(decode("caf%C3%A9.png"), "café.png");
        assert_eq!(decode("100%"), "100%");
    }
}
