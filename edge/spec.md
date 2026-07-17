# appa-edge

The layer between protocol adapters and `appa-core`. Protocol-agnostic: it
knows conversations and verdicts, never wire formats.

## Why

Every embedding of appa-core today (appa-proxy `replay::Session`, demo
`gateway::Session`, dojo `policy`) hand-rolls the same logic: build a
trajectory from conversation history, label ingress, translate proposed tool
calls into requests, run `pursue`, act on the verdict, drive the dispatch
cycle. This is the code where a mistake is a security hole, duplicated three
times. appa-edge implements it once.

## Position

```
harness / protocol adapter  ⇄  appa-edge  ⇄  appa-core
        (wire formats)     (sessions, verdicts)  (pure policy)
```

- appa-edge is the **only caller of the engine** (the target position; the
  gateway demo and dojo still drive appa-core directly today).
- appa-edge owns **all async orchestration and I/O lifecycles**. Actual
  protocol I/O is performed through adapter-supplied ports; appa-edge calls
  them and owns what happens before and after each call. appa-core stays pure,
  synchronous, and never calls anyone.
- Protocol translation (OpenAI wire JSON, MCP, AgentDojo bridge) stays in the
  adapters; they drive a appa-edge session directly.

## Concepts

**Session.** One conversation's working state, in memory only — appa-edge
never stores it and never tracks a conversation between requests. The
adapter keeps the source history and reconstructs a fresh session from it
by driving it in conversation order: feed a user turn (with its label),
feed an assistant turn's proposed calls, feed a tool result, ask for a
verdict. The session is dropped when the request is answered — with one
exception:
- *parked for approval* — while an external ruling is in flight, the
  session stays in memory until the ruling is applied or times out, then is
  dropped. This is forced by core's linearity: a `PendingApproval` binds
  the exact trajectory instance it was minted on, so the two must survive
  together or not at all. Parked sessions are bounded by in-flight
  approvals, never by conversation count, and a restart loses them by
  design.

**Labeling.** Every user turn enters the session with a label (who may read
it, how trusted it is), and the label is a required argument — appa-edge
has no default. An adapter that labels every turn identically (the proxy's
fixed TOML label) writes that choice in its own code, where a reviewer can
see it. A default inside appa-edge would hand the same shortcut to every
future adapter silently — an adapter that knows its real users could forget
to label and nothing would fail.

**Verdict loop.** One proposed tool call in, one verdict out. appa-edge
builds the engine's request itself — adapters never assemble one — and
runs the check. For a permitted call it also does the engine's bookkeeping
around execution: spend the token (`release`) to obtain the canonical
request, hand exactly that to the adapter to execute, then close the
action (`record_output` / `record_failure`) with what came back. Adapters
never touch tokens or receipts. Every blocked outcome maps to a
fail-closed result for the adapter.

**AuthorityResolver.** When a verdict defers to an external `Authority`,
appa-edge performs the outbound call (webhook, later MCP / judge model)
and feeds the ruling back. Rules:
- Outbound only. appa-edge is a client everywhere; it never listens.
- appa-edge never rules. A deny is an authority's decision; on timeout,
  transport error, or no resolver for the named authority, appa-edge
  applies no ruling at all — it abandons the pending flow and reports it
  blocked. The flow fails closed by the absence of a grant, never by a
  ruling the authority did not make.
- One ruling per approval; approvals never survive a process restart
  (core's linearity — only rulings are durable, and they live outside).
- Shipped implementation: webhook, routed per authority from the policy's
  declared endpoints (the proxy drives it live). With no resolver
  configured, escalations simply remain blocked.

## Principles

- If an operation requires I/O, then it lives in appa-edge, never in core.
- If an operation can fail open or closed, it fails closed.
- If a capability crosses a state change or a process boundary, it is dead;
  only facts (events, rulings, contracts) are durable.

## Follow-up work: dynamic contract resolution

Out of scope for the first PR. In scope for the design: appa-edge will later
gain a `ContractResolver` that fetches a tool's contract (output label,
requirements, effects) at evaluation time — when a tool call is about to be checked — rather
than from static TOML only. The first PR must not make this hard:

- **Single contract seam.** Engine construction is encapsulated inside
  appa-edge, and contracts reach it through one seam, even while the only
  source is static config. The resolver later becomes a second source behind
  the same seam.
- **Epoch semantics.** A frozen engine never takes a late contract. If a
  resolved contract differs from what the current engine holds, then
  appa-edge builds a fresh engine and the session is reconstructed from the
  adapter's source history; in-flight capabilities die at that boundary,
  fail closed. This is why adapters must stay able to reconstruct a session
  (see Session above). If long-lived sessions later need self-contained
  rebuilds, an edge-owned event log may be introduced then — not now.
- **Trust direction.** A tool never describes itself; contracts come from an
  operator-controlled source. Precedence: pinned static contract → resolved
  contract → no contract (core's fail-closed default). Resolution improves
  precision, never safety — a resolver outage degrades to blocked flows.
- **Resolve at evaluation, nowhere else.** appa-edge calls the resolver
  exactly when a tool call is about to be checked. No background refresh,
  no schedule, no inbound notifications — appa-edge never listens.

## Non-goals

- Policy authoring and TOML parsing — stays in appa-contracts.
- Wire formats — stays in the protocol adapters.
- Any change to appa-core's surface or invariants.

## Concept and type budget

appa-edge reuses appa-core types whenever their meaning is unchanged. It
does not wrap, rename, or duplicate a core entity merely to make it look
edge-owned. When appa-edge implements a core concept, the integration point
is called `<Entity>Resolver`: `AuthorityResolver` implements the outbound leg
of core's external `Authority`, and the follow-up `ContractResolver` resolves
core's `ToolContract`.

appa-edge may define the small number of types needed for its own
orchestration and adapter boundaries, starting with `Session`. A new type must
carry an edge-specific responsibility that no core type already expresses; a
new domain noun needs an explicit reason in this file. The first API should
prefer a few coarse operations over a family of one-method wrapper types.
