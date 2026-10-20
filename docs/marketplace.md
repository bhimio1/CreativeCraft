# Marketplace

The Marketplace lists two kinds of plugins:

- **Plugins for agents**: MCP servers that give your agents tools for more apps (Blender, Godot,
  Inkscape, …). The hub starts an installed plugin when an agent needs it, and its tools appear to
  the agent as `<plugin id>__<tool>`, exactly like a Craft app's.
- **Audio plugins**: CLAP and VST3 plugins for music apps (DAWs), such as MusicCraft's effects and
  instruments. See [Audio plugins](#audio-plugins).

## Using it

- **Install** shows what will run on your computer first: the plugin's one-time setup steps (for
  example installing its add-on into Blender) and the command agents start it with. Nothing runs
  until you confirm.
- A plugin that needs a program you don't have (such as `uv` for Python plugins or Node.js for
  `npx` ones) shows **Get … first**, linking to where to get it.
- The switch on an installed plugin turns its tools on or off for agents; **…** → **Remove**
  forgets it. Anything its setup added to an app (an add-on) stays in that app until you remove it
  there.
- Read each plugin's notes: many talk to the app while it's open, so open the app first.
- **Check for new plugins** fetches the newest list from the CreativeCraft repository, so new
  plugins appear without updating CreativeCraft.
- **Add your own** goes to Settings → Connections, for any MCP server that isn't listed.

From a terminal: `creativecraft-cli plugins` (both kinds; audio ones are marked), `plugins
install|remove|enable|disable <id>`, `plugins refresh`.

Plugins are made by their own authors, not by CreativeCraft, and run with your permissions. They
come from their publishers' own packages (PyPI through `uvx`, npm through `npx`), fetched when you
install or start them; CreativeCraft hosts none of them. Inside the Flatpak they run on the host,
like everything else the hub starts.

## Listing a plugin

Plugins are listed in [`marketplace/plugins.toml`](../marketplace/plugins.toml). To add one, open a
pull request with an entry like:

```toml
[[plugin]]
id = "inkscape-mcp"            # lowercase letters, digits, '-': it prefixes the tools
name = "Inkscape MCP"
for_app = "inkscape"           # optional: the catalog app it works with
category = "Vector"
summary = "What agents can do with it, in one sentence."
author = "aravindev"
license = "MIT"                # an OSI-approved licence
homepage = "https://github.com/aravindev/inkscape_mcp"
run = { command = "uvx", args = ["inkscape_mcp"] }
requires = [{ program = "uvx", name = "uv", url = "https://docs.astral.sh/uv/getting-started/installation/" }]
setup = []                     # one-time steps, each { label, command, args }
notes = "What the user still has to do."
```

and bump `version` at the top of the file. Rules:

- The plugin is open source, maintained, and published by its author to a public registry the
  `run` command fetches from (PyPI, npm, crates.io, …), or as a program the user installs.
- `run` and `setup` commands are plain program names looked up on the user's `PATH`, with their
  arguments as a list: no shell lines, paths or flags as the command (the index is checked for
  this). Setup steps must be safe to run again.
- No re-hosting: never point at a copy of someone else's package.
- Describe it in your own words; name other products only to identify what the plugin works with.

## Audio plugins

An audio plugin is a CLAP or VST3 bundle that your music app loads. **Install** first says what it
will do, for example: "Downloads MusicCraft-Plugins for this computer from github.com/bhimio1/musiccraft
releases and copies MusicCraft Effects.clap and .vst3 into ~/.clap and ~/.vst3." When you confirm,
the hub:

1. resolves the publisher's latest GitHub release and picks the asset built for this computer,
   from among the assets whose names start with the listing's `asset`;
2. downloads it and checks it against the release's published SHA-256 sums, so a mismatch stops
   the install;
3. finds `<bundle>.clap` / `<bundle>.vst3` in the archive (a file, or a folder bundle) and copies
   each one into your own plugin folder. It writes a temporary copy first, so an older version is
   replaced only once the new one is complete.

| OS | CLAP | VST3 |
|---|---|---|
| Linux, FreeBSD | `~/.clap/` | `~/.vst3/` |
| macOS | `~/Library/Audio/Plug-Ins/CLAP/` | `~/Library/Audio/Plug-Ins/VST3/` |
| Windows | `%LOCALAPPDATA%\Programs\Common\CLAP\` | `%LOCALAPPDATA%\Programs\Common\VST3\` |

These are per-user folders, so no admin rights are needed. The Flatpak has home-folder access and
writes to the same folders. Set `CREATIVECRAFT_AUDIO_PLUGIN_DIR` to put both formats under one
folder instead (`<dir>/clap`, `<dir>/vst3`), for portable setups and tests.

**Rescan plugins in your music app** after installing, updating or removing one. **Update**
installs the newest release if there is one, and otherwise says the plugin is up to date.
**…** → **Remove** deletes exactly the bundles the hub copied, and nothing else in those folders.
Audio plugins have no tools for agents, so they have no on/off switch.

### Listing an audio plugin

Audio plugins are listed in [`marketplace/audio-plugins.toml`](../marketplace/audio-plugins.toml).
They are kept out of `plugins.toml` so that released hubs, which read that file strictly, keep
working. Ids must be unique across both files. An entry:

```toml
[[plugin]]
id = "musiccraft-effects"        # lowercase letters, digits, '-'
name = "MusicCraft Effects"
for_app = "musiccraft"           # optional: the catalog app (its icon is shown)
category = "Effects"
summary = "What it is, in one sentence."
author = "bhimio1"
license = "GPL-3.0-or-later"     # an OSI-approved licence
homepage = "https://github.com/bhimio1/musiccraft"
repo = "bhimio1/musiccraft"      # the GitHub repository whose latest release it downloads
asset = "MusicCraft-Plugins"     # release asset name prefix; the build for the computer is picked
bundle = "MusicCraft Effects"    # bundle name inside the archive, without extension
formats = ["clap", "vst3"]
notes = "What the user still has to do."
```

Then bump `version` at the top of the file. Rules:

- **No re-hosting.** The release must be the publisher's own, on their own GitHub repository.
  Never point at a copy.
- Release assets are named `<asset>-<version>-<os>-<arch>.zip` (or `.tar.gz`), like the apps'
  archives, and should come with a `SHA256SUMS` file.
- `bundle` is a plain name, with no folders or `..`. `formats` lists only `clap` and `vst3`.
