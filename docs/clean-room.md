# Clean room and IP policy

The goal is **zero copyright or trademark problems**. These are engineering rules, not legal
advice; have a lawyer review before a commercial launch.

## Copyright: we write everything ourselves

- Don't copy code, UI text, icons, fonts, illustrations, sounds or documentation from any
  commercial product, and don't decompile, disassemble or inspect its binaries or network traffic.
- Public specifications and documentation of *open formats and protocols* are fine (MCP spec,
  Google Drive REST API docs, ZIP/tar/gzip, OAuth 2.0 RFCs).
- Open-source dependencies must be under licences compatible with MIT OR Apache-2.0 (MIT, Apache,
  BSD, ISC, Zlib, MPL-2.0 as a dependency). **No GPL/AGPL code linked into CreativeCraft.** Check with
  `cargo deny check licenses` (see ROADMAP).
- Third-party apps (Blender, Krita, GIMP… many are GPL) are never linked, bundled or redistributed.
  The hub only detects them, launches them as separate processes, and links to their official
  downloads. Running a separate GPL program is not linking.
- Craft apps (MIT OR Apache-2.0) are downloaded from their own releases at the user's request.
  If we ever bundle them, ship their LICENSE and NOTICE files alongside.
- ArtCraft's main AI studio uses a source-available "fair source" licence, not an open-source one.
  It is not in the catalog; don't add it without reading that licence.

## Trademarks: our own name and look

- The product name is **CreativeCraft**. Two known risks to clear with a trademark search and,
  ideally, the ArtCraft Team before launch:
  - The ArtCraft name, wordmark and logos are ArtCraft Team trademarks and are **not** covered by
    the Craft apps' open-source licences. Their apps follow a `{Function}Craft` naming pattern, so
    "CreativeCraft" can read as one of theirs. Keep the NOTICE disclaimer visible (About box,
    website) and never use their logos; the catalog shows generated monograms instead. Asking them
    for a nod (or offering the hub to them) removes the risk entirely.
  - "Creative" sits close to a well-known commercial "Creative …" suite name. Don't pair it with
    that company's marks or styling.
  To rename later, change `Cargo.toml`, the window title, `APP_ID` and docs.
- Never use another company's product names, logos or slogans in our name, icon, window chrome or
  marketing (calling ourselves an "alternative" to a named commercial suite in an ad is a
  comparative claim: get it reviewed).
- Naming other apps to identify them ("Install PhotoCraft", "Open Blender") is nominative use and
  fine. Don't imply endorsement (see NOTICE).

## Pitch build branding

The current build uses the ArtCraft wordmark, mark and the Craft app icons, for a presentation to
the ArtCraft Team. That use needs their written permission before any public release (see NOTICE).

## Other apps' icons

CreativeCraft bundles no third-party app icons. An external app that is installed shows the icon
its own installation provides, read at runtime the way any launcher does (`.desktop` entry and
icon theme on Linux, the bundle's `.icns` on macOS); one that isn't installed gets a plain letter
tile.

## Trade dress: our own design

- The layout follows the common creative-suite launcher pattern (top bar with search, short
  sidebar, a list of apps with one Open/Install button each), which is a generic convention.
  Don't copy a competitor's distinctive elements on top of that: its logo, app-icon system,
  brand colours, typeface, illustrations or wording.
  CreativeCraft's palette, layout and copy live in `crates/ui-egui/src/theme.rs` and are our own.

## Cloud and privacy

- Google Drive access uses the `drive.file` scope only. Tokens are stored per-user (file mode 0600
  on Unix; OS keychain on the roadmap). Nothing is sent anywhere except the user's chosen storage
  and the app publishers' release servers.
