---
name: appa-tool-sync
description: Inventory the MCP servers and toolsets available to this Claude Code installation, present one plan for how each tool's data is treated, and on approval generate the policy entries — spawning subagents to write the rules and the dynamic resolvers for tools whose sensitivity depends on their arguments — then write the running APPA runtime's policy and reload it. Accepts optional extra instructions as an argument. Use when the user installs a new MCP server, wants the APPA policy to cover their tools, or sees calls blocked as undeclared tools.
---

# appa-tool-sync

Bring the running runtime's policy up to date with the tools actually
installed. Three phases, in order: **inventory** the toolsets and MCP
servers, present **one plan**, and on approval **generate** — subagents
draft the policy entries and the dynamic resolvers, you assemble,
write, and reload.

Marking is a grant. A tool the policy does not name is blocked, so
every entry you add releases something. Nothing is written before the
user approves the plan.

This skill edits one configuration file, may add resolver scripts
beside it, and calls one endpoint. It tells you **where to look**, not
what you will find: do not assume the policy dialect or which servers
exist — read them from the machine each time.

## Extra instructions

The user can pass extra instructions when invoking the skill
(`/appa-tool-sync <instructions>`). Read them before phase 1. They can
name servers to skip, marks to force, or the config path; a single
blocked flow as the argument scopes the whole run to fixing that flow
narrowly. They override the defaults in this document. They do not
skip the approval — the user still confirms the plan.

## How to speak to the user

Short sentences. Plain words. No jargon.

- One short message per phase. Never a wall of text, and never a
  question dialog (AskUserQuestion): the plan must be fully visible,
  and corrections come as a reply, not a menu.
- Say what happened, then say what to do. Do not explain the model or
  the algebra. Never put a rule id, a TOML key, or a term like
  *delta*, *audience*, or *label* in a sentence addressed to the user.
  Write "these tools can send data outside", not "these tools require
  a public audience".
- Report only what is there. A check that found nothing gets no
  sentence.
- Ask nothing before the plan. The plan carries your assumptions;
  the user's reply corrects them.
- Pace: the user should see the plan, not the research. Phases 1 and 2
  are mechanical — batch the lookups and decide from what is in front
  of you. A minute of silence before the plan is a failure.
- Never read memory files, `MEMORY.md`, project docs, or anything
  beyond this file and the machine state it names. Recalled memory
  describes past machine states and stale pitfalls; every rule this
  run needs is written here, and the machine is the source of truth.

## Phase 1 — inventory

Two tool calls in most sessions: one shell call for the runtime
process, one read of the config. The tool surface — wire names,
descriptions, which servers answered, which need auth — is already in
your context; do not re-probe it. Run `claude mcp list` only when the
context leaves a server's state genuinely unclear: it health-probes
every server and waits out the timeouts of the dead ones.

Find the runtime and its config:

```sh
ps ax -o command | grep appa-runtime-v2 | grep -v grep
```

Take the `--config` path from the command line. A process started
without the flag reads `APPA_CONFIG`, or `appa.toml` in its working
directory; when the path is not on the command line — or no process
runs — ask the user for it, the one question this skill ever asks
outside the plan. The runtime's address is
`${APPA_RUNTIME_URL:-http://127.0.0.1:8787}`.

Then walk the tool surface:

- `claude mcp list` names the configured servers. The session's own
  tool surface carries the wire names: `mcp__<server>__<tool>`, and
  `mcp__plugin_<plugin>_<server>__<tool>` for plugin-provided servers.
  The policy must name the exact wire name the harness sends.
- Keep each tool's description — it is the evidence the plan marks
  from.
- A server that is configured but does not connect (refused,
  unauthenticated) is **skipped**: one line in the plan names it and
  says to sync again once it connects. Never probe it with questions,
  never invent its tools.
- A gateway server (search/run tools that proxy other servers) is
  unpacked: plan for the downstream servers it fronts, not for the
  gateway's own generic tools.
- **Never declare the runtime's own remedy tool**
  (`mcp__plugin_appa-runtime_appa__execute_remedy_plan`, or whatever
  reserved name the running plugin serves): the runtime provides it
  and refuses a policy that names it.
- Read the config: which tools are already declared, and the entry
  shape — key names, reader IDs, comments — to preserve.

## Phase 2 — one plan, one approval

Decide, from each tool's name and description, how its data is
treated. Three treatments, in plain words:

- **stays private** — the result is the user's own content (mail,
  files, notes, dashboards): the entry narrows the audience.
- **can send outside** — the tool publishes, posts, shares, or
  uploads where other people read: the entry requires unrestricted
  data, and that is the wall — nothing private reaches it.
- **depends on what it touches** — the same tool reads public and
  private things depending on an argument (a file path, a URL, a
  channel): the entry binds a **dynamic resolver** that labels each
  call by that argument. Name the argument in the plan.

When a description does not decide the treatment, pick the safer one —
private over public, resolver over a blanket mark — and state the
assumption as part of that server's plan line. The user flips it in
their reply if it is wrong. Do not ask beforehand.

Print the plan as plain text, grouped by server: one line per server —
name, tool count, treatment — expanding to per-tool lines only where
one server's tools differ. Name every tool that can send outside; name
every resolver with the argument it keys on; name the skipped servers.
Also compare against the config: declared-but-uninstalled tools are
reported, never deleted unasked.

End the message with one call to action: approve, or say what to
change. Apply corrections, show the changed lines, repeat until
approved. Never write without approval.

## Generation reference

Hand this to the drafting subagents; it never reaches the user. Use
two audience states only: **public** is the absence of restriction;
**private** is one restricted reader set — reuse the reader ID the
config already writes, or `private` when it has none.

```toml
# stays private — the result narrows the audience:
delta = { audience = { exactly = ["private"] } }

# can send outside — the sink takes unrestricted values only:
requires = { audience = { includes = ["public"] } }
delta = {}
```

Two rules the loader enforces: an entry carrying `requires` must also
carry a `delta` key (`UNK-7`), and every audience mention carries its
operator — `exactly`, `includes`, `cap`, `may_add` — a bare list is a
load error (`CFG-8`). A tool that both reads private data and sends
outward carries both lines and is then blocked until a remedy clears
the gap. This skill marks the audience dimension only: no `trust`, no
attention marks, no effects.

## Phase 3 — generate, write, reload

On approval, spawn subagents in parallel (the Agent tool), and give
each one only its own job:

- **One subagent per server** drafts that server's policy entries, in
  the exact dialect of the config being edited, from the approved
  treatments. Tools the plan leaves untreated are named bare — the
  runtime admits their results as fully unknown, which blocks nothing
  by itself and keeps them visible as ungenerated.
- **One subagent per resolver** generates it: a small loopback HTTP
  service beside the config (`<config dir>/resolvers/<name>`), plus
  the config wiring that binds the tool's keyed argument to it. The
  contract is on the machine, not in this file: the subagent reads the
  config's existing dynamic-resolver entries, and when there are none,
  the served runtime's own documentation of its externals, before
  writing either the script or the wiring. The wiring must declare the
  argument it keys on, or the reload will refuse it.

Assemble the drafts yourself: apply them to the config file,
preserving existing entries and comments. Start each generated
resolver (background, loopback only) and check it answers before
reloading. Then:

```sh
curl --fail-with-body -sS -X POST "${APPA_RUNTIME_URL:-http://127.0.0.1:8787}/reload"
```

The process keeps serving and no session is interrupted. A refusal
answers 422 with the reason and changes nothing: give the user the
reason in plain words, fix, and reload again.

Close in about five sentences. What happened, and what to do:

```text
Added 9 tools from 3 servers. The policy is live now.
Two can send data outside: <name>, <name>.
<server>'s reads are labeled per file by a resolver I generated;
it must be running or those reads are blocked — restart it with
<command> after a reboot.
To use the new tools, start a new clappa session.
```

Two closing lines matter and are easy to get wrong. A generated
resolver is a running process: name the restart command, because after
a reboot its tools' reads are blocked until it is back. And a session
keeps the policy it started with, so the tools you just added do not
work in the session that added them — name `clappa`, since only
sessions started with it are protected.
