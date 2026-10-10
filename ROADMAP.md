# Roadmap

## M0: scaffold (this commit)
- [x] Workspace, layering, lints mirroring the Craft apps
- [x] Catalog (12 Craft apps + 8 external open-source apps) with user overlay
- [x] Installer: GitHub releases, asset matching, SHA-256 verify, unpack, side-by-side update, launch
- [x] External app detection (PATH, macOS bundles, Program Files)
- [x] Three-way sync planner (tested) + executor; Google Drive and folder backends
- [x] Google OAuth (PKCE + loopback), `drive.file` scope
- [x] MCP client + hub MCP server with lazy app servers and namespaced tools
- [x] egui window: Apps, Projects & cloud, AI agent, Settings; CLI with `mcp`

## M0.1: AI chat (this commit)
- [x] Renamed to CreativeCraft
- [x] `creativecraft-agent`: Claude Code, OpenCode, Hermes Agent, Antigravity CLI and custom backends
- [x] Per-run MCP config pointing each backend at the hub; Hermes one-time setup
- [x] Stream parsing (exact for Claude Code, tolerant for the rest) + transcript; chat page and `creativecraft-cli chat`
- [x] Sync lock shared by window, CLI and agent

## M1: make it solid
- [ ] Record real transcripts from OpenCode, Hermes and Antigravity; tighten their parsers with fixtures
- [ ] Multi-turn without a process per message where supported (Claude Code / Antigravity `--input-format stream-json`)
- [x] Assistant page as a terminal running the agent CLI's own interactive UI (PTY + `vt100`)
- [x] Projects page: recent files with embedded previews; `creativecraft-cli projects`
- [x] Install sources: a fork's releases, or git builds (official or fork, any branch) with cargo
- [x] Installed external apps show their own icon (Linux desktop entries, macOS bundles)
- [x] Marketplace: plugin index (built-in + fetched), MCP-server plugins (Blender, Godot, Inkscape), install with confirmation, on/off, remove, CLI
- [ ] Marketplace: plugins that add apps to the catalog, plugin updates and versions, ratings
- [ ] Cancel a running git build; Windows icons for external apps (from the executable's resources)
- [ ] Projects: previews for PDF (needs a pure-Rust renderer) and compressed `.blend`
- [ ] Terminal: mouse reporting for full-screen TUIs, double-click word selection, a font-size setting
- [x] Cloud sync: WebDAV, S3-compatible and rclone backends, NAS guidance, retries, checked downloads, hash cache, `.creativecraftignore`, secrets file
- [ ] Cloud sync: S3 multipart for files over 5 GB, resumable WebDAV uploads, sync on file change
- [x] Agent tabs: several agents at once, each with its own app instances (`app_open_instance`)
- [ ] First `cargo build` on all three OSes; fix API drift (egui/eframe 0.36, ureq, zip)
- [ ] Confirm each Craft app's real release asset names; add `asset_hint` to the catalog if needed
- [ ] Token storage in the OS keychain (`keyring` crate) instead of a 0600 file
- [ ] Resumable *chunked* Drive uploads with retry for very large files; exponential backoff on 429/5xx
- [ ] Sync on file change (`notify` crate) in addition to the timer; pause while an app has a file open
- [ ] `cargo deny` (licences, advisories) and `cargo xtask layers` (enforce L0–L4) in CI
- [ ] Integration test: a fake MCP app server (test binary) behind the hub

## M2: feel like a studio
- [ ] Recent projects across all apps with thumbnails; "open with…"
- [ ] App update notifications; release notes view
- [ ] Settings import/export; multiple cloud profiles
- [ ] Packaging like the Craft apps: signed/notarized macOS zip, Windows zip + optional MSI,
      AppImage/deb/rpm/Flatpak; self-update for the hub

## M3: ecosystem
- [ ] Signed catalog feed (so new apps appear without a hub update)
- [ ] Optional first-party MCP bridges for external apps where their licences allow a separate,
      user-installed add-on (never linked into the hub)
- [ ] More backends: WebDAV/Nextcloud, S3-compatible
