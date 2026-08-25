---
name: appa-tool-sync
description: Probe every MCP server available to this Claude Code installation, use the ready-made rules OpenAPPA ships for servers it knows, mark the remaining tools by what each one reads and sends, and write it all into the policy config of the currently running APPA runtime for the user to review. Accepts optional extra instructions as an argument. Use when the user installs a new MCP server, wants the APPA policy to cover their MCP tools, or sees calls blocked as undeclared tools.
---

# appa-tool-sync

Bring the running runtime's policy up to date with the MCP tools
actually installed. OpenAPPA ships ready-made rules for some tool sets
(the repository's `batteries/` directory); use those first. Declare
every other tool yourself and mark it: what its result carries, and
whether it sends data out of the session. Mark what the tool's own
purpose makes plain. Ask the user once about the servers you cannot
judge.

Marking is a grant. A tool the policy does not name is blocked, so
every entry you add releases something. The user sees the full
overview in step 7 and approves it before anything is written.

This skill edits one configuration file and calls one endpoint. It
reads no database and no runtime state. It tells you **where to look**,
not what you will find: do not assume the policy dialect or which
servers exist — read them from the machine each time.

## Extra instructions

The user can pass extra instructions when invoking the skill
(`/appa-tool-sync <instructions>`). Read them before step 1. They can
name servers to skip, ready-made rules to skip, marks to force, or
the config path. They
override the defaults in this document. They do not skip the approval
in step 7 — the user still confirms the overview.

## How to speak to the user

Short sentences. Plain words. No jargon.

- Work as a dialogue, not a report: one short message per step, one
  question at a time, a clear call to action — "approve, or tell me
  what to change". Never one wall of text.
- Say what happened, then say what to do. Do not explain the model,
  the dimensions, or the algebra. Never put a rule id, a TOML key, or
  a term like *delta*, *audience*, or *label* in a sentence addressed
  to the user. Write "these tools can send data outside", not "these
  tools require a public audience". Say "ready-made rules for Slack",
  never "battery" or "include".
- Do not narrate your own mechanics. Sentences like "the written file
  is byte-identical to the previous sync" mean nothing to the user.
  Say what changed, or "nothing changed", and stop.
- Report only what is there. A check that found nothing gets no
  sentence: never write lines like "nothing in the policy points at a
  tool that is no longer installed" or "no servers needed a question".
- Ask nothing before the scan. After it, at most two questions: one
  about the servers you could not judge (step 6), then the overview
  with its approval (step 7).

## 1. Find the config the runtime is serving

```sh
ps ax -o command | grep appa-runtime | grep -v grep
```

Take the `--config` path from the command line. A process started
without that flag reads `APPA_CONFIG`, or `appa.toml` in its working
directory; if the path is not on the command line, ask the user for it.
Ask as well when no process is running.

The runtime's address is `${APPA_RUNTIME_URL:-http://127.0.0.1:8787}`,
the same variable the plugin's hooks and statusline read.

## 2. Inventory MCP servers and their tools

- `claude mcp list` names the servers configured for this user and
  project.
- The session's own tool surface carries the wire names: MCP tools
  appear as `mcp__<server>__<tool>`, plugin-provided servers as
  `mcp__plugin_<plugin>_<server>__<tool>`. The policy must name the
  exact wire name the harness sends — a readable alias will not match.
- Keep each tool's description. It is the evidence you mark from in
  step 6; a wire name alone often does not say whether a tool reads or
  sends.
- For servers that are configured but not visible in this session
  (disconnected, unauthenticated), report them as unprobed. Do not
  invent their tool lists.

## 3. Look for ready-made rules

OpenAPPA ships rules for tool sets it knows, one directory per set
under `batteries/` in its repository. Each directory holds an
`appa.toml` and, sometimes, small scripts the rules run. Match a
battery by the tool names its `appa.toml` declares, not by the
directory name: the `slack` battery names `mcp__claude_ai_Slack__*`
tools, so it matches a server whose tools carry that prefix. The
`claude-code` battery names the harness's built-in tools and matches
every session.

Find them without the network first:

```sh
ls ~/.claude/plugins/marketplaces/appa/batteries/
```

That directory is the clone of the repository the plugin was installed
from; `claude plugin marketplace update appa` refreshes it. If it or
`batteries/` is missing, list them from GitHub instead:
`gh api repos/archestra-ai/OpenAPPA/contents/batteries --jq '.[].name'`,
and fetch a matched directory's files with
`gh api repos/archestra-ai/OpenAPPA/contents/batteries/<name>`.

For every battery whose declared tool names share a prefix with an
installed server's tools, compare its list with the server's real tool
list from step 2: the rules usually cover a few
tools, not all, and an undeclared tool is blocked. The tools it does
not name are marked by you in step 5, like any other.

Do not run or edit anything in a battery directory.

## 4. Read the current policy

Read the config file from step 1. Learn the tool-entry shape from the
existing entries of the config being edited — the table header, the
key names, and the reader IDs it already writes — and preserve it.
List which tools the policy already declares, and which ready-made
rules the root already includes (`include = [...]` at the top of the
file, one path per battery under `./batteries/`).

## 5. Mark each tool

Use two audience states only. **Public** is the absence of any
restriction. **Private** is one restricted reader set: reuse the
reader ID the config already writes for private data, or write
`private` when it has none.

Mark only the tools no matched ready-made rules declare (step 3).
Decide two things per tool, from its name and its description.

**What its result carries — the `delta`.** A tool that returns content
from a private source narrows the audience:

```toml
delta = { audience = ["private"] }
```

Every other tool carries nothing:

```toml
delta = {}
```

**Whether it sends data out of the session — the `requires`.** A tool
that publishes, posts, sends, shares, or uploads to a place other
people read may carry unrestricted values only:

```toml
requires = { audience = { contains = ["public"] } }
delta = {}
```

Only a Public audience contains `public`, so this is the
wall: nothing a private tool returned reaches that sink. A tool that
publishes nowhere gets no `requires`.

Two rules the loader enforces:

- Write a `delta` key on every entry. A `requires` on an entry with no
  `delta` is refused at load.
- A `delta` audience is a bare list of readers. A `requires` audience
  names its check instead: `contains` (the flow's readers include every
  reader listed) or `within` (the flow's readers are all among those
  listed). A bare list under `requires` is a load error.

A tool can be both. One that reads private data and sends it outward
carries the private `delta` and the public `requires` together, and is
then blocked until a remedy plan clears the gap.

This skill marks the audience dimension only. It sets no `trust`, no
attention marks, and no effects. If that limit is worth telling the
user, one line in the step 9 close is the place — not earlier.

## 6. Ask once, about the servers you could not mark

Some servers state their purpose plainly. A web search returns public
pages. A local filesystem or a notes server returns private ones. Mark
those yourself.

For the rest, ask **one** question, about servers and not tools: which
of these give data that must stay private? Put every
unclear server in that single question and let the user select. Do not
ask per tool. Do not ask a second question about the tools that send —
mark those from their descriptions and show them in step 7.

## 7. Show the marks you came up with, then get approval or corrections

Before writing anything, show how each server ends up configured.
Compare the inventory against the declarations:

- installed but undeclared → candidate entries;
- declared but no longer installed → tell the user, never delete
  unasked.

Group the overview by server. One line per server: its name, how many
tools, and the mark in plain words — no restriction, keeps data
private, or can send data outside. A server with ready-made rules
says so on its line, in one of three forms:

- "ready-made rules cover 2 of your 14 Slack tools; I'll add the other 12";
- "ready-made rules already in place" — the root includes them from an
  earlier run;
- nothing about ready-made rules — none exist for this server. Expand to one line per tool only where the
tools of one server differ. Name the tools that can send data outside
separately — those decide what gets blocked later.

Print the overview as plain text in the chat, end your turn with the
call to action on the last line, and wait for the reply. Do not use a
question dialog (AskUserQuestion): its preview box truncates long
plans, and the user must see every server. Never ask "write N
entries?" with only a count.

The call to action is one line: approve, or name anything you want
treated with extra care — data that must stay private, servers to
skip, marks to change. Apply each correction, show the changed lines
again, and repeat until the user approves. Never write the config
without approval.

## 8. Write the config

For each ready-made rule set the user approved and the root does not
include yet: copy its whole directory — the `appa.toml` and every
script beside it — from the clone (or from GitHub) to
`<config directory>/batteries/<name>/`, next to the config file, and
add its path to the root's `include` list:

```toml
include = ["./batteries/claude-code/appa.toml", "./batteries/slack/appa.toml"]
```

Copy, do not link: the path must stay valid when the plugin updates,
and the scripts run from that directory. A directory that is already
there is left alone — its rules are in place — unless the user asked
to refresh it; then replace it.

Root rules run before included ones, so every entry you generate goes
into the root file. Never edit a file under `batteries/`.

Apply the approved entries to the config file, preserving its
existing entries and comments. The user approved the exact entries in
step 7, so do not show a diff. Show one only when the write had to
deviate from what was approved.

## 9. Reload the runtime

```sh
curl --fail-with-body -sS -X POST "${APPA_RUNTIME_URL:-http://127.0.0.1:8787}/reload"
```

The process keeps serving and no session is interrupted. The runtime
validates the file before it installs it: a refusal answers 422 with
the reason, changes nothing, and leaves the policy that was already
serving in place. Give the user the reason in plain words, fix the
config, and reload again.

Then close in about three sentences. What happened, and what it
protects:

```text
Added ready-made rules for Slack and Claude Code, and 9 more tools
from 3 servers. The policy is live now.
Two of them can send data outside: <name>, <name>.
Notes and files stay private, so those two will block them.
```

Do not put the new-session reminder here — it opens step 10. Only when
step 10 is skipped does the reminder end the close instead, as the same
warning line step 10 prescribes.

## 10. Offer one prompt that shows the protection

After the close, offer the user one prompt they can paste into a new
session to watch the protection work. Open with the new-session
warning, bold with a warning emoji, then the paste invitation in the
same paragraph:

```text
⚠️ **The new tools do not work in this session — it keeps the policy
it started with. Start a new `clappa` session to use them.** If you
want to watch the protection work, paste this into that new session:
```

The warning is easy to get wrong: a session keeps the policy file it
started with, so the tools you just added do not work in the session
that added them. Name `clappa` — only sessions started with it are
protected.

Then give the prompt as a blockquote. Compose it from the entries you
just wrote — never from a fixed example and never from this document:

- Pick one tool you marked as keeping data private. Steer away from
  the most sensitive sources — meeting recordings, mail.
- Pick one tool you marked as able to send data outside. Prefer one
  the user has already authenticated and whose target exists, so the
  only failure the demo can show is the block.
- Write the prompt as three numbered steps: fetch one real item with
  the first tool; write a short summary that copies a few exact lines
  from it; send that summary with the second tool. Name a concrete
  destination the user owns. End the prompt with: use only MCP tools —
  no Bash, no local files.

The copied lines are what makes the demo certain: they make the sent
text visibly come from the private data, so the send is refused on
every run, not judged case by case.

Then tell the user, in two sentences and plain words, what they will
see: the fetch works and the data comes in; the send is refused,
because the summary carries what the private tool returned.

If the sync wrote no private tool or no sending tool, skip this step
and say nothing about it — but keep the warning: it then ends the
step 9 close as its last line.
