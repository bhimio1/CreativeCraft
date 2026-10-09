use crate::{McpError, PROTOCOL_VERSIONS};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

/// An MCP client over a child process's stdio.
pub struct Client {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
    timeout: Duration,
    /// What the server reported in `initialize`.
    pub server_info: Value,
}

impl Client {
    /// Take over a spawned server (stdin and stdout must be piped) and run the MCP handshake.
    pub fn start(mut child: Child) -> Result<Client, McpError> {
        let stdin = child.stdin.take().ok_or(McpError::Closed)?;
        let stdout = child.stdout.take().ok_or(McpError::Closed)?;
        let (tx, rx) = channel();
        // a reader thread, so calls can time out instead of hanging on a stuck app
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(l) if l.trim().is_empty() => continue,
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let mut c = Client { child, stdin, lines: rx, next_id: 1, timeout: Duration::from_secs(120), server_info: Value::Null };
        let init = c.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSIONS[0],
                "capabilities": {},
                "clientInfo": { "name": "creativecraft-hub", "version": env!("CARGO_PKG_VERSION") },
            }),
        )?;
        c.server_info = init.get("serverInfo").cloned().unwrap_or(Value::Null);
        c.notify("notifications/initialized", json!({}))?;
        Ok(c)
    }

    pub fn set_timeout(&mut self, t: Duration) {
        self.timeout = t;
    }

    fn send(&mut self, msg: &Value) -> Result<(), McpError> {
        let mut line = serde_json::to_vec(msg)?;
        line.push(b'\n');
        self.stdin.write_all(&line)?;
        self.stdin.flush()?;
        Ok(())
    }

    pub fn notify(&mut self, method: &str, params: Value) -> Result<(), McpError> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    /// Send a request and wait for its response. Notifications and server-initiated requests that
    /// arrive meanwhile are logged and skipped (the Craft apps don't send any we must answer).
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        loop {
            let line = match self.lines.recv_timeout(self.timeout) {
                Ok(l) => l,
                Err(RecvTimeoutError::Timeout) => return Err(McpError::Timeout(self.timeout.as_secs())),
                Err(RecvTimeoutError::Disconnected) => return Err(McpError::Closed),
            };
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                log::debug!("non-JSON line from MCP server: {line}");
                continue;
            };
            if msg.get("id").and_then(Value::as_u64) != Some(id) || msg.get("method").is_some() {
                log::debug!("skipping MCP message: {line}");
                continue;
            }
            if let Some(err) = msg.get("error") {
                return Err(McpError::Rpc {
                    code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: err.get("message").and_then(Value::as_str).unwrap_or("").to_string(),
                });
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// All tools, following `nextCursor` pagination.
    pub fn list_tools(&mut self) -> Result<Vec<Value>, McpError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let r = self.request("tools/list", params)?;
            if let Some(t) = r.get("tools").and_then(Value::as_array) {
                tools.extend(t.iter().cloned());
            }
            match r.get("nextCursor").and_then(Value::as_str) {
                Some(c) => cursor = Some(c.to_string()),
                None => return Ok(tools),
            }
        }
    }

    /// Call a tool; returns the MCP `CallToolResult` (`content`, `isError`, …) unchanged.
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, McpError> {
        self.request("tools/call", json!({ "name": name, "arguments": arguments }))
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // closing stdin is the polite stop for stdio servers; kill if it lingers
        let _ = self.stdin.flush();
        std::thread::sleep(Duration::from_millis(50));
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
