//! The AI assistant (L2).
//!
//! CreativeCraft does not ship its own model client. The assistant is an **agent CLI the user
//! already has** — Claude Code, OpenCode, Hermes Agent, Antigravity CLI, or any command they
//! configure — run in the projects folder with exactly one MCP server configured: the hub itself
//! (`creativecraft-cli mcp`), which fronts every installed app's own MCP server.
//!
//! Two ways to run it:
//! - [`interactive_launch`]: the CLI's own interactive UI, which the window hosts in a terminal.
//! - [`start_turn`]: one headless turn whose streamed output is parsed into [`ChatEvent`]s
//!   (`creativecraft-cli chat`).
//!
//! ```text
//! terminal / CLI chat ─▶ backend CLI
//!                          └─ MCP ─▶ creativecraft-cli mcp ─▶ photocraft-cli mcp, vectorcraft-cli mcp, …
//! ```
//!
//! Why external backends: the user keeps their own model, account, billing and agent settings;
//! CreativeCraft stays a thin, model-agnostic host and never handles API keys.
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod backend;
mod chat;
mod parse;
mod run;

pub use backend::{Backend, BackendStatus, hermes_snippet, setup_hermes};
pub use chat::{Conversation, Message, Role};
pub use parse::{ChatEvent, Parser};
pub use run::Running;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Name of the hub server in every backend's MCP config. Claude Code exposes its tools as
/// `mcp__creativecraft__<tool>`.
pub const SERVER_NAME: &str = "creativecraft";

/// Context every backend gets: as an appended system prompt where the CLI supports one, otherwise
/// in front of the first message of a conversation.
pub const SYSTEM_CONTEXT: &str = "You are the assistant inside CreativeCraft, a hub for open-source creative apps \
(image, vector, video, photo, PDF, motion, layout, 3D and more). Drive the apps through the `creativecraft` MCP server: \
call hub_list_apps to see what is installed, hub_install_app / hub_launch_app when needed, and app_tools with an app id \
to start that app's automation and list its tools, which you then call as <app>__<tool>. \
To work on several documents at once, app_open_instance starts another independent instance of an app (<app>-2, …). \
The current directory is the user's projects folder, synced to their cloud storage; when an app tool takes a file path, \
give it relative to that folder (e.g. \"house-on-a-hill.png\"). Apps open in a window the user can see, unless they turned that off. \
Save new versions rather than overwriting the user's files, and ask before deleting anything.";

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("{name} was not found. Install it ({homepage}) or set its path in Settings → Assistant.")]
    NotInstalled { name: &'static str, homepage: &'static str },
    #[error("{0}")]
    Setup(String),
    #[error("a custom backend needs a command line; set one in Settings → Assistant")]
    NoCustomCommand,
}

/// How the assistant runs, saved in the hub's settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSettings {
    pub backend: Backend,
    /// Path or name of the CLI, if not the default (`claude`, `opencode`, `hermes`, `agy`).
    pub binary: Option<String>,
    /// Model, in the backend's own notation (`sonnet`, `anthropic/claude-sonnet-5`, …). Empty = the CLI's default.
    pub model: Option<String>,
    /// Let the agent use *all* its tools (shell, file edits, …) without asking. Off by default:
    /// then only the hub's tools are pre-approved where the backend supports that.
    pub auto_approve: bool,
    /// For `Backend::Custom`: a command line with `{prompt}` and optionally `{mcp_config}`
    /// placeholders (whole arguments), e.g. `mytool run --mcp {mcp_config} {prompt}`.
    pub custom_command: String,
}

impl Default for AgentSettings {
    fn default() -> AgentSettings {
        AgentSettings { backend: Backend::ClaudeCode, binary: None, model: None, auto_approve: false, custom_command: String::new() }
    }
}

/// The MCP server the agent should talk to (normally the hub).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    pub command: String,
    pub args: Vec<String>,
}

impl McpServer {
    /// `creativecraft-cli mcp`, preferring the CLI that sits next to the running executable.
    /// Inside Flatpak that CLI's path (`/app/bin/…`) only exists in the sandbox, while MCP hosts
    /// (and the agent CLIs the Assistant starts) run on the host, so they go through `flatpak run`.
    pub fn hub() -> McpServer {
        if let Some(id) = std::env::var("FLATPAK_ID").ok().filter(|id| !id.is_empty()) {
            return McpServer::flatpak(&id);
        }
        let exe = if cfg!(windows) { "creativecraft-cli.exe" } else { "creativecraft-cli" };
        let command = std::env::current_exe()
            .ok()
            .map(|p| p.with_file_name(exe))
            .filter(|p| p.is_file())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| exe.to_string());
        McpServer { command, args: vec!["mcp".into()] }
    }

    /// The hub of the Flatpak `app_id`, started from the host.
    fn flatpak(app_id: &str) -> McpServer {
        let args = ["run", "--command=creativecraft-cli", app_id, "mcp"];
        McpServer { command: "flatpak".into(), args: args.map(String::from).to_vec() }
    }
}

/// Everything one turn needs.
pub struct TurnRequest<'a> {
    pub settings: &'a AgentSettings,
    /// Working directory for the agent: the projects folder.
    pub workspace: &'a Path,
    /// Where per-run config files go (`<projects>/.creativecraft/agent`, never synced).
    pub state_dir: &'a Path,
    pub hub: &'a McpServer,
    pub prompt: &'a str,
    /// The backend's own session/conversation id from the previous turn, if any.
    pub session: Option<&'a str>,
}

/// Everything an interactive session needs.
pub struct SessionRequest<'a> {
    pub settings: &'a AgentSettings,
    /// Working directory for the agent: the projects folder.
    pub workspace: &'a Path,
    /// Where config files go (`<projects>/.creativecraft/agent`, never synced).
    pub state_dir: &'a Path,
    pub hub: &'a McpServer,
}

/// A program to run in a terminal: what, with which arguments and extra environment, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
}

/// How to start the chosen CLI's own interactive UI, wired to the hub (config files are written
/// here). The caller runs it in a terminal.
pub fn interactive_launch(req: &SessionRequest) -> Result<LaunchSpec, AgentError> {
    std::fs::create_dir_all(req.state_dir)?;
    backend::interactive(req)
}

/// Start one turn. Events stream from the returned [`Running`].
pub fn start_turn(req: &TurnRequest) -> Result<Running, AgentError> {
    std::fs::create_dir_all(req.state_dir)?;
    let cmd = backend::command(req)?;
    Running::spawn(cmd, Parser::new(req.settings.backend))
}

/// `<projects>/.creativecraft/agent`
pub fn state_dir(projects: &Path) -> PathBuf {
    projects.join(".creativecraft").join("agent")
}

#[cfg(test)]
mod tests {
    use super::McpServer;

    #[test]
    fn the_flatpak_hub_starts_through_flatpak_run() {
        let hub = McpServer::flatpak("org.creativecraft.CreativeCraft");
        assert_eq!(hub.command, "flatpak");
        assert_eq!(hub.args, ["run", "--command=creativecraft-cli", "org.creativecraft.CreativeCraft", "mcp"]);
    }
}
