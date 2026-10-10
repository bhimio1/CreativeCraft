//! Blocking HTTP shared by the adapters. Generation requests are sent once (a retry could be
//! billed twice); polling and downloads tolerate a few transient failures.

use crate::GenError;
use serde_json::Value;
use std::io::Read;
use std::time::{Duration, Instant};

/// Largest file accepted from a service (a long 4K video fits).
const MAX_DOWNLOAD: u64 = 2 * 1024 * 1024 * 1024;
const UA: &str = concat!("CreativeCraft/", env!("CARGO_PKG_VERSION"), " (https://github.com/bhimio1/CreativeCraft)");

pub(crate) fn agent() -> ureq::Agent {
    // generous: some services answer a create call only once the image is done
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(20)).timeout_read(Duration::from_secs(300)).user_agent(UA).build()
}

/// A readable message from an error reply: `{"error":{"message"}}`, `{"error":"…"}`,
/// `{"detail":…}`, `{"message"}`, or the start of the body.
fn explain(body: &str) -> String {
    let v: Option<Value> = serde_json::from_str(body).ok();
    let pick = v.as_ref().and_then(|v| {
        v.pointer("/error/message")
            .or_else(|| v.get("error").filter(|e| e.is_string()))
            .or_else(|| v.get("detail"))
            .or_else(|| v.get("message"))
            .or_else(|| v.get("title"))
            .map(|m| m.as_str().map(String::from).unwrap_or_else(|| m.to_string()))
    });
    pick.unwrap_or_else(|| body.split_whitespace().collect::<Vec<_>>().join(" ")).chars().take(400).collect()
}

pub(crate) fn err(service: &str, e: ureq::Error) -> GenError {
    match e {
        ureq::Error::Status(status, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let hint = match status {
                401 | 403 => "the key wasn't accepted, or it can't use this model: ",
                402 => "the account needs credit: ",
                404 => "not found (check the model name or address): ",
                429 => "too many requests or out of quota: ",
                _ => "",
            };
            GenError::Http { service: service.to_string(), status, body: format!("{hint}{}", explain(&body)) }
        }
        ureq::Error::Transport(t) if matches!(t.kind(), ureq::ErrorKind::ConnectionFailed) => {
            let at = t
                .url()
                .map(|u| format!("{}://{}", u.scheme(), u.host_str().unwrap_or("")) + &u.port().map(|p| format!(":{p}")).unwrap_or_default())
                .unwrap_or_default();
            GenError::Net(format!("{service} isn't answering at {at}: is it running? Check the address in Settings → AI generation"))
        }
        ureq::Error::Transport(t) if matches!(t.kind(), ureq::ErrorKind::Dns) => {
            GenError::Net(format!("{service}: couldn't find the server; check the address and your connection"))
        }
        other => GenError::Net(format!("{service}: {other}")),
    }
}

pub(crate) fn with_headers(mut r: ureq::Request, headers: &[(&str, String)]) -> ureq::Request {
    for (k, v) in headers {
        r = r.set(k, v);
    }
    r
}

pub(crate) fn post_json(service: &str, url: &str, headers: &[(&str, String)], body: &Value) -> Result<Value, GenError> {
    let resp = with_headers(agent().post(url), headers).send_json(body.clone()).map_err(|e| err(service, e))?;
    resp.into_json().map_err(|e| GenError::Net(format!("{service}: {e}")))
}

/// Worth asking again: busy or failing servers and dropped connections. Not: a refusal, a server
/// that isn't running, or an address that doesn't exist.
fn retryable(e: &ureq::Error) -> bool {
    match e {
        ureq::Error::Status(s, _) => *s >= 500 || *s == 429,
        ureq::Error::Transport(t) => {
            !matches!(t.kind(), ureq::ErrorKind::ConnectionFailed | ureq::ErrorKind::Dns | ureq::ErrorKind::InvalidUrl | ureq::ErrorKind::UnknownScheme)
        }
    }
}

/// GET JSON, retrying a few times on dropped connections and 5xx (it changes nothing).
pub(crate) fn get_json(service: &str, url: &str, headers: &[(&str, String)]) -> Result<Value, GenError> {
    let mut last = None;
    for attempt in 0..4 {
        match with_headers(agent().get(url), headers).call() {
            Ok(r) => return r.into_json().map_err(|e| GenError::Net(format!("{service}: {e}"))),
            Err(e) if !retryable(&e) => return Err(err(service, e)),
            Err(e) => last = Some(err(service, e)),
        }
        nap(Duration::from_millis(500 << attempt))?;
    }
    Err(last.unwrap_or_else(|| GenError::Net(format!("{service}: no answer"))))
}

/// Download a file (following redirects), with a size limit.
pub(crate) fn get_bytes(service: &str, url: &str, headers: &[(&str, String)]) -> Result<Vec<u8>, GenError> {
    let mut last = None;
    for attempt in 0..3 {
        match with_headers(agent().get(url), headers).call() {
            Ok(r) => {
                let mut buf = Vec::new();
                r.into_reader().take(MAX_DOWNLOAD).read_to_end(&mut buf)?;
                return Ok(buf);
            }
            Err(e) if !retryable(&e) => return Err(err(service, e)),
            Err(e) => last = Some(err(service, e)),
        }
        nap(Duration::from_millis(800 << attempt))?;
    }
    Err(last.unwrap_or_else(|| GenError::Net(format!("{service}: download failed"))))
}

/// Call `step` until it returns `Some`, waiting `every` between calls (growing slowly), for at
/// most `limit`.
pub(crate) fn poll<T>(limit: Duration, every: Duration, mut step: impl FnMut() -> Result<Option<T>, GenError>) -> Result<T, GenError> {
    let start = Instant::now();
    // tests talk to a local fake server: no need to wait between polls
    let every = if cfg!(test) { Duration::from_millis(5) } else { every };
    let mut wait = every;
    loop {
        crate::check_cancel()?;
        if let Some(v) = step()? {
            return Ok(v);
        }
        if start.elapsed() > limit {
            return Err(GenError::Timeout(limit.as_secs() / 60));
        }
        nap(wait)?;
        wait = (wait + wait / 4).min(every * 4);
    }
}

/// Sleep, waking early (with `Cancelled`) if the generation is stopped.
fn nap(d: Duration) -> Result<(), GenError> {
    let until = Instant::now() + d;
    while Instant::now() < until {
        crate::check_cancel()?;
        std::thread::sleep((until - Instant::now()).min(Duration::from_millis(100)));
    }
    Ok(())
}

/// One field of a multipart form.
pub(crate) enum Part<'a> {
    Text(&'a str, String),
    File(&'a str, &'a crate::InputImage),
}

/// `multipart/form-data` POST (OpenAI's edit and video endpoints, ComfyUI's upload). Never retried.
pub(crate) fn post_multipart(service: &str, url: &str, headers: &[(&str, String)], parts: &[Part]) -> Result<Value, GenError> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let boundary = format!("----creativecraft{nanos:x}{:x}", std::process::id());
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match part {
            Part::Text(name, value) => {
                body.extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
            }
            Part::File(name, img) => {
                let file = img.name.replace(['"', '\r', '\n'], "_");
                body.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{file}\"\r\nContent-Type: {}\r\n\r\n", img.mime).as_bytes(),
                );
                body.extend_from_slice(&img.bytes);
                body.extend_from_slice(b"\r\n");
            }
        }
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    let req = with_headers(agent().post(url), headers).set("Content-Type", &format!("multipart/form-data; boundary={boundary}"));
    let resp = req.send_bytes(&body).map_err(|e| err(service, e))?;
    resp.into_json().map_err(|e| GenError::Net(format!("{service}: {e}")))
}

/// Tell a service to stop a job; best effort (the user asked to stop, so a failure here only
/// means the service finishes the job on its own).
pub(crate) fn send_stop(service: &str, method: &str, url: &str, headers: &[(&str, String)], body: Option<&Value>) {
    let req = with_headers(agent().request(method, url), headers);
    let result = match body {
        Some(b) => req.send_json(b.clone()),
        None => req.call(),
    };
    if let Err(e) = result {
        log::warn!("{service}: couldn't stop the job: {e}");
    }
}

pub(crate) fn b64(s: &str) -> Result<Vec<u8>, GenError> {
    use base64::Engine as _;
    // tolerate a data: URI prefix
    let s = s.split_once(";base64,").map_or(s, |(_, d)| d);
    base64::engine::general_purpose::STANDARD.decode(s.trim()).map_err(|e| GenError::Failed(format!("bad image data: {e}")))
}

/// How long a video job may take.
pub(crate) const VIDEO_LIMIT: Duration = Duration::from_secs(30 * 60);
/// How long an image job may take on queue-based services.
pub(crate) const IMAGE_LIMIT: Duration = Duration::from_secs(10 * 60);
