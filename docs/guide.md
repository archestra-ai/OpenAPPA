# APPA: the guide

This is the whole model in one sitting. It states what APPA guarantees and
what it costs, and leaves the proofs to `spec.md`.

## What APPA does

APPA sits between an agent and its tools and answers one question before
every call: may this data go there? It either allows the call, or refuses it
and names what would make it pass. To answer, the engine does not look at
the call alone: everything the agent has read so far folds into one running
label, and the tool's contract is checked against that — so the same call is
legal early in a run and refused once the agent has touched a customer
record.

**The label** travels with the data: who may read it, how far you can trust
it. Reading anything folds its label into the run's, and it stays folded for
the rest of the run.

**The log** travels with the run: an append-only record of what already
happened — sends, approvals, the agent's own acknowledgements.

Policy is declarative — contracts, authorities, sanitizers and casts are
data, never code. Judgment isn't, so it can live outside the engine:
whether a fetched page is trustworthy, whether this send may go, who sits
behind an email address. You register a component for each — a regex, a
classifier, a human on a pager — and registration fixes what its answer may
do. An authority's mandate names which gap its rulings cover and how far; a
sanitizer's names the one label transition it may claim.

## Labels only move one way

More restrictive, never less.

A tool's contract declares its `delta` — what a successful call does to the
run's label. Every delta restricts: it intersects the readers, lowers the
trust, or both. No delta widens. So reading the internal CRM makes the run
internal, and reading something suspicious makes the run suspicious. Nothing
afterwards makes it public or trusted again — no tool, no approval, no
clever sequence of steps.

Two things follow. You never replay a run to know where it stands; the
current label is a fold over every delta so far:

```ts
label = deltasSoFar.reduce(narrow, startingLabel)   // narrow only ever restricts
```

And a run cannot be laundered, because no step in the system moves a label
up: there is no sequence of calls that walks a secret back into a public
context.

The obvious objection is that the agent can then never mail an outsider. It
can. An approval admits one specific call without touching the label — the
mail goes, the run stays exactly as restricted as it was, and a second mail
needs a second approval. For a whole ongoing exchange you fork a child to
carry it; the child dies with the thread, and the parent's label never
contains the outsider at all.

## Reading data costs the agent reach

APPA runs two checks on every call. The first asks whether the flow is
legal. The second asks whether it is worth it.

Nothing leaks when an agent reads the internal CRM. But the run is internal
from that moment on, and it stays internal — so every later step negotiates
from a worse position. Sends that would have gone through now need approval.
Some sinks are closed for good.

Without the second check the agent finds that out three steps later, at a
send that no longer works, with the data already in its context and nothing
to be done about it. So APPA stops the call *before* the fetch and tells the
agent exactly what it is about to give up. APPA calls this a **narrowing**.
If the agent still wants the data, it accepts and the call proceeds. A later
call that restricts nothing further passes without stopping: the question is
asked once per step down, not once per call.

No approver is involved and nothing is granted here; the agent is choosing,
not asking. Going down is free; coming back up needs an authority.

## A refusal comes with the ways out

When APPA refuses a call it returns the remedies: get an approval, clean the
data first, do a missing step first, accept the narrowing. Most are
executable objects with an id, so the agent runs one directly rather than
guessing at a sequence; the rest name a call the agent makes for itself and
gets checked on like any other. Every remedy comes from something you
registered — an authority that can approve, a sanitizer that can clean, a
tool that does the missing step — and the engine can therefore enumerate
them all.

A nonempty list says a route exists. It does not promise the route works:
the authority can still decline. An empty list is a proof: under this
configuration, no route exists, and the agent can stop and say so instead of
burning turns on the same call. The proof is scoped to the configuration in
force, and says nothing about what a different one would allow.

## One fetch, two endings

An agent has three tools: pull a ticket from the internal CRM, send email,
file a public GitHub issue. The contracts say what each does — the CRM read
makes the run internal, the send needs its recipient among the run's
readers, the filing needs the run to be public.

```toml
[[tool]]
name  = "get_ticket_from_crm"
delta = { audience = { exactly = ["internal"] } }

[[tool]]
name     = "send_email"                    # send_email(body, to: $recipient)
requires = { audience = { includes = ["$recipient"] } }
effects  = ["egress"]

[[tool]]
name     = "file_github_issue"
requires = { audience = { includes = ["public"] } }
effects  = ["egress", "mutation"]
```

The run starts public and trusted, since nothing has been read yet. The
agent's first call is `get_ticket_from_crm()`, and APPA stops it — nothing
leaks by reading a ticket, but the run would go from public to internal, and
after that the GitHub tool is closed for the rest of the run. So the agent
gets the choice up front: accept the restriction, or run the fetch through
the registered `remove_pii` sanitizer, which returns a scrubbed ticket and
leaves the run public. That second plan needs a deployment that can hold the
raw ticket back from the model; a deployment that can't will only offer the
first. It is also the part of the model the reference implementation has not
built yet — the planner enumerates the acceptance and leaves the sanitizer
route out, and `spec.md` §10.2 marks it deferred.

Say the job is to file the ticket publicly. The agent takes the sanitizer,
the run stays public, `file_github_issue` passes with nothing to negotiate,
and `egress` and `mutation` land in the log.

Say instead the job is to email the raw ticket to an outside auditor. The
agent accepts the restriction, the run becomes internal, and
`send_email(ticket, auditor@…)` resolves its requirement against the actual
argument: the run's readers must include the auditor, and `internal` does
not. This is a second and separate gate. Accepting the restriction was the
agent's own call and granted no permission to disclose anything.

The remedy is a ruling. What reaches the approver is APPA's own account of
the call rather than the agent's: which tool, bound to these exact arguments
by a digest, going to this auditor, over data that came from the CRM at this
label. The message body is not in it — an approver rules on the disclosure,
and shipping the payload would hand every approval endpoint the data the
policy is protecting. On approval the mail goes and `egress` lands in the
log. The run's label does not change, so a second auditor email needs a
second approval.

## You don't have to annotate every tool

A real deployment has fifty tools, and you will not write fifty contracts
before the first run. APPA is built for that: every tool the agent may call
is registered, but a registration can be a bare name. Annotate the ones that
matter and leave the rest at a name. Such a tool returns data whose label is
**Unknown** — not a low trust rank, but a fact you have not established
yet — and Unknown spreads, so once the run has read one unknown value the
run's label is unknown too. A tool that is not registered at all is a
different case: a call naming it is refused as unknown rather than run at
Unknown. Whether the agent is even shown such a tool depends on the host —
the gateway advertises from the registry, so the two lists cannot drift,
while a framework embedding the SDK keeps its own tool list and has to keep
them in step itself.

That does not stop the agent. Calls that don't care about the dimension keep
working, and the run stops only where it reaches a tool whose contract does
care — a send that requires trusted data, say — where APPA refuses and names
the values it could not establish. So annotating five high-risk tools
already buys you the obvious flows, and you extend coverage where a refusal
tells you it's missing rather than guessing up front.

To resolve an Unknown you register a **cast**: a rule for what unknown
values become. It can be a constant — everything unknown is suspicious, or
everything unknown is trusted — or a service you call per value, so a
deployment can start blunt and get precise later.

## Not every host can hold data back

APPA has to sit where it can see the whole run and stop a call before it
dispatches. That rules out a plain MCP gateway, which sees tool calls but
not the conversation that gives them meaning, and leaves the harness itself
or an inference proxy paired with a tool gateway.

Among those, one capability splits deployments in two: can the layer run a
tool call and keep the result out of the model's context? A harness can,
since it decides what goes into the next request. Some proxy setups cannot.
APPA calls the first kind a **confining** deployment.

The split matters because once the model has read something, it has read it,
and no later policy un-sees it. So every construction that depends on
withholding bytes only exists on the confining side — running a fetch
through a sanitizer and showing the model only the clean version, handing a
suspicious page to a quarantined child and letting back only the version
number it extracted. In a deployment that cannot withhold, those remedies
are never offered.

Everything else is unaffected. Checks run, labels propagate, refusals carry
the remedies that remain. A non-confining deployment can stop a flow; it
just cannot offer to clean one, so its agents hit more dead ends and its
policies have to be written knowing that.

## The guarantees hold under four assumptions

APPA assumes an agent that can be fooled, not one trying to defeat you. A
model smuggling secrets through its choice of actions is out of scope.

Attacks arrive through data the agent reads, and APPA tracks that data.
Sources you have marked trusted are trusted by definition: if your CRM
starts serving attacker-controlled text, nothing here helps.

The guarantees rest on what you register. Your tool contracts describe what
each tool does; your authorities decide the cases the algebra cannot. Both
are yours to get right. An auto-approve authority and a cast that calls
every unknown value trusted are a legal configuration — they will run, and
they will void exactly the guarantees they touch, in a log that says so.

The log is yours to store: an append-only file or table, wherever you
already keep state. One agent process needs nothing more; concurrent
branches share a single log.

## What you already have becomes a component

Most teams evaluating APPA already run something: a permission prompt, a
model judging whether a message is safe to send, a fine-tuned classifier for
PII, a few hundred lines of if-statements. None of it is wasted. APPA does
not replace judgment, it gives judgment a place to stand and a ceiling it
cannot exceed.

| what you run today | what it becomes | its ceiling |
|---|---|---|
| a permission prompt, or an auto-approve mode | an authority on the `hitl` channel — a resolver today, since the reference implementation has no queue to put a person in yet | its mandate: which gaps it may cover, up to what rank or reader set |
| a model judging whether an action should proceed | an authority resolver | the same mandate |
| a model or classifier judging whether content is trustworthy | a cast resolver | `may_cast` — the states it may resolve to |
| a trained PII or injection detector that redacts | a sanitizer | its one declared audience transition |
| a regex or allowlist output filter | a sanitizer | the same |
| ifs that gate a flow between two systems | a tool contract | none needed; the algebra does it |

The migration is small because you keep the thing you built. A judge already
exposed over HTTP becomes an authority by adding a block of TOML that names
its endpoint and declares what its answers are allowed to do. The model
stays, the prompt stays, the weights stay.

What changes is the frame around it. The judge is asked about the call APPA
identified rather than the one the agent described, so a steered model
cannot put a flattering question to it. Its answer is bounded by a mandate,
so a classifier that is wrong, or compromised, can approve at most what you
declared it could approve. A judge that times out or errors abstains and the
refusal stands. And every decision it makes lands in the log next to the
context it saw.

The last row is the one that shrinks a codebase. Rules that gate a flow
between two systems — CRM data must not reach Slack, a customer record must
not leave by email — do not become a component at all. They are what
contracts express natively, so that part of the if-pile is deleted rather
than migrated.

## What adoption costs

Three things cost real effort. Tool contracts are the smallest of them: a
first draft comes from what you already have — tool descriptions, argument
schemas, the ACLs behind them — and a person reviews it. Reviewing
one is reading four lines and asking whether they describe the tool
honestly, which is why `contracts.md` is written as a guide to reading
contracts rather than writing them.

Authorities cost attention, and this is where deployments actually fail. If
every restricted send pages a human, the humans learn to approve without
reading, and an approval nobody reads is worse than no approval at all. The
design keeps the volume down — accepting a narrowing is the agent's own step
and never reaches a person, and an authority is consulted only where a call
would exceed what the run's label already allows. Whether that lands at a
handful of approvals a day or a hundred depends on your contracts, and it is
the number to watch during a pilot.

Coverage is the third, and it is incremental by construction. Annotate the
tools that touch data you care about, leave the rest Unknown, and extend
where a refusal tells you something is missing.

## Where next

`spec.md` is the normative account: every rule, with an id you can cite.
`contracts.md` covers the configuration dialect and how to review a policy
someone else wrote. `rationale.md` answers the design questions this guide
skipped — why labels never widen, why there are two dimensions and not
three. The paper states the model formally, with theorem scoping and
citations.
