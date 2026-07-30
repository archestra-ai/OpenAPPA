# Design rationale

Why APPA works the way it does. Nothing here is normative — `spec.md`
settles what the engine must do, and this file records the arguments behind
it, including the alternatives that lost.

## Labels

### Why labels never widen

Every delta restricts, which makes the current label a fold over the run's
history: intersect the readers, take the lowest trust. Three properties fall
out of that one restriction. The label settles rather than oscillating, so
the fold terminates on a finite chain. Computing it needs no replay, since
the whole history collapses into a running intersection and minimum. And no
sequence of unruled steps can launder a secret back into a public context,
because no step raises anything.

The price is that a genuine widening has nowhere to live in the label. That
is paid by rulings, which admit one dispatch without moving the label, and
by branching, which quarantines the narrowing in a child that dies with it
so the parent keeps its width.

### Why the epoch-wide raise was rejected

The alternative to branching was a ruling-carried permissive delta — an
authority raises the run's label for an epoch, and every send inside that
epoch passes without further review. It was rejected.

A permissive delta reintroduces the sign rule the algebra exists to avoid.
The fold stops being a minimum, the label stops settling, and every proof
that depends on monotonicity acquires a case. Branching buys the same
outcome — many sends under one review — with the widening structurally
unable to outlive the child or cross back to the parent. The companion paper
states the trade and the algebra a raise would drag back in.

### Why two dimensions and not more

Audience and trust are enough because customer taxonomies embed into
audience rather than extending the label. A classification scheme is written
out as its member readers, so `level-3` is a reader set and not a new axis,
and the set operations stay exact.

Budget was the recurring candidate for a third dimension and stays out.
Spend is not a property of information, it is a property of a run, and
gating it belongs in an effect vocabulary plus an authority that keeps the
running account in its own systems. A deployment whose effect vocabulary
sprawls is asking for a workflow engine on top, not a bigger policy engine.

### Why narrowing blocks at all

Reading internal data leaks nothing, so a pure information-flow reading says
allow it silently. APPA stops it because the silent version is a ratchet:
the agent discovers the cost three steps later, at a send that no longer
works, with the data already in its context and nothing left to do about it.

Stopping before the fetch turns that into a choice the agent makes while it
still has options — and while the clean route is still open, since a fetch
can run in a child and cross back sanitized only while the raw bytes are
not yet in the agent's context. The stop costs an acceptance, which is
free, agent-side, and recorded.

## The check

### Why the narrowing check runs first

Label requirements evaluate on the label the dispatch would commit, which
means the call's own delta is already in force. Checked in the other order,
a call could outrun its own consequences.

The concrete attack is a tool that both reads and releases. Give
`search_and_share` a requirement of `audience ⊇ public` and a delta of
`audience = internal`. On the pre-narrowing label the call passes as public,
but the bytes it shares are the internal data its own dispatch commits. With
the narrowing in force the `includes` fails and the release takes a ruling.

History requirements run on the other clock — the log as it stands — so a
call's own `emits` can never satisfy its own precondition.

### Why acceptance is not a security power

An acceptance clears no requirement and grants no access. It acknowledges
that the run is about to lose reach, which is the agent's own business:
nothing is disclosed, no authority is exceeded, and the only party affected
is the agent itself.

Making it a mandate power would put a human in the loop on every fetch of
restricted data, which is the fastest route to approval fatigue and buys
nothing — the fetch was already legal. A deployer who wants human eyes on an
expensive narrowing attaches an attention mark to the narrowing tool, which
is opt-in and explicit.

The two gates compose but never substitute. One call may need both a ruling
for its gap and an acceptance for its narrowing; an authority approving the
dispatch does not accept the loss of reach on the agent's behalf, and the
agent's acceptance clears no requirement.

## State

### Why the label and the log stay separate

The fold direction is not the reason — a union of effect kinds would settle
by the same finiteness argument the label does. Two structural differences
are.

Branching scopes them differently. A label is copied at fork and returns
only through the returned value, while the log is one shared record that
every branch appends to in realtime. An abandoned child returns no value,
and yet its `egress` must stay visible to its parent, because the email is
in someone's inbox regardless of which branch sent it.

Their clocks differ. Label requirements evaluate on the label the call would
commit, history requirements on the log as it stands. Folding events into
the label would silently make history branch-scoped and check it on the
wrong clock.

### Why the engine keeps no counts

Every history check is kind-containment: `prior(k)` and `no_prior(k)` ask
whether a matching effect exists, never how many or how large. The engine
keeps no magnitude view and feeds none to any authority.

An authority whose decision needs "how much has been spent so far" keeps
that account in its own systems, out of band. Putting counters in the engine
would make the log a ledger, and a ledger needs semantics — units,
reconciliation, rollback — that a policy kernel has no business holding.

### Why effects are the sanctioned escape valve

Almost any bespoke gating ritual encodes as effect vocabulary plus a dynamic
authority: a `finance.spend` effect routed to an authority that decides
between auto-approving and paging a human. This is the pressure release the
model needs, and it is better than the alternatives, because the hack shows
up as a named effect in an auditable log rather than as a distortion of the
label rules. Everything else keeps its guarantees.

## Authorities and rulings

### Why a ruling never edits the label

A ruling admits a dispatch despite a gap. The trajectory changes only
through what the admitted call itself commits, so a ruling over a call with
no delta and no emits changes nothing but the log.

The alternative — an authority that rewrites the label — makes the label a
record of who was persuaded rather than a description of the information. It
also destroys the fold: once the label can move up, the run's state stops
being recomputable from the history.

Rulings are call-scoped for the same reason sudo is per-command. One review
admits one rendered call, is consumed by the dispatch it admitted, and
leaves nothing ambient behind.

### Why routing is tags only

A mandate says what an authority may grant; its scope says over which calls.
Tags carry the second question because they have no algebraic life — they
never fold, never enter a check, never reach the log.

Trust, audience and effects are checked currencies and must not double as
routing keys. Coupling jurisdiction to the effect vocabulary would let an
accounting rename silently move an authority's reach.

The consequence is a clean split: soundness is tag-independent and only
completeness is tag-dependent. A mis-tagged catalog routes a gap to the
wrong authority, who still cannot exceed their mandate and still rules on
the rendered call, or fails to route it at all and produces a spuriously
terminal block. A misdeclared effect or audience, by contrast, perturbs the
checks themselves.

### Why APPA does not police who may hold a power

A loader that refused an in-process auto-approver holding a covering
mandate would be protecting the deployer from their own configuration.
Configuration is the trusted base per `THR-3`: an open gate declared there
is legitimate and voids its guarantees auditably, and the policy review —
reading mandates beside their implementations — is where a rubber stamp is
caught. So no rule constrains which implementation may hold which mandate,
and a human approver is the builtin `"hitl"` on the same terms as any
other.

### Why the user cannot approve their own sight of restricted data

When a run is restricted enough that showing content to the user is itself a
release, that release takes a ruling from an authority other than the user —
whatever mandate the user otherwise holds.

The reason is structural rather than a matter of trust. The approval request
would arrive on the very channel being released, so an in-band
self-confirmation is not a check at all.

The bar has a precondition. It bites only where tool credentials outrun
the user's own read rights — an agent on service-account credentials — so
that the run's audience can exclude the user. Where the agent's tools act
with the user's credentials, nothing the run fetches excludes the user,
and the bar is vacuous rather than wrong.

### Why the staged review carries the argument payload

An authority asked to release `send_email(text, recipient)` cannot judge
the release without the text, and a review stripped to labels and
provenance pushes the real decision back onto whoever can see the bytes.
Authorities are already the deployer's trusted base per `THR-3`: a reviewer
the deployer would not show the data to has no business holding a mandate
over its release. The rejected alternative — label-checking the review
itself, with the authority standing as a reader — added a second flow check
for no gain the trusted-base assumption does not already provide.

### Why remedies are safe to hand to a steered agent

Two invariants make plan selection security-irrelevant, even when injected
content is steering the model.

Every suggested plan is individually sound, so which one the agent picks
cannot matter. Selection immunity comes from plan soundness, not from
policing the selector.

Authorities rule on the engine-rendered call plus provenance, never on the
agent's paraphrase. A steered model asking "may I email the compliance
archive?" while omitting that the address is attacker-derived is the
remaining social-engineering channel, and rendering the exact checked call
closes it.

### Why engine-side plans are executable objects with ids

A plan handed over as prose has to be interpreted, and interpretation is
exactly the surface a steered model exploits. An id executes. Plans with
no engine-side step — do-this-call-first remedies — carry no id and are
ordinary separately checked calls (`RMD-2`).

The tool that executes them is present from the start of the run rather than
injected when a block occurs, because introducing tools mid-conversation
invalidates prompt caches.

## Sanitizers

### Why a sanitizer's mandate binds either dimension

Registered externals are trusted to do their jobs, and the interface decides
which job each one is handed. A sanitizer receives bytes and returns bytes,
so its mandate is a claim fixed at registration about every derivation it
will ever produce: that the output discloses only what a declared audience
may see, or that it carries none of the steering a suspicious source might
have planted. Both claims are unconditional in the same way, because the
transformer acts on the value rather than ruling on it, and a failure to act
is a defect in the transformer that `SAN-6` puts on whoever registered it.

Trust looks like the harder of the two claims, because trust is a fact about
provenance and bytes do not carry their own history. What settles it is that
a transformer need not read provenance in order to change it. A reviewer who
deletes the paragraph addressed to the agent has edited the value, so the
derivation that comes back is clean by construction of the edit rather than
by a judgment about where the page came from, and nothing in the algebra
separates that from a redactor stripping account numbers.

What the two dimensions do not share is the record. A sanitizer's
application names a registered transition and the digest it ran on, and that
is the whole entry; an authority's ruling persists the review's typed
context verbatim (`RUL-8`) and reaches the call through a declared scope
(`AUT-7`).
A deployment that needs to know which person cleared which value still wants
an authority, and one that wants a value cleared once for all downstream use
wants a sanitizer. Permission to raise trust does not merge the two
instruments, and a policy review has to read the sanitizer table to know
which one a deployment chose.

A mandate binds a transition, not the information the transition is claimed
over. `remove_pii` is a sound remedy for CRM tickets and a laundering
machine for data that is all PII, and the same gap runs on the trust
dimension, where a transform registered against the injections found in
fetched pages makes the identical claim over every other value routed to
it. Scoping mandates by information type is open work.

## Branching

### Why a child starts at the parent's current label

A child starting at the neutral label could be asked to "summarise what we
know", and the summary would cross back public. That is a laundering
primitive, so the child inherits the parent's label at fork.

### Why history is global across branches

There is one log and every branch appends to it in realtime, so a child's
`egress` fails a parent's `no_prior(egress)`. The conservatism is intended:
an effect is a fact about the world and not about a branch, and the email is
in someone's inbox regardless of which branch sent it. There is no
branch-scoped `prior(k)`.

### Why a void return does not consume the return channel

A child returns at most once, because the fork's mandate covers one errand
and one result. A void return — `submit_result` with no value — ends the
errand while crossing nothing.

A void carries zero bits of child-derived content, so nothing folds into the
parent's label — the same position the parent would be in had the branch
simply died. That is about the label, not the log: a child's effects and
rulings sit in the shared log either way, because one log per family is the
whole design. Since nothing crossed, there is no value crossing to count,
and at-most-once binds value crossings rather than endings.

## Deferred, and why

| item | status | why it isn't live |
|---|---|---|
| input sanitizers (`tool_input`) | design direction | the loader refuses the registration rather than carry an inert one |
| quarantine-exit attestation | design direction | a sanitizer's transition is claimed over the bytes it derives; an attestation is claimed over extracted structure, and no registered kind makes that claim |
| named audience groups with membership resolvers | design direction | trades revocation freshness for exactness of the set operations; the current dialect keeps exactness |
| the response sink's contract | out of scope | only the structural bar on user self-approval is normative in this version |
| external interface protocols | placeholder | see `EXT` in `spec.md` |

## Accepted gaps

| gap | why it is accepted |
|---|---|
| invoke/append crash | effects append on success, so a host failing between a successful invoke and the append may lose effects. A durable outbox committing invocation and effects as one record is future work for the outer layer. |
| failed send, real egress | a call that dispatched and failed appends nothing, so `no_prior(egress)` can pass after a send that reached an inbox. A positive `prior(k)` proves the tool reported success and nothing more about the outer world. |
| confined-result crash | a pending-cast success checkpoints its effects durably before the confined offer exists in host memory, so a crash between the two leaves an open dispatch whose lapse record never lands. Effects stand honestly; the close is lost with the host. |
| approval fatigue | a deployment concern rather than an engine one, and the real attack surface of any human-in-the-loop scheme. `guide.md` states the operational cost. |
