# Architecture

## Crates and layers

```text
                    apps/creativecraft (eframe)     apps/creativecraft-cli (CLI + `mcp`)
                              │                         │
L4  ui-egui ──────────────────┘                         │
                              │                         │
L3  engine    Session { catalog, installer, settings, marketplace } + Command registry
              implements creativecraft_mcp::HubBackend
                 │            │             │
L2  install         cloud           mcp                        agent
    GitHub releases  CloudStore      Client (stdio JSON-RPC)    agent-CLI backends
    verify/unpack    GoogleDrive     HubServer (aggregates app servers)
    detect/launch    WebDav, S3                                 parse streams, transcript
    sources, icons   Rclone, Folder
                     sync executor
                 │            │
L1  catalog   (apps.toml model)      marketplace (plugins.toml model)
L0  platform (std)        sync-plan (std)
```

A crate only depends on lower layers. `mcp` reaches the engine through the `HubBackend` trait so
L2 never depends on L3.

## Stock media

`creativecraft-stock` (L2) wraps StockCraft's `stockcraft-local` (async, its own runtime) in a
blocking API. The engine's `Command::StockDownload` and the hub's `stock_search` / `stock_download`
save into `<projects>/Stock`; the Stock page searches and fetches previews on its own threads. See
[stock.md](stock.md).

## AI generation

`creativecraft-generate` (L2) has one adapter per kind of API: OpenAI (images and edits),
OpenAI-compatible, xAI, Google (Gemini images and Veo), Replicate, fal (queue), ComfyUI (built-in or
user API-format workflow) and Stable Diffusion WebUI. All of them use blocking `ureq`, and long jobs
are polled. Generation POSTs are never retried, so a request is never billed twice. Reads and
downloads are retried. Presets come from `catalog/generation.toml`, and every field is editable.
The user's services are `settings.generation`. Their API keys are kept in `secrets.json` under
`generate <id>`.

The engine's `Command::Generate` builds a `GenerationJob` (a copy of the provider and request) and
runs it. The job saves to `<projects>/Generated/<date>/` with a `<file>.json` sidecar, writing to a
temp file and then renaming. The Generate page runs a job on its own thread, so a long video
never holds the session. The hub's `generate_image` / `generate_video` / `generation_providers` go
through the same command, gated by `agents_can_generate`. See [generation.md](generation.md).

## Plugins

`marketplace/plugins.toml` (crate `creativecraft-marketplace`) lists MCP-server plugins; the
engine loads it, or a newer copy fetched from the repository into `<data>/marketplace/`. Installed
plugins are `settings.plugins` (id → enabled). `InstallPlugin` checks `requires` on the host `PATH`
and runs `setup` steps through `HostCommand`; `Session::spawn_mcp` starts an enabled plugin's `run`
command, so the hub exposes it as `<id>__<tool>` like an app. Commands are plain program names
(validated), never shell lines. See [marketplace.md](marketplace.md).

## Commands

`creativecraft_engine::Command` is the single list of actions. The window runs commands on a worker
thread (installs and syncs can take minutes) and receives `Event`s over a channel; the CLI runs
them inline and prints progress to stderr; the MCP server runs them for the agent.

## Installing an app

1. `GET api.github.com/repos/<owner>/<repo>/releases/latest`.
2. `platform::pick_asset` scores every asset for this OS/arch and picks the best portable
   archive (`.tar.gz` / `.zip`; never `.msi`/`.dmg`/`.deb`, which need admin rights).
3. Stream to `downloads/<asset>.part`, hashing SHA-256; compare with `<asset>.sha256` or
   `SHA256SUMS` if the release has one.
4. Unpack into `apps/<id>/.staging-<tag>`, rename to `apps/<id>/<tag>`, find the `gui` and
   `cli` executables, write `apps/installed.json`, then delete the previous version.

`settings.app_sources` can change where an app comes from (`install::Source`):

- **Fork**: the same steps against `<fork owner>/<name>`'s latest release.
- **Git**: with the user's own `git` and `cargo`, shallow-clone (or fetch and reset) the
  repository into `src/<id>/<key>/`, run `cargo build --release -p <gui> -p <cli>` with
  `CARGO_TARGET_DIR=build/<id>/<key>/` (falling back to the workspace's default build if a fork
  names its packages differently), copy the two executables to `apps/<id>/git-<rev>/` and register
  them like a release. Build output streams to the UI as `Progress::Building` / `Progress::Log`.
  Uninstalling removes the checkout and build cache too.

## Sync

Backends (`creativecraft-cloud`): Google Drive, WebDAV, S3 (SigV4 signed here), rclone (an external
program, like git), and folders. Stores that report no MD5 give a fingerprint (ETag, or size and
modification time) and `sync.rs` remembers which MD5 each stands for (`remote-hashes.json`); local
hashes are cached by size, modification and change time (`hash-cache.json`). Downloads land in
`.creativecraft/incoming/`, are checked, then renamed into place. User ignore rules come from
`.creativecraftignore` (`sync_plan::IgnoreRules`). See [cloud-sync.md](cloud-sync.md) for the
user's side.


`sync-plan` compares **local**, **remote** and **base** (the state after the last good sync):

| local vs base | remote vs base | action |
|---|---|---|
| same | changed | download |
| changed | same | upload |
| changed | changed (different) | **conflict**: keep local, save remote beside it |
| deleted | same | trash remote |
| same | deleted | trash local |
| deleted | changed | download (edit wins) |
| changed | deleted | upload (edit wins) |

The executor applies actions one by one; a failure is reported and that path keeps its old base,
so it's retried next time. Hashes are MD5 because Google Drive reports `md5Checksum` natively.

Drive layout: one "CreativeCraft Projects" folder mirroring the local tree. Every item carries short
private `appProperties` (`cc_kind`, `cc_root`), so the whole tree lists with one paginated query;
paths are rebuilt from names and parents (Drive caps each property at 124 bytes, too small for
paths). The hub never needs broader access than `drive.file`.

## Agent hub

```text
MCP host (Claude Desktop, …) ⇄ stdio ⇄ creativecraft-cli mcp (HubServer)
                                          ├─ hub_* tools → Session commands
                                          ├─ photocraft-cli mcp  (spawned on first use)
                                          ├─ vectorcraft-cli mcp
                                          └─ user servers (e.g. Blender bridge)
```

Tools from app servers are re-exported as `<app>__<tool>`; the hub sends
`notifications/tools/list_changed` when an app server starts so the host refreshes its tool list.
`app_tools` / `app_call` give hosts that don't refresh a way in, too.

**Windows (live mode).** By default an agent works in the app's visible window, so the user can
watch and take over (Settings → Assistant → "Show apps while agents work"; tools take
`window: true/false` to override). For an app with a catalog `live` entry the hub starts the
desktop app with a localhost control channel on a free port (`--control {port}`, plus a private
`{token}` file under `<config>/control/` for apps that authenticate it), waits until it listens,
then starts `<app>-cli` with its bridge arguments (`--bridge`/`--connect`). Apps without one, and
headless runs, use the `mcp` arguments. `{projects}` grants the projects folder to apps that only
read and write beneath folders they are given (PhotoCraft's automation roots, PdfCraft's
`--root`), so agents can open and save files there. Closing an instance stops the automation;
the window stays open for the user.

An app can run as several **instances**, each its own `<app>-cli mcp` process with its own
documents: the first is named like the app, `app_open_instance` starts `photocraft-2`,
`photocraft-3`, … whose tools are exported as `photocraft-2__<tool>`. `app_instances` lists them
and `app_close_instance` stops one. An extra instance that dies isn't restarted silently (its
documents are gone); the first instance of an app restarts on its next call as before.

## AI assistant

See [ai-chat.md](ai-chat.md). The Assistant page gets a launch spec from
`creativecraft_agent::interactive_launch` and runs it in a PTY (`portable-pty`), drawing the screen
from a `vt100` parser (`crates/ui-egui/src/terminal.rs`); `creativecraft-cli chat` runs headless
turns with `start_turn`. Either way the backend CLI runs as a child process in the projects folder
and connects back to the hub through a fresh `creativecraft-cli mcp` process, so everything the
agent does still goes through engine commands. Syncs take a lock file (`<projects>/.creativecraft/sync.lock`) because the window, the
CLI and the agent's hub process can each start one.

## Projects page

`creativecraft_engine::projects::scan` walks the projects folder (skipping hidden folders and
`.creativecraft/`, bounded in depth and file count) for files whose extension some catalog app lists
in `opens`; `projects::thumbnail` reads a preview from the file itself with size limits. The window
runs both on background threads and caches decoded previews by path and modification time.

## Running programs (and Flatpak)

Every program the hub starts (agent CLIs, app windows and MCP servers, `git`/`cargo` source
builds, user MCP servers, the Assistant terminals) is built as a `platform::HostCommand`.
Outside Flatpak that is exactly the `std::process::Command` it describes. Inside Flatpak
(`FLATPAK_ID` set) it runs `flatpak-spawn --host --watch-bus [--directory=…] [--env=K=V]… <program>
<args>`, because those programs live on the host; env and directory travel as flags since the
outer process's would only reach `flatpak-spawn`. App windows the user opens are spawned without
`--watch-bus` so they outlive the hub. `platform::find_on_path` asks the host's `PATH`
(`sh -c 'command -v -- "$1"'`, cached), and `platform::stop` sends SIGTERM before SIGKILL, which
`flatpak-spawn` can't forward. External apps installed as Flatpaks are found under
`~/.local/share/flatpak/exports` and `/var/lib/flatpak/exports` (catalog `flatpak` id, or an
exported desktop entry's `--command=`).

## On disk

```text
<data>/apps/<id>/<version>/…      unpacked apps
<data>/apps/installed.json
<data>/downloads/*.part
<config>/settings.json
<config>/apps.toml                optional catalog overlay
<config>/google-token.json        (0600)
<config>/secrets.json             (0600) cloud passwords/secret keys, AI generation API keys
<data>/chat.json                  `creativecraft-cli chat` transcript (the model history lives in the backend's session)
<projects>/Generated/<date>/…     AI generation results, each with a .json sidecar
<projects>/.creativecraft/sync-base.json, sync.lock, trash/, agent/
<projects>/.agents/mcp_config.json   Antigravity's workspace MCP config (not synced)
```

`<data>` and `<config>` come from `directories` (per-OS standard locations) or `CREATIVECRAFT_HOME`.
