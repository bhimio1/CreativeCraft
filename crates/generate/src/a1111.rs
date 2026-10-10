//! Stable Diffusion WebUI (AUTOMATIC1111, Forge, SD.Next) on the user's computer, started with
//! `--api`: `POST /sdapi/v1/txt2img` returns base64 PNGs.

use crate::http::{b64, get_json, post_json};
use crate::{GenError, Output, Provider, Request, sniff_ext};
use serde_json::{Value, json};

pub(crate) fn txt2img(p: &Provider, r: &Request) -> Result<Vec<Output>, GenError> {
    let (w, h) = r.aspect.pixels();
    let model = if r.model.trim().is_empty() { p.image_model.trim().to_string() } else { r.model.trim().to_string() };
    let mut body = json!({
        "prompt": r.prompt, "negative_prompt": r.negative, "width": w, "height": h,
        "batch_size": r.count, "n_iter": 1, "steps": 25, "seed": r.seed.map_or(-1, |s| s as i64),
    });
    if !model.is_empty() {
        body["override_settings"] = json!({ "sd_model_checkpoint": model });
    }
    if let Value::Object(m) = &mut body {
        m.extend(p.extra()?);
    }
    let resp = match post_json(&p.name, &format!("{}/sdapi/v1/txt2img", p.base()), &[], &body) {
        Err(GenError::Http { status: 404, .. }) => {
            return Err(GenError::Config(format!("{} answered, but without its API: start it with --api", p.name)));
        }
        other => other?,
    };
    let images = resp.get("images").and_then(Value::as_array).cloned().unwrap_or_default();
    let label = if model.is_empty() { "loaded checkpoint".to_string() } else { model };
    images
        .iter()
        .filter_map(Value::as_str)
        .map(|data| {
            let bytes = b64(data)?;
            Ok(Output { ext: sniff_ext(&bytes).to_string(), bytes, model: label.clone() })
        })
        .collect()
}

pub(crate) fn check(p: &Provider) -> Result<String, GenError> {
    let v = get_json(&p.name, &format!("{}/sdapi/v1/sd-models", p.base()), &[])?;
    Ok(format!("{} is running with {} checkpoints", p.name, v.as_array().map_or(0, Vec::len)))
}
