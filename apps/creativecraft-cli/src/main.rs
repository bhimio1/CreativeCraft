//! `creativecraft-cli`: the hub without a window.
//!
//! ```text
//! creativecraft-cli list [--json]          every app and its status
//! creativecraft-cli install <app>…         install or update
//! creativecraft-cli remove <app>…
//! creativecraft-cli open <app> [files…]
//! creativecraft-cli sync                   one cloud sync pass
//! creativecraft-cli sync --preview         what a sync would do, changing nothing
//! creativecraft-cli login | logout         Google Drive sign-in
//! creativecraft-cli projects [--json]     recent files in the projects folder
//! creativecraft-cli plugins [install|remove|enable|disable <id> | refresh]   Marketplace plugins
//! creativecraft-cli stock search <words…> [--kind photo|illustration|artwork|video|audio]
//! creativecraft-cli stock download <source> <id> [size label]   into <projects>/Stock
//! creativecraft-cli stock list [words…]                         what's downloaded (offline)
//! creativecraft-cli generate image|video <prompt…> [--provider id] [--model m] [--aspect a] [--count n] [--seconds n] [--image file] [--strength n]
//! creativecraft-cli generate providers | presets | add <preset> | key <id> | remove <id> | test <id> | default image|video <id>
//! creativecraft-cli bug [app]              report a bug in an app, or in CreativeCraft
//! creativecraft-cli chat <message>         one AI chat turn, streamed (continues the current chat)
//! creativecraft-cli chat --new              start a fresh chat
//! creativecraft-cli agent-setup             one-time setup for the chat's agent CLI (Hermes)
//! creativecraft-cli mcp                    serve MCP on stdio (point your AI assistant here)
//! ```
//!
//! In `mcp` mode stdout carries protocol messages only; logs go to stderr.
#![forbid(unsafe_code)]

/// `println!` that ends the program quietly when stdout has gone away (`creativecraft-cli list | head`)
/// instead of panicking.
macro_rules! out {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        if writeln!(std::io::stdout(), $($t)*).is_err() {
            std::process::exit(0);
        }
    }};
}

use creativecraft_engine::{Command, EngineError, Event, Session};
use creativecraft_install::{Progress, Status};
use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).target(env_logger::Target::Stderr).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("creativecraft: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), EngineError> {
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    if matches!(cmd, "help" | "-h" | "--help") {
        out!("{}", include_str!("usage.txt"));
        return Ok(());
    }
    if matches!(cmd, "version" | "-V" | "--version") {
        out!("creativecraft-cli {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let mut s = Session::open()?;
    let rest = &args[1..];
    match cmd {
        "list" if rest.iter().any(|a| a == "--json") => {
            out!("{}", serde_json::to_string_pretty(&s.apps_json()).unwrap_or_default());
        }
        "list" => {
            for a in &s.catalog.apps {
                let status = match s.status(a) {
                    Status::NotInstalled => "-".to_string(),
                    Status::Installed { version } => version,
                    Status::Detected { path } => format!("found at {}", path.display()),
                };
                out!("{:<12} {:<14} {:<12} {}", a.id, a.name, a.category, status);
            }
        }
        "install" | "update" if rest.iter().any(|a| matches!(a.as_str(), "--fork" | "--git" | "--release")) => {
            // choose (and remember) where this app installs from, then install it
            let id = rest.first().filter(|a| !a.starts_with("--")).ok_or_else(|| EngineError::UnknownApp(String::new()))?.clone();
            let value = |flag: &str| rest.iter().position(|a| a == flag).and_then(|i| rest.get(i + 1)).filter(|v| !v.starts_with("--")).cloned();
            let source = if rest.iter().any(|a| a == "--release") {
                creativecraft_engine::Source::Release
            } else if rest.iter().any(|a| a == "--fork") {
                creativecraft_engine::Source::Fork { repo: value("--fork").unwrap_or_default() }
            } else {
                creativecraft_engine::Source::Git { url: value("--git").unwrap_or_default(), branch: value("--branch").unwrap_or_default() }
            };
            source.validate().map_err(EngineError::from)?;
            s.app(&id)?;
            // install with the new source; remember it only once that worked
            let previous = s.settings.app_sources.clone();
            if source == creativecraft_engine::Source::Release {
                s.settings.app_sources.remove(&id);
            } else {
                s.settings.app_sources.insert(id.clone(), source);
            }
            let result = s.execute(Command::Install(id), &mut print_progress);
            eprintln!();
            match result {
                Ok(msg) => {
                    s.save_settings()?;
                    out!("{msg}");
                }
                Err(e) => {
                    s.settings.app_sources = previous;
                    return Err(e);
                }
            }
        }
        "install" | "update" | "remove" | "uninstall" => {
            for id in rest {
                let c = if cmd == "install" || cmd == "update" { Command::Install(id.clone()) } else { Command::Uninstall(id.clone()) };
                let msg = s.execute(c, &mut print_progress)?;
                eprintln!();
                out!("{msg}");
            }
        }
        "open" | "launch" => {
            let id = rest.first().ok_or_else(|| EngineError::UnknownApp(String::new()))?;
            let files = rest[1..].iter().map(Into::into).collect();
            out!("{}", s.execute(Command::Launch(id.clone(), files), &mut |_| {})?);
        }
        "sync" if rest.iter().any(|a| a == "--test") => out!("{}", s.execute(Command::TestCloud, &mut |_| {})?),
        "sync" if rest.iter().any(|a| a == "--preview") => {
            let plan = s.preview_sync()?;
            if plan.is_empty() {
                out!("Everything is up to date");
            }
            for a in plan {
                out!("{a:?}");
            }
        }
        "sync" => out!("{}", s.execute(Command::Sync, &mut print_progress)?),
        "chat" if rest.iter().any(|a| a == "--new") => {
            creativecraft_engine::agent::Conversation::default().save(&s.chat_file())?;
            out!("Started a new chat");
        }
        "chat" => chat(&s, &rest.join(" "))?,
        "projects" => {
            let files = creativecraft_engine::projects::scan(&s.settings.projects_dir, &s.catalog, 100);
            if rest.iter().any(|a| a == "--json") {
                let v: Vec<_> = files
                    .iter()
                    .map(|f| {
                        let modified = f.modified.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                        serde_json::json!({ "path": f.path, "app": f.app, "modified": modified, "size": f.size })
                    })
                    .collect();
                out!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            } else {
                for f in &files {
                    out!("{:<12} {}", f.app.as_deref().unwrap_or("-"), f.path.display());
                }
            }
        }
        "plugins" | "plugin" => {
            let id = || rest.get(1).cloned().ok_or_else(|| EngineError::Plugin("which plugin? (see `creativecraft-cli plugins`)".into()));
            match rest.first().map(String::as_str) {
                None | Some("list") => {
                    for p in &s.marketplace.plugins {
                        let state = match s.settings.plugins.get(&p.id) {
                            Some(st) if st.enabled => "installed".to_string(),
                            Some(_) => "turned off".to_string(),
                            None => match Session::missing_requirements(p).first() {
                                Some((name, _)) => format!("needs {name}"),
                                None => "-".to_string(),
                            },
                        };
                        out!("{:<14} {:<16} {:<12} {:<12} {}", p.id, p.name, p.category, state, p.homepage);
                    }
                }
                Some("install") => {
                    let p = s.plugin(&id()?)?;
                    for step in &p.setup {
                        eprintln!("{}: runs `{} {}`", step.label, step.command, step.args.join(" "));
                    }
                    out!("{}", s.execute(Command::InstallPlugin(p.id), &mut |_| {})?);
                }
                Some("remove") => out!("{}", s.execute(Command::RemovePlugin(id()?), &mut |_| {})?),
                Some("enable") => out!("{}", s.execute(Command::SetPluginEnabled(id()?, true), &mut |_| {})?),
                Some("disable") => out!("{}", s.execute(Command::SetPluginEnabled(id()?, false), &mut |_| {})?),
                Some("refresh") => out!("{}", s.execute(Command::RefreshMarketplace, &mut |_| {})?),
                Some(other) => return Err(EngineError::Plugin(format!("unknown plugins command `{other}`"))),
            }
        }
        "stock" => {
            let words = |from: usize| rest.iter().skip(from).take_while(|a| !a.starts_with("--")).cloned().collect::<Vec<_>>().join(" ");
            match rest.first().map(String::as_str) {
                Some("search") => {
                    let kind = rest.iter().position(|a| a == "--kind").and_then(|i| rest.get(i + 1)).map(String::as_str);
                    let v = s.stock_search_json(&words(1), kind, 1)?;
                    for a in v["results"].as_array().into_iter().flatten() {
                        let size = match (a["width"].as_u64(), a["height"].as_u64()) {
                            (Some(w), Some(h)) => format!("{w}×{h}"),
                            _ => String::new(),
                        };
                        out!(
                            "{:<10} {:<14} {:<12} {:<11} {}",
                            a["source"].as_str().unwrap_or(""),
                            a["id"].as_str().unwrap_or(""),
                            a["kind"].as_str().unwrap_or(""),
                            size,
                            a["title"].as_str().unwrap_or("")
                        );
                    }
                    for f in v["unavailable_sources"].as_array().into_iter().flatten() {
                        eprintln!("({} didn't answer: {})", f["source"].as_str().unwrap_or(""), f["message"].as_str().unwrap_or(""));
                    }
                }
                Some("download") => {
                    let (Some(source), Some(id)) = (rest.get(1), rest.get(2)) else {
                        return Err(EngineError::Plugin("usage: creativecraft-cli stock download <source> <id> [size label]".into()));
                    };
                    let size = rest.get(3).cloned();
                    out!("{}", s.execute(Command::StockDownload { source: source.clone(), id: id.clone(), size, max_width: None }, &mut |_| {})?);
                }
                Some("list") => {
                    let lib = s.stock()?;
                    for item in lib.local(&words(1)) {
                        for f in &item.files {
                            out!("{:<10} {:<14} {:<10} {}", item.asset.source, item.asset.id, f.label, item.dir.clone() + "/" + &f.name);
                        }
                    }
                }
                _ => return Err(EngineError::Plugin("usage: creativecraft-cli stock search|download|list …".into())),
            }
        }
        "generate" | "gen" => generate(&mut s, rest)?,
        "bug" | "report-bug" => {
            // print the address too, for terminals without a browser
            let id = rest.first().cloned();
            out!("{}", s.bug_report_url(id.as_deref())?);
            out!("{}", s.execute(Command::ReportBug(id), &mut |_| {})?);
        }
        "agent-setup" => out!("{}", s.execute(Command::SetupAgent, &mut |_| {})?),
        "login" => out!("{}", s.execute(Command::SignInGoogle, &mut |_| {})?),
        "logout" => out!("{}", s.execute(Command::SignOutGoogle, &mut |_| {})?),
        "mcp" => {
            let stdin = std::io::stdin().lock();
            let stdout = std::io::stdout().lock();
            let mut server = creativecraft_mcp::HubServer::new(s);
            server.serve(stdin, stdout).map_err(|e| EngineError::Io(std::io::Error::other(e.to_string())))?;
        }
        other => {
            eprintln!("unknown command `{other}`\n");
            out!("{}", include_str!("usage.txt"));
        }
    }
    Ok(())
}

/// One chat turn: text to stdout as it streams, tool activity and errors to stderr.
fn chat(s: &Session, message: &str) -> Result<(), EngineError> {
    use creativecraft_engine::agent::{ChatEvent, Conversation};
    if message.trim().is_empty() {
        eprintln!("usage: creativecraft-cli chat <message>");
        return Ok(());
    }
    let file = s.chat_file();
    let mut convo = Conversation::load(&file);
    let backend = s.settings.agent.backend;
    convo.push_user(backend, message);
    let session = convo.session_for(backend).map(str::to_string);
    let run = s.start_chat_turn(message, session.as_deref())?;
    let mut out = std::io::stdout();
    for ev in run.events.iter() {
        match &ev {
            ChatEvent::Text(t) => {
                let _ = write!(out, "{t}");
                let _ = out.flush();
            }
            ChatEvent::ToolUse { name, .. } => eprintln!("\n  → {name}"),
            ChatEvent::ToolResult { is_error: true, text } => eprintln!("  ✗ {text}"),
            ChatEvent::Error(e) => eprintln!("\n{}: {e}", backend.name()),
            _ => {}
        }
        convo.apply(&ev);
        if ev == ChatEvent::Done {
            break;
        }
    }
    out!();
    convo.save(&file)?;
    Ok(())
}

/// `creativecraft-cli generate …`: make images and videos, and manage the services that make them.
fn generate(s: &mut Session, rest: &[String]) -> Result<(), EngineError> {
    use creativecraft_engine::generation::{Aspect, Kind, Request};
    let bad = |m: &str| EngineError::Generate(m.into());
    let value = |flag: &str| rest.iter().position(|a| a == flag).and_then(|i| rest.get(i + 1)).cloned();
    let number = |flag: &str| -> Result<Option<u64>, EngineError> {
        value(flag).map(|v| v.parse::<u64>().map_err(|_| bad(&format!("{flag} needs a number")))).transpose()
    };
    let arg = |i: usize, what: &str| {
        rest.get(i).cloned().ok_or_else(|| bad(&format!("usage: creativecraft-cli generate {} <{what}>", rest.first().map_or("", String::as_str))))
    };
    match rest.first().map(String::as_str) {
        Some(kind @ ("image" | "video")) => {
            // the prompt is every word that isn't a flag or a flag's value
            let mut words = Vec::new();
            let mut i = 1;
            while i < rest.len() {
                if rest[i].starts_with("--") {
                    i += 2;
                } else {
                    words.push(rest[i].as_str());
                    i += 1;
                }
            }
            let aspect = match value("--aspect") {
                Some(a) => Aspect::from_name(&a).ok_or_else(|| bad("--aspect is square, landscape or portrait"))?,
                None if kind == "video" => Aspect::Landscape,
                None => Aspect::Square,
            };
            // --image may be given several times
            let images: Vec<std::path::PathBuf> = rest.windows(2).filter(|w| w[0] == "--image").map(|w| std::path::PathBuf::from(&w[1])).collect();
            let request = Request {
                kind: if kind == "image" { Kind::Image } else { Kind::Video },
                prompt: words.join(" "),
                negative: value("--negative").unwrap_or_default(),
                model: value("--model").unwrap_or_default(),
                aspect,
                count: number("--count")?.map_or(1, |n| n.clamp(1, 4) as u32),
                seconds: number("--seconds")?.map(|n| n.min(60) as u32),
                seed: number("--seed")?,
                images,
                strength: number("--strength")?.map(|n| n.clamp(1, 100) as u8),
                mask: value("--mask").map(std::path::PathBuf::from),
            };
            // Ctrl-C: stop the job (on services that allow it, so it stops billing), then a second
            // one quits at once
            let stop = s.generation_stop.clone();
            let _ = ctrlc::set_handler(move || {
                if stop.is_cancelled() {
                    std::process::exit(130);
                }
                eprintln!("\nstopping… (Ctrl-C again to quit now)");
                stop.cancel();
            });
            let result = s.execute(Command::Generate { request, provider: value("--provider") }, &mut print_progress);
            eprintln!();
            out!("{}", result?);
        }
        Some("providers") | None => {
            let g = &s.settings.generation;
            if g.providers.is_empty() {
                out!("No AI generation services yet. See `creativecraft-cli generate presets`, then `generate add <preset>`.");
            }
            let v = s.generation_providers_json();
            for p in v["providers"].as_array().into_iter().flatten() {
                let defaults: Vec<&str> = p["default_for"].as_array().into_iter().flatten().filter_map(|d| d.as_str()).collect();
                let key = if p["local"] == true {
                    "local"
                } else if p["has_key"] == true {
                    "key set"
                } else {
                    "no key"
                };
                out!(
                    "{:<16} {:<24} {:<8} image: {:<28} video: {:<24} {}",
                    p["id"].as_str().unwrap_or(""),
                    p["name"].as_str().unwrap_or(""),
                    key,
                    p["image_model"].as_str().unwrap_or("-"),
                    p["video_model"].as_str().unwrap_or("-"),
                    if defaults.is_empty() { String::new() } else { format!("(default for {})", defaults.join(", ")) }
                );
            }
        }
        Some("presets") => {
            for p in creativecraft_engine::generation::presets() {
                out!("{:<14} {:<26} {}", p.id, p.name, p.note);
            }
        }
        Some("add") => {
            let preset = arg(1, "preset")?;
            let p = s.add_generation_provider(&preset)?;
            if let Some(u) = value("--url") {
                p.base_url = u;
            }
            if let Some(m) = value("--image-model") {
                p.image_model = m;
            }
            if let Some(m) = value("--video-model") {
                p.video_model = m;
            }
            let (id, local) = (p.id.clone(), p.kind.is_local());
            s.save_settings()?;
            out!(
                "Added {id}.{}",
                if local {
                    ""
                } else {
                    " Set its key with `creativecraft-cli generate key <id>` (read from standard input, so it stays out of your shell history)."
                }
            );
        }
        Some("key") => {
            let id = arg(1, "id")?;
            if !s.settings.generation.providers.iter().any(|p| p.id == id) {
                return Err(bad(&format!("no AI generation service with id `{id}`")));
            }
            eprint!("Paste the API key for {id} and press Enter (empty removes it): ");
            let mut key = String::new();
            std::io::stdin().read_line(&mut key)?;
            if let Some(p) = s.settings.generation.providers.iter_mut().find(|p| p.id == id) {
                p.api_key = key.trim().to_string();
            }
            s.save_settings()?;
            out!("{}", if key.trim().is_empty() { "Key removed" } else { "Key saved (in secrets.json, readable only by you)" });
        }
        Some("remove") => {
            let id = arg(1, "id")?;
            let before = s.settings.generation.providers.len();
            s.settings.generation.providers.retain(|p| p.id != id);
            if s.settings.generation.providers.len() == before {
                return Err(bad(&format!("no AI generation service with id `{id}`")));
            }
            s.save_settings()?;
            out!("Removed {id} and its key");
        }
        Some("default") => {
            let (kind, id) = (arg(1, "image|video")?, arg(2, "id")?);
            s.generation_provider(Some(&id), Kind::Image)?;
            match kind.as_str() {
                "image" => s.settings.generation.image_provider = id.clone(),
                "video" => s.settings.generation.video_provider = id.clone(),
                _ => return Err(bad("usage: creativecraft-cli generate default image|video <id>")),
            }
            s.save_settings()?;
            out!("{id} is now the default for {kind}");
        }
        Some("test") => out!("{}", s.execute(Command::TestGenerator(arg(1, "id")?), &mut |_| {})?),
        Some(other) => return Err(bad(&format!("unknown generate command `{other}`; see creativecraft-cli help"))),
    }
    Ok(())
}

fn print_progress(e: Event) {
    let mut err = std::io::stderr();
    let line = match e {
        Event::Install(id, Progress::Downloading { done, total: Some(t) }) if t > 0 => {
            format!("\r{id}: {:>3}%  {:.1}/{:.1} MB", done * 100 / t, done as f64 / 1e6, t as f64 / 1e6)
        }
        Event::Install(id, Progress::Downloading { done, .. }) => format!("\r{id}: {:.1} MB", done as f64 / 1e6),
        Event::Install(id, Progress::Building { crates, current }) => format!("\r{id}: compiling ({crates} crates) {current:<40.40}"),
        // git and cargo output is summarised above; a failure reports its last lines
        Event::Install(_, Progress::Log(_)) => return,
        Event::Install(id, p) => format!("\r{id}: {p:?}{:<40}", ""),
        Event::Sync { done, total, path } => format!("\r[{done}/{total}] {path:<60.60}"),
        Event::Generate(status) => format!("\r{status:<70.70}"),
    };
    let _ = write!(err, "{line}");
    let _ = err.flush();
}
