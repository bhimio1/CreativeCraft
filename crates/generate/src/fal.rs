//! fal: any model endpoint through the queue API (`POST {queue}/{model}`, poll `status_url`,
//! fetch `response_url`). Images come back as `images[].url`, video as `video.url`.

use crate::http::{IMAGE_LIMIT, VIDEO_LIMIT, get_bytes, get_json, poll, post_json};
use crate::replicate::{common_input, minimal_input, urls};
use crate::{GenError, Kind, Output, Provider, Request, sniff_ext};
use serde_json::Value;
use std::time::Duration;

pub(crate) fn run(p: &Provider, r: &Request, status: &mut dyn FnMut(&str)) -> Result<Vec<Output>, GenError> {
    let model = p.model(r)?;
    let headers = vec![("Authorization", format!("Key {}", p.needs_key()?))];
    let url = format!("{}/{}", p.base(), model.trim_matches('/'));
    let job = match post_json(&p.name, &url, &headers, &Value::Object(common_input(p, r, "num_images")?)) {
        Err(GenError::Http { status: 422, .. }) => post_json(&p.name, &url, &headers, &Value::Object(minimal_input(p, r)?))?,
        other => other?,
    };
    let status_url = job.get("status_url").and_then(Value::as_str).ok_or_else(|| GenError::Failed(format!("{} didn't queue the job", p.name)))?.to_string();
    let response_url = job.get("response_url").and_then(Value::as_str).unwrap_or("").to_string();
    let limit = if r.kind == Kind::Video { VIDEO_LIMIT } else { IMAGE_LIMIT };
    poll(limit, Duration::from_secs(2), || {
        let v = get_json(&p.name, &status_url, &headers)?;
        match v.get("status").and_then(Value::as_str).unwrap_or("") {
            "COMPLETED" => match v.get("error").filter(|e| !e.is_null()) {
                Some(e) => Err(GenError::Failed(format!("{}: {}", p.name, e.as_str().map(String::from).unwrap_or_else(|| e.to_string())))),
                None => Ok(Some(())),
            },
            "IN_QUEUE" => {
                status(&match v.get("queue_position").and_then(Value::as_u64) {
                    Some(n) => format!("queued ({n} ahead)"),
                    None => "queued".into(),
                });
                Ok(None)
            }
            s => {
                status(if s.is_empty() { "working" } else { "generating" });
                Ok(None)
            }
        }
    })?;
    let result = get_json(&p.name, &response_url, &headers)?;
    let mut found = Vec::new();
    for key in ["images", "image", "video", "videos"] {
        if let Some(v) = result.get(key) {
            urls(v, &mut found);
        }
    }
    if found.is_empty() {
        urls(&result, &mut found);
    }
    if found.is_empty() {
        return Err(GenError::Failed(format!("{} finished without any files", p.name)));
    }
    let mut out = Vec::new();
    for u in found {
        let bytes = if u.starts_with("data:") { crate::http::b64(&u)? } else { get_bytes(&p.name, &u, &[])? };
        out.push(Output { ext: sniff_ext(&bytes).to_string(), bytes, model: model.clone() });
    }
    Ok(out)
}

/// fal has no account endpoint to ask, so only the key's presence is checked; it's used on the
/// first generation.
pub(crate) fn check(p: &Provider) -> Result<String, GenError> {
    p.needs_key()?;
    Ok(format!("{} key saved; fal checks it on the first generation", p.name))
}
