//! Per-backend wiring: where the CLI is, how it gets the hub as an MCP server, how a headless turn
//! is invoked. Flags follow each project's published docs (checked October 2026); see
//! docs/ai-chat.md for links and for what to re-check when a CLI changes.

use crate::{AgentError, McpServer, SERVER_NAME, SYSTEM_CONTEXT, TurnRequest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    /// Anthropic's Claude Code: `claude -p … --output-format stream-json --mcp-config …`
    ClaudeCode,
    /// OpenCode: `opencode run --format json …`, MCP via a config file named in `OPENCODE_CONFIG`.
    OpenCode,
    /// Nous Research's Hermes Agent: `hermes chat -q … --format stream-json`, MCP in `~/.hermes/config.yaml`.
    Hermes,
    /// Google's Antigravity CLI: `agy -p … --output-format stream-json`, MCP in `.agents/mcp_config.json`.
    Antigravity,
    /// Any other CLI, from a command-line template.
    Custom,
}

impl Backend {
    pub const ALL: [Backend; 5] = [Backend::ClaudeCode, Backend::OpenCode, Backend::Hermes, Backend::Antigravity, Backend::Custom];

    pub fn name(self) -> &'static str {
        match self {
            Backend::ClaudeCode => "Claude Code",
            Backend::OpenCode => "OpenCode",
            Backend::Hermes => "Hermes Agent",
            Backend::Antigravity => "Antigravity CLI",
            Backend::Custom => "Custom command",
        }
    }

    pub fn default_binary(self) -> &'static str {
        match self {
            Backend::ClaudeCode => "claude",
            Backend::OpenCode => "opencode",
            Backend::Hermes => "hermes",
            Backend::Antigravity => "agy",
            Backend::Custom => "",
        }
    }

    pub fn homepage(self) -> &'static str {
        match self {
            Backend::ClaudeCode => "https://code.claude.com/docs",
            Backend::OpenCode => "https://opencode.ai/docs",
            Backend::Hermes => "https://hermes-agent.nousresearch.com/docs",
            Backend::Antigravity => "https://antigravity.google/docs",
            Backend::Custom => "",
        }
    }

    /// Whether its output is a JSON event stream (else plain text is shown as it arrives).
    pub fn streams_json(self) -> bool {
        self != Backend::Custom
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendStatus {
    Found(PathBuf),
    Missing,
    /// Found, but needs a one-time step before it can see the hub (Hermes).
    NeedsSetup(PathBuf, String),
}

impl Backend {
    /// Is the CLI installed (and set up)?
    pub fn status(self, binary: Option<&str>) -> BackendStatus {
        if self == Backend::Custom {
            return BackendStatus::Missing;
        }
        let Some(path) = find_binary(binary.unwrap_or(self.default_binary())) else { return BackendStatus::Missing };
        if self == Backend::Hermes && !hermes_configured() {
            return BackendStatus::NeedsSetup(path, "add CreativeCraft to Hermes' MCP servers".into());
        }
        BackendStatus::Found(path)
    }
}

/// A binary given as a path, or looked up on `PATH` (with `.exe`/`.cmd` on Windows, since npm
/// installs `claude.cmd`, `opencode.cmd`).
pub(crate) fn find_binary(name: &str) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    let p = Path::new(name);
    if p.components().count() > 1 {
        return p.is_file().then(|| p.to_path_buf());
    }
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).flat_map(|d| exts.iter().map(move |e| d.join(format!("{name}{e}")))).find(|c| c.is_file())
}

/// An argument that can never be mistaken for a flag.
fn as_arg(prompt: &str) -> String {
    if prompt.starts_with('-') { format!(" {prompt}") } else { prompt.to_string() }
}

/// `{"mcpServers": {"creativecraft": {"command", "args"}}}` — the shape Claude Code, Antigravity and
/// most other MCP hosts read.
fn mcp_servers_json(hub: &McpServer) -> Value {
    json!({ "mcpServers": { SERVER_NAME: { "command": hub.command, "args": hub.args } } })
}

fn write_json(path: &Path, v: &Value) -> Result<(), AgentError> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(v)?)?;
    Ok(())
}

fn first_turn_prompt(req: &TurnRequest) -> String {
    if req.session.is_none() { format!("{SYSTEM_CONTEXT}\n\n---\n\n{}", req.prompt) } else { req.prompt.to_string() }
}

/// Build the process for one turn.
pub(crate) fn command(req: &TurnRequest) -> Result<Command, AgentError> {
    let s = req.settings;
    let kind = s.backend;
    let binary = if kind == Backend::Custom {
        String::new()
    } else {
        let name = s.binary.as_deref().filter(|b| !b.trim().is_empty()).unwrap_or(kind.default_binary());
        find_binary(name).ok_or(AgentError::NotInstalled { name: kind.name(), homepage: kind.homepage() })?.display().to_string()
    };
    let model = s.model.as_deref().filter(|m| !m.trim().is_empty());

    let mut cmd = match kind {
        Backend::ClaudeCode => {
            let cfg = req.state_dir.join("claude-mcp.json");
            write_json(&cfg, &mcp_servers_json(req.hub))?;
            let mut c = Command::new(&binary);
            c.arg("-p").arg(as_arg(req.prompt));
            c.args(["--output-format", "stream-json", "--verbose"]);
            c.arg("--mcp-config").arg(&cfg).arg("--strict-mcp-config");
            c.arg("--append-system-prompt").arg(SYSTEM_CONTEXT);
            if s.auto_approve {
                c.arg("--dangerously-skip-permissions");
            } else {
                // every tool of the hub server, nothing else, runs without a prompt
                c.arg("--allowedTools").arg(format!("mcp__{SERVER_NAME}"));
            }
            if let Some(id) = req.session {
                c.arg("--resume").arg(id);
            }
            if let Some(m) = model {
                c.arg("--model").arg(m);
            }
            c
        }
        Backend::OpenCode => {
            let cfg = req.state_dir.join("opencode.json");
            let mut command = vec![req.hub.command.clone()];
            command.extend(req.hub.args.iter().cloned());
            write_json(
                &cfg,
                &json!({
                    "$schema": "https://opencode.ai/config.json",
                    "mcp": { SERVER_NAME: { "type": "local", "command": command, "enabled": true, "timeout": 30000 } },
                }),
            )?;
            let mut c = Command::new(&binary);
            c.env("OPENCODE_CONFIG", &cfg);
            c.args(["run", "--format", "json"]);
            if let Some(id) = req.session {
                c.arg("--session").arg(id);
            }
            if let Some(m) = model {
                c.arg("--model").arg(m);
            }
            if s.auto_approve {
                c.arg("--auto");
            }
            c.arg(as_arg(&first_turn_prompt(req)));
            c
        }
        Backend::Hermes => {
            if !hermes_configured() {
                return Err(AgentError::Setup(format!(
                    "Hermes Agent doesn't know about CreativeCraft yet. Click “Set up” in the chat, or add this to ~/.hermes/config.yaml:\n\n{}",
                    hermes_snippet(req.hub)
                )));
            }
            // the message goes through a file: Hermes' documented path for programmatic bodies
            let q = req.state_dir.join("hermes-query.txt");
            std::fs::write(&q, first_turn_prompt(req))?;
            let mut c = Command::new(&binary);
            c.arg("chat").arg("--query-file").arg(&q).args(["--format", "stream-json", "--quiet"]);
            if let Some(id) = req.session {
                c.arg("--resume").arg(id);
            }
            if let Some(m) = model {
                c.arg("--model").arg(m);
            }
            if s.auto_approve {
                c.arg("--yolo");
            }
            c
        }
        Backend::Antigravity => {
            // workspace-level config; merged so the user's own entries survive
            let cfg = req.workspace.join(".agents").join("mcp_config.json");
            let mut v: Value = std::fs::read(&cfg).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| json!({}));
            if !v.get("mcpServers").is_some_and(Value::is_object) {
                v["mcpServers"] = json!({});
            }
            v["mcpServers"][SERVER_NAME] = mcp_servers_json(req.hub)["mcpServers"][SERVER_NAME].clone();
            write_json(&cfg, &v)?;
            let mut c = Command::new(&binary);
            c.arg("-p").arg(as_arg(&first_turn_prompt(req)));
            c.args(["--output-format", "stream-json"]);
            if let Some(id) = req.session {
                c.arg("--conversation").arg(id);
            }
            if let Some(m) = model {
                c.arg("--model").arg(m);
            }
            if s.auto_approve {
                c.arg("--dangerously-skip-permissions");
            }
            c
        }
        Backend::Custom => {
            let words: Vec<&str> = s.custom_command.split_whitespace().collect();
            let (prog, rest) = words.split_first().ok_or(AgentError::NoCustomCommand)?;
            let cfg = req.state_dir.join("mcp.json");
            write_json(&cfg, &mcp_servers_json(req.hub))?;
            let mut c = Command::new(prog);
            let mut had_prompt = false;
            for w in rest {
                match *w {
                    "{prompt}" => {
                        had_prompt = true;
                        c.arg(first_turn_prompt(req));
                    }
                    "{mcp_config}" => {
                        c.arg(&cfg);
                    }
                    other => {
                        c.arg(other);
                    }
                }
            }
            if !had_prompt {
                c.arg(first_turn_prompt(req));
            }
            c
        }
    };
    cmd.current_dir(req.workspace);
    Ok(cmd)
}

// ─────────────────────────── Hermes: one-time setup ───────────────────────────

fn hermes_config() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".hermes").join("config.yaml"))
}

fn hermes_configured() -> bool {
    hermes_config().and_then(|p| std::fs::read_to_string(p).ok()).is_some_and(|t| yaml_has_server(&t))
}

/// Does the YAML have `creativecraft:` indented under a top-level `mcp_servers:` block?
fn yaml_has_server(text: &str) -> bool {
    let mut inside = false;
    for line in text.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented {
            inside = line.trim_end() == "mcp_servers:";
            continue;
        }
        if inside && line.trim_start().starts_with(&format!("{SERVER_NAME}:")) {
            return true;
        }
    }
    false
}

/// The YAML to add to `~/.hermes/config.yaml`.
pub fn hermes_snippet(hub: &McpServer) -> String {
    let args: Vec<String> = hub.args.iter().map(|a| format!("{a:?}")).collect();
    format!("mcp_servers:\n  {SERVER_NAME}:\n    command: {:?}\n    args: [{}]\n", hub.command, args.join(", "))
}

/// Add the hub to Hermes' config. Only edits the file when that's unambiguous (no existing
/// `mcp_servers:` block); otherwise returns the snippet for the user to paste, rather than risk
/// rewriting their YAML. A backup is written first.
pub fn setup_hermes(hub: &McpServer) -> Result<String, AgentError> {
    let path = hermes_config().ok_or_else(|| AgentError::Setup("couldn't find your home folder".into()))?;
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if yaml_has_server(&existing) {
        return Ok("Hermes Agent is already set up".into());
    }
    if existing.lines().any(|l| l.trim_end() == "mcp_servers:") {
        return Err(AgentError::Setup(format!(
            "Your Hermes config already has an mcp_servers block. Add these lines under it in {}:\n\n{}",
            path.display(),
            hermes_snippet(hub).trim_start_matches("mcp_servers:\n")
        )));
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    if !existing.is_empty() {
        std::fs::write(path.with_extension("yaml.creativecraft-backup"), &existing)?;
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("\n# Added by CreativeCraft: lets Hermes drive your creative apps.\n");
    text.push_str(&hermes_snippet(hub));
    std::fs::write(&path, text)?;
    Ok(format!("Added CreativeCraft to {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_detection() {
        assert!(yaml_has_server("model: x\nmcp_servers:\n  other:\n    command: a\n  creativecraft:\n    command: b\n"));
        assert!(!yaml_has_server("mcp_servers:\n  other:\n    command: a\nfoo:\n  creativecraft: 1\n"));
        assert!(!yaml_has_server("# mcp_servers:\n#   creativecraft:\n"));
    }

    #[test]
    fn snippet_is_valid_shape() {
        let s = hermes_snippet(&McpServer { command: "/opt/cc/creativecraft-cli".into(), args: vec!["mcp".into()] });
        assert_eq!(s, "mcp_servers:\n  creativecraft:\n    command: \"/opt/cc/creativecraft-cli\"\n    args: [\"mcp\"]\n");
        assert!(yaml_has_server(&s));
    }

    #[test]
    fn prompts_never_become_flags() {
        assert_eq!(as_arg("--help me"), " --help me");
        assert_eq!(as_arg("make a poster"), "make a poster");
    }
}
