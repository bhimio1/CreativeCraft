//! AI image and video generation into the projects folder: the user's own services (an API key
//! they bring, a custom endpoint, or ComfyUI / Stable Diffusion WebUI on their computer) make the
//! files, and they land in `<projects>/Generated/<date>/` with a sidecar describing how.

use crate::{EngineError, Event, GenerationSettings, Session, Settings};
use creativecraft_generate::{Kind, Provider, Request};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The folder results go to, inside the projects folder.
pub const GENERATED_DIR: &str = "Generated";

/// (year, month, day) from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// ("YYYY-MM-DD", "HHMMSS", RFC 3339) for a Unix time, in UTC.
fn stamps(secs: u64) -> (String, String, String) {
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let t = secs % 86_400;
    let (hh, mm, ss) = (t / 3600, t % 3600 / 60, t % 60);
    (format!("{y:04}-{m:02}-{d:02}"), format!("{hh:02}{mm:02}{ss:02}"), format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z"))
}

/// A short file-name-safe slug of the prompt's first words.
pub(crate) fn slug(prompt: &str) -> String {
    let mut out = String::new();
    for word in prompt.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        if out.len() + word.len() > 40 {
            break;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&word.to_lowercase());
    }
    if out.is_empty() { "generated".into() } else { out }
}

/// Write `bytes` beside `path` then rename, so a half-written file never appears.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("{}.part", path.extension().and_then(|e| e.to_str()).unwrap_or("")));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

/// `base.ext`, or `base-2.ext`, `base-3.ext`… if taken.
fn free_name(dir: &Path, base: &str, ext: &str) -> PathBuf {
    let mut n = 1;
    loop {
        let name = if n == 1 { format!("{base}.{ext}") } else { format!("{base}-{n}.{ext}") };
        let p = dir.join(name);
        if !p.exists() {
            return p;
        }
        n += 1;
    }
}

/// A provider's summary for agents and `generate providers` (never the key).
pub fn provider_json(p: &Provider, image_default: bool, video_default: bool) -> Value {
    let default_for: Vec<&str> = [image_default.then_some("image"), video_default.then_some("video")].into_iter().flatten().collect();
    json!({
        "id": p.id, "name": p.name, "kind": p.kind, "local": p.kind.is_local(),
        "image_model": p.image_model, "video_model": if p.makes_video() { Some(&p.video_model) } else { None },
        "has_key": !p.api_key.trim().is_empty(),
        "default_for": default_for,
    })
}

impl GenerationSettings {
    /// The provider for a request: the one named, else the default for its kind, else the first
    /// that can make it.
    pub fn provider(&self, id: Option<&str>, kind: Kind) -> Result<&Provider, EngineError> {
        let g = self;
        if g.providers.is_empty() {
            return Err(EngineError::Generate("no AI generation service is set up yet; add one in Settings → AI generation".into()));
        }
        if let Some(id) = id.map(str::trim).filter(|s| !s.is_empty()) {
            return g.providers.iter().find(|p| p.id == id).ok_or_else(|| EngineError::Generate(format!("no AI generation service with id `{id}`")));
        }
        let default = if kind == Kind::Image { &g.image_provider } else { &g.video_provider };
        let capable = |p: &&Provider| kind == Kind::Image || p.makes_video();
        g.providers
            .iter()
            .find(|p| !default.is_empty() && p.id == *default)
            .or_else(|| g.providers.iter().find(capable))
            .ok_or_else(|| EngineError::Generate("none of your AI generation services can make video; add one that can in Settings → AI generation".into()))
    }

    /// Add a service from a preset in `catalog/generation.toml`, with an id not yet taken
    /// (`comfyui`, `comfyui-2`, …). The caller saves settings.
    pub fn add_preset(&mut self, preset: &str) -> Result<&mut Provider, EngineError> {
        let preset = creativecraft_generate::presets().into_iter().find(|p| p.id == preset).ok_or_else(|| {
            let ids: Vec<String> = creativecraft_generate::presets().into_iter().map(|p| p.id).collect();
            EngineError::Generate(format!("no preset `{preset}`; choose one of {}", ids.join(", ")))
        })?;
        let mut p = preset.provider();
        let taken = |id: &str| self.providers.iter().any(|q| q.id == id);
        let mut n = 2;
        while taken(&p.id) {
            p.id = format!("{}-{n}", preset.id);
            p.name = format!("{} {n}", preset.name);
            n += 1;
        }
        let providers = &mut self.providers;
        providers.push(p);
        let last = providers.len() - 1;
        Ok(&mut providers[last])
    }
}

/// A generation ready to run; see [`GenerationJob::new`].
#[derive(Debug, Clone)]
pub struct GenerationJob {
    pub provider: Provider,
    pub request: Request,
    projects_dir: PathBuf,
}

/// What a generation saved.
#[derive(Debug, Clone)]
pub struct Generated {
    pub files: Vec<PathBuf>,
    /// "Saved Generated/… with <service>"
    pub message: String,
}

impl GenerationJob {
    /// Everything a generation needs, copied out of the settings so it can run (for minutes, for
    /// video) without holding the session.
    pub fn new(settings: &Settings, request: &Request, provider: Option<&str>) -> Result<GenerationJob, EngineError> {
        if request.prompt.trim().is_empty() {
            return Err(EngineError::Generate("describe what to make first".into()));
        }
        Ok(GenerationJob {
            provider: settings.generation.provider(provider, request.kind)?.clone(),
            request: request.clone(),
            projects_dir: settings.projects_dir.clone(),
        })
    }

    /// Make the images or video and save them under `Generated/<date>/`, each with a sidecar
    /// `<file>.json` recording how it was made.
    pub fn run(&self, events: &mut dyn FnMut(Event)) -> Result<Generated, EngineError> {
        let (p, r) = (&self.provider, &self.request);
        events(Event::Generate(format!("asking {}…", p.name)));
        let outputs = creativecraft_generate::generate(p, r, &mut |s| events(Event::Generate(format!("{}: {s}", p.name))))
            .map_err(|e| EngineError::Generate(e.to_string()))?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let (day, time, created) = stamps(now);
        let dir = self.projects_dir.join(GENERATED_DIR).join(day);
        std::fs::create_dir_all(&dir)?;
        let base = format!("{}-{time}", slug(&r.prompt));
        let mut files = Vec::new();
        for o in &outputs {
            let path = free_name(&dir, &base, &o.ext);
            write_atomic(&path, &o.bytes)?;
            let sidecar = json!({
                "prompt": r.prompt, "negative": r.negative, "kind": r.kind, "provider": p.name, "provider_id": p.id,
                "model": o.model, "aspect": r.aspect, "seconds": r.seconds, "seed": r.seed, "created": created,
            });
            write_atomic(&sidecar_path(&path), &serde_json::to_vec_pretty(&sidecar).map_err(std::io::Error::other)?)?;
            files.push(path);
        }
        let rel: Vec<String> = files.iter().map(|f| f.strip_prefix(&self.projects_dir).unwrap_or(f).display().to_string()).collect();
        Ok(Generated { message: format!("Saved {} with {}", rel.join(", "), p.name), files })
    }
}

/// `x.png` → `x.png.json`: how a generated file was made.
pub fn sidecar_path(file: &Path) -> PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(".json");
    file.with_file_name(name)
}

impl Session {
    pub fn generated_dir(&self) -> PathBuf {
        self.settings.projects_dir.join(GENERATED_DIR)
    }

    pub fn generation_provider(&self, id: Option<&str>, kind: Kind) -> Result<&Provider, EngineError> {
        self.settings.generation.provider(id, kind)
    }

    pub(crate) fn generate(&self, r: &Request, provider: Option<&str>, events: &mut dyn FnMut(Event)) -> Result<String, EngineError> {
        Ok(GenerationJob::new(&self.settings, r, provider)?.run(events)?.message)
    }

    /// Check a provider can be reached and accepts its key, without generating anything.
    pub(crate) fn test_generator(&self, id: &str) -> Result<String, EngineError> {
        let p = self.generation_provider(Some(id), Kind::Image)?;
        creativecraft_generate::check(p).map_err(|e| EngineError::Generate(e.to_string()))
    }

    pub fn add_generation_provider(&mut self, preset: &str) -> Result<&mut Provider, EngineError> {
        self.settings.generation.add_preset(preset)
    }

    /// The configured services, for agents.
    pub fn generation_providers_json(&self) -> Value {
        let g = &self.settings.generation;
        let image = self.generation_provider(None, Kind::Image).ok().map(|p| p.id.clone());
        let video = self.generation_provider(None, Kind::Video).ok().map(|p| p.id.clone());
        let list: Vec<Value> = g.providers.iter().map(|p| provider_json(p, image.as_deref() == Some(&p.id), video.as_deref() == Some(&p.id))).collect();
        json!({ "providers": list, "agents_can_generate": g.agents_can_generate, "saves_to": format!("{GENERATED_DIR}/<date>/") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_slugs() {
        assert_eq!(stamps(0), ("1970-01-01".into(), "000000".into(), "1970-01-01T00:00:00Z".into()));
        assert_eq!(stamps(1_791_504_000 + 3_723).0, "2026-10-09");
        assert_eq!(stamps(951_782_400).0, "2000-02-29");
        assert_eq!(slug("A fox, in the snow!"), "a-fox-in-the-snow");
        assert_eq!(slug("???"), "generated");
        assert!(slug(&"word ".repeat(40)).len() <= 40);
    }

    #[test]
    fn picks_named_then_default_then_capable() {
        let root = std::env::temp_dir().join(format!("cc-gen-pick-{}", std::process::id()));
        let mut s = Session::open_at(creativecraft_install::HubDirs::under(&root)).unwrap();
        assert!(s.generation_provider(None, Kind::Image).is_err());
        let preset = |id: &str| creativecraft_generate::presets().into_iter().find(|p| p.id == id).unwrap().provider();
        s.settings.generation.providers = vec![preset("automatic1111"), preset("openai")];
        assert_eq!(s.generation_provider(None, Kind::Image).unwrap().id, "automatic1111");
        assert_eq!(s.generation_provider(None, Kind::Video).unwrap().id, "openai", "A1111 can't make video");
        s.settings.generation.image_provider = "openai".into();
        assert_eq!(s.generation_provider(None, Kind::Image).unwrap().id, "openai");
        assert_eq!(s.generation_provider(Some("automatic1111"), Kind::Image).unwrap().id, "automatic1111");
        assert!(s.generation_provider(Some("nope"), Kind::Image).is_err());
        assert!(!s.generation_providers_json().to_string().contains("api_key"));
        assert_eq!(s.add_generation_provider("openai").unwrap().id, "openai-2");
        assert!(s.add_generation_provider("nope").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn saves_results_with_a_sidecar() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            let mut buf = [0u8; 65536];
            let _ = c.read(&mut buf);
            // "iVBORw0KGgo=" is the PNG signature
            let body = r#"{"images":["iVBORw0KGgo="]}"#;
            let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        });
        let root = std::env::temp_dir().join(format!("cc-gen-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut s = Session::open_at(creativecraft_install::HubDirs::under(&root)).unwrap();
        s.settings.projects_dir = root.join("projects");
        let mut p = creativecraft_generate::presets().into_iter().find(|p| p.id == "automatic1111").unwrap().provider();
        p.base_url = format!("http://127.0.0.1:{port}");
        s.settings.generation.providers = vec![p];
        let request = Request {
            kind: Kind::Image,
            prompt: "A red kite".into(),
            negative: String::new(),
            model: String::new(),
            aspect: Default::default(),
            count: 1,
            seconds: None,
            seed: Some(7),
        };
        let out = s.execute(crate::Command::Generate { request, provider: None }, &mut |_| {}).unwrap();
        assert!(out.starts_with("Saved Generated/") && out.contains("a-red-kite-"), "{out}");
        let day = std::fs::read_dir(s.generated_dir()).unwrap().next().unwrap().unwrap().path();
        let mut names: Vec<String> = std::fs::read_dir(&day).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names[0].ends_with(".png") && names[1].ends_with(".png.json"));
        let side: Value = serde_json::from_slice(&std::fs::read(day.join(&names[1])).unwrap()).unwrap();
        assert_eq!(side["prompt"], "A red kite");
        assert_eq!(side["seed"], 7);
        let _ = std::fs::remove_dir_all(&root);
    }
}
