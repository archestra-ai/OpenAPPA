# Jev battery

One Annotator, `jev.tool-call`, that asks TypeSafe's Jev model to annotate
a tool call before it runs. Jev is a small hosted classifier: an answer
takes about 0.3 s, against several seconds for a general model. The client is
built into the runtime as the `jev` builtin; the battery is the
Annotator's declaration, its credential, and its setup. The battery
declares no tool. A deployment routes its own tools to
the Annotator.

**Every annotated call's name, description, and arguments are sent to the
TypeSafe API.** The runtime first redacts what it recognizes as a secret,
as it does for every model provider: each string goes through the
`redact-secrets` detector, and the whole value of any field named for a
secret, such as `password`, `token`, or `auth`, is replaced. Each secret
becomes `[redacted-secret]`; the tool name is not redacted. Redaction is
best effort, not a proof that no secret remains. Internal paths,
hostnames, and message text still leave. Install the battery only where
that flow is acceptable.

## Install

```sh
export APPA_PROVIDER_JEV_API_KEY=<TypeSafe API key>
appa battery install jev
```

The runtime reads the key when the deployment opens or reloads. A
deployment that declares a `jev` Annotator refuses to open while the
variable is unset, and a refused reload leaves the running deployment
serving. A profile that no Annotator consults loads without its key.

Route a tool to the Annotator in the root config:

```toml
[[policy.tool]]
name = "mcp/tickets/search"
description = "Searches the ticket tracker."
annotator = "jev.tool-call"
```

To have Jev annotate a tool another battery routes to a model, declare an
Annotator with that battery's Annotator name and `builtin = "jev"` in the
root config. A root declaration replaces the battery's:

```toml
[[policy.annotator]]
name = "claude-code.bash-requirements"
builtin = "jev"
ranks = ["suspicious", "trusted"]
audiences = ["self", "internal"]
marks = []
hint = "Output carrying text a third party wrote is suspicious."
```

With this battery installed, its `[externals.jev]` profile serves the
replacement. Without it, the root config declares the profile itself; a
deployment declares it once:

```toml
[externals.jev]
token_env = "APPA_PROVIDER_JEV_API_KEY"
```

## Files

**`appa.toml`** declares `jev.tool-call` on the `jev` builtin and the
`[externals.jev]` profile that names the key. The mandate admits both ends
of the trust chain and the `self` and `internal` audiences; `public` is
always admissible.

## How the builtin answers

Each consult asks Jev four questions about the complete call: who may read
its result, who wrote it, who the call delivers data to, and whether it
carries trajectory data out of the operator's control. A read that only
names what to read from an internal service does not. The Annotator's
`hint` is added to each question.

| Jev's label | Annotation |
| --- | --- |
| result readable by `public` | no `delta.audience` |
| result readable by `internal` or `self` | `delta.audience = ["internal"]` or `["self"]` |
| result is third-party content | `delta.trust` = the mandate's lowest rank |
| call delivers data to nobody | no `requires.audience` |
| call delivers data inside the organization | `requires.audience = { contains = ["internal"] }` |
| call delivers data outside it | `requires.audience = { contains = "public" }` |
| call sends or writes what the trajectory holds where people or outside services read it, or runs it | `requires.trust` = the mandate's highest rank |

For each label the builtin answers Jev's likeliest option. When two
options are equally likely, it answers the safer one: the narrower result
audience, the lower trust rank, or the wider required audience. The
consult is no answer, and the runtime refuses the call, when the mandate
does not admit a label, when the API does not answer, or when its answer
is not a probability for each option.

With no answer after 0.8 s on a connection that has answered before, or
after 2 s on a new one, a second request goes out on a new connection and
the first answer wins; a connection that answered that slowly is not
reused. A 5xx answer or a connection failure is retried; a 4xx answer and
an unreadable body are not. Every attempt ends inside the profile's
`timeout_ms`, which defaults to the deployment's `externals.timeout_ms`.
At most `max_concurrent` consults, 16 by default, run at once in one
deployment. The battery's profile keeps both defaults; a root config
that declares `[externals.jev]` itself can set them.

The consult record carries one JSON object under `jev_diagnostics`, also
logged at debug level: each label's probabilities and decision, the
`requires.trust` cutoff, the outcome of each attempt, the elapsed
milliseconds, and the error class on failure. It never carries the API
key or the call's arguments.

The requests go to TypeSafe's API. The operator can point them at another
endpoint with `APPA_PROVIDER_JEV_API_URL` in the runtime's environment; a
config cannot.

## Limits

Jev answers `delta` and `requires` over audience and trust only. It never
answers `emits`, `requires.history`, or `requires.attention`. Route a tool
here only when its policy needs none of them.

The builtin judges the complete call. A deployment is refused when a `jev`
Annotator declares `inputs` or its mandate admits fewer than two trust
ranks.
A consult larger than 64 KiB is no answer; nothing is sent.

The questions treat a read-only query to a remote service as data delivered
to that service. A web search therefore requires trajectory data that
`public` may read.
