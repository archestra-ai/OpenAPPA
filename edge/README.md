# appa-edge

The layer between protocol adapters and `appa-core`. Protocol-agnostic: it
knows conversations and verdicts, never wire formats.

```
harness / protocol adapter  ⇄  appa-edge  ⇄  appa-core
        (wire formats)     (sessions, verdicts)  (pure policy)
```

Every embedding of appa-core hand-rolls the same logic: build a trajectory
from conversation history, label ingress, translate proposed tool calls into
requests, run `pursue`, act on the verdict, drive the dispatch cycle. This is
the code where a mistake is a security hole. appa-edge implements it once;
adapters drive it and render its typed verdicts into their own wire text.

## Concepts

**Session** — one conversation's working state, in memory only. appa-edge
never stores it; the adapter keeps the source history and reconstructs a
fresh session from it by driving it in conversation order: feed a user turn
(with its label — a required argument, the edge has no default), feed an
assistant turn's proposed calls, feed a tool result, ask for a verdict.
Engine construction is encapsulated in `Session::new` — the single seam
contracts pass through.

**Verdict loop** — one proposed tool call in, one typed verdict out.
appa-edge builds the engine's request itself — adapters never assemble one.
`verdict` is check-only (the adapter's harness executes permitted calls);
`dispatch` also drives the engine's dispatch cycle: spend the token to obtain
the canonical request, hand exactly that to the adapter's executor, close the
action with what came back. Adapters never touch tokens or receipts. Every
blocked outcome is fail-closed.

**AuthorityResolver** — when a verdict defers to an external `Authority`,
appa-edge performs the outbound call and feeds the ruling back. Outbound
only: the edge is a client everywhere, it never listens. The edge never
rules — on timeout, transport error, or no resolver, no ruling is applied at
all; the flow fails closed by the absence of a grant. Shipped implementation:
`WebhookResolver`, built from the policy's declared endpoints
(`Contracts::endpoints`): each approval is POSTed to the endpoint of exactly
the authority it names, with that endpoint's timeout; an authority with no
declared endpoint fails closed without any HTTP call. The client follows no
redirects, uses no ambient proxy, and never retries — one ruling per
approval. With `NoResolver`, escalations simply remain blocked.

**Transformers and canonical arguments** — `Session::new` registers the
policy's inline transformers (`Contracts::transformers`) beside its
contracts and authorities; the engine's remedy walk applies them without
any resolver — inline code needs no channel. When a derivation substitutes
a call's payload, the granted verdict carries the **canonical arguments**
(the exact bytes the engine checked); the adapter must ship those, never
the proposal's. Before surfacing them, the edge runs a recipient-integrity
guard: if the derivation rewrote a contract-designated recipient field away
from the checked recipient set — or made it unreadable — the flow ends in
a distinct `IntegrityBlocked` verdict, never a dispatch (and never a
fabricated `Terminal`, which stays reserved for the engine's proven
no-remedy claim).

**Cancellation** — `verdict`/`dispatch` hold linear core capabilities across
their awaits. A future dropped mid-await poisons the session: every further
mutating call fails with `EdgeError::Poisoned`, and the adapter reconstructs
from source history. Fail closed — a capability that crosses a state change
or a process boundary is dead.

## Non-goals

- Policy authoring and TOML parsing — stays in `appa-contracts`.
- Wire formats — stays in the protocol adapters (`appa-proxy` is the first).
- Any change to `appa-core`'s surface or invariants.
