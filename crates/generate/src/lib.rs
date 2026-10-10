//! AI image and video generation (L2).
//!
//! CreativeCraft talks to the services the user picks, with their own API key, or to generators
//! running on their own computer. Adapters:
//!
//! | kind | service | images | video |
//! |---|---|---|---|
//! | `openai` | OpenAI | GPT Image (`/images/generations`) | Sora (`/videos`) |
//! | `openai_compatible` | any OpenAI-style server: Together, LocalAI, gateways, custom | `/images/generations` | |
//! | `xai` | xAI | Grok Imagine (OpenAI-style) | `/videos/generations` |
//! | `google` | Gemini API | Gemini image models (`generateContent`) | Veo (`predictLongRunning`) |
//! | `replicate` | Replicate | any model, `owner/name` | any model |
//! | `fal` | fal | any model endpoint | any model endpoint |
//! | `comfyui` | ComfyUI (local) | built-in workflow or your own | your own workflow |
//! | `automatic1111` | AUTOMATIC1111 / Forge / SD.Next (local) | `/sdapi/v1/txt2img` | |
//!
//! [`generate`] runs one request to completion (polling where the service is asynchronous) and
//! returns the files' bytes; saving them is the caller's job. Keys are sent only to the service
//! they belong to and never logged.
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod a1111;
mod comfy;
mod fal;
mod google;
mod http;
mod openai;
mod replicate;

use serde::{Deserialize, Serialize};

const PRESETS: &str = include_str!("../../../catalog/generation.toml");

#[derive(Debug, thiserror::Error)]
pub enum GenError {
    #[error("{0}")]
    Config(String),
    #[error("{service} answered {status}: {body}")]
    Http { service: String, status: u16, body: String },
    #[error("network: {0}")]
    Net(String),
    #[error("{0}")]
    Failed(String),
    #[error("gave up waiting after {0} minutes; the job may still finish on the service")]
    Timeout(u64),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
}

/// What to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Image,
    Video,
}

/// The shape of the result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Aspect {
    #[default]
    Square,
    Landscape,
    Portrait,
}

impl Aspect {
    /// "1:1", "16:9", "9:16"
    pub fn ratio(self) -> &'static str {
        match self {
            Aspect::Square => "1:1",
            Aspect::Landscape => "16:9",
            Aspect::Portrait => "9:16",
        }
    }

    /// Pixel size for local diffusion models (SDXL-friendly buckets).
    pub fn pixels(self) -> (u32, u32) {
        match self {
            Aspect::Square => (1024, 1024),
            Aspect::Landscape => (1344, 768),
            Aspect::Portrait => (768, 1344),
        }
    }

    pub fn from_name(s: &str) -> Option<Aspect> {
        match s.trim().to_ascii_lowercase().as_str() {
            "square" | "1:1" => Some(Aspect::Square),
            "landscape" | "16:9" | "wide" => Some(Aspect::Landscape),
            "portrait" | "9:16" | "tall" => Some(Aspect::Portrait),
            _ => None,
        }
    }
}

/// One generation request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub kind: Kind,
    pub prompt: String,
    /// What to avoid, where the service supports it.
    #[serde(default)]
    pub negative: String,
    /// Overrides the provider's default model for this request.
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub aspect: Aspect,
    /// Images to make (1–4); video is always one.
    #[serde(default = "one")]
    pub count: u32,
    /// Video length, where the service lets you choose (rounded to what it offers).
    #[serde(default)]
    pub seconds: Option<u32>,
    #[serde(default)]
    pub seed: Option<u64>,
}

fn one() -> u32 {
    1
}

/// Which adapter a provider uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Openai,
    OpenaiCompatible,
    Xai,
    Google,
    Replicate,
    Fal,
    Comfyui,
    Automatic1111,
}

impl ProviderKind {
    /// Runs on the user's own computer (no key, no cost).
    pub fn is_local(self) -> bool {
        matches!(self, ProviderKind::Comfyui | ProviderKind::Automatic1111)
    }

    /// Can make video at all (with a suitable model or workflow).
    pub fn does_video(self) -> bool {
        matches!(self, ProviderKind::Openai | ProviderKind::Xai | ProviderKind::Google | ProviderKind::Replicate | ProviderKind::Fal | ProviderKind::Comfyui)
    }
}

/// A configured service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provider {
    /// Unique among the user's providers (e.g. "openai", "comfyui").
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    #[serde(default)]
    pub image_model: String,
    #[serde(default)]
    pub video_model: String,
    /// Kept out of settings.json (the engine stores it in secrets.json).
    #[serde(default, skip_serializing)]
    pub api_key: String,
    /// ComfyUI: a workflow exported with "Export (API)", with `{{prompt}}`, `{{negative}}`,
    /// `{{seed}}`, `{{width}}`, `{{height}}`, `{{count}}` placeholders.
    #[serde(default)]
    pub workflow: String,
    /// A JSON object merged into every request's model input (Replicate, fal, OpenAI-compatible,
    /// Stable Diffusion WebUI), for model-specific options.
    #[serde(default)]
    pub extra: String,
}

impl Provider {
    pub(crate) fn model(&self, r: &Request) -> Result<String, GenError> {
        let m = if !r.model.trim().is_empty() {
            r.model.trim()
        } else if r.kind == Kind::Image {
            self.image_model.trim()
        } else {
            self.video_model.trim()
        };
        if m.is_empty() && !self.kind.is_local() {
            let what = if r.kind == Kind::Image { "image" } else { "video" };
            return Err(GenError::Config(format!("{} has no {what} model set; choose one in Settings → AI generation", self.name)));
        }
        Ok(m.to_string())
    }

    /// Can make video as set up: ComfyUI only with a workflow of the user's own.
    pub fn makes_video(&self) -> bool {
        self.kind.does_video() && (self.kind != ProviderKind::Comfyui || !self.workflow.trim().is_empty())
    }

    pub(crate) fn base(&self) -> String {
        self.base_url.trim().trim_end_matches('/').to_string()
    }

    pub(crate) fn needs_key(&self) -> Result<&str, GenError> {
        let k = self.api_key.trim();
        if k.is_empty() {
            return Err(GenError::Config(format!("{} needs an API key; add it in Settings → AI generation", self.name)));
        }
        Ok(k)
    }

    /// `extra` as a JSON object (empty when unset).
    pub(crate) fn extra(&self) -> Result<serde_json::Map<String, serde_json::Value>, GenError> {
        if self.extra.trim().is_empty() {
            return Ok(serde_json::Map::new());
        }
        match serde_json::from_str(&self.extra)? {
            serde_json::Value::Object(m) => Ok(m),
            _ => Err(GenError::Config(format!("{}: extra options must be a JSON object", self.name))),
        }
    }
}

/// A generated file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub bytes: Vec<u8>,
    /// Lower-case file extension ("png", "mp4", …).
    pub ext: String,
    /// The model that made it, as the service named it.
    pub model: String,
}

/// A service offered in Settings, from `catalog/generation.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub key_url: String,
    pub image_model: String,
    pub video_model: String,
    pub note: String,
}

impl Preset {
    pub fn provider(&self) -> Provider {
        Provider {
            id: self.id.clone(),
            name: self.name.clone(),
            kind: self.kind,
            base_url: self.base_url.clone(),
            image_model: self.image_model.clone(),
            video_model: self.video_model.clone(),
            api_key: String::new(),
            workflow: String::new(),
            extra: String::new(),
        }
    }
}

#[derive(Deserialize)]
struct PresetFile {
    #[allow(dead_code)]
    version: u32,
    preset: Vec<Preset>,
}

/// The built-in list of services.
pub fn presets() -> Vec<Preset> {
    toml::from_str::<PresetFile>(PRESETS).map(|f| f.preset).unwrap_or_default()
}

/// Services sometimes quote the key back in an error ("Incorrect API key provided: sk-…"); errors
/// reach agents and logs, so the key never does.
fn redact(p: &Provider, e: GenError) -> GenError {
    let key = p.api_key.trim();
    if key.len() < 4 || !e.to_string().contains(key) {
        return e;
    }
    GenError::Failed(e.to_string().replace(key, "[your key]"))
}

/// Make what `r` asks for with provider `p`. `status` hears progress ("queued", "45%", …).
pub fn generate(p: &Provider, r: &Request, status: &mut dyn FnMut(&str)) -> Result<Vec<Output>, GenError> {
    run(p, r, status).map_err(|e| redact(p, e))
}

fn run(p: &Provider, r: &Request, status: &mut dyn FnMut(&str)) -> Result<Vec<Output>, GenError> {
    if r.prompt.trim().is_empty() {
        return Err(GenError::Config("describe what to make".into()));
    }
    let mut r = r.clone();
    r.count = if r.kind == Kind::Video { 1 } else { r.count.clamp(1, 4) };
    match (p.kind, r.kind) {
        (ProviderKind::Openai | ProviderKind::OpenaiCompatible | ProviderKind::Xai, Kind::Image) => openai::image(p, &r),
        (ProviderKind::Openai, Kind::Video) => openai::sora(p, &r, status),
        (ProviderKind::Xai, Kind::Video) => openai::xai_video(p, &r, status),
        (ProviderKind::Google, Kind::Image) => google::image(p, &r),
        (ProviderKind::Google, Kind::Video) => google::veo(p, &r, status),
        (ProviderKind::Replicate, _) => replicate::run(p, &r, status),
        (ProviderKind::Fal, _) => fal::run(p, &r, status),
        (ProviderKind::Comfyui, _) => comfy::run(p, &r, status),
        (ProviderKind::Automatic1111, Kind::Image) => a1111::txt2img(p, &r),
        (_, Kind::Video) => Err(GenError::Config(format!("{} doesn't make video", p.name))),
    }
}

/// Check that a provider is reachable and its key works, without generating anything.
pub fn check(p: &Provider) -> Result<String, GenError> {
    let result = match p.kind {
        ProviderKind::Openai | ProviderKind::OpenaiCompatible | ProviderKind::Xai => openai::check(p),
        ProviderKind::Google => google::check(p),
        ProviderKind::Replicate => replicate::check(p),
        ProviderKind::Fal => fal::check(p),
        ProviderKind::Comfyui => comfy::check(p),
        ProviderKind::Automatic1111 => a1111::check(p),
    };
    result.map_err(|e| redact(p, e))
}

/// Lower-case extension for some bytes, from their magic number ("bin" if unknown).
pub fn sniff_ext(b: &[u8]) -> &'static str {
    if b.starts_with(b"\x89PNG") {
        "png"
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        "jpg"
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        "webp"
    } else if b.starts_with(b"GIF8") {
        "gif"
    } else if b.len() > 12 && &b[4..8] == b"ftyp" {
        if &b[8..10] == b"qt" { "mov" } else { "mp4" }
    } else if b.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        "webm"
    } else {
        "bin"
    }
}

#[cfg(test)]
mod tests;
