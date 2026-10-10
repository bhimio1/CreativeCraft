//! Google's Gemini API: Gemini image models through `generateContent`, Veo video through
//! `predictLongRunning` (polled, then downloaded with the key).

use crate::http::{VIDEO_LIMIT, b64, get_bytes, get_json, poll, post_json};
use crate::{Aspect, GenError, Output, Provider, Request, sniff_ext};
use serde_json::{Value, json};
use std::time::Duration;

fn key(p: &Provider) -> Result<Vec<(&'static str, String)>, GenError> {
    Ok(vec![("x-goog-api-key", p.needs_key()?.to_string())])
}

/// Every inline image in a response, wherever the parts are nested (`inlineData` or `inline_data`).
fn inline_images(v: &Value, out: &mut Vec<(String, String)>) {
    match v {
        Value::Object(m) => {
            for key in ["inlineData", "inline_data"] {
                if let Some(d) = m.get(key)
                    && let Some(data) = d.get("data").and_then(Value::as_str)
                {
                    let mime = d.get("mimeType").or_else(|| d.get("mime_type")).and_then(Value::as_str).unwrap_or("image/png");
                    if mime.starts_with("image/") {
                        out.push((mime.to_string(), data.to_string()));
                    }
                }
            }
            for (k, child) in m {
                if k != "inlineData" && k != "inline_data" {
                    inline_images(child, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|c| inline_images(c, out)),
        _ => {}
    }
}

/// Text the model said instead of drawing (e.g. why it declined).
fn text_parts(v: &Value) -> String {
    v.pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)
        .map(|parts| parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

pub(crate) fn image(p: &Provider, r: &Request) -> Result<Vec<Output>, GenError> {
    let model = p.model(r)?;
    let url = format!("{}/models/{model}:generateContent", p.base());
    let headers = key(p)?;
    let body = |with_aspect: bool| {
        let mut config = json!({ "responseModalities": ["TEXT", "IMAGE"] });
        if with_aspect {
            config["imageConfig"] = json!({ "aspectRatio": r.aspect.ratio() });
        }
        json!({ "contents": [{ "parts": [{ "text": r.prompt }] }], "generationConfig": config })
    };
    let mut out = Vec::new();
    // one image per call
    for _ in 0..r.count {
        let resp = match post_json(&p.name, &url, &headers, &body(r.aspect != Aspect::Square)) {
            // an older model without aspect ratios: ask again without one
            Err(GenError::Http { status: 400, body: b, .. }) if b.contains("imageConfig") || b.contains("aspect") => {
                post_json(&p.name, &url, &headers, &body(false))?
            }
            other => other?,
        };
        let mut found = Vec::new();
        inline_images(&resp, &mut found);
        if found.is_empty() {
            let said = text_parts(&resp);
            return Err(GenError::Failed(if said.is_empty() {
                format!("{} returned no image", p.name)
            } else {
                format!("{} returned no image: {said}", p.name)
            }));
        }
        for (_, data) in found {
            let bytes = b64(&data)?;
            out.push(Output { ext: sniff_ext(&bytes).to_string(), bytes, model: model.clone() });
        }
    }
    Ok(out)
}

/// The nearest length Veo offers: 4, 6 or 8 seconds.
fn veo_seconds(s: Option<u32>) -> &'static str {
    match s.unwrap_or(8) {
        0..=4 => "4",
        5..=6 => "6",
        _ => "8",
    }
}

pub(crate) fn veo(p: &Provider, r: &Request, status: &mut dyn FnMut(&str)) -> Result<Vec<Output>, GenError> {
    let model = p.model(r)?;
    let headers = key(p)?;
    let aspect = if r.aspect == Aspect::Portrait { "9:16" } else { "16:9" };
    let body = json!({
        "instances": [{ "prompt": r.prompt }],
        "parameters": { "aspectRatio": aspect, "durationSeconds": veo_seconds(r.seconds) },
    });
    let op = post_json(&p.name, &format!("{}/models/{model}:predictLongRunning", p.base()), &headers, &body)?;
    let name = op.get("name").and_then(Value::as_str).ok_or_else(|| GenError::Failed(format!("{} didn't start the video", p.name)))?.to_string();
    let done = poll(VIDEO_LIMIT, Duration::from_secs(10), || {
        let v = get_json(&p.name, &format!("{}/{name}", p.base()), &headers)?;
        if let Some(e) = v.get("error") {
            return Err(GenError::Failed(format!("{}: {}", p.name, e.get("message").and_then(Value::as_str).unwrap_or("the video failed"))));
        }
        if v.get("done").and_then(Value::as_bool) == Some(true) {
            return Ok(Some(v));
        }
        status("generating");
        Ok(None)
    })?;
    let uri = done
        .pointer("/response/generateVideoResponse/generatedSamples/0/video/uri")
        .and_then(Value::as_str)
        .ok_or_else(|| GenError::Failed(format!("{} finished without a video (it may have been filtered)", p.name)))?;
    let bytes = get_bytes(&p.name, uri, &headers)?;
    Ok(vec![Output { ext: "mp4".into(), bytes, model }])
}

pub(crate) fn check(p: &Provider) -> Result<String, GenError> {
    let v = get_json(&p.name, &format!("{}/models", p.base()), &key(p)?)?;
    let n = v.get("models").and_then(Value::as_array).map_or(0, Vec::len);
    Ok(format!("{} answered: {n} models available", p.name))
}
