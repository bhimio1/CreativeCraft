//! ComfyUI on the user's computer. A built-in text-to-image workflow (checkpoint loader, two text
//! encoders, sampler, decoder, save) works with any Stable Diffusion checkpoint; for anything else
//! (video, ControlNet, FLUX…) the user points at their own workflow exported with "Export (API)",
//! with `{{prompt}}`, `{{negative}}`, `{{seed}}`, `{{width}}`, `{{height}}`, `{{count}}` and
//! `{{model}}` where the values go.

use crate::http::{IMAGE_LIMIT, VIDEO_LIMIT, get_bytes, get_json, poll, post_json};
use crate::{GenError, Kind, Output, Provider, Request, sniff_ext};
use serde_json::{Value, json};
use std::time::Duration;

/// The first checkpoint ComfyUI has, for the built-in workflow.
fn first_checkpoint(p: &Provider) -> Result<String, GenError> {
    let v = get_json(&p.name, &format!("{}/object_info/CheckpointLoaderSimple", p.base()), &[])?;
    v.pointer("/CheckpointLoaderSimple/input/required/ckpt_name/0/0")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| GenError::Config(format!("{} has no checkpoints yet: put one in ComfyUI's models/checkpoints folder", p.name)))
}

fn seed(r: &Request) -> u64 {
    r.seed.unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64 % 1_000_000_000_000))
}

/// The built-in text-to-image workflow, in ComfyUI's API format.
pub(crate) fn builtin_workflow(r: &Request, checkpoint: &str) -> Value {
    let (w, h) = r.aspect.pixels();
    json!({
        "3": { "class_type": "KSampler", "inputs": {
            "seed": seed(r), "steps": 25, "cfg": 7.0, "sampler_name": "euler", "scheduler": "normal", "denoise": 1.0,
            "model": ["4", 0], "positive": ["6", 0], "negative": ["7", 0], "latent_image": ["5", 0] } },
        "4": { "class_type": "CheckpointLoaderSimple", "inputs": { "ckpt_name": checkpoint } },
        "5": { "class_type": "EmptyLatentImage", "inputs": { "width": w, "height": h, "batch_size": r.count } },
        "6": { "class_type": "CLIPTextEncode", "inputs": { "text": r.prompt, "clip": ["4", 1] } },
        "7": { "class_type": "CLIPTextEncode", "inputs": { "text": r.negative, "clip": ["4", 1] } },
        "8": { "class_type": "VAEDecode", "inputs": { "samples": ["3", 0], "vae": ["4", 2] } },
        "9": { "class_type": "SaveImage", "inputs": { "filename_prefix": "CreativeCraft", "images": ["8", 0] } },
    })
}

fn walk(v: &mut Value, f: &dyn Fn(&str) -> Option<Value>) {
    match v {
        Value::String(s) => {
            if let Some(new) = f(s) {
                *v = new;
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| walk(x, f)),
        Value::Object(m) => m.values_mut().for_each(|x| walk(x, f)),
        _ => {}
    }
}

/// Fill the `{{…}}` placeholders in a user's workflow. A string that is exactly a numeric
/// placeholder becomes a number; text placeholders are replaced inside strings.
pub(crate) fn fill_workflow(v: &mut Value, r: &Request, model: &str) {
    let (w, h) = r.aspect.pixels();
    let s = seed(r);
    walk(v, &|text: &str| match text {
        "{{seed}}" => Some(json!(s)),
        "{{width}}" => Some(json!(w)),
        "{{height}}" => Some(json!(h)),
        "{{count}}" => Some(json!(r.count)),
        t if t.contains("{{") => Some(json!(t.replace("{{prompt}}", &r.prompt).replace("{{negative}}", &r.negative).replace("{{model}}", model))),
        _ => None,
    });
}

/// Files a finished prompt produced: every `{filename, subfolder, type}` under its outputs
/// (`images`, `gifs`, `videos`, …).
pub(crate) fn output_files(history_entry: &Value) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    if let Some(outputs) = history_entry.get("outputs").and_then(Value::as_object) {
        for node in outputs.values() {
            let Some(node) = node.as_object() else { continue };
            for list in node.values().filter_map(Value::as_array) {
                for f in list {
                    if let Some(name) = f.get("filename").and_then(Value::as_str) {
                        let sub = f.get("subfolder").and_then(Value::as_str).unwrap_or("").to_string();
                        let kind = f.get("type").and_then(Value::as_str).unwrap_or("output").to_string();
                        // only saved results, not previews or temporary files
                        if kind == "output" {
                            out.push((name.to_string(), sub, kind));
                        }
                    }
                }
            }
        }
    }
    out
}

fn query_escape(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

pub(crate) fn run(p: &Provider, r: &Request, status: &mut dyn FnMut(&str)) -> Result<Vec<Output>, GenError> {
    let model = if r.model.trim().is_empty() { p.image_model.trim().to_string() } else { r.model.trim().to_string() };
    let workflow = if p.workflow.trim().is_empty() {
        if r.kind == Kind::Video {
            return Err(GenError::Config(format!(
                "{}: video needs a workflow of your own (save one with \"Export (API)\" and choose it in Settings → AI generation)",
                p.name
            )));
        }
        let checkpoint = if model.is_empty() { first_checkpoint(p)? } else { model.clone() };
        builtin_workflow(r, &checkpoint)
    } else {
        let text = std::fs::read_to_string(p.workflow.trim())
            .map_err(|e| GenError::Config(format!("{}: can't read the workflow {}: {e}", p.name, p.workflow.trim())))?;
        let mut v: Value = serde_json::from_str(&text)
            .map_err(|e| GenError::Config(format!("{}: the workflow isn't valid JSON ({e}); export it with \"Export (API)\"", p.name)))?;
        fill_workflow(&mut v, r, &model);
        v
    };
    let queued = post_json(&p.name, &format!("{}/prompt", p.base()), &[], &json!({ "prompt": workflow }))?;
    let id = queued.get("prompt_id").and_then(Value::as_str).ok_or_else(|| GenError::Failed(format!("{} didn't queue the workflow", p.name)))?.to_string();
    let limit = if r.kind == Kind::Video { VIDEO_LIMIT } else { IMAGE_LIMIT };
    let entry = poll(limit, Duration::from_secs(1), || {
        let h = get_json(&p.name, &format!("{}/history/{id}", p.base()), &[])?;
        let Some(entry) = h.get(&id) else {
            status("running");
            return Ok(None);
        };
        if entry.pointer("/status/status_str").and_then(Value::as_str) == Some("error") {
            return Err(GenError::Failed(format!("{}: the workflow failed; see ComfyUI's window for the node that stopped", p.name)));
        }
        if entry.pointer("/status/completed").and_then(Value::as_bool) == Some(false) {
            status("running");
            return Ok(None);
        }
        Ok(Some(entry.clone()))
    })?;
    let files = output_files(&entry);
    if files.is_empty() {
        return Err(GenError::Failed(format!("{}: the workflow finished but saved nothing (it needs a Save node)", p.name)));
    }
    let mut out = Vec::new();
    for (name, sub, kind) in files {
        let url = format!("{}/view?filename={}&subfolder={}&type={}", p.base(), query_escape(&name), query_escape(&sub), query_escape(&kind));
        let bytes = get_bytes(&p.name, &url, &[])?;
        let ext = name.rsplit('.').next().filter(|e| e.len() <= 5 && *e != name).map_or_else(|| sniff_ext(&bytes).to_string(), str::to_ascii_lowercase);
        out.push(Output { ext, bytes, model: if model.is_empty() { "ComfyUI workflow".into() } else { model.clone() } });
    }
    Ok(out)
}

pub(crate) fn check(p: &Provider) -> Result<String, GenError> {
    let v = get_json(&p.name, &format!("{}/system_stats", p.base()), &[])?;
    let version = v.pointer("/system/comfyui_version").and_then(Value::as_str).unwrap_or("");
    Ok(if version.is_empty() { format!("{} is running", p.name) } else { format!("{} {version} is running", p.name) })
}
