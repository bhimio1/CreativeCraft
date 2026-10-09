# Atelier

**One home for open-source creative apps.** Atelier installs and updates the seven Craft apps
(PhotoCraft, VectorCraft, FilmCraft, LightCraft, PdfCraft, EffectCraft, DesignCraft), finds and
launches other open-source tools you already use (Blender, Krita, Inkscape, GIMP, Kdenlive,
darktable, Godot, ComfyUI), keeps your project files in sync through Google Drive or any synced
folder, and gives AI assistants **one MCP server** that can drive every agent-ready app.

Pure Rust, built the same way as the Craft apps: a layered Cargo workspace, a native egui/eframe
window on wgpu (no Electron, Tauri or webview), a headless CLI, and every action going through one
command registry, so anything you can click, an agent can do.

> Status: **pre-alpha scaffold.** Architecture, sync engine, installer, Drive backend and MCP hub
> are in place with tests; see [ROADMAP.md](ROADMAP.md) for what's next.

## Build and run

Rust 1.95+ (edition 2024).

```sh
cargo build --release
./target/release/atelier          # the hub window
./target/release/atelier-cli list # or the CLI
cargo test                        # all tests (sync, installer, MCP, catalog)
```

Linux builds need the usual windowing dev packages for eframe (Wayland/X11, xkbcommon).

## What's in the box

| Feature | How it works |
|---|---|
| **Install & update apps** | Downloads each Craft app's portable archive straight from its own GitHub releases, verifies SHA-256 when published, unpacks per-user (no admin rights). Updates install side by side and only replace the old version once the new one works. |
| **Other open-source apps** | Detected on `PATH`, in `/Applications`, or under Program Files; "Get it" opens the official download page. Atelier never redistributes them. |
| **Cloud saves** | A projects folder synced with **Google Drive** (REST API, `drive.file` scope: Atelier can only see files it created) or **any folder** your sync client mirrors (Drive for desktop, Dropbox, OneDrive, Nextcloud…). Three-way sync: conflicting edits are never overwritten; the other version is saved alongside as `name (cloud copy for <computer>).ext`. Deletions go to the trash. |
| **AI agent hub** | `atelier-cli mcp` is an MCP server (stdio). It exposes hub tools (`hub_list_apps`, `hub_install_app`, `hub_launch_app`, `hub_sync_projects`) and starts each app's own `<app>-cli mcp` on demand, re-exporting its tools as `<app>__<tool>`. Add your own MCP servers (e.g. a Blender add-on bridge) in the Agent page. |

### Connect an AI assistant

Point any MCP host at the CLI (the Agent page shows the exact path and a Copy button):

```json
{ "mcpServers": { "atelier": { "command": "/path/to/atelier-cli", "args": ["mcp"] } } }
```

### Set up Google Drive

Drive needs an OAuth client that **you, the publisher,** own:

1. In Google Cloud Console, create a project, enable the **Google Drive API**.
2. Configure the OAuth consent screen; add the scope `.../auth/drive.file` only.
3. Create credentials → **OAuth client ID** → application type **Desktop app**.
4. Put the id and secret in Settings → Cloud saves → Google Drive (or set
   `ATELIER_GOOGLE_CLIENT_ID` / `ATELIER_GOOGLE_CLIENT_SECRET` at build/packaging time), then
   Projects → **Sign in to Google Drive**. Sign-in uses your browser and a loopback redirect
   with PKCE; no embedded webview.

For a public release, Google must verify the consent screen; `drive.file` is a non-sensitive
scope, which keeps that review light.

## Layout

```text
crates/
  platform     L0  host OS/arch, release-asset matching, version compare   (std only)
  sync-plan    L0  three-way sync planner: pure, deterministic             (std only)
  catalog      L1  app catalog model; built-in catalog/apps.toml + user overlay
  install      L2  GitHub releases, download + verify, unpack, detect, launch
  cloud        L2  CloudStore trait, Google Drive + folder backends, sync executor
  mcp          L2  MCP stdio client, hub MCP server
  engine       L3  Session + command registry (UI, CLI and agent all call this)
  ui-egui      L4  the window (thin)
apps/
  atelier          desktop app (eframe/wgpu)
  atelier-cli      headless CLI, incl. `mcp`
catalog/apps.toml  which apps exist and where they come from
```

See [docs/architecture.md](docs/architecture.md) and, before contributing, [AGENTS.md](AGENTS.md)
and [docs/clean-room.md](docs/clean-room.md).

## License

MIT OR Apache-2.0, like the Craft apps. See [NOTICE](NOTICE) for trademarks and affiliation.
