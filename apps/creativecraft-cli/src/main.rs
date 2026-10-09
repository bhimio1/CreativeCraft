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
//! creativecraft-cli bug [app]              report a bug in an app, or in CreativeCraft
//! creativecraft-cli chat <message>         one AI chat turn, streamed (continues the current chat)
//! creativecraft-cli chat --new              start a fresh chat
//! creativecraft-cli agent-setup             one-time setup for the chat's agent CLI (Hermes)
//! creativecraft-cli mcp                    serve MCP on stdio (point your AI assistant here)
//! ```
//!
//! In `mcp` mode stdout carries protocol messages only; logs go to stderr.
#![forbid(unsafe_code)]

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
        println!("{}", include_str!("usage.txt"));
        return Ok(());
    }
    if matches!(cmd, "version" | "-V" | "--version") {
        println!("creativecraft-cli {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let mut s = Session::open()?;
    let rest = &args[1..];
    match cmd {
        "list" if rest.iter().any(|a| a == "--json") => {
            println!("{}", serde_json::to_string_pretty(&s.apps_json()).unwrap_or_default());
        }
        "list" => {
            for a in &s.catalog.apps {
                let status = match s.status(a) {
                    Status::NotInstalled => "-".to_string(),
                    Status::Installed { version } => version,
                    Status::Detected { path } => format!("found at {}", path.display()),
                };
                println!("{:<12} {:<14} {:<12} {}", a.id, a.name, a.category, status);
            }
        }
        "install" | "update" | "remove" | "uninstall" => {
            for id in rest {
                let c = if cmd == "install" || cmd == "update" { Command::Install(id.clone()) } else { Command::Uninstall(id.clone()) };
                let msg = s.execute(c, &mut print_progress)?;
                eprintln!();
                println!("{msg}");
            }
        }
        "open" | "launch" => {
            let id = rest.first().ok_or_else(|| EngineError::UnknownApp(String::new()))?;
            let files = rest[1..].iter().map(Into::into).collect();
            println!("{}", s.execute(Command::Launch(id.clone(), files), &mut |_| {})?);
        }
        "sync" if rest.iter().any(|a| a == "--preview") => {
            let plan = s.preview_sync()?;
            if plan.is_empty() {
                println!("Everything is up to date");
            }
            for a in plan {
                println!("{a:?}");
            }
        }
        "sync" => println!("{}", s.execute(Command::Sync, &mut print_progress)?),
        "chat" if rest.iter().any(|a| a == "--new") => {
            creativecraft_engine::agent::Conversation::default().save(&s.chat_file())?;
            println!("Started a new chat");
        }
        "chat" => chat(&s, &rest.join(" "))?,
        "bug" | "report-bug" => {
            // print the address too, for terminals without a browser
            let id = rest.first().cloned();
            println!("{}", s.bug_report_url(id.as_deref())?);
            println!("{}", s.execute(Command::ReportBug(id), &mut |_| {})?);
        }
        "agent-setup" => println!("{}", s.execute(Command::SetupAgent, &mut |_| {})?),
        "login" => println!("{}", s.execute(Command::SignInGoogle, &mut |_| {})?),
        "logout" => println!("{}", s.execute(Command::SignOutGoogle, &mut |_| {})?),
        "mcp" => {
            let stdin = std::io::stdin().lock();
            let stdout = std::io::stdout().lock();
            let mut server = creativecraft_mcp::HubServer::new(s);
            server.serve(stdin, stdout).map_err(|e| EngineError::Io(std::io::Error::other(e.to_string())))?;
        }
        other => {
            eprintln!("unknown command `{other}`\n");
            println!("{}", include_str!("usage.txt"));
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
    println!();
    convo.save(&file)?;
    Ok(())
}

fn print_progress(e: Event) {
    let mut err = std::io::stderr();
    let line = match e {
        Event::Install(id, Progress::Downloading { done, total: Some(t) }) if t > 0 => {
            format!("\r{id}: {:>3}%  {:.1}/{:.1} MB", done * 100 / t, done as f64 / 1e6, t as f64 / 1e6)
        }
        Event::Install(id, Progress::Downloading { done, .. }) => format!("\r{id}: {:.1} MB", done as f64 / 1e6),
        Event::Install(id, p) => format!("\r{id}: {p:?}            "),
        Event::Sync { done, total, path } => format!("\r[{done}/{total}] {path:<60.60}"),
    };
    let _ = write!(err, "{line}");
    let _ = err.flush();
}
