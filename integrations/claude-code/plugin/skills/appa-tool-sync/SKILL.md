---
name: appa-tool-sync
description: Probe every MCP server available to this Claude Code installation, collect their tools' wire names, and declare them in the policy config of the currently running APPA runtime for the user to review. Use when the user installs a new MCP server, wants the APPA policy to cover their MCP tools, or sees calls blocked as undeclared tools.
---

# appa-tool-sync

Bring the running runtime's policy up to date with the MCP tools
actually installed. You inventory and declare; what each tool is
allowed to do is a later policy edit the user makes. A name-only entry
admits results as fully unknown (fail-closed), so declaring a tool
never silently releases anything.

This skill tells you **where to look**, not what you will find. Do not
assume the policy dialect, the runtime's flags, or which servers exist —
read them from the machine each time.

## 1. Find the running runtime and its config

```sh
ps ax -o command | grep 'appa-runtime-v2 --config' | grep -v grep
```

Take the `--config` and `--db` paths from the command line. If no
process is running, ask the user for the config path before proceeding.

## 2. Inventory MCP servers and their tools

- `claude mcp list` names the servers configured for this user and
  project.
- The session's own tool surface carries the wire names: MCP tools
  appear as `mcp__<server>__<tool>`, plugin-provided servers as
  `mcp__plugin_<plugin>_<server>__<tool>`. The policy must name the
  exact wire name the harness sends — a readable alias will not match.
- For servers that are configured but not visible in this session
  (disconnected, unauthenticated), report them as unprobed. Do not
  invent their tool lists.

## 3. Read the current policy

Read the config file from step 1. Learn the tool-entry shape from the
existing entries of the config being edited — do not write keys from
memory. List which tools the policy already declares.

## 4. Propose the diff, then get a decision

Compare the inventory against the declarations:

- installed but undeclared → candidate entries;
- declared but no longer installed → flag for the user, never delete
  unasked.

Show the candidate list grouped by server and let the user confirm or
trim it. Write each confirmed tool as a name-only entry — no `delta`
key — so its results are admitted as fully unknown (fail-closed). Do
not propose per-tool annotations and do not reason from the shipped
examples: this skill only declares tools. Annotating them is a
separate policy edit the user makes in the config afterwards.

## 5. Write the config

Apply the confirmed entries to the config file, preserving its
existing entries and comments. Show the diff.

## 6. Restart the runtime — who does it depends on gating

A changed policy is a new deployment: the runtime refuses to open the
old database, so the process must be restarted with a fresh `--db`
path. Whether you can do that yourself depends on whether **this**
session is gated.

Detect it from the store: a gated session has a log under its own id.
Query the `--db` from step 1 for a row whose `root` is `cc:` followed by
this conversation's session id:

```sh
sqlite3 <db> "SELECT 1 FROM logs WHERE root = 'cc:<session-id>' LIMIT 1;"
```

A row means this session is gated by that runtime.

Either way, first warn the user that any other gated session still
running is attached to the old deployment and should be wound down
before the restart — do not assume such sessions survive it.

- **This session is not gated**: restart it yourself — stop the
  process, start it with the same flags and a fresh `--db` path, and
  verify with `curl /health` before reporting done.
- **This session is gated**: you cannot restart it from inside —
  between stop and start every command of this session is blocked,
  including the start command, and a combined stop-and-start command
  wedges the session instead (the new process opens a fresh database
  that has no record of this session). Print the exact commands for the
  user to run in a terminal, built from the process line found in
  step 1 (same flags, new `--db`), and say clearly that gated sessions
  are blocked while the process is down.
