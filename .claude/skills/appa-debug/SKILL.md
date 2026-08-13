---
name: appa-debug
description: Explain what the APPA runtime-v2 recorded for a gated Claude Code session — which tool calls ran, which were blocked, and why — in plain language, by reading the runtime's SQLite database and the session transcript. Use when the user asks why a tool call was blocked, what happened in a gated session, or wants an APPA decision log explained.
---

# appa-debug

Reconstruct and explain a gated session from the APPA runtime-v2 store.
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
- a **screenshot or pasted excerpt** of the gated session showing the
  calls and the `[appa]` error text.

If the user gave neither, ask for one before proceeding. A screenshot
alone is workable: match its prompt/tool names against the store's
recorded requests to find the session. If several sessions match or
none does, show the candidates (id + first line of prompt) and ask.

## 1. Find the store and the policy

The db and config paths are flags of the running process:

```sh
ps ax -o command | grep 'appa-runtime-v2 --config' | grep -v grep
```

If no process is running, ask the user for the `.db` path. Read the
config file it names: every explanation must be grounded in what the
policy actually declares for the tools involved (their `[[policy.tool]]`
entries, `delta`, `requires`, and any casts/authorities/sanitizers).

If the runtime wrote a log (`-v`), find it too — it records one line per
decision, in order, and is the quickest released/blocked timeline.

## 2. Explore the store

Discover the schema instead of assuming it:

```sh
sqlite3 <db> .schema
```

Orient from what you find — typically: which trajectories exist (and
which are subagents of which), what the user asked, which tool calls
were released and their state, and whether any offers are pending.
Trajectory ids embed the harness session id; use that plus the recorded
request text to match the session from step 0.

## 3. Decode the recorded trail

The log rows are the actual record — decode their bytes (inspect the
format; deserialize accordingly, one fact per line). For what each fact
kind means, read the fact definitions in `appa-engine/src/fact.rs` and
the glossary in `docs/spec.md`; do not guess from names.

While decoding, build:

- the ordered list of released calls and their outcomes;
- every admitted value with its label, numbered in admission order
  (errors cite `ValueId(N)` — map N back to the producing tool via its
  provenance);
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

- **Undeclared tool** — no policy entry for the name; not a label
  problem.
- **Unestablished dimension** — a consumed value was never labeled;
  name the tool that produced it.
- **Requirement gap** — the session's accumulated label cannot satisfy
  the tool's `requires`; name the value that narrowed the mix and the
  policy line that demands more.

A block can have several reasons at once while the error text surfaces
only one — check for the others and explain all that hold.

Before stating whether a block is final or liftable, check what the
current runtime actually supports (casts, authorities, offers):
`appa-runtime-v2/CLAUDE.md` and `docs/engine.md` record the current
state and interims. Do not assert capabilities or gaps from memory.

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
| Unknown dimension | unstamped — nobody declared who may see it |
| fold / bound | the mix: strictest ingredient wins, intersection never widens |
| dispatch | a released tool call |
| cast | an examiner that inspects a value and stamps it |
| offer / remedy plan | a proposed narrower alternative |
| delta | the policy's claim about what a tool's output carries |
| requires | the condition a tool demands of data flowing into it |

Rules for the explanation:

- Ground every claim in a specific fact, dispatch row, policy line, or
  transcript line; quote the exact `[appa]` error the user saw.
- If the surfaced error names a `ValueId`, always resolve it to "the
  result of tool X" — the number alone explains nothing.
- Name the concrete change that would lift a liftable block (annotate
  the tool with `delta`, declare the tool, widen an audience), and note
  when a policy change means restarting on a fresh `--db`.
- Do not speculate beyond the log. If the db and transcript disagree or
  a row is missing, say so.
- Plain language means literal language: no metaphors or imagery ("the
  poison entered, but nothing ever tried to drink it"). State the
  mechanism directly ("the unstamped value was in the session, but no
  call that requires that dimension was ever proposed").
