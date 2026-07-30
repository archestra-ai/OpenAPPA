---
title: How OpenAPPA works
category: Get started
order: 2
description: The whole model in one sitting — what OpenAPPA guarantees and what it costs.
---

## What OpenAPPA does

OpenAPPA sits between an agent and its tools and answers one question before
every call: may this data go there? It either allows the call, or refuses it
and names what would make it pass. To answer, the engine does not look at
the call alone: everything the agent has read so far folds into one running
label, and the tool's contract is checked against that — so the same call is
legal early in a run and refused once the agent has touched a customer
record.

**The label** travels with the data and has two dimensions: an audience —
the set of readers it may reach, `public` meaning everyone — and a trust
rank, `suspicious` below `trusted`. Reading anything folds its label into
the run's — the audiences intersect, the lower trust wins — and it stays
folded for the rest of the run.

**The log** travels with the run: an append-only record of what already
happened — sends, approvals, denials.

Policy is declarative — contracts, authorities, sanitizers and casts are
data, never code. Judgment isn't, so it can live outside the engine:
whether a fetched page is trustworthy, whether this send may go, who sits
behind an email address. You register a component for each — a regex, a
classifier, a human on a pager — and registration fixes what its answer may
do. An authority's mandate names which gap its rulings cover and how far; a
sanitizer's names the one label transition it may claim.

## Labels only move one way

A tool's contract declares its `delta` — what taking the call's result
into the run does to the label. Every delta restricts: it intersects the readers, lowers the
trust, or both. No delta widens. So reading the internal CRM makes the run
internal, and reading something suspicious makes the run suspicious. Nothing
afterwards makes it public or trusted again — no tool, no approval, no
clever sequence of steps.

You never replay a run to know where it stands; the
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
needs a second approval. A whole ongoing exchange can run on that plan too,
one ruling per message, and sometimes that is the right shape — each send is
reviewed. When it isn't, you fork a child to carry the exchange: the
outsider's replies narrow only the child, and the child dies with the
thread.

## Reading data costs the agent reach

OpenAPPA runs two checks on every call. One asks whether the flow is legal.
The other asks whether it is worth it.

Nothing leaks when an agent reads the internal CRM. But the run is internal
from that moment on, and it stays internal — so every later step negotiates
from a worse position. Sends that would have gone through now need approval.
Some sinks are closed for good.

Without that check the agent finds that out three steps later, at a
send that no longer works, with the data already in its context and nothing
to be done about it. So OpenAPPA stops the call *before* the fetch and tells
the agent exactly what it is about to give up. OpenAPPA calls this a
**narrowing**. If the agent still wants the data, it accepts on the record
and the call proceeds. A later call that restricts nothing further passes
without stopping: the question is asked once per step down, not once per
call.

No approver is involved and nothing is granted here. Going down is free;
reaching above the label takes an authority, one call at a time.

## A child's narrowing dies with it

The fork that carried the outsider exchange is a host capability with
fixed rules. A child starts at the parent's current label, and everything
it reads narrows the child alone. It returns at most one value, over one
channel, and the return folds into the parent like any other read — so a
raw return that would narrow the parent stops at the merge, to be accepted
or passed through a registered sanitizer that clears it. The branches
share one log throughout: the child's sends and approvals sit on the
parent's record, and the fork scopes only the label.

## A refusal comes with the ways out

When OpenAPPA refuses a flow it returns the remedies: get an approval, clean
the data first, run a step the contract requires first, accept the
narrowing. Most are executable objects with an id, so the agent runs one
directly; the rest name a call the agent makes for itself, checked like any
other. Every remedy but one comes from something
you registered — an authority that can approve, a sanitizer that can
clean, a tool that does the missing step — so the engine can enumerate
them all; the exception is accepting the narrowing, which grants nothing
and needs no registration.

A nonempty list says a route exists. It does not promise the route works:
the authority can still decline, and the denial is recorded. An empty list
is a proof: under this configuration, no route exists, and the agent can
stop and say so instead of burning turns on the same call. The proof is
scoped to the configuration in force, to what the registered components
answered at that moment, and to any denial already on record for this
exact call.

## One fetch, two endings

An agent has three tools: pull a ticket from the internal CRM, send email,
file a public GitHub issue. The contracts say what each does — the CRM read
makes the run internal, the send needs its recipient among the run's
readers, the filing needs the run to be public.

```toml
[[tool]]
name  = "get_ticket_from_crm"
delta = { audience = { exactly = ["internal"] } }   # "internal" is a single reader id

[[tool]]
name     = "send_email"                    # send_email(body, to: $recipient)
requires = { audience = { includes = ["$recipient"] } }
delta    = {}                              # annotated: the result carries nothing
effects  = ["egress"]

[[tool]]
name     = "file_github_issue"
requires = { audience = { includes = ["public"] } }
delta    = {}
effects  = ["egress", "mutation"]

[[sanitizer]]                              # the child route crosses this
name = "remove_pii"
on   = ["tool_output"]

[sanitizer.mandate]
audience = { from = { includes = ["internal"] }, to = { exactly = ["public"] } }

[sanitizer.implementation]
resolver = { url = "https://pii.corp/redact", timeout_ms = 10000 }

[[authority]]                              # who can approve the auditor mail
name = "disclosure-officer"

[authority.mandate]
can_add_readers = { may_add = ["public"] } # may vouch any recipient

[authority.implementation]
resolver = { url = "https://approvals.corp/rule", timeout_ms = 30000 }
```

The run starts where the deployment says it starts — the starting label is
configuration, and this one uses the neutral `{public, trusted}`, since
nothing has been read yet. A deployment that expects users to paste customer
names into the first prompt starts its runs restricted instead, because a
secret typed directly at the agent enters before any contract can label it.
The agent's first call is `get_ticket_from_crm()`, and OpenAPPA stops it —
nothing leaks by reading a ticket, but the run would go from public to
internal, and after that the GitHub tool is closed for the rest of the run.
The agent gets the choice up front:

| the agent's move | the parent run |
|---|---|
| accept the narrowing | internal from here on; GitHub closed |
| fetch in a child, return through `remove_pii` | stays public; GitHub open |

The refusal itself lists only the acceptance; the branch is the agent's own
move, and it needs a host that can branch and hold the raw return back — in
a deployment that can't, the acceptance is all there is.

Say the job is to file the ticket publicly. The agent takes the child
route, the parent stays public, `file_github_issue` passes with nothing to
negotiate, and `egress` and `mutation` land in the log.

Say instead the job is to email the raw ticket to an outside auditor. The
agent accepts the narrowing, the run becomes internal, and
`send_email(ticket, auditor@…)` resolves its requirement against the actual
argument: the run's readers must include the auditor, and `internal` does
not. This is a second and separate gate. Accepting the narrowing was the
agent's own call and granted no permission to disclose anything.

The remedy is a ruling. What reaches the approver is OpenAPPA's own account
of the call rather than the agent's: which tool, bound to these exact
arguments by a digest, going to this auditor, over data that came from the
CRM at this label — the message body included, since an approver asked to
release a text has to read it. An approver is part of the deployment's
trusted base: register one you would show the data to, because ruling on a
disclosure means seeing it. On approval the mail goes and `egress` lands in
the log. The run's label does not change, so a second auditor email needs a
second approval.

## You don't have to annotate every tool

A real deployment has fifty tools, and you will not write fifty contracts
before the first run. OpenAPPA is built for that: every tool the agent may
call is registered, but a registration can be a bare name. Annotate the ones
that matter and leave the rest at a name. Such a tool returns data whose
label is **Unknown** — not a low trust rank, but a fact you have not
established yet — and Unknown spreads, so once the run has read one unknown
value the run's label is unknown too. A tool that is not registered at all
is a different case: a call naming it is refused as unknown rather than run
at Unknown.

That does not stop the agent. Calls that don't care about the dimension keep
working, and the run stops only where it reaches a tool whose contract does
care — a send that requires trusted data, say — where OpenAPPA refuses and
names the values it could not establish. So annotating five high-risk tools
already buys you the obvious flows.

The limit case makes the mechanics plain: with no annotations at all, no
tool call ever blocks. Unknown fails closed only at a `requires` that
consumes it, and a narrowing stop only fires on a declared `delta`, so a
policy of bare names refuses exactly one thing — a call to a tool that is
not registered. A host that branches adds one stall: a child's return
carries Unknown, and the merge holds until a registered cast establishes
it — or blocks naming the value, where no cast can. Past those, the first
requirement you write is the first place the engine can say no.

To resolve an Unknown you register a **cast**: a rule for what unknown
values become. It can be a constant — everything unknown is suspicious, or
everything unknown is trusted — or a service you call per value, so a
deployment can start blunt and get precise later. The per-value service is
an ordinary registered external, which is where the surface stays small
while the deployment grows: a resolver that remembers its own answers
annotates your tools one value at a time, and swapping it in changes no
engine, no spec, and no contract.

## Not every host can hold data back

OpenAPPA has to sit where it can see the whole run and stop a call before it
dispatches. That rules out a plain MCP gateway, which sees tool calls but
not the conversation that gives them meaning, and leaves the harness itself
or an inference proxy paired with a tool gateway. The tool half of that
pairing need not be a separate network hop: an inference proxy plus
harness-level hooks that check each call before dispatch sits in the same
position, and hooks can withhold a result.

Withholding is the capability that sorts deployments, because once the
model has read something, no later policy un-sees it. A harness can
withhold, since it decides what enters the model's next request; some proxy
setups cannot. What a host can hold back decides which remedies its agents
are ever offered:

| the host can | the spec calls it | what that unlocks |
|---|---|---|
| check every call, withhold nothing | — | every check and refusal, no cleaning remedies |
| bound a child's context and take its single return | context control | the child route: the raw return stays behind, a sanitizer's derivation crosses |
| keep a result's bytes out of every model context | a **confining** deployment | pending-cast results held until a cast rules, quarantined children returning only what they extracted |

Checks run, labels propagate, refusals carry the remedies that remain. A
deployment that cannot withhold can still stop a flow; it just cannot offer
to clean one, so its agents hit more dead ends and its policies have to be
written knowing that.

## The guarantees hold under four assumptions

| the assumption | what falls outside it |
|---|---|
| the agent is benign but confusable | a model smuggling secrets through its choice of actions is out of scope |
| attacks arrive through data the agent reads | a source you marked trusted is trusted by definition — a CRM serving attacker-controlled text is not caught |
| what you register is right | an auto-approve authority and a cast calling every unknown value trusted are legal; they run, and they void exactly the guarantees they touch, in a log that says so |
| the log is durable and in order | a history check is only as sound as the log it has seen |

Contracts describe what each tool does and authorities decide the cases the
algebra cannot; both are yours to get right. An append-only file or table
wherever you already keep state is enough for the log, and concurrent
branches share a single one, appends serialized.

## What you already have becomes a component

Most teams evaluating OpenAPPA already run something: a permission prompt, a
model judging whether a message is safe to send, a fine-tuned classifier for
PII, a few hundred lines of if-statements. OpenAPPA does not replace
judgment, it gives judgment a place to stand and a ceiling it cannot
exceed.

| what you run today | what it becomes | its ceiling |
|---|---|---|
| a permission prompt, or an auto-approve mode | an authority implemented as `builtin = "hitl"` — abstaining today, since no queue puts a person in the loop yet | its mandate: which gaps it may cover, up to what rank or reader set |
| a model judging whether an action should proceed | an authority resolver | the same mandate |
| a model or classifier judging whether content is trustworthy | a cast resolver | `may_cast` — the states it may resolve to |
| a trained PII or injection detector that redacts | a sanitizer | its one declared transition — the audience the derivation may reach, or the trust it may carry |
| a regex or allowlist output filter | a sanitizer | the same |
| ifs that gate a flow between two systems | a tool contract | none needed; the algebra does it |

The migration is small because you keep the thing you built. A judge already
exposed over HTTP becomes an authority by adding a block of TOML that names
its endpoint and declares what its answers are allowed to do. The model
stays, the prompt stays, the weights stay.

The judge is asked about the call
OpenAPPA identified rather than the one the agent described, so a steered
model cannot put a flattering question to it. Its answer is bounded by a
mandate, so a classifier that is wrong, or compromised, can approve at most
what you declared it could approve. A judge that times out or errors
abstains and the refusal stands. And every decision it makes lands in the
log next to the context it saw.

The last row is the one that shrinks a codebase. Rules that gate a flow
between two systems — CRM data must not reach Slack, a customer record must
not leave by email — do not become a component at all. They are what
contracts express natively, so that part of the if-pile is simply deleted.

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

The [AgentDojo harness](/docs/agentdojo) runs the engine against the
prompt-injection benchmark. The repository's `docs/` holds the rest:
`spec.md` is the normative account, every rule with an id you can cite;
`contracts.md` covers the configuration dialect and how to review a policy
someone else wrote; `rationale.md` answers the design questions skipped
here — why labels never widen, why there are two dimensions and not
three. The paper states the model formally, with theorem scoping and
citations.
