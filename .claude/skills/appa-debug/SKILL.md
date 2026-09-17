---
name: appa-debug
description: Explain what the APPA runtime recorded for a protected Claude Code session — which tool calls ran, which were blocked, and why — in plain language, by reading the runtime's SQLite store and the session transcript. Use when the user asks why a tool call was blocked, what happened in a protected session, or wants an APPA decision log explained.
---

# appa-debug

Reconstruct and explain a protected session from the APPA runtime store.
The audience is a person, possibly not an APPA expert: the final
explanation must be plain language, with wire terms shown only where the
user will see them in errors.

This skill tells you **where to look**, not what you will find. Do not
assume schema details, fact shapes, error wording, or which features the
runtime currently supports — read them from the store, the policy, and
the repo docs each time. Ground every claim in something you observed.

## 0. Required input: which session?

You need to identify the session before anything else. Accept either:

- a **Claude Code session id** (a UUID — from the transcript filename,
  `/status`, or a trajectory id in the store), or
- a **screenshot or pasted excerpt** of the protected session showing
  the calls and the `[appa]` error text.

If the user gave neither, ask for one before proceeding. A screenshot
alone is workable: match its prompt/tool names against the store's
recorded requests to find the session. If several sessions match or
none does, show the candidates (id + first line of prompt) and ask.

## 1. Find the deployment: config, store, logs

Start from the installed deployment, not from `ps`:

```sh
appa describe
```

Its `Config:` line names the live config (honouring `APPA_CONFIG` and
`APPA_CONFIG_DIR`), and the rest lists the effective policy tools,
included batteries, Authorities, audience sources, and named audiences.
Then confirm which runtime is actually serving:

```sh
ps ax -o command | grep '[a]ppa runtime'
```

The process is `appa runtime --listen … --config <path> --db <path>`.
Take the store path from its `--db` argument. If it names a different
config than `appa describe`, ask the user which deployment they mean. If
no runtime is running, ask the user for the `.db` path; the installed
one is `appa.db` in the deployment's data directory.

Read the config file: every explanation must be grounded in what the
policy actually declares for the tools involved (their `[[policy.tool]]`
entries, `delta`, `requires`, and any annotators/authorities/sanitizers).

The runtime always writes `runtime.stderr.log` and `runtime.stdout.log`
beside the store in the data directory (`-v`/`-vv` only raise the level).
The stderr log records one line per decision, in order, and is the
quickest released/blocked timeline.

## 2. Explore the store

The store is owned by `appa-eventlog` (repository root). Discover the
schema instead of assuming it:

```sh
sqlite3 <db> .schema
```

Orient from what you find — typically: which trajectories exist (and
which are subagents of which), what the user asked, which tool calls
were released and their state, and whether any offers are pending.
Trajectory ids embed the harness session id; use that plus the recorded
request text to match the session from step 0.

## 3. Decode the recorded trail

The log rows are the actual record — inspect the encoding of the facts
column (`appa-eventlog/src/lib.rs` says how a batch is serialized: an
engine batch is a bare JSON array of facts, and a batch carrying a host
observation is a JSON object with `facts` and `host`) and decode
accordingly, one fact per line. For what each fact kind means,
read the fact definitions in `appa-engine/src/fact.rs`; do not guess
from names.

While decoding, build:

- the ordered list of released calls and their outcomes;
- every admitted value with its label, numbered in admission order, and
  the producing tool from its provenance;
- any subagent starts and returns.

Compare against the harness's view (step 4): a proposed call that left
no trace in the store was decided without appending — the runtime log
and transcript are the evidence for how it was decided.

## 4. Cross-reference the transcript

The Claude Code transcript holds the model's side: every attempted call
and the exact `[appa]` text delivered back. With the session id:

`~/.claude/projects/<cwd-slug>/<session-id>.jsonl` — scan `assistant`
entries for `tool_use` (name + input) and `user` entries for
`tool_result` whose content contains `[appa]`.

If only a screenshot was provided, use it as the transcript excerpt and
say so in the explanation.

## 5. Diagnose blocks

Rebuild the label state at the moment of the block by folding the
admitted values in order, then compare against the blocked tool's
declarations in the policy. Distinguish at least:

- **Undeclared tool** — no policy entry for the name and no wildcard
  tool rule (`name = "*"`) covering it, so the call was refused before
  it was judged; not a label problem. With a wildcard rule, its
  annotator produces the call's complete contract and a block is a
  requirement gap like any other.
- **Annotation failure** — the tool's annotator produced no admissible
  annotation (unreachable, malformed, or naming vocabulary outside its
  mandate), so the runtime failed closed; an operational refusal, not a
  policy denial.
- **Requirement gap** — the session's accumulated label cannot satisfy
  the tool's `requires`; name the value that narrowed the mix and the
  policy line that demands more.

A block can have several reasons at once while the error text surfaces
only one — check for the others and explain all that hold.

Before stating whether a block is final or liftable, check what the
policy language and this deployment actually support: the policy-review
guide `website/content/docs/contracts.md` for annotators, authorities,
sanitizers and offers, and the `appa describe` output for what this
config binds. Do not assert capabilities or gaps from memory.

To reproduce a decision without a live session, write the calls as a
trace and run `appa replay --config <path> <trace>`; the shipped traces
in `examples/tests/` show the format.

## 6. Explain in plain language

Structure: what the user asked the agent to do → the tool calls in
order (released vs blocked) → for each block, the reason in one or two
plain sentences → what (if anything) would unblock it.

Translation table — use the right column in prose, keep the left column
only where it appears verbatim in errors or config the user must touch:

| wire term | say instead |
|---|---|
| trajectory | the session (or "the subagent's run") |
| fact / fact log | the recorded trail |
| label, audience | the stamp saying who may see the data |
| fold / bound | the mix: strictest ingredient wins, intersection never widens |
| dispatch | a released tool call |
| annotator | a per-call examiner that produces the tool's complete contract |
| offer / remedy plan | a proposed narrower alternative |
| delta | the policy's claim about what a tool's output carries |
| requires | the condition a tool demands of data flowing into it |

Rules for the explanation:

- Ground every claim in a specific fact, dispatch row, policy line, or
  transcript line; quote the exact `[appa]` error the user saw.
- Name every value the explanation relies on as "the result of tool X";
  an admission number or id alone explains nothing.
- Name the concrete change that would lift a liftable block (annotate
  the tool with `delta`, declare the tool, widen an audience). A policy
  change takes effect after a runtime reload (`POST /reload`) and only
  for sessions started afterwards; a running session keeps the policy it
  started with.
- Do not speculate beyond the log. If the db and transcript disagree or
  a row is missing, say so.
- Plain language means literal language: no metaphors or imagery ("the
  poison entered, but nothing ever tried to drink it"). State the
  mechanism directly ("the unstamped value was in the session, but no
  call that requires that dimension was ever proposed").
