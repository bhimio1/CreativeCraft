# AI assistant

The assistant in CreativeCraft is an **agent CLI you already use**. CreativeCraft doesn't call any
model API itself and never sees an API key: your agent keeps its own account, model choice and
settings. What CreativeCraft adds is the tools: the agent gets the hub (`creativecraft-cli mcp`) as
an MCP server, which fronts all your agent-ready apps.

There are two ways in:

- **The Assistant page** runs agents in tabs. Each tab is a terminal with an agent's own
  interactive UI (`claude`, `opencode`, `hermes chat`, `agy`, or your command) in the projects
  folder, wired to the hub, and you use it exactly as you would in any terminal. **+ New agent**
  opens another tab with any agent, so several can work on different tasks at once; tabs keep
  running in the background. A tab starts when it's first shown; **Restart** begins a fresh
  session and **End** stops it; double-click a tab to rename it. Closing a running tab asks first.
  Ctrl+C copies when text is selected and interrupts otherwise; drag to select, scroll for history.

  Every agent starts its own `creativecraft-cli mcp`, so each has its own app instances and two
  agents never edit the same open document. They do share the projects folder, so give them
  different files to work on.
- **`creativecraft-cli chat "…"`** runs one headless turn per message, resuming the agent's session,
  and prints the parsed reply.

```text
Assistant terminal ─▶ agent CLI, interactive        creativecraft-cli chat ─▶ agent CLI, one headless run per message
                 └─ MCP ─▶ creativecraft-cli mcp ─┬─ hub_list_apps / hub_install_app / hub_launch_app / hub_sync_projects
                                                   ├─ photocraft-cli mcp   (started on first use, tools as photocraft__…)
                                                   ├─ vectorcraft-cli mcp …
                                                   └─ your own MCP servers (e.g. a Blender add-on bridge)
```

The agent runs in your projects folder, so it can also read and write project files with its own
tools when you allow that.

## Backends

Interactive sessions use the same MCP wiring as headless turns, without the prompt and output
flags. Claude Code gets the hub through `--mcp-config` but keeps your other MCP servers (no
`--strict-mcp-config`), and the CreativeCraft context through `--append-system-prompt`; the other
CLIs have no system-prompt flag, so there the agent learns the tools from the hub's tool list.

The headless invocations:

| Backend | Program | How CreativeCraft passes the hub | Headless invocation | Resumes with |
|---|---|---|---|---|
| Claude Code | `claude` | `--mcp-config <file> --strict-mcp-config` (per run) | `-p <msg> --output-format stream-json --verbose --append-system-prompt …` | `--resume <session_id>` |
| OpenCode | `opencode` | `OPENCODE_CONFIG=<file>` with an `mcp.creativecraft` local server (per run) | `run --format json <msg>` | `--session <id>` |
| Hermes Agent | `hermes` | `mcp_servers.creativecraft` in `~/.hermes/config.yaml` (one-time **Set up**) | `chat --query-file <file> --format stream-json --quiet` | `--resume <id>` |
| Antigravity CLI | `agy` | `.agents/mcp_config.json` in the projects folder (merged, per run) | `-p <msg> --output-format stream-json` | `--conversation <id>` |
| Custom | anything | `{mcp_config}` placeholder → a standard `mcpServers` JSON file | your template; `{prompt}` is the message | — |

Config files CreativeCraft writes live in `<projects>/.creativecraft/agent/` and `.agents/`; both
are excluded from cloud sync.

**Permissions.** By default only the hub's tools are pre-approved (Claude Code:
`--allowedTools mcp__creativecraft`); anything else the agent would need to ask about is refused
in a headless run. "Let the agent use all its tools" in Settings passes each CLI's own
auto-approve flag (`--dangerously-skip-permissions`, `--auto`, `--yolo`). Leave it off unless you
trust what you're asking for.

**Hermes setup.** Hermes reads MCP servers from its global config only, so CreativeCraft adds a
`creativecraft` entry there once. It edits the file only if it has no `mcp_servers:` block yet
(after writing a backup); otherwise it shows the lines to paste.

## Output parsing

Claude Code's `stream-json` is parsed exactly. OpenCode, Hermes and Antigravity emit their own JSON
event shapes; a tolerant parser reads the common fields (type, text, tool name, session id), and any
line that isn't JSON is shown as plain text, so a format change degrades to a plain transcript
rather than a blank chat. Fixtures are in `crates/agent/src/parse.rs`.

## Keeping up with the CLIs

These tools move fast. Flags were taken from each project's docs in October 2026:

- Claude Code CLI reference: https://code.claude.com/docs/en/cli-reference
- OpenCode CLI and MCP config: https://opencode.ai/docs/cli/ · https://opencode.ai/docs/mcp-servers
- Hermes Agent CLI and MCP: https://hermes-agent.nousresearch.com/docs/reference/cli-commands · https://hermes-agent.nousresearch.com/docs/user-guide/features/mcp
- Antigravity CLI headless mode and MCP: https://antigravity.google/docs/cli/headless · https://antigravity.google/docs/mcp

When one changes: capture a real transcript (`<cli> … > fixture.jsonl`), add it as a parser test,
then adjust `crates/agent/src/backend.rs`. Things most worth verifying on first run: OpenCode's and
Hermes' JSON event field names, and Hermes' `--query-file` together with `chat`.
