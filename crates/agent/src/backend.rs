//! Per-backend wiring: where the CLI is, how it gets the hub as an MCP server, how a headless turn
//! is invoked. Flags follow each project's published docs (checked October 2026); see
//! docs/ai-chat.md for links and for what to re-check when a CLI changes.

use crate::{AgentError, LaunchSpec, McpServer, SERVER_NAME, SYSTEM_CONTEXT, SessionRequest, TurnRequest};
use creativecraft_platform::HostCommand;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

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
/// installs `claude.cmd`, `opencode.cmd`). Inside Flatpak this is the host's `PATH`.
pub(crate) fn find_binary(name: &str) -> Option<PathBuf> {
    creativecraft_platform::find_on_path(name)
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
/// Build the process for one turn, to run on the host (see `creativecraft_platform::HostCommand`).
pub(crate) fn command(req: &TurnRequest) -> Result<HostCommand, AgentError> {
    let s = req.settings;
    let kind = s.backend;
    let binary = if kind == Backend::Custom { String::new() } else { resolve_binary(s)? };
    let model = s.model.as_deref().filter(|m| !m.trim().is_empty());

    let mut cmd = match kind {
        Backend::ClaudeCode => {
            let cfg = req.state_dir.join("claude-mcp.json");
            write_json(&cfg, &mcp_servers_json(req.hub))?;
            let mut c = HostCommand::new(&binary);
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
            write_opencode_config(&cfg, req.hub)?;
            let mut c = HostCommand::new(&binary);
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
            let mut c = HostCommand::new(&binary);
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
            write_antigravity_config(req.workspace, req.hub)?;
            let mut c = HostCommand::new(&binary);
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
            let mut c = HostCommand::new(prog);
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

/// The CLI's path from the settings (or its default name on `PATH`).
fn resolve_binary(s: &crate::AgentSettings) -> Result<String, AgentError> {
    let kind = s.backend;
    let name = s.binary.as_deref().filter(|b| !b.trim().is_empty()).unwrap_or(kind.default_binary());
    Ok(find_binary(name).ok_or(AgentError::NotInstalled { name: kind.name(), homepage: kind.homepage() })?.display().to_string())
}

/// Merge the hub into `<workspace>/.agents/mcp_config.json`, keeping the user's own entries.
fn write_antigravity_config(workspace: &Path, hub: &McpServer) -> Result<(), AgentError> {
    let cfg = workspace.join(".agents").join("mcp_config.json");
    let mut v: Value = std::fs::read(&cfg).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| json!({}));
    if !v.get("mcpServers").is_some_and(Value::is_object) {
        v["mcpServers"] = json!({});
    }
    v["mcpServers"][SERVER_NAME] = mcp_servers_json(hub)["mcpServers"][SERVER_NAME].clone();
    write_json(&cfg, &v)
}

fn write_opencode_config(path: &Path, hub: &McpServer) -> Result<(), AgentError> {
    let mut command = vec![hub.command.clone()];
    command.extend(hub.args.iter().cloned());
    write_json(
        path,
        &json!({
            "$schema": "https://opencode.ai/config.json",
            "mcp": { SERVER_NAME: { "type": "local", "command": command, "enabled": true, "timeout": 30000 } },
        }),
    )
}

/// The CLI's interactive UI with the hub as its MCP server. Same wiring as a headless turn, minus
/// the prompt and the JSON output flags; the user types into the CLI directly.
pub(crate) fn interactive(req: &SessionRequest) -> Result<LaunchSpec, AgentError> {
    let s = req.settings;
    let model = s.model.as_deref().filter(|m| !m.trim().is_empty());
    let mut env = Vec::new();
    let mut args: Vec<String> = Vec::new();
    let program = match s.backend {
        Backend::ClaudeCode => {
            let cfg = req.state_dir.join("claude-mcp.json");
            write_json(&cfg, &mcp_servers_json(req.hub))?;
            args.extend(["--mcp-config".into(), cfg.display().to_string(), "--append-system-prompt".into(), SYSTEM_CONTEXT.into()]);
            if s.auto_approve {
                args.push("--dangerously-skip-permissions".into());
            } else {
                args.extend(["--allowedTools".into(), format!("mcp__{SERVER_NAME}")]);
            }
            if let Some(m) = model {
                args.extend(["--model".into(), m.into()]);
            }
            resolve_binary(s)?
        }
        Backend::OpenCode => {
            let cfg = req.state_dir.join("opencode.json");
            write_opencode_config(&cfg, req.hub)?;
            env.push(("OPENCODE_CONFIG".into(), cfg.display().to_string()));
            if let Some(m) = model {
                args.extend(["--model".into(), m.into()]);
            }
            resolve_binary(s)?
        }
        Backend::Hermes => {
            if !hermes_configured() {
                return Err(AgentError::Setup(format!(
                    "Hermes Agent doesn't know about CreativeCraft yet. Click “Set up”, or add this to ~/.hermes/config.yaml:\n\n{}",
                    hermes_snippet(req.hub)
                )));
            }
            args.push("chat".into());
            if let Some(m) = model {
                args.extend(["--model".into(), m.into()]);
            }
            if s.auto_approve {
                args.push("--yolo".into());
            }
            resolve_binary(s)?
        }
        Backend::Antigravity => {
            write_antigravity_config(req.workspace, req.hub)?;
            if let Some(m) = model {
                args.extend(["--model".into(), m.into()]);
            }
            if s.auto_approve {
                args.push("--dangerously-skip-permissions".into());
            }
            resolve_binary(s)?
        }
        Backend::Custom => {
            let words: Vec<&str> = s.custom_command.split_whitespace().collect();
            let (prog, rest) = words.split_first().ok_or(AgentError::NoCustomCommand)?;
            let cfg = req.state_dir.join("mcp.json");
            write_json(&cfg, &mcp_servers_json(req.hub))?;
            for w in rest {
                match *w {
                    "{prompt}" => {} // interactive: the user types the prompt
                    "{mcp_config}" => args.push(cfg.display().to_string()),
                    other => args.push(other.into()),
                }
            }
            prog.to_string()
        }
    };
    Ok(LaunchSpec { program, args, env, cwd: req.workspace.to_path_buf() })
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

    fn session<'a>(settings: &'a crate::AgentSettings, dir: &'a Path, hub: &'a McpServer) -> SessionRequest<'a> {
        SessionRequest { settings, workspace: dir, state_dir: dir, hub }
    }

    #[test]
    fn custom_interactive_drops_prompt_and_fills_config() {
        let dir = std::env::temp_dir().join(format!("cc-agent-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hub = McpServer { command: "cc".into(), args: vec!["mcp".into()] };
        let settings = crate::AgentSettings { backend: Backend::Custom, custom_command: "mytool --mcp {mcp_config} {prompt} -i".into(), ..Default::default() };
        let spec = interactive(&session(&settings, &dir, &hub)).unwrap();
        assert_eq!(spec.program, "mytool");
        assert_eq!(spec.args, vec!["--mcp".to_string(), dir.join("mcp.json").display().to_string(), "-i".into()]);
        assert_eq!(spec.cwd, dir);
        let cfg: Value = serde_json::from_slice(&std::fs::read(dir.join("mcp.json")).unwrap()).unwrap();
        assert_eq!(cfg["mcpServers"][SERVER_NAME]["command"], "cc");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_cli_is_reported() {
        let dir = std::env::temp_dir();
        let hub = McpServer { command: "cc".into(), args: vec![] };
        let settings = crate::AgentSettings { binary: Some("/nonexistent/claude".into()), ..Default::default() };
        assert!(matches!(interactive(&session(&settings, &dir, &hub)), Err(AgentError::NotInstalled { .. })));
    }

    #[test]
    fn prompts_never_become_flags() {
        assert_eq!(as_arg("--help me"), " --help me");
        assert_eq!(as_arg("make a poster"), "make a poster");
    }
}
