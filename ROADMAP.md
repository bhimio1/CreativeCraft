# Roadmap

## M0: scaffold (this commit)
- [x] Workspace, layering, lints mirroring the Craft apps
- [x] Catalog (7 Craft apps + 8 external open-source apps) with user overlay
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
- [ ] Attach files and images to a chat message; show images the agent produces inline
- [ ] Chat history list (several conversations), rename/delete
- [ ] Approve tool calls from the chat UI instead of all-or-nothing (Claude Code `--permission-prompt-tool`)
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
