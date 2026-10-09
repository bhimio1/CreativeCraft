# AGENTS.md: guide for AI agents and contributors

CreativeCraft is a hub for open-source creative apps, written in **Rust only**. It mirrors the Craft
apps' architecture on purpose, so the same habits apply in every repo.

## Rules

1. **Rust only.** The desktop UI is native egui/eframe on wgpu. Never add Tauri, Electron, a
   webview or a JS/TS UI. Prefer pure-Rust dependencies (rustls, not OpenSSL; `flate2` with the
   Rust backend; no C toolkits).
2. **Layers.** A crate depends only on lower layers (see the table in README.md). L0 crates
   (`platform`, `sync-plan`) stay **std only** so they're trivially testable and portable.
3. **Every action is a command.** UI, CLI and MCP all go through `creativecraft_engine::Session::execute`.
   Adding a feature = add a `Command` variant + its implementation; then wire the UI button, the
   CLI verb and (if agents should use it) a hub MCP tool.
4. **Never lose user work.** Sync never overwrites a file both sides changed; deletes go to a
   trash; writes go to a temp file then rename. Any change to `sync-plan` needs a test.
5. **Clean room and no IP borrowing.** Read [docs/clean-room.md](docs/clean-room.md). In short:
   no other product's code, assets, icons, fonts, screenshots or UI copy; no third-party
   trademarks in our own product's name, UI chrome or marketing; name other apps only to identify
   them.
6. **No re-hosting.** The hub downloads apps from their publishers' own release pages at the
   user's request. Don't mirror binaries or bundle other projects' artwork.
7. `unsafe_code` is forbidden workspace-wide. No `unwrap`/`expect`/`panic!` outside tests.
8. **The chat never holds model credentials.** Backends are external agent CLIs with their own
   sign-in; CreativeCraft only passes them a prompt and an MCP config. Don't add API-key handling.
9. **Agents' tools are user-facing.** Hub MCP tool names, descriptions and results are part of the
   product: keep them short, accurate, and safe by default (no destructive tool without an
   explicit argument).

## Orientation

| Read | Why |
|---|---|
| `docs/architecture.md` | Crates, data flow, threads, file layout on disk |
| `docs/clean-room.md` | What we may and may not use, and why |
| `ROADMAP.md` | Milestones and current focus |
| `catalog/apps.toml` | Every app the hub knows about |
| `docs/ai-chat.md` | How the chat drives external agent CLIs, and how to update a backend |

## Commands

```sh
cargo test                     # everything
cargo test -p creativecraft-sync-plan
cargo clippy --all-targets -- -D warnings
cargo fmt --all
CREATIVECRAFT_HOME=/tmp/at cargo run -p creativecraft-cli -- list   # isolated data folder
```

Before every commit, run `cargo fmt --all` and `cargo clippy --all-targets -- -D warnings`: CI
fails on any formatting difference and stops before building.

## Releasing

1. Bump `version` in the root `Cargo.toml` (and run `cargo check` so `Cargo.lock` follows).
2. Commit and push to `main`, and wait for CI to pass.
3. Push a tag `vX.Y.Z` matching the version, or run **Actions → Release → Run workflow** with
   that tag. The Release workflow builds Linux (x86_64, aarch64), macOS (universal) and Windows,
   and publishes a pre-release with the archives and `SHA256SUMS`. It also builds Flatpak bundles
   (`packaging/flatpak/`) and an AUR `creativecraft-bin` PKGBUILD (`packaging/aur/PKGBUILD.in`),
   test-builds the PKGBUILD against the published release, and pushes it to the AUR when the
   `AUR_SSH_PRIVATE_KEY` secret is set.
