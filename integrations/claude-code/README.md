# Claude Code integration

Everything needed to gate a Claude Code session through the
appa-runtime-v2 process lives in this directory: the plugin (hooks, the
`execute_remedy_plan` MCP server, the `appa-debug` skill), the
statusline script, example policies, and the install and uninstall
instructions below. The process itself is the `appa-runtime-v2` crate;
its [README](../../appa-runtime-v2/README.md) covers build,
configuration, and start.

How it works, in one paragraph: the plugin registers hooks on every
session event — prompt, tool call, tool result, subagent start and
finish. Each hook posts the event to the runtime process and blocks the
action unless the process answers yes. The hooks fail closed: while the
process is down, every action in a gated session is blocked — silence
never means yes.

## What is here

- `plugin/` — the Claude Code plugin: `hooks/hooks.json`, the `appa`
  MCP server (`.mcp.json`), the `appa-tool-sync` skill (declares
  installed MCP tools in the running runtime's policy), and
  `statusline.sh`.
- `.claude-plugin/marketplace.json` — the marketplace manifest;
  `claude plugin marketplace add` points at this directory.
- `examples/claude-code.appa.toml` — a complete starting policy: every
  built-in Claude Code tool released with the neutral annotation, web
  tool results marked suspicious.
- `examples/claude-code-hitl.appa.toml` — the same plus GitHub MCP
  tools, with issue writes requiring a human sign-off served over MCP
  elicitation.

## Install

Paste this into a Claude Code session started in your OpenAPPA
checkout:

```text
Set up APPA gating for Claude Code sessions I start through the
clappa alias. Work from this OpenAPPA checkout and use absolute
paths everywhere.

1. Statusline overlay: create ~/.claude/appa-session-settings.json
   whose statusLine runs
   <checkout>/integrations/claude-code/plugin/statusline.sh. If my
   ~/.claude/settings.json already has a statusLine, keep it: point the
   overlay at one wrapper script that pipes stdin to both and prints my
   existing rows first, APPA's beneath. Leave ~/.claude/settings.json
   untouched — the overlay loads only through the alias.

2. Alias: add to my shell rc, as one line:
     alias clappa='claude --settings ~/.claude/appa-session-settings.json --plugin-dir <checkout>/integrations/claude-code/plugin'
   Remind me that only sessions started with clappa are gated, and
   that a gated session blocks whenever the runtime process is down.

3. Check the runtime: curl -sS -m 2 http://127.0.0.1:8787/health should
   print "ok". If it does not, offer to start it with the default
   policy. Copy the default example to a working config first — do not
   point the process at the shipped example in place; later policy
   edits belong in the copy:
     cp integrations/claude-code/examples/claude-code.appa.toml appa.toml
     cargo run --bin appa-runtime-v2 -- --config appa.toml --db appa.db
   Run it in the background and keep it running after you finish.

4. The default policy names only Claude Code's built-in tools, and APPA
   blocks a tool the policy does not name — every MCP tool I have
   installed stays blocked until the policy declares it. Close by
   telling me to start a gated session and run /appa-tool-sync there:
   the skill ships with the plugin, so it exists only in clappa
   sessions. It inventories my MCP servers and proposes policy entries
   for their tools, and I review each one.

Show me what you changed when you are done.
```

The alias form is opt-in gating: nothing stops a plain `claude` in the
same repo, and nothing outside a clappa session changes.

If the process listens on a port other than 8787, set
`APPA_RUNTIME_URL=http://127.0.0.1:<port>` in the session's
environment; the hooks and the MCP server both follow it.

## Uninstall

Paste this into a Claude Code session. If this session is gated, the
runtime must still be running — a blocked session cannot run its own
uninstall:

```text
Remove APPA gating from my Claude Code sessions. Remove only the
Claude Code <-> APPA wiring and shut the runtime down; the policy
survives the uninstall.

1. Remove the clappa alias from my shell rc.

2. Statusline: delete ~/.claude/appa-session-settings.json and the
   wrapper script it points at, if there is one. Do not touch the
   statusLine in my ~/.claude/settings.json.

3. Find the appa-runtime-v2 process (ps ax | grep appa-runtime-v2), show
   it to me, and ask before killing it. The .db file it was started with
   can be deleted too if I say so. Never delete or offer to delete the
   policy config the process was started with (--config, e.g.
   appa.toml) — the policy and its edits outlive the integration.

Changes take effect in new sessions; this one keeps its hooks until it
ends.
```

If a gated session is blocked because the runtime is not running, start
a plain `claude` session instead — the alias gates nothing else.

## Statusline, manually

Claude Code reads `statusLine` only from your own settings — a plugin
cannot set it. The script shows the APPA pixel mascot plus the
session's current Trust and Audience, read from the process's
`GET /status`. It needs `jq` and `curl` and fails open: runtime down,
session not gated, or tools missing all print the mascot alone, never
a blocked action.

To set it without delegating to Claude, write this to the overlay file
the alias loads (`~/.claude/appa-session-settings.json`), pointing at
your checkout:

```json
{
  "statusLine": {
    "type": "command",
    "command": "/path/to/OpenAPPA/integrations/claude-code/plugin/statusline.sh"
  }
}
```

Because the overlay loads only through the alias's `--settings` flag,
plain `claude` sessions keep whatever statusline your own settings
name. Merge the same block into `~/.claude/settings.json` instead if
you want the mascot in every session; it fails open, so ungated
sessions just show the mascot alone.

To keep an existing statusline (for example claude-powerline) and add
the APPA rows beneath it, run both and tee stdin. Pin the exact version
you vetted — `@latest` would fetch and run new third-party code on
every statusline refresh:

```json
{
  "statusLine": {
    "type": "command",
    "command": "input=$(cat); printf '%s' \"$input\" | npx -y @owloops/claude-powerline@1.4.0; printf '%s' \"$input\" | /path/to/OpenAPPA/integrations/claude-code/plugin/statusline.sh"
  }
}
```

## Things to know

- **A changed policy is a new deployment.** Edit `[policy]` and the old
  database refuses to open; start the process on a fresh `--db` path.
- **Stopping the process blocks gated sessions.** That is the design,
  not a fault. Start a plain `claude` if you want an ungated session.
- **The plugin adds roughly zero tokens to a session.** The gating is
  hooks and an MCP server, not prompt text.
