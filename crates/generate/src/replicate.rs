//! Replicate: any model by `owner/name` (`POST /models/{owner}/{name}/predictions`), waited on
//! with `Prefer: wait` and polled when it takes longer. Output files need the token to download.

use crate::http::{IMAGE_LIMIT, VIDEO_LIMIT, get_bytes, get_json, poll, post_json};
use crate::{GenError, Kind, Output, Provider, Request, sniff_ext};
use serde_json::{Map, Value, json};
use std::time::Duration;

/// The commonly accepted inputs, plus the provider's extra options.
pub(crate) fn common_input(p: &Provider, r: &Request, count_field: &str) -> Result<Map<String, Value>, GenError> {
    let mut m = Map::new();
    m.insert("prompt".into(), json!(r.prompt));
    m.insert("aspect_ratio".into(), json!(r.aspect.ratio()));
    if r.kind == Kind::Image && r.count > 1 {
        m.insert(count_field.into(), json!(r.count));
    }
    if !r.negative.trim().is_empty() {
        m.insert("negative_prompt".into(), json!(r.negative));
    }
    if let Some(s) = r.seed {
        m.insert("seed".into(), json!(s));
    }
    m.extend(p.extra()?);
    Ok(m)
}

/// Just the prompt and the extra options, for models that reject the common inputs.
pub(crate) fn minimal_input(p: &Provider, r: &Request) -> Result<Map<String, Value>, GenError> {
    let mut m = Map::new();
    m.insert("prompt".into(), json!(r.prompt));
    m.extend(p.extra()?);
    Ok(m)
}

/// Every URL in an output: a string, a list of strings, or objects with `url`.
pub(crate) fn urls(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) if s.starts_with("http") || s.starts_with("data:") => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| urls(x, out)),
        Value::Object(m) => {
            if let Some(u) = m.get("url") {
                urls(u, out);
            } else {
                m.values().for_each(|x| urls(x, out));
            }
        }
        _ => {}
    }
}

pub(crate) fn run(p: &Provider, r: &Request, status: &mut dyn FnMut(&str)) -> Result<Vec<Output>, GenError> {
    let model = p.model(r)?;
    let Some((owner, name)) = model.split_once('/') else {
        return Err(GenError::Config(format!("a Replicate model is written owner/name, e.g. black-forest-labs/flux-schnell (got \"{model}\")")));
    };
    let token = p.needs_key()?.to_string();
    let auth = vec![("Authorization", format!("Bearer {token}"))];
    let mut headers = auth.clone();
    headers.push(("Prefer", "wait=60".into()));
    let url = format!("{}/models/{owner}/{name}/predictions", p.base());
    let pred = match post_json(&p.name, &url, &headers, &json!({ "input": common_input(p, r, "num_outputs")? })) {
        Err(GenError::Http { status: 422, .. }) => post_json(&p.name, &url, &headers, &json!({ "input": minimal_input(p, r)? }))?,
        other => other?,
    };
    let id = pred.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    let limit = if r.kind == Kind::Video { VIDEO_LIMIT } else { IMAGE_LIMIT };
    let mut current = pred;
    let done = poll(limit, Duration::from_secs(3), || {
        match current.get("status").and_then(Value::as_str).unwrap_or("") {
            "succeeded" => return Ok(Some(current.clone())),
            s @ ("failed" | "canceled") => {
                let why = current.get("error").and_then(Value::as_str).unwrap_or(s);
                return Err(GenError::Failed(format!("{}: {why}", p.name)));
            }
            s => status(s),
        }
        current = get_json(&p.name, &format!("{}/predictions/{id}", p.base()), &auth)?;
        Ok(None)
    })?;
    let mut found = Vec::new();
    urls(done.get("output").unwrap_or(&Value::Null), &mut found);
    if found.is_empty() {
        return Err(GenError::Failed(format!("{} finished without any files", p.name)));
    }
    let mut out = Vec::new();
    for u in found {
        let bytes = if u.starts_with("data:") { crate::http::b64(&u)? } else { get_bytes(&p.name, &u, &auth)? };
        out.push(Output { ext: sniff_ext(&bytes).to_string(), bytes, model: model.clone() });
    }
    Ok(out)
}

pub(crate) fn check(p: &Provider) -> Result<String, GenError> {
    let v = get_json(&p.name, &format!("{}/account", p.base()), &[("Authorization", format!("Bearer {}", p.needs_key()?))])?;
    Ok(format!("{} answered: signed in as {}", p.name, v.get("username").and_then(Value::as_str).unwrap_or("?")))
}
