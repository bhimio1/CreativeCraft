# Architecture

## Crates and layers

```text
                    apps/atelier (eframe)     apps/atelier-cli (CLI + `mcp`)
                              │                         │
L4  ui-egui ──────────────────┘                         │
                              │                         │
L3  engine    Session { catalog, installer, settings } + Command registry
              implements atelier_mcp::HubBackend
                 │            │             │
L2  install         cloud           mcp
    GitHub releases  CloudStore      Client (stdio JSON-RPC)
    verify/unpack    GoogleDrive     HubServer (aggregates app servers)
    detect/launch    FolderStore
                     sync executor
                 │            │
L1  catalog   (apps.toml model)
L0  platform (std)        sync-plan (std)
```

A crate only depends on lower layers. `mcp` reaches the engine through the `HubBackend` trait so
L2 never depends on L3.

## Commands

`atelier_engine::Command` is the single list of actions. The window runs commands on a worker
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

## Sync

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

Drive layout: one "Atelier Projects" folder mirroring the local tree. Every item carries private
`appProperties` (`atelier_kind`, `atelier_root`, `atelier_path`), so the whole tree lists with one
paginated query and the hub never needs broader access than `drive.file`.

## Agent hub

```text
MCP host (Claude Desktop, …) ⇄ stdio ⇄ atelier-cli mcp (HubServer)
                                          ├─ hub_* tools → Session commands
                                          ├─ photocraft-cli mcp  (spawned on first use)
                                          ├─ vectorcraft-cli mcp
                                          └─ user servers (e.g. Blender bridge)
```

Tools from app servers are re-exported as `<app>__<tool>`; the hub sends
`notifications/tools/list_changed` when an app server starts so the host refreshes its tool list.
`app_tools` / `app_call` give hosts that don't refresh a way in, too.

## On disk

```text
<data>/apps/<id>/<version>/…      unpacked apps
<data>/apps/installed.json
<data>/downloads/*.part
<config>/settings.json
<config>/apps.toml                optional catalog overlay
<config>/google-token.json        (0600)
<projects>/.atelier/sync-base.json, trash/
```

`<data>` and `<config>` come from `directories` (per-OS standard locations) or `ATELIER_HOME`.
