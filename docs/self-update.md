# Updating CreativeCraft itself

CreativeCraft updates itself from its own GitHub releases, the same way it installs the Craft apps:
it downloads the release file that matches how this copy was installed, checks it against the
release's `SHA256SUMS` (a missing or wrong checksum stops the update before anything changes), and
installs it. The running copy keeps going; **Restart now** starts the new version.

## Where it's offered

- **At start.** When "Check for updates at start" is on (Settings → General, on by default) and a
  newer CreativeCraft exists, a window offers **Update now** or **Later**.
- **Apps page.** The updates banner at the top keeps the offer (Update now, then Restart now).
- **Settings → General.** The CreativeCraft row shows this version, how it's installed and, when
  there is one, **Update to X**.
- **CLI.** `creativecraft-cli self-update`; `creativecraft-cli updates` says whether one exists.

## How each install updates

| Installed as | How it's detected | What the update does |
|---|---|---|
| Flatpak bundle (`.flatpak` from the release) | `FLATPAK_ID` is set; `/.flatpak-info` says per-user or system | Downloads `creativecraft-<v>-linux-<arch>.flatpak` and runs `flatpak install --user\|--system --bundle` on the host through `flatpak-spawn --host`. Flatpak keeps the running copy until it exits. A system-wide install may ask for your password. |
| Portable folder (`.tar.gz` / `.zip`) | Any other folder holding the executable | Unpacks the matching archive and swaps `creativecraft` and `creativecraft-cli` in place: the new file is copied beside the old one and renamed over it (on Windows the running file is renamed aside first; leftovers are removed at the next start). |
| macOS app (`.dmg` or zip) | The executable is inside `*.app` | Unpacks `creativecraft-<v>-macos-universal.zip` and swaps the whole `CreativeCraft.app` (copied with `ditto`, so its signature survives). |
| Windows installer (`.msi`) | The executable is under Program Files | Downloads the `.msi`. On **Restart now** CreativeCraft closes, the installer runs (Windows asks for administrator rights) and CreativeCraft opens again when it's done. |

## Where it can't update itself

CreativeCraft says so in the banner and in Settings, and links to the release page instead:

- **Package manager** (executable under `/usr`, e.g. the AUR's `creativecraft-bin`): update with
  your package manager, which owns those files.
- **Nix** (`/nix/store`): read-only; update through Nix.
- **A Flatpak from another remote** (a store rather than the release bundle): checked with
  `flatpak info --show-origin` before installing; update with `flatpak update` or the software
  center.
- **A build from source** (`target/debug` or `target/release`): `git pull` and `cargo build`.
- A portable folder it can't write to: the update stops with "can't write to …" and changes
  nothing.

Copies older than the first release with self-update (0.3.0 and earlier) have to be updated by hand
once; from then on they update themselves.
