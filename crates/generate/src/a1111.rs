//! Stable Diffusion WebUI (AUTOMATIC1111, Forge, SD.Next) on the user's computer, started with
//! `--api`: `POST /sdapi/v1/txt2img` (or `img2img` from a starting picture) returns base64 PNGs.

use crate::http::{b64, get_json, post_json, send_stop};
use crate::{GenError, Output, Provider, Request, sniff_ext};
use serde_json::{Value, json};

pub(crate) fn txt2img(p: &Provider, r: &Request) -> Result<Vec<Output>, GenError> {
    // editing keeps the picture's own shape
    let (w, h) = r.picture_size().unwrap_or_else(|| r.aspect.pixels());
    let model = if r.model.trim().is_empty() { p.image_model.trim().to_string() } else { r.model.trim().to_string() };
    let mut body = json!({
        "prompt": r.prompt, "negative_prompt": r.negative, "width": w, "height": h,
        "batch_size": r.count, "n_iter": 1, "steps": 25, "seed": r.seed.map_or(-1, |s| s as i64),
    });
    if !model.is_empty() {
        body["override_settings"] = json!({ "sd_model_checkpoint": model });
    }
    let inputs = r.inputs()?;
    if !inputs.is_empty() {
        body["init_images"] = json!(inputs.iter().map(crate::InputImage::base64).collect::<Vec<_>>());
        body["denoising_strength"] = json!(r.denoise());
        if let Some(mask) = r.mask()? {
            // inpaint the white part, starting from what's there
            body["mask"] = json!(mask.white.base64());
            body["inpainting_fill"] = json!(1);
            body["mask_blur"] = json!(4);
        }
    }
    let endpoint = if inputs.is_empty() { "txt2img" } else { "img2img" };
    if let Value::Object(m) = &mut body {
        m.extend(p.extra()?);
    }
    // the request blocks until the images are done, so stopping is a call from another thread
    let watcher = crate::current_cancel().map(|cancel| {
        let (name, url) = (p.name.clone(), format!("{}/sdapi/v1/interrupt", p.base()));
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = done_rx.recv_timeout(std::time::Duration::from_millis(200)) {
                if cancel.is_cancelled() {
                    send_stop(&name, "POST", &url, &[], None);
                    return;
                }
            }
        });
        done_tx
    });
    let result = post_json(&p.name, &format!("{}/sdapi/v1/{endpoint}", p.base()), &[], &body);
    drop(watcher);
    crate::check_cancel()?;
    let resp = match result {
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
