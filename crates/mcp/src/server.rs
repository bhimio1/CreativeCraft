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
    /// Start `id`'s MCP server process (stdio piped). `window`: drive the app's visible window
    /// where the app supports it (`None`: the user's default).
    fn spawn_mcp(&mut self, id: &str, window: Option<bool>) -> Result<Child, String>;
    /// Search public-domain stock media: `{results:[…], has_more, unavailable_sources}`.
    fn stock_search(&mut self, query: &str, kind: Option<&str>, page: u32) -> Result<Value, String>;
    /// Download a stock asset into the projects folder; returns where it went and its credit line.
    fn stock_download(&mut self, source: &str, id: &str, size: Option<&str>, max_width: Option<u32>) -> Result<String, String>;
}

/// One running app automation server. An app can have several, each its own process with its
/// own documents: the first is named like the app (`photocraft`), the others `photocraft-2`, ….
struct Instance {
    app: String,
    /// Asked for the app's window (`None`: the user's default).
    window: Option<bool>,
    client: Client,
    /// Its tools, un-namespaced.
    tools: Vec<Value>,
}

/// The hub's MCP server (stdio). See the crate docs.
pub struct HubServer<B: HubBackend> {
    backend: B,
    /// Running app instances by instance id.
    apps: BTreeMap<String, Instance>,
    /// Notifications to send after the current response.
    pending: Vec<Value>,
}

fn text_result(text: impl Into<String>, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }], "isError": is_error })
}

fn hub_tools() -> Vec<Value> {
    let id_arg = json!({ "type": "object", "properties": { "app": { "type": "string", "description": "App id from hub_list_apps, e.g. \"photocraft\"" } }, "required": ["app"] });
    let app_window_arg = json!({ "type": "object", "properties": {
        "app": { "type": "string", "description": "App id from hub_list_apps, e.g. \"photocraft\"" },
        "window": { "type": "boolean", "description": "Work in the app's visible window (true) or headless (false). Omit to use the user's setting." }
    }, "required": ["app"] });
    vec![
        json!({ "name": "hub_list_apps", "description": "List every app the hub knows: id, name, category, install status and whether it can be driven over MCP.", "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "hub_install_app", "description": "Install an app, or update it to its latest release.", "inputSchema": id_arg }),
        json!({ "name": "hub_launch_app", "description": "Open a window of an app for the user, optionally with files. Each call opens a new window.", "inputSchema": { "type": "object", "properties": { "app": { "type": "string" }, "files": { "type": "array", "items": { "type": "string" } } }, "required": ["app"] } }),
        json!({ "name": "hub_sync_projects", "description": "Sync the user's projects folder with their cloud storage now. Never overwrites: conflicting edits are kept side by side.", "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "app_tools", "description": "Start an installed app's automation (if needed) and list its tools. Afterwards they are also callable directly as `<app>__<tool>`. `app` may also be an instance id from app_open_instance. Apps with \"window\": true in hub_list_apps open their window so the user can watch; save files with paths relative to the projects folder.", "inputSchema": app_window_arg.clone() }),
        json!({ "name": "app_call", "description": "Call one of an app's tools (see app_tools). `app` may be an app id or an instance id.", "inputSchema": { "type": "object", "properties": { "app": { "type": "string" }, "tool": { "type": "string" }, "arguments": { "type": "object" } }, "required": ["app", "tool"] } }),
        json!({ "name": "app_open_instance", "description": "Start another, independent instance of an app (its own window where the app has one), with its own documents and state, to work on several things at once. Returns the instance id (e.g. \"photocraft-2\"); call its tools as `<instance>__<tool>`.", "inputSchema": app_window_arg }),
        json!({ "name": "stock_search", "description": "Search free stock media: public-domain and CC0 photos, artworks, illustrations, videos and audio from museums, NASA and open archives. Returns source, id, title, rights, a credit line and the sizes on offer.", "inputSchema": { "type": "object", "properties": {
            "query": { "type": "string" },
            "kind": { "type": "string", "enum": ["photo", "illustration", "artwork", "video", "audio"] },
            "page": { "type": "integer", "minimum": 1 }
        }, "required": ["query"] } }),
        json!({ "name": "stock_download", "description": "Download a stock asset (source and id from stock_search) into the user's projects folder under Stock/, ready to open in an app. Picks a size up to 3000 px wide unless `size` (a label from stock_search) or `max_width` is given. Returns the file's path in the projects folder and its credit line.", "inputSchema": { "type": "object", "properties": {
            "source": { "type": "string" }, "id": { "type": "string" },
            "size": { "type": "string", "description": "A size label from stock_search, e.g. \"Large\"" },
            "max_width": { "type": "integer", "minimum": 1 }
        }, "required": ["source", "id"] } }),
        json!({ "name": "app_instances", "description": "List the running app instances: instance id, app id and tool count.", "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "app_close_instance", "description": "Stop one app instance's automation. Headless: anything unsaved is lost, so save first. A visible window stays open for the user.", "inputSchema": { "type": "object", "properties": { "instance": { "type": "string", "description": "Instance id from app_open_instance or app_instances" } }, "required": ["instance"] } }),
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
                    "serverInfo": { "name": "creativecraft-hub", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "CreativeCraft is a hub for creative apps. Call hub_list_apps first. To work inside an app, call app_tools with its id; its tools then appear as <app>__<tool>. For several documents at once, app_open_instance starts another independent instance (<app>-2, …) with its own tools. Files are the user's: prefer saving new versions over overwriting.",
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
        for (name, inst) in &self.apps {
            for t in &inst.tools {
                let mut t = t.clone();
                let tool = t.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                t["name"] = json!(namespaced(name, &tool));
                let desc = t.get("description").and_then(Value::as_str).unwrap_or("").to_string();
                t["description"] = json!(format!("[{name}] {desc}"));
                tools.push(t);
            }
        }
        tools
    }

    fn start_instance(&mut self, name: &str, app: &str, window: Option<bool>) -> Result<(), String> {
        let child = self.backend.spawn_mcp(app, window)?;
        let mut client = Client::start(child).map_err(|e| format!("{name}: {e}"))?;
        let tools = client.list_tools().map_err(|e| format!("{name}: {e}"))?;
        self.apps.insert(name.to_string(), Instance { app: app.to_string(), window, client, tools });
        self.pending.push(json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" }));
        Ok(())
    }

    /// The instance called `name`, starting it if it's an app's first instance. An extra instance
    /// that stopped is not restarted silently: its documents are gone, and the agent should know.
    fn ensure_app(&mut self, name: &str, window: Option<bool>) -> Result<&mut Instance, String> {
        let alive = self.apps.get_mut(name).map(|i| (i.client.is_running(), i.app.clone()));
        match alive {
            Some((true, _)) => {}
            Some((false, app)) if app != name => {
                self.apps.remove(name);
                self.pending.push(json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" }));
                return Err(format!("instance {name} has stopped; open a new one with app_open_instance"));
            }
            _ => {
                // a restart keeps the window choice it was first started with
                let window = self.apps.remove(name).map_or(window, |i| i.window);
                self.start_instance(name, name, window)?;
            }
        }
        self.apps.get_mut(name).ok_or_else(|| format!("{name} is not running"))
    }

    /// Start a new instance of `app`: `app` itself if it isn't running, else `app-2`, `app-3`, ….
    fn open_instance(&mut self, app: &str, window: Option<bool>) -> Result<String, String> {
        if let Some(inst) = self.apps.get(app)
            && inst.app != app
        {
            return Err(format!("{app} is an instance id; open an instance of its app ({}) instead", inst.app));
        }
        let name = next_instance_name(app, |n| self.apps.get_mut(n).is_some_and(|i| i.client.is_running()));
        self.apps.remove(&name);
        self.start_instance(&name, app, window)?;
        Ok(name)
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let window = args.get("window").and_then(Value::as_bool);
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
            "stock_search" => {
                let page = args.get("page").and_then(Value::as_u64).unwrap_or(1) as u32;
                let kind = args.get("kind").and_then(Value::as_str).map(String::from);
                match self.backend.stock_search(&s("query"), kind.as_deref(), page) {
                    Ok(v) => text_result(v.to_string(), false),
                    Err(e) => text_result(e, true),
                }
            }
            "stock_download" => {
                let size = args.get("size").and_then(Value::as_str).map(String::from);
                let max_width = args.get("max_width").and_then(Value::as_u64).map(|w| w.min(u64::from(u32::MAX)) as u32);
                done(self.backend.stock_download(&s("source"), &s("id"), size.as_deref(), max_width))
            }
            "app_tools" => {
                let app = s("app");
                match self.ensure_app(&app, window) {
                    Ok(Instance { tools, .. }) => {
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
            "app_open_instance" => match self.open_instance(&s("app"), window) {
                Ok(name) => {
                    let n = self.apps.get(&name).map_or(0, |i| i.tools.len());
                    text_result(json!({ "instance": name, "tools": n, "call_as": format!("{name}__<tool>") }).to_string(), false)
                }
                Err(e) => text_result(e, true),
            },
            "app_instances" => {
                // forget instances whose process has ended
                self.apps.retain(|_, i| i.client.is_running());
                let list: Vec<Value> = self.apps.iter().map(|(name, i)| json!({ "instance": name, "app": i.app, "tools": i.tools.len() })).collect();
                text_result(Value::Array(list).to_string(), false)
            }
            "app_close_instance" => {
                let name = s("instance");
                match self.apps.remove(&name) {
                    // dropping the client stops the process
                    Some(_) => {
                        self.pending.push(json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" }));
                        text_result(format!("closed {name}"), false)
                    }
                    None => text_result(format!("no instance called {name}"), true),
                }
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
        match self.ensure_app(app, None) {
            Ok(Instance { client, .. }) => match client.call_tool(tool, args) {
                Ok(r) => r,
                Err(e) => text_result(format!("{app}: {e}"), true),
            },
            Err(e) => text_result(e, true),
        }
    }
}

/// The first free instance name for `app`: `app`, then `app-2`, `app-3`, ….
fn next_instance_name(app: &str, mut taken: impl FnMut(&str) -> bool) -> String {
    if !taken(app) {
        return app.to_string();
    }
    (2..).map(|n| format!("{app}-{n}")).find(|n| !taken(n)).unwrap_or_else(|| app.to_string())
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
        fn spawn_mcp(&mut self, id: &str, _: Option<bool>) -> Result<Child, String> {
            Err(format!("{id} has no MCP server"))
        }
        fn stock_search(&mut self, query: &str, _: Option<&str>, _: u32) -> Result<Value, String> {
            Ok(json!({ "results": [{ "source": "met", "id": "1", "title": query }] }))
        }
        fn stock_download(&mut self, source: &str, id: &str, _: Option<&str>, _: Option<u32>) -> Result<String, String> {
            Ok(format!("Saved Stock/media/{source}/{id}/x.jpg"))
        }
    }

    fn rpc<B: HubBackend>(s: &mut HubServer<B>, id: u64, method: &str, params: Value) -> Value {
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
    fn instance_names() {
        let taken = ["photocraft", "photocraft-2"];
        assert_eq!(next_instance_name("photocraft", |n| taken.contains(&n)), "photocraft-3");
        assert_eq!(next_instance_name("vectorcraft", |n| taken.contains(&n)), "vectorcraft");
        assert_eq!(next_instance_name("photocraft", |n| n == "photocraft-2"), "photocraft");
    }

    /// A stand-in app server: answers initialize (id 1) and tools/list (id 2), then waits.
    #[cfg(unix)]
    struct ShApp;
    #[cfg(unix)]
    impl HubBackend for ShApp {
        fn list_apps(&mut self) -> Value {
            json!([])
        }
        fn install(&mut self, _: &str) -> Result<String, String> {
            Err("no".into())
        }
        fn launch(&mut self, _: &str, _: &[String]) -> Result<String, String> {
            Err("no".into())
        }
        fn sync(&mut self) -> Result<String, String> {
            Err("no".into())
        }
        fn stock_search(&mut self, _: &str, _: Option<&str>, _: u32) -> Result<Value, String> {
            Err("no".into())
        }
        fn stock_download(&mut self, _: &str, _: &str, _: Option<&str>, _: Option<u32>) -> Result<String, String> {
            Err("no".into())
        }
        fn spawn_mcp(&mut self, _: &str, _: Option<bool>) -> Result<Child, String> {
            let script = r#"read l; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"fake"}}}'; read l; read l; echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"layer_add","description":"Add a layer","inputSchema":{"type":"object"}}]}}'; cat > /dev/null"#;
            std::process::Command::new("sh")
                .args(["-c", script])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| e.to_string())
        }
    }

    #[cfg(unix)]
    #[test]
    fn several_instances_of_one_app() {
        let mut s = HubServer::new(ShApp);
        let call =
            |s: &mut HubServer<ShApp>, id: u64, name: &str, args: Value| rpc(s, id, "tools/call", json!({ "name": name, "arguments": args }))["result"].clone();
        let text = |r: &Value| r["content"][0]["text"].as_str().unwrap().to_string();

        let a = call(&mut s, 1, "app_open_instance", json!({ "app": "photocraft" }));
        assert!(text(&a).contains("\"instance\":\"photocraft\""), "{a}");
        let b = call(&mut s, 2, "app_open_instance", json!({ "app": "photocraft" }));
        assert!(text(&b).contains("\"instance\":\"photocraft-2\""), "{b}");
        let err = call(&mut s, 3, "app_open_instance", json!({ "app": "photocraft-2" }));
        assert_eq!(err["isError"], true);

        let tools = rpc(&mut s, 4, "tools/list", json!({}));
        let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"photocraft__layer_add") && names.contains(&"photocraft-2__layer_add"), "{names:?}");

        let list = call(&mut s, 5, "app_instances", json!({}));
        assert!(text(&list).contains("photocraft-2"));
        let closed = call(&mut s, 6, "app_close_instance", json!({ "instance": "photocraft-2" }));
        assert_eq!(text(&closed), "closed photocraft-2");
        assert_eq!(call(&mut s, 7, "app_close_instance", json!({ "instance": "photocraft-2" }))["isError"], true);
        // the next instance reuses the freed name
        let c = call(&mut s, 8, "app_open_instance", json!({ "app": "photocraft" }));
        assert!(text(&c).contains("photocraft-2"), "{c}");
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
