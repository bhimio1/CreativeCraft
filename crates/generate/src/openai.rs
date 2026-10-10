//! OpenAI (GPT Image), OpenAI-compatible image servers, and xAI (Grok Imagine images and video).

use crate::http::{Part, VIDEO_LIMIT, b64, get_bytes, get_json, poll, post_json, post_multipart};
use crate::{Aspect, GenError, Output, Provider, ProviderKind, Request, sniff_ext};
use serde_json::{Value, json};
use std::time::Duration;

fn auth(p: &Provider) -> Result<Vec<(&'static str, String)>, GenError> {
    // OpenAI and xAI need a key; a local or custom OpenAI-style server may not
    let key = if p.kind == ProviderKind::OpenaiCompatible { p.api_key.trim() } else { p.needs_key()? };
    Ok(if key.is_empty() { Vec::new() } else { vec![("Authorization", format!("Bearer {key}"))] })
}

/// `POST /images/generations` → `data[].b64_json` or `data[].url`.
pub(crate) fn image(p: &Provider, r: &Request) -> Result<Vec<Output>, GenError> {
    let model = p.model(r)?;
    let mut body = json!({ "model": model, "prompt": r.prompt, "n": r.count });
    if p.kind == ProviderKind::Openai {
        // GPT image models' sizes
        body["size"] = json!(match r.aspect {
            Aspect::Square => "1024x1024",
            Aspect::Landscape => "1536x1024",
            Aspect::Portrait => "1024x1536",
        });
    }
    if let Value::Object(m) = &mut body {
        m.extend(p.extra()?);
    }
    let inputs = r.inputs()?;
    let resp = if inputs.is_empty() {
        post_json(&p.name, &format!("{}/images/generations", p.base()), &auth(p)?, &body)?
    } else if p.kind == ProviderKind::Xai {
        // xAI edits take the picture as a URL (a data: URI works) in JSON
        body["image"] = json!({ "url": inputs[0].data_uri(), "type": "image_url" });
        post_json(&p.name, &format!("{}/images/edits", p.base()), &auth(p)?, &body)?
    } else {
        // OpenAI-style edits: the same fields as a form, with the pictures as files (GPT image
        // models take several as image[]; older and compatible servers one as image)
        let field = if p.kind == ProviderKind::Openai { "image[]" } else { "image" };
        let mut parts: Vec<Part> = Vec::new();
        if let Value::Object(m) = &body {
            for (k, v) in m {
                parts.push(Part::Text(k, v.as_str().map_or_else(|| v.to_string(), String::from)));
            }
        }
        let take = if p.kind == ProviderKind::Openai { inputs.len() } else { 1 };
        parts.extend(inputs.iter().take(take).map(|i| Part::File(field, i)));
        post_multipart(&p.name, &format!("{}/images/edits", p.base()), &auth(p)?, &parts)?
    };
    let items = resp.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
    if items.is_empty() {
        return Err(GenError::Failed(format!("{} returned no images", p.name)));
    }
    let mut out = Vec::new();
    for item in items {
        let bytes = match (item.get("b64_json").and_then(Value::as_str), item.get("url").and_then(Value::as_str)) {
            (Some(data), _) => b64(data)?,
            (None, Some(url)) => get_bytes(&p.name, url, &[])?,
            _ => continue,
        };
        out.push(Output { ext: sniff_ext(&bytes).to_string(), bytes, model: model.clone() });
    }
    Ok(out)
}

/// xAI: `POST /videos/generations` → `request_id`; poll `GET /videos/{id}` until `done`; `video.url`.
pub(crate) fn xai_video(p: &Provider, r: &Request, status: &mut dyn FnMut(&str)) -> Result<Vec<Output>, GenError> {
    let model = p.model(r)?;
    let headers = auth(p)?;
    let mut body = json!({ "model": model, "prompt": r.prompt });
    if let Some(s) = r.seconds {
        body["duration"] = json!(s.clamp(1, 15));
    }
    if let Some(first) = r.inputs()?.first() {
        body["image"] = json!({ "url": first.data_uri() });
    }
    let job = post_json(&p.name, &format!("{}/videos/generations", p.base()), &headers, &body)?;
    let id = job
        .get("request_id")
        .or_else(|| job.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| GenError::Failed(format!("{} didn't return a request id", p.name)))?
        .to_string();
    let video = poll(VIDEO_LIMIT, Duration::from_secs(5), || {
        let v = get_json(&p.name, &format!("{}/videos/{id}", p.base()), &headers)?;
        match v.get("status").and_then(Value::as_str).unwrap_or("") {
            "done" => Ok(Some(v.pointer("/video/url").and_then(Value::as_str).unwrap_or("").to_string())),
            s @ ("failed" | "expired") => Err(GenError::Failed(format!("{}: the video {s}", p.name))),
            s => {
                status(if s.is_empty() { "working" } else { s });
                Ok(None)
            }
        }
    })?;
    if video.is_empty() {
        return Err(GenError::Failed(format!("{} finished without a video", p.name)));
    }
    let bytes = get_bytes(&p.name, &video, &[])?;
    Ok(vec![Output { ext: sniff_ext(&bytes).to_string(), bytes, model }])
}

/// `GET /models`: the key works and the server answers.
pub(crate) fn check(p: &Provider) -> Result<String, GenError> {
    match get_json(&p.name, &format!("{}/models", p.base()), &auth(p)?) {
        Ok(v) => {
            let n = v.get("data").and_then(Value::as_array).map_or(0, Vec::len);
            Ok(if n > 0 { format!("{} answered: {n} models available", p.name) } else { format!("{} answered", p.name) })
        }
        // some image-only servers have no model list; reaching them is enough
        Err(GenError::Http { status: 404, .. }) if p.kind == ProviderKind::OpenaiCompatible => Ok(format!("{} is reachable", p.name)),
        Err(e) => Err(e),
    }
}
