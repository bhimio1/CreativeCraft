//! MCP for the hub (L2).
//!
//! Every Craft app ships `<app>-cli mcp`: an MCP server speaking JSON-RPC 2.0, one message per
//! line, on stdio. This crate has
//!
//! - [`Client`]: drives one such server (initialize, `tools/list`, `tools/call`);
//! - [`HubServer`]: the hub's own stdio MCP server. An agent (Claude Desktop, Claude Code, any MCP
//!   host) connects to **one** server, `atelier-cli mcp`, and gets hub tools (list / install /
//!   launch apps, sync projects) plus every running app's tools, namespaced `<app>__<tool>`. App
//!   servers start lazily, the first time the agent asks for one.
//!
//! The hub's capabilities come in through [`HubBackend`], so this crate doesn't depend on the
//! engine (layering: L2 never depends on L3).
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod client;
mod server;

pub use client::Client;
pub use server::{HubBackend, HubServer};

/// MCP protocol revisions we speak, newest first. The Craft apps implement 2025-06-18.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("the app's MCP server stopped")]
    Closed,
    #[error("the app's MCP server did not answer within {0} s")]
    Timeout(u64),
    #[error("MCP error {code}: {message}")]
    Rpc { code: i64, message: String },
}

/// Namespaced tool names: `photocraft__layer_add` ↔ (`photocraft`, `layer_add`).
/// App ids never contain `_`, so the first `__` is always the separator.
pub fn namespaced(app: &str, tool: &str) -> String {
    format!("{app}__{tool}")
}

pub fn split_namespaced(name: &str) -> Option<(&str, &str)> {
    let (app, tool) = name.split_once("__")?;
    (!app.is_empty() && !tool.is_empty() && !app.contains('_')).then_some((app, tool))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespacing() {
        assert_eq!(namespaced("photocraft", "layer_add"), "photocraft__layer_add");
        assert_eq!(split_namespaced("photocraft__layer_add"), Some(("photocraft", "layer_add")));
        assert_eq!(split_namespaced("photocraft__a__b"), Some(("photocraft", "a__b")));
        assert_eq!(split_namespaced("hub_list_apps"), None);
        assert_eq!(split_namespaced("__x"), None);
    }
}
