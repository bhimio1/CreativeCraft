//! Replicate: any model by `owner/name` (`POST /models/{owner}/{name}/predictions`), waited on
//! with `Prefer: wait` and polled when it takes longer. Output files need the token to download.

use crate::http::{IMAGE_LIMIT, VIDEO_LIMIT, get_bytes, get_json, poll, post_json, send_stop};
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

/// Names models give their starting-picture input, most specific first.
const IMAGE_INPUTS: &[&str] =
    &["image_input", "input_images", "input_image", "image", "start_image", "first_frame_image", "init_image", "image_url", "image_urls"];

/// Where a model takes starting pictures: the first of [`IMAGE_INPUTS`] its input schema has, and
/// whether it takes a list. Unknown schema: `image`, one picture.
fn image_input(p: &Provider, owner: &str, name: &str, auth: &[(&str, String)]) -> (String, bool) {
    let schema = get_json(&p.name, &format!("{}/models/{owner}/{name}", p.base()), auth).ok();
    let props = schema.as_ref().and_then(|s| s.pointer("/latest_version/openapi_schema/components/schemas/Input/properties")).and_then(Value::as_object);
    if let Some(props) = props
        && let Some(key) = IMAGE_INPUTS.iter().find(|k| props.contains_key(**k))
    {
        let list = props[*key].get("type").and_then(Value::as_str) == Some("array");
        return ((*key).to_string(), list);
    }
    ("image".into(), false)
}

/// Starting pictures as data URIs under `key` (a list or the first one).
pub(crate) fn add_images(m: &mut Map<String, Value>, r: &Request, key: &str, list: bool) -> Result<(), GenError> {
    let inputs = r.inputs()?;
    if inputs.is_empty() {
        return Ok(());
    }
    let uris: Vec<String> = inputs.iter().map(crate::InputImage::data_uri).collect();
    m.insert(key.into(), if list { json!(uris) } else { json!(uris[0]) });
    if r.strength.is_some() {
        m.entry("prompt_strength").or_insert(json!(r.denoise()));
    }
    Ok(())
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
    let (mut full, mut minimal) = (common_input(p, r, "num_outputs")?, minimal_input(p, r)?);
    if !r.images.is_empty() {
        let (key, list) = image_input(p, owner, name, &auth);
        add_images(&mut full, r, &key, list)?;
        add_images(&mut minimal, r, &key, list)?;
        // the picture sets the shape
        full.remove("aspect_ratio");
    }
    let pred = match post_json(&p.name, &url, &headers, &json!({ "input": full })) {
        Err(GenError::Http { status: 422, .. }) => post_json(&p.name, &url, &headers, &json!({ "input": minimal }))?,
        other => other?,
    };
    let id = pred.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    let cancel_url = pred.pointer("/urls/cancel").and_then(Value::as_str).map_or_else(|| format!("{}/predictions/{id}/cancel", p.base()), String::from);
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
    })
    .inspect_err(|e| {
        if matches!(e, GenError::Cancelled) && !id.is_empty() {
            send_stop(&p.name, "POST", &cancel_url, &auth, None);
        }
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
