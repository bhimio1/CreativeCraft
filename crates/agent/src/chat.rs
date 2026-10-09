use crate::{Backend, ChatEvent};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    /// A tool call or result, shown small.
    Tool,
    /// An error or notice from CreativeCraft itself.
    Notice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub text: String,
}

/// The chat as shown in the window. The model-side history lives in the backend's own session
/// (`session`), which each turn resumes; this is the readable transcript.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Conversation {
    pub backend: Option<Backend>,
    pub session: Option<String>,
    pub messages: Vec<Message>,
}

impl Conversation {
    pub fn load(path: &Path) -> Conversation {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, path)
    }

    /// The session to resume with `backend`; switching backends starts a fresh session.
    pub fn session_for(&self, backend: Backend) -> Option<&str> {
        (self.backend == Some(backend)).then_some(self.session.as_deref()).flatten()
    }

    pub fn push_user(&mut self, backend: Backend, text: &str) {
        if self.backend != Some(backend) {
            self.backend = Some(backend);
            self.session = None;
        }
        self.messages.push(Message { role: Role::User, text: text.to_string() });
    }

    /// Fold one streamed event into the transcript.
    pub fn apply(&mut self, ev: &ChatEvent) {
        match ev {
            ChatEvent::Text(t) => match self.messages.last_mut() {
                // consecutive text is one reply (streamed in pieces); a tool call in between starts a new one
                Some(m) if m.role == Role::Assistant => m.text.push_str(t),
                _ => self.messages.push(Message { role: Role::Assistant, text: t.clone() }),
            },
            ChatEvent::ToolUse { name, input } => {
                let input = if input.is_empty() || input == "{}" { String::new() } else { format!("  {input}") };
                self.messages.push(Message { role: Role::Tool, text: format!("→ {name}{input}") });
            }
            ChatEvent::ToolResult { text, is_error } => {
                let mark = if *is_error { "✗" } else { "✓" };
                let first = text.lines().next().unwrap_or("");
                self.messages.push(Message { role: Role::Tool, text: format!("{mark} {first}") });
            }
            ChatEvent::Session(id) => self.session = Some(id.clone()),
            ChatEvent::Error(e) => self.messages.push(Message { role: Role::Notice, text: e.clone() }),
            ChatEvent::Done => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_and_sessions() {
        let mut c = Conversation::default();
        c.push_user(Backend::ClaudeCode, "make a poster");
        for ev in [
            ChatEvent::Session("s1".into()),
            ChatEvent::Text("Sure.".into()),
            ChatEvent::ToolUse { name: "app_tools".into(), input: "{}".into() },
            ChatEvent::ToolResult { text: "ok\nmore".into(), is_error: false },
            ChatEvent::Text("Done".into()),
            ChatEvent::Text(" — saved.".into()),
            ChatEvent::Done,
        ] {
            c.apply(&ev);
        }
        let texts: Vec<_> = c.messages.iter().map(|m| (m.role, m.text.as_str())).collect();
        assert_eq!(
            texts,
            vec![
                (Role::User, "make a poster"),
                (Role::Assistant, "Sure."),
                (Role::Tool, "→ app_tools"),
                (Role::Tool, "✓ ok"),
                (Role::Assistant, "Done — saved.")
            ]
        );
        assert_eq!(c.session_for(Backend::ClaudeCode), Some("s1"));
        assert_eq!(c.session_for(Backend::OpenCode), None);
        c.push_user(Backend::OpenCode, "hi");
        assert_eq!(c.session, None);
    }
}
