//! Turn each backend's streamed output into chat events.
//!
//! Claude Code's `stream-json` shape is well documented and parsed exactly. OpenCode, Hermes and
//! Antigravity each emit newline-delimited JSON events with their own field names; those go
//! through a tolerant parser that looks for the common fields (`type`, text, tool name, session
//! id). Lines that aren't JSON are shown as text, so a format change degrades to a plain
//! transcript instead of a blank chat. Fixtures for each backend live in the tests below: when a
//! CLI changes its output, add a fixture first.

use crate::Backend;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEvent {
    /// Assistant text to append to the current reply.
    Text(String),
    /// The agent called a tool.
    ToolUse {
        name: String,
        input: String,
    },
    /// A tool returned.
    ToolResult {
        text: String,
        is_error: bool,
    },
    /// The backend's session / conversation id, to resume next turn.
    Session(String),
    Error(String),
    /// The turn is over.
    Done,
}

pub struct Parser {
    backend: Backend,
    saw_text: bool,
}

/// First string found at any of these (dot-separated) paths.
fn pick(v: &Value, paths: &[&str]) -> Option<String> {
    paths.iter().find_map(|p| {
        let mut cur = v;
        for k in p.split('.') {
            cur = cur.get(k)?;
        }
        match cur {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            _ => None,
        }
    })
}

fn short(v: &Value, max: usize) -> String {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    if s.chars().count() > max { format!("{}…", s.chars().take(max).collect::<String>()) } else { s }
}

/// Text of an MCP-style content value: a string, or an array of `{type:"text", text}` blocks.
fn content_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().filter_map(|i| i.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// Claude Code tool names for our server are `mcp__creativecraft__<tool>`; show `<tool>`.
fn tidy_tool_name(name: &str) -> String {
    name.strip_prefix(&format!("mcp__{}__", crate::SERVER_NAME)).unwrap_or(name).to_string()
}

impl Parser {
    pub fn new(backend: Backend) -> Parser {
        Parser { backend, saw_text: false }
    }

    pub fn line(&mut self, line: &str) -> Vec<ChatEvent> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return vec![];
        }
        let json = if self.backend.streams_json() { serde_json::from_str::<Value>(trimmed).ok() } else { None };
        let Some(v) = json.filter(Value::is_object) else {
            self.saw_text = true;
            return vec![ChatEvent::Text(format!("{line}\n"))];
        };
        let out = match self.backend {
            Backend::ClaudeCode => self.claude(&v),
            _ => self.generic(&v),
        };
        if out.iter().any(|e| matches!(e, ChatEvent::Text(_))) {
            self.saw_text = true;
        }
        out
    }

    fn claude(&mut self, v: &Value) -> Vec<ChatEvent> {
        let mut out = vec![];
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "system" => out.extend(pick(v, &["session_id"]).map(ChatEvent::Session)),
            "assistant" => {
                for block in v.pointer("/message/content").and_then(Value::as_array).into_iter().flatten() {
                    match block.get("type").and_then(Value::as_str) {
                        // whole blocks, not deltas: separate a block from text already shown
                        Some("text") => {
                            if let Some(t) = pick(block, &["text"]) {
                                let sep = if self.saw_text || out.iter().any(|e| matches!(e, ChatEvent::Text(_))) { "\n\n" } else { "" };
                                out.push(ChatEvent::Text(format!("{sep}{t}")));
                            }
                        }
                        Some("tool_use") => out.push(ChatEvent::ToolUse {
                            name: tidy_tool_name(block.get("name").and_then(Value::as_str).unwrap_or("tool")),
                            input: short(block.get("input").unwrap_or(&Value::Null), 200),
                        }),
                        _ => {}
                    }
                }
            }
            "user" => {
                for block in v.pointer("/message/content").and_then(Value::as_array).into_iter().flatten() {
                    if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                        out.push(ChatEvent::ToolResult {
                            text: short(&Value::String(content_text(block.get("content").unwrap_or(&Value::Null))), 300),
                            is_error: block.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                        });
                    }
                }
            }
            "result" => {
                out.extend(pick(v, &["session_id"]).map(ChatEvent::Session));
                if v.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
                    out.push(ChatEvent::Error(pick(v, &["result", "error"]).unwrap_or_else(|| "the agent stopped with an error".into())));
                } else if !self.saw_text
                    && let Some(r) = pick(v, &["result"])
                {
                    out.push(ChatEvent::Text(r));
                }
            }
            _ => {}
        }
        out
    }

    /// OpenCode (`--format json`), Hermes (`--format stream-json`), Antigravity (`--output-format stream-json`).
    fn generic(&mut self, v: &Value) -> Vec<ChatEvent> {
        let mut out = vec![];
        let ty = pick(v, &["type", "event", "kind"]).unwrap_or_default().to_ascii_lowercase();

        if let Some(id) = pick(v, &["session_id", "sessionID", "sessionId", "conversation_id", "conversationId", "part.sessionID", "properties.sessionID"]) {
            out.push(ChatEvent::Session(id));
        }
        if ty == "error" || v.get("error").is_some_and(|e| e.is_string() || e.is_object()) {
            let msg = pick(v, &["error.message", "error.data.message", "error", "message"]).unwrap_or_else(|| short(v, 300));
            out.push(ChatEvent::Error(msg));
            return out;
        }

        let tool_name = pick(v, &["name", "tool", "tool_name", "part.tool", "part.name", "step.tool"]);
        if ty.contains("tool") {
            let name = tool_name.map(|n| tidy_tool_name(&n)).unwrap_or_else(|| "tool".into());
            let finished = ty.contains("result") || ty.contains("output") || pick(v, &["part.state.status"]).as_deref() == Some("completed");
            if finished {
                let text = pick(v, &["output", "result", "content", "part.state.output", "text"]).unwrap_or_default();
                let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false) || pick(v, &["part.state.status"]).as_deref() == Some("error");
                out.push(ChatEvent::ToolResult { text: short(&Value::String(text), 300), is_error });
            } else {
                let input = v.get("input").or_else(|| v.get("arguments")).or_else(|| v.pointer("/part/state/input")).unwrap_or(&Value::Null);
                out.push(ChatEvent::ToolUse { name, input: short(input, 200) });
            }
            return out;
        }

        // the final envelope repeats the reply on some CLIs; only use it if nothing streamed
        if ty == "result" || ty == "done" || ty == "finish" {
            if !self.saw_text
                && let Some(r) = pick(v, &["response", "result", "text", "part.text"])
            {
                out.push(ChatEvent::Text(r));
            }
            return out;
        }

        if matches!(ty.as_str(), "text" | "assistant" | "message" | "delta" | "content" | "step_update" | "response") || ty.ends_with("text") {
            // a user echo is not part of the reply
            if pick(v, &["role", "message.role"]).as_deref() == Some("user") {
                return out;
            }
            if let Some(t) = pick(v, &["text", "delta", "content", "part.text", "message.content", "step.text", "response"]) {
                out.push(ChatEvent::Text(t));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(b: Backend, lines: &[&str]) -> Vec<ChatEvent> {
        let mut p = Parser::new(b);
        lines.iter().flat_map(|l| p.line(l)).collect()
    }

    #[test]
    fn claude_stream_json() {
        let ev = run(
            Backend::ClaudeCode,
            &[
                r#"{"type":"system","subtype":"init","session_id":"s-1","tools":[]}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Opening PhotoCraft."},{"type":"tool_use","id":"t1","name":"mcp__creativecraft__app_tools","input":{"app":"photocraft"}}]}}"#,
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"[...]"}]}]}}"#,
                r#"{"type":"result","subtype":"success","is_error":false,"result":"Opening PhotoCraft.","session_id":"s-1"}"#,
            ],
        );
        assert_eq!(
            ev,
            vec![
                ChatEvent::Session("s-1".into()),
                ChatEvent::Text("Opening PhotoCraft.".into()),
                ChatEvent::ToolUse { name: "app_tools".into(), input: r#"{"app":"photocraft"}"#.into() },
                ChatEvent::ToolResult { text: "[...]".into(), is_error: false },
                ChatEvent::Session("s-1".into()),
            ]
        );
    }

    #[test]
    fn claude_error_result() {
        let ev = run(Backend::ClaudeCode, &[r#"{"type":"result","is_error":true,"result":"Not logged in","session_id":"s"}"#]);
        assert!(ev.contains(&ChatEvent::Error("Not logged in".into())));
    }

    #[test]
    fn opencode_like_events() {
        let ev = run(
            Backend::OpenCode,
            &[
                r#"{"type":"text","sessionID":"ses_1","part":{"type":"text","text":"Done: poster saved."}}"#,
                r#"{"type":"tool_use","sessionID":"ses_1","part":{"tool":"creativecraft_hub_list_apps","state":{"status":"completed","output":"[]"}}}"#,
            ],
        );
        assert!(ev.contains(&ChatEvent::Session("ses_1".into())));
        assert!(ev.contains(&ChatEvent::Text("Done: poster saved.".into())));
        assert!(ev.iter().any(|e| matches!(e, ChatEvent::ToolResult { .. })));
    }

    #[test]
    fn antigravity_like_events() {
        let ev = run(
            Backend::Antigravity,
            &[
                r#"{"type":"init","conversation_id":"c-9"}"#,
                r#"{"type":"step_update","text":"Working on it"}"#,
                r#"{"type":"result","conversation_id":"c-9","status":"ok","response":"Working on it"}"#,
            ],
        );
        // the final response isn't duplicated when text already streamed
        assert_eq!(ev.iter().filter(|e| matches!(e, ChatEvent::Text(_))).count(), 1);
        assert!(ev.contains(&ChatEvent::Session("c-9".into())));
    }

    #[test]
    fn hermes_like_events_and_plain_text_fallback() {
        let ev = run(
            Backend::Hermes,
            &[
                r#"{"type":"system","session_id":"h1"}"#,
                r#"{"type":"tool_use","name":"creativecraft.hub_list_apps","input":{}}"#,
                r#"{"type":"tool_result","output":"ok"}"#,
                r#"{"type":"text","text":"Here you go"}"#,
                "Warning: something not JSON",
                r#"{"type":"result","exit_code":0}"#,
            ],
        );
        assert_eq!(ev[0], ChatEvent::Session("h1".into()));
        assert!(matches!(&ev[1], ChatEvent::ToolUse { name, .. } if name == "creativecraft.hub_list_apps"));
        assert!(matches!(ev[2], ChatEvent::ToolResult { .. }));
        assert!(ev.contains(&ChatEvent::Text("Here you go".into())));
        assert!(ev.contains(&ChatEvent::Text("Warning: something not JSON\n".into())));
    }

    #[test]
    fn custom_is_plain_text() {
        assert_eq!(run(Backend::Custom, &[r#"{"type":"text"}"#]), vec![ChatEvent::Text("{\"type\":\"text\"}\n".into())]);
    }
}
