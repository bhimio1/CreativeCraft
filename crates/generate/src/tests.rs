//! Adapters against a fake HTTP server on 127.0.0.1 that answers like each service does
//! (shapes from their API references). No network, no keys, no cost.

use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// One request the fake server saw.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

type Route = dyn Fn(&Seen, usize) -> (u16, Vec<u8>) + Send + Sync;

/// Serve `route` (given the request and how many times this path was asked for) on a free port.
fn serve(route: Box<Route>) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::<Seen>::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() || line.is_empty() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
            let mut headers = Vec::new();
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    if k.eq_ignore_ascii_case("content-length") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    headers.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).unwrap();
            let req = Seen { method, path: path.clone(), headers, body: String::from_utf8_lossy(&body).into_owned() };
            let n = {
                let mut l = log.lock().unwrap();
                l.push(req.clone());
                l.iter().filter(|s| s.path == path).count()
            };
            let (status, body) = route(&req, n);
            let head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n", body.len());
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    (base, seen)
}

fn json(v: serde_json::Value) -> (u16, Vec<u8>) {
    (200, v.to_string().into_bytes())
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfakeimage";
const MP4: &[u8] = b"\0\0\0\x18ftypmp42fakevideo";

fn png_b64() -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(PNG)
}

fn provider(kind: ProviderKind, base: &str) -> Provider {
    Provider {
        id: "t".into(),
        name: "Test".into(),
        kind,
        base_url: base.into(),
        image_model: "img-model".into(),
        video_model: "vid-model".into(),
        api_key: "sk-test".into(),
        workflow: String::new(),
        extra: String::new(),
    }
}

fn req(kind: Kind) -> Request {
    Request {
        kind,
        prompt: "a lighthouse".into(),
        negative: String::new(),
        model: String::new(),
        aspect: Aspect::Landscape,
        count: 2,
        seconds: Some(8),
        seed: None,
        images: Vec::new(),
        strength: None,
    }
}

/// A request starting from a picture on disk.
fn from_picture(kind: Kind, tag: &str) -> Request {
    let path = std::env::temp_dir().join(format!("cc-gen-input-{}-{}.png", tag.replace('/', "-"), std::process::id()));
    std::fs::write(&path, PNG).unwrap();
    Request { images: vec![path], count: 1, strength: Some(40), ..req(kind) }
}

fn quiet() -> impl FnMut(&str) {
    |_: &str| {}
}

#[test]
fn presets_parse() {
    let p = presets();
    assert!(p.iter().any(|x| x.id == "openai" && x.kind == ProviderKind::Openai));
    assert!(p.iter().any(|x| x.kind == ProviderKind::Comfyui && x.key_url.is_empty()));
    let ids: std::collections::BTreeSet<_> = p.iter().map(|x| &x.id).collect();
    assert_eq!(ids.len(), p.len(), "unique ids");
}

#[test]
fn sniffing() {
    assert_eq!(sniff_ext(PNG), "png");
    assert_eq!(sniff_ext(MP4), "mp4");
    assert_eq!(sniff_ext(&[0xff, 0xd8, 0xff, 0xe0]), "jpg");
    assert_eq!(sniff_ext(b"RIFF\0\0\0\0WEBPVP8 "), "webp");
}

#[test]
fn openai_images_send_size_and_key() {
    let b64 = png_b64();
    let (base, seen) = serve(Box::new(move |_r, _| json(serde_json::json!({ "data": [{ "b64_json": b64 }, { "b64_json": b64 }] }))));
    let out = generate(&provider(ProviderKind::Openai, &base), &req(Kind::Image), &mut quiet()).unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!((out[0].ext.as_str(), out[0].bytes.as_slice()), ("png", PNG));
    let s = seen.lock().unwrap()[0].clone();
    assert_eq!((s.method.as_str(), s.path.as_str()), ("POST", "/images/generations"));
    assert_eq!(s.header("authorization"), Some("Bearer sk-test"));
    assert_eq!(s.json()["size"], "1536x1024");
    assert_eq!(s.json()["n"], 2);
}

#[test]
fn compatible_servers_without_a_key_and_url_results() {
    let (base, seen) = serve(Box::new(|r, _| {
        if r.path == "/images/generations" {
            let me = r.header("host").unwrap_or("").to_string();
            json(serde_json::json!({ "data": [{ "url": format!("http://{me}/files/1.png") }] }))
        } else {
            (200, PNG.to_vec())
        }
    }));
    let mut p = provider(ProviderKind::OpenaiCompatible, &base);
    p.api_key.clear();
    p.extra = r#"{"steps": 4}"#.into();
    let out = generate(&p, &Request { count: 1, ..req(Kind::Image) }, &mut quiet()).unwrap();
    assert_eq!(out[0].bytes, PNG);
    let s = seen.lock().unwrap()[0].clone();
    assert!(s.header("authorization").is_none());
    assert!(s.json().get("size").is_none(), "no OpenAI-specific sizes to other servers");
    assert_eq!(s.json()["steps"], 4, "extra options are merged in");
}

#[test]
fn openai_video_explains_that_sora_was_retired() {
    let p = provider(ProviderKind::Openai, "http://127.0.0.1:9");
    assert!(!ProviderKind::Openai.does_video());
    let e = generate(&p, &req(Kind::Video), &mut quiet()).unwrap_err().to_string();
    assert!(e.contains("retired") && e.contains("Google"), "{e}");
}

#[test]
fn errors_are_explained() {
    let (base, _) = serve(Box::new(|_, _| (401, br#"{"error":{"message":"Incorrect API key provided"}}"#.to_vec())));
    let e = generate(&provider(ProviderKind::Openai, &base), &req(Kind::Image), &mut quiet()).unwrap_err();
    let msg = e.to_string();
    assert!(msg.contains("401") && msg.contains("Incorrect API key") && msg.contains("key wasn't accepted"), "{msg}");
}

#[test]
fn keys_quoted_back_are_redacted_and_offline_services_say_so() {
    let (base, _) = serve(Box::new(|_, _| (401, br#"{"error":{"message":"Incorrect API key provided: sk-test"}}"#.to_vec())));
    let p = provider(ProviderKind::Openai, &base);
    for msg in [generate(&p, &req(Kind::Image), &mut quiet()).unwrap_err().to_string(), check(&p).unwrap_err().to_string()] {
        assert!(!msg.contains("sk-test") && msg.contains("[your key]"), "{msg}");
    }
    // nothing listens on the discard port
    let msg = check(&provider(ProviderKind::Comfyui, "http://127.0.0.1:9")).unwrap_err().to_string();
    assert!(msg.contains("isn't answering at http://127.0.0.1:9"), "{msg}");
}

#[test]
fn comfyui_makes_video_only_with_a_workflow() {
    let mut p = provider(ProviderKind::Comfyui, "http://127.0.0.1:9");
    assert!(!p.makes_video());
    p.workflow = "/w.json".into();
    assert!(p.makes_video());
    assert!(!provider(ProviderKind::Automatic1111, "").makes_video());
}

#[test]
fn missing_key_and_model_say_where_to_fix_them() {
    let mut p = provider(ProviderKind::Openai, "http://127.0.0.1:9");
    p.api_key.clear();
    assert!(generate(&p, &req(Kind::Image), &mut quiet()).unwrap_err().to_string().contains("needs an API key"));
    let mut p = provider(ProviderKind::Openai, "http://127.0.0.1:9");
    p.image_model.clear();
    assert!(generate(&p, &req(Kind::Image), &mut quiet()).unwrap_err().to_string().contains("no image model"));
    let p = provider(ProviderKind::Automatic1111, "http://127.0.0.1:9");
    assert!(generate(&p, &req(Kind::Video), &mut quiet()).unwrap_err().to_string().contains("doesn't make video"));
}

#[test]
fn xai_video_flow() {
    let (base, _) = serve(Box::new(|r, n| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/videos/generations") => json(serde_json::json!({ "request_id": "req_9" })),
        ("GET", "/videos/req_9") if n < 2 => json(serde_json::json!({ "status": "pending" })),
        ("GET", "/videos/req_9") => {
            let me = r.header("host").unwrap_or("").to_string();
            json(serde_json::json!({ "status": "done", "video": { "url": format!("http://{me}/out.mp4") } }))
        }
        _ => (200, MP4.to_vec()),
    }));
    let out = generate(&provider(ProviderKind::Xai, &base), &req(Kind::Video), &mut quiet()).unwrap();
    assert_eq!(out[0].bytes, MP4);
}

#[test]
fn gemini_images_and_veo() {
    let b64 = png_b64();
    let (base, seen) = serve(Box::new(move |r, n| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/models/img-model:generateContent") => json(serde_json::json!({
            "candidates": [{ "content": { "parts": [{ "text": "Here you go" }, { "inlineData": { "mimeType": "image/png", "data": b64 } }] } }]
        })),
        ("POST", "/models/vid-model:predictLongRunning") => json(serde_json::json!({ "name": "operations/op1" })),
        ("GET", "/operations/op1") if n < 2 => json(serde_json::json!({ "name": "operations/op1", "done": false })),
        ("GET", "/operations/op1") => {
            let me = r.header("host").unwrap_or("").to_string();
            json(
                serde_json::json!({ "done": true, "response": { "generateVideoResponse": { "generatedSamples": [{ "video": { "uri": format!("http://{me}/files/v.mp4") } }] } } }),
            )
        }
        _ => (200, MP4.to_vec()),
    }));
    let p = provider(ProviderKind::Google, &base);
    let out = generate(&p, &Request { count: 1, ..req(Kind::Image) }, &mut quiet()).unwrap();
    assert_eq!(out[0].bytes, PNG);
    let out = generate(&p, &req(Kind::Video), &mut quiet()).unwrap();
    assert_eq!(out[0].bytes, MP4);
    let seen = seen.lock().unwrap();
    assert!(seen.iter().all(|s| s.header("x-goog-api-key") == Some("sk-test")), "the key goes with every call, downloads included");
    let veo = seen.iter().find(|s| s.path.ends_with(":predictLongRunning")).unwrap().json();
    assert_eq!((veo["parameters"]["aspectRatio"].as_str(), veo["parameters"]["durationSeconds"].as_str()), (Some("16:9"), Some("8")));
}

#[test]
fn gemini_refusal_shows_what_it_said() {
    let (base, _) = serve(Box::new(|_, _| json(serde_json::json!({ "candidates": [{ "content": { "parts": [{ "text": "I can't make that." }] } }] }))));
    let e = generate(&provider(ProviderKind::Google, &base), &Request { count: 1, ..req(Kind::Image) }, &mut quiet()).unwrap_err();
    assert!(e.to_string().contains("I can't make that."), "{e}");
}

#[test]
fn replicate_waits_polls_and_downloads_with_the_token() {
    let (base, seen) = serve(Box::new(|r, n| match (r.method.as_str(), r.path.as_str()) {
        // the first try is refused as if the model didn't take aspect_ratio
        ("POST", "/models/owner/model/predictions") if n == 1 => (422, br#"{"detail":"aspect_ratio is not a valid input"}"#.to_vec()),
        ("POST", "/models/owner/model/predictions") => json(serde_json::json!({ "id": "p1", "status": "starting" })),
        ("GET", "/predictions/p1") if n < 2 => json(serde_json::json!({ "id": "p1", "status": "processing" })),
        ("GET", "/predictions/p1") => {
            let me = r.header("host").unwrap_or("").to_string();
            json(serde_json::json!({ "id": "p1", "status": "succeeded", "output": [format!("http://{me}/o/1.png"), format!("http://{me}/o/2.png")] }))
        }
        _ => (200, PNG.to_vec()),
    }));
    let mut p = provider(ProviderKind::Replicate, &base);
    p.image_model = "owner/model".into();
    let out = generate(&p, &req(Kind::Image), &mut quiet()).unwrap();
    assert_eq!(out.len(), 2);
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].json()["input"]["aspect_ratio"], "16:9");
    assert_eq!(seen[0].json()["input"]["num_outputs"], 2);
    assert!(seen[1].json()["input"].get("aspect_ratio").is_none(), "retried with just the prompt");
    assert!(seen.iter().filter(|s| s.path.starts_with("/o/")).all(|s| s.header("authorization") == Some("Bearer sk-test")));
    let mut bad = provider(ProviderKind::Replicate, &base);
    bad.image_model = "no-slash".into();
    assert!(generate(&bad, &req(Kind::Image), &mut quiet()).is_err());
}

#[test]
fn stopping_a_job_stops_it_on_the_service() {
    // a Replicate prediction and a ComfyUI prompt that never finish
    let (base, seen) = serve(Box::new(|r, _| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/models/owner/model/predictions") => json(serde_json::json!({ "id": "p1", "status": "starting" })),
        ("GET", "/predictions/p1") => json(serde_json::json!({ "id": "p1", "status": "processing" })),
        ("POST", "/prompt") => json(serde_json::json!({ "prompt_id": "c1" })),
        ("GET", "/history/c1") => json(serde_json::json!({})),
        _ => json(serde_json::json!({})),
    }));
    let mut rep = provider(ProviderKind::Replicate, &base);
    rep.image_model = "owner/model".into();
    let mut comfy = provider(ProviderKind::Comfyui, &base);
    comfy.image_model = "sd.safetensors".into();
    for p in [rep, comfy] {
        let cancel = Cancel::default();
        let stopper = cancel.clone();
        let polls = std::sync::atomic::AtomicUsize::new(0);
        let mut status = |_: &str| {
            if polls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 2 {
                stopper.cancel();
            }
        };
        let e = generate_cancellable(&p, &req(Kind::Image), &mut status, &cancel).unwrap_err();
        assert!(matches!(e, GenError::Cancelled), "{e}");
    }
    let log = seen.lock().unwrap().clone();
    assert!(log.iter().any(|s| s.method == "POST" && s.path == "/predictions/p1/cancel"), "Replicate told to cancel");
    assert!(log.iter().any(|s| s.path == "/queue" && s.json()["delete"][0] == "c1"), "ComfyUI queue entry removed");
    assert!(log.iter().any(|s| s.path == "/interrupt"), "ComfyUI interrupted");
    // stopped before it starts: nothing is sent
    let cancel = Cancel::default();
    cancel.cancel();
    let p = provider(ProviderKind::Openai, &base);
    assert!(matches!(generate_cancellable(&p, &req(Kind::Image), &mut quiet(), &cancel), Err(GenError::Cancelled)));
    assert_eq!(seen.lock().unwrap().len(), log.len());
}

#[test]
fn fal_queue_flow() {
    let (base, seen) = serve(Box::new(|r, n| {
        let me = format!("http://{}", r.header("host").unwrap_or(""));
        match (r.method.as_str(), r.path.as_str()) {
            ("POST", "/fal-ai/flux/dev") => json(serde_json::json!({
                "request_id": "r1",
                "status_url": format!("{me}/fal-ai/flux/dev/requests/r1/status"),
                "response_url": format!("{me}/fal-ai/flux/dev/requests/r1"),
            })),
            ("GET", "/fal-ai/flux/dev/requests/r1/status") if n < 2 => json(serde_json::json!({ "status": "IN_QUEUE", "queue_position": 3 })),
            ("GET", "/fal-ai/flux/dev/requests/r1/status") => json(serde_json::json!({ "status": "COMPLETED" })),
            ("GET", "/fal-ai/flux/dev/requests/r1") => json(serde_json::json!({ "images": [{ "url": format!("{me}/f/1.png"), "width": 1 }] })),
            _ => (200, PNG.to_vec()),
        }
    }));
    let mut p = provider(ProviderKind::Fal, &base);
    p.image_model = "fal-ai/flux/dev".into();
    let mut said = Vec::new();
    let out = generate(&p, &Request { count: 1, ..req(Kind::Image) }, &mut |s: &str| said.push(s.to_string())).unwrap();
    assert_eq!(out[0].bytes, PNG);
    assert!(said.iter().any(|s| s.contains("3 ahead")));
    assert_eq!(seen.lock().unwrap()[0].header("authorization"), Some("Key sk-test"));
}

#[test]
fn comfyui_builtin_workflow() {
    let (base, seen) = serve(Box::new(|r, n| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/object_info/CheckpointLoaderSimple") => {
            json(serde_json::json!({ "CheckpointLoaderSimple": { "input": { "required": { "ckpt_name": [["sdxl.safetensors", "other.ckpt"]] } } } }))
        }
        ("POST", "/prompt") => json(serde_json::json!({ "prompt_id": "abc", "number": 1 })),
        ("GET", "/history/abc") if n < 2 => json(serde_json::json!({})),
        ("GET", "/history/abc") => json(serde_json::json!({ "abc": {
            "status": { "status_str": "success", "completed": true },
            "outputs": { "9": { "images": [
                { "filename": "CreativeCraft_00001_.png", "subfolder": "", "type": "output" },
                { "filename": "preview.png", "subfolder": "", "type": "temp" }
            ] } }
        } })),
        _ => (200, PNG.to_vec()),
    }));
    let mut p = provider(ProviderKind::Comfyui, &base);
    p.image_model.clear();
    p.api_key.clear();
    let out = generate(&p, &Request { count: 1, ..req(Kind::Image) }, &mut quiet()).unwrap();
    assert_eq!(out.len(), 1, "temp previews are skipped");
    assert_eq!(out[0].ext, "png");
    let seen = seen.lock().unwrap();
    let wf = seen.iter().find(|s| s.path == "/prompt").unwrap().json();
    assert_eq!(wf["prompt"]["4"]["inputs"]["ckpt_name"], "sdxl.safetensors");
    assert_eq!(wf["prompt"]["5"]["inputs"]["width"], 1344);
    assert!(seen.iter().any(|s| s.path.starts_with("/view?filename=CreativeCraft_00001_.png")));
    // video needs a workflow of the user's own
    assert!(generate(&p, &req(Kind::Video), &mut quiet()).unwrap_err().to_string().contains("workflow"));
}

#[test]
fn comfyui_placeholders() {
    let mut wf = serde_json::json!({
        "1": { "inputs": { "text": "photo of {{prompt}}, best quality", "neg": "{{negative}}", "seed": "{{seed}}", "w": "{{width}}", "n": "{{count}}", "ckpt": "{{model}}" } }
    });
    let r = Request { negative: "blurry".into(), seed: Some(42), count: 1, ..req(Kind::Image) };
    comfy::fill_workflow(&mut wf, &r, "flux.safetensors", None);
    let i = &wf["1"]["inputs"];
    assert_eq!(i["text"], "photo of a lighthouse, best quality");
    assert_eq!(i["neg"], "blurry");
    assert_eq!((i["seed"].as_u64(), i["w"].as_u64(), i["n"].as_u64()), (Some(42), Some(1344), Some(1)));
    assert_eq!(i["ckpt"], "flux.safetensors");
}

#[test]
fn stable_diffusion_webui() {
    let b64 = png_b64();
    let (base, seen) = serve(Box::new(move |_, _| json(serde_json::json!({ "images": [b64, b64], "parameters": {} }))));
    let mut p = provider(ProviderKind::Automatic1111, &base);
    p.image_model = "dreamshaper_8".into();
    let out = generate(&p, &req(Kind::Image), &mut quiet()).unwrap();
    assert_eq!(out.len(), 2);
    let body = seen.lock().unwrap()[0].json();
    assert_eq!(body["batch_size"], 2);
    assert_eq!(body["override_settings"]["sd_model_checkpoint"], "dreamshaper_8");
}

#[test]
fn starting_pictures_reach_each_service_its_way() {
    let b64 = png_b64();
    let (base, seen) = serve(Box::new(move |r, _| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/images/edits") => json(serde_json::json!({ "data": [{ "b64_json": b64 }] })),
        ("POST", p) if p.contains(":generateContent") => {
            json(serde_json::json!({ "candidates": [{ "content": { "parts": [{ "inlineData": { "mimeType": "image/png", "data": b64 } }] } }] }))
        }
        ("GET", "/models/owner/kontext") => {
            json(serde_json::json!({ "latest_version": { "openapi_schema": { "components": { "schemas": { "Input": { "properties": {
            "prompt": { "type": "string" }, "input_image": { "type": "string" } } } } } } } }))
        }
        ("POST", "/models/owner/kontext/predictions") => {
            json(serde_json::json!({ "id": "k1", "status": "succeeded", "output": format!("data:image/png;base64,{b64}") }))
        }
        ("POST", "/upload/image") => json(serde_json::json!({ "name": "start.png", "subfolder": "", "type": "input" })),
        ("POST", "/prompt") => json(serde_json::json!({ "prompt_id": "i1" })),
        ("GET", "/history/i1") => json(
            serde_json::json!({ "i1": { "status": { "completed": true }, "outputs": { "9": { "images": [{ "filename": "o.png", "subfolder": "", "type": "output" }] } } } }),
        ),
        ("POST", "/sdapi/v1/img2img") => json(serde_json::json!({ "images": [b64] })),
        _ => (200, PNG.to_vec()),
    }));
    let run = |kind: ProviderKind, model: &str| {
        let mut p = provider(kind, &base);
        p.image_model = model.into();
        generate(&p, &from_picture(Kind::Image, model), &mut quiet()).unwrap()
    };
    run(ProviderKind::Openai, "gpt-image");
    run(ProviderKind::Google, "gemini-image");
    run(ProviderKind::Replicate, "owner/kontext");
    run(ProviderKind::Comfyui, "sd.safetensors");
    run(ProviderKind::Automatic1111, "dreamshaper");
    let seen = seen.lock().unwrap();
    let find = |path: &str| seen.iter().find(|s| s.path.starts_with(path)).unwrap().clone();

    // OpenAI: a form with the fields and the picture as image[]
    let edit = find("/images/edits");
    assert!(edit.header("content-type").unwrap().starts_with("multipart/form-data; boundary="));
    assert!(edit.body.contains("name=\"prompt\"\r\n\r\na lighthouse") && edit.body.contains("name=\"image[]\"; filename=") && edit.body.contains("fakeimage"));
    // Gemini: the picture as a part before the words
    let parts = find("/models/gemini-image:generateContent").json()["contents"][0]["parts"].clone();
    assert_eq!(parts[0]["inlineData"]["mimeType"], "image/png");
    assert_eq!(parts[1]["text"], "a lighthouse");
    // Replicate: under the name the model's schema uses, as a data URI, without a forced shape
    let input = find("/models/owner/kontext/predictions").json()["input"].clone();
    assert!(input["input_image"].as_str().unwrap().starts_with("data:image/png;base64,"));
    assert!(input.get("aspect_ratio").is_none());
    // ComfyUI: uploaded, then loaded and partly re-sampled by the built-in workflow
    assert!(find("/upload/image").body.contains("name=\"image\"; filename="));
    let wf = find("/prompt").json()["prompt"].clone();
    assert_eq!(wf["10"]["inputs"]["image"], "start.png");
    assert_eq!(wf["3"]["inputs"]["denoise"], 0.4);
    assert_eq!(wf["3"]["inputs"]["latent_image"][0], "12");
    // Stable Diffusion WebUI: img2img with the strength
    let a1111 = find("/sdapi/v1/img2img").json();
    assert_eq!(a1111["denoising_strength"], 0.4);
    assert_eq!(a1111["init_images"].as_array().unwrap().len(), 1);
}

#[test]
fn starting_pictures_are_checked() {
    let mut r = from_picture(Kind::Image, "check");
    r.images.push(std::path::PathBuf::from("/no/such/picture.png"));
    assert!(r.inputs().unwrap_err().to_string().contains("can't read the starting picture"));
    let text = std::env::temp_dir().join(format!("cc-gen-notpic-{}.txt", std::process::id()));
    std::fs::write(&text, "hello").unwrap();
    r.images = vec![text];
    assert!(r.inputs().unwrap_err().to_string().contains("isn't a PNG"));
}
