# Marketplace

The Marketplace lists **plugins**: MCP servers that give your agents tools for more apps (Blender,
Godot, Inkscape, …). An installed plugin is started by the hub when an agent needs it, and its
tools appear to the agent as `<plugin id>__<tool>`, exactly like a Craft app's.

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

From a terminal: `creativecraft-cli plugins`, `plugins install|remove|enable|disable <id>`,
`plugins refresh`.

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
