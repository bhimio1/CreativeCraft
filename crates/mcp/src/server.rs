use crate::{Client, McpError, PROTOCOL_VERSIONS, namespaced, split_namespaced};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::process::Child;

/// What the hub can do, provided by the engine.
pub trait HubBackend {
    /// Every catalog app with its status: `[{id, name, category, status, version?, mcp}]`.
    fn list_apps(&mut self) -> Value;
    /// Install or update; returns a one-line result for the agent.
    fn install(&mut self, id: &str) -> Result<String, String>;
    fn launch(&mut self, id: &str, files: &[String]) -> Result<String, String>;
    /// Run a cloud sync pass; returns its summary.
    fn sync(&mut self) -> Result<String, String>;
    /// Start `id`'s MCP server process (stdio piped).
    fn spawn_mcp(&mut self, id: &str) -> Result<Child, String>;
}

/// The hub's MCP server (stdio). See the crate docs.
pub struct HubServer<B: HubBackend> {
    backend: B,
    /// Running app servers and their (un-namespaced) tool lists.
    apps: BTreeMap<String, (Client, Vec<Value>)>,
    /// Notifications to send after the current response.
    pending: Vec<Value>,
}

fn text_result(text: impl Into<String>, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }], "isError": is_error })
}

fn hub_tools() -> Vec<Value> {
    let id_arg = json!({ "type": "object", "properties": { "app": { "type": "string", "description": "App id from hub_list_apps, e.g. \"photocraft\"" } }, "required": ["app"] });
    vec![
        json!({ "name": "hub_list_apps", "description": "List every app the hub knows: id, name, category, install status and whether it can be driven over MCP.", "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "hub_install_app", "description": "Install an app, or update it to its latest release.", "inputSchema": id_arg }),
        json!({ "name": "hub_launch_app", "description": "Open an app's window for the user, optionally with files.", "inputSchema": { "type": "object", "properties": { "app": { "type": "string" }, "files": { "type": "array", "items": { "type": "string" } } }, "required": ["app"] } }),
        json!({ "name": "hub_sync_projects", "description": "Sync the user's projects folder with their cloud storage now. Never overwrites: conflicting edits are kept side by side.", "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "app_tools", "description": "Start an installed app's automation server and list its tools. Afterwards they are also callable directly as `<app>__<tool>`.", "inputSchema": id_arg }),
        json!({ "name": "app_call", "description": "Call one of an app's tools (see app_tools).", "inputSchema": { "type": "object", "properties": { "app": { "type": "string" }, "tool": { "type": "string" }, "arguments": { "type": "object" } }, "required": ["app", "tool"] } }),
    ]
}

impl<B: HubBackend> HubServer<B> {
    pub fn new(backend: B) -> HubServer<B> {
        HubServer { backend, apps: BTreeMap::new(), pending: Vec::new() }
    }

    pub fn backend(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Serve until stdin closes.
    pub fn serve(&mut self, input: impl BufRead, mut output: impl Write) -> Result<(), McpError> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let reply = match serde_json::from_str::<Value>(&line) {
                Ok(msg) => self.handle(&msg),
                Err(e) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": e.to_string() } })),
            };
            for out in reply.into_iter().chain(self.take_notifications()) {
                serde_json::to_writer(&mut output, &out)?;
                output.write_all(b"\n")?;
                output.flush()?;
            }
        }
        Ok(())
    }

    fn take_notifications(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.pending)
    }

    /// One JSON-RPC message in, at most one response out (notifications get none).
    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        let method = msg.get("method").and_then(Value::as_str)?;
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let result: Result<Value, (i64, String)> = match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
                let v = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(PROTOCOL_VERSIONS[0]);
                Ok(json!({
                    "protocolVersion": v,
                    "capabilities": { "tools": { "listChanged": true } },
                    "serverInfo": { "name": "atelier-hub", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "Atelier is a hub for creative apps. Call hub_list_apps first. To work inside an app, call app_tools with its id; its tools then appear as <app>__<tool>. Files are the user's: prefer saving new versions over overwriting.",
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": self.all_tools() })),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(self.call(&name, args))
            }
            m if m.starts_with("notifications/") => return None,
            m => Err((-32601, format!("method not found: {m}"))),
        };
        let id = id?; // a request without an id is a notification: no reply
        Some(match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
        })
    }

    fn all_tools(&self) -> Vec<Value> {
        let mut tools = hub_tools();
        for (app, (_, app_tools)) in &self.apps {
            for t in app_tools {
                let mut t = t.clone();
                let name = t.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                t["name"] = json!(namespaced(app, &name));
                let desc = t.get("description").and_then(Value::as_str).unwrap_or("").to_string();
                t["description"] = json!(format!("[{app}] {desc}"));
                tools.push(t);
            }
        }
        tools
    }

    fn ensure_app(&mut self, app: &str) -> Result<&mut (Client, Vec<Value>), String> {
        let alive = self.apps.get_mut(app).is_some_and(|(c, _)| c.is_running());
        if !alive {
            self.apps.remove(app);
            let child = self.backend.spawn_mcp(app)?;
            let mut client = Client::start(child).map_err(|e| format!("{app}: {e}"))?;
            let tools = client.list_tools().map_err(|e| format!("{app}: {e}"))?;
            self.apps.insert(app.to_string(), (client, tools));
            self.pending.push(json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" }));
        }
        self.apps.get_mut(app).ok_or_else(|| format!("{app} is not running"))
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let done = |r: Result<String, String>| match r {
            Ok(t) => text_result(t, false),
            Err(e) => text_result(e, true),
        };
        match name {
            "hub_list_apps" => text_result(self.backend.list_apps().to_string(), false),
            "hub_install_app" => done(self.backend.install(&s("app"))),
            "hub_launch_app" => {
                let files: Vec<String> =
                    args.get("files").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
                done(self.backend.launch(&s("app"), &files))
            }
            "hub_sync_projects" => done(self.backend.sync()),
            "app_tools" => {
                let app = s("app");
                match self.ensure_app(&app) {
                    Ok((_, tools)) => {
                        let names: Vec<Value> = tools.iter().map(|t| json!({ "name": namespaced(&app, t.get("name").and_then(Value::as_str).unwrap_or("")), "description": t.get("description").cloned().unwrap_or(Value::Null) })).collect();
                        text_result(Value::Array(names).to_string(), false)
                    }
                    Err(e) => text_result(e, true),
                }
            }
            "app_call" => {
                let inner = args.get("arguments").cloned().unwrap_or(json!({}));
                self.forward(&s("app"), &s("tool"), inner)
            }
            other => match split_namespaced(other) {
                Some((app, tool)) => {
                    let (app, tool) = (app.to_string(), tool.to_string());
                    self.forward(&app, &tool, args)
                }
                None => text_result(format!("unknown tool {other}"), true),
            },
        }
    }

    fn forward(&mut self, app: &str, tool: &str, args: Value) -> Value {
        match self.ensure_app(app) {
            Ok((client, _)) => match client.call_tool(tool, args) {
                Ok(r) => r,
                Err(e) => text_result(format!("{app}: {e}"), true),
            },
            Err(e) => text_result(e, true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;
    impl HubBackend for Fake {
        fn list_apps(&mut self) -> Value {
            json!([{ "id": "photocraft", "status": "installed", "mcp": true }])
        }
        fn install(&mut self, id: &str) -> Result<String, String> {
            Ok(format!("installed {id}"))
        }
        fn launch(&mut self, id: &str, _files: &[String]) -> Result<String, String> {
            Err(format!("{id} is not installed"))
        }
        fn sync(&mut self) -> Result<String, String> {
            Ok("Everything is up to date".into())
        }
        fn spawn_mcp(&mut self, id: &str) -> Result<Child, String> {
            Err(format!("{id} has no MCP server"))
        }
    }

    fn rpc(s: &mut HubServer<Fake>, id: u64, method: &str, params: Value) -> Value {
        s.handle(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).unwrap()
    }

    #[test]
    fn handshake_tools_and_calls() {
        let mut s = HubServer::new(Fake);
        let init = rpc(&mut s, 1, "initialize", json!({ "protocolVersion": "2024-11-05" }));
        assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
        let init = rpc(&mut s, 2, "initialize", json!({ "protocolVersion": "1999-01-01" }));
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
        assert!(s.handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).is_none());

        let tools = rpc(&mut s, 3, "tools/list", json!({}));
        assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), hub_tools().len());

        let r = rpc(&mut s, 4, "tools/call", json!({ "name": "hub_install_app", "arguments": { "app": "vectorcraft" } }));
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(r["result"]["content"][0]["text"], "installed vectorcraft");

        let r = rpc(&mut s, 5, "tools/call", json!({ "name": "photocraft__layer_add", "arguments": {} }));
        assert_eq!(r["result"]["isError"], true);

        let r = rpc(&mut s, 6, "nope", json!({}));
        assert_eq!(r["error"]["code"], -32601);
    }

    #[test]
    fn serve_reads_lines_and_answers_parse_errors() {
        let mut s = HubServer::new(Fake);
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n\nnot json\n";
        let mut out = Vec::new();
        s.serve(&input[..], &mut out).unwrap();
        let lines: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["result"], json!({}));
        assert_eq!(lines[1]["error"]["code"], -32700);
    }
}
