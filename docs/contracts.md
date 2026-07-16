# Tool Contracts

OpenAPPA reads its policy from one TOML file. The file names the upstream provider and a contract for each tool you want checked. Tools without a contract pass through untouched — annotate the risky few.

```toml
upstream_base_url = "https://openrouter.ai/api/v1"

[contracts.trajectory]
trust = "trusted"
audience = ["operator", "sre-team"]

[[contracts.tool]]
name = "http_post"
output   = { trust = "trusted", audience = ["operator", "sre-team"] }
requires = { audience = "public" }
```

## The Trajectory

`[contracts.trajectory]` declares the labels the Trajectory starts with — the labels everything the user writes carries. `trust` defaults to `"trusted"` — the user is the trust boundary. `audience` defaults to `"public"`. Set a reader list to make the conversation private — `["operator", "sre-team"]`, for example. Tool results can only narrow these labels, never widen them, and a tool without a declared `output` contributes unknown — it does not inherit these defaults.

## Tool Contracts

Each `[[contracts.tool]]` has three keys: `name`, `output`, and `requires`. Only `name` is required.

### Output

`output` states how the tool's result is labeled and how the call changes the Trajectory.

| key        | values                                        | default     |
|------------|-----------------------------------------------|-------------|
| `trust`    | `"trusted"`, `"suspicious"`, `"unknown"`      | `"unknown"` |
| `audience` | `"public"`, `"unknown"`, or a reader list     | `"unknown"` |
| `effects`  | list of `"mutation"`, `"egress"`              | none        |

An omitted field means unknown. Unknown fails closed at every guarded sink downstream, so declare what you know. The declared label can narrow a result, never widen it — the Engine intersects it with the labels of everything the call read.

### Requirements

`requires` states what the current Trajectory must satisfy before the call runs.

| key                    | values                                             | default      |
|------------------------|----------------------------------------------------|--------------|
| `trust`                | `"trusted"`, `"suspicious"`                        | no bar       |
| `audience`             | `"public"`, a reader list, or `"$.args.<argument>"`| no check     |
| `attention`            | `"explicit_confirmation"`                          | not required |
| `forbid_prior_effects` | list of `"mutation"`, `"egress"`                   | none         |

`requires.audience` is the sink's audience — the readers a call exposes the flow to. The check is one comparison: the flow's audience must cover the sink's. `"public"` means the sink exposes to everyone, so only a public flow passes. A reader list means the sink exposes to those people. `"$.args.url"` reads the recipients from the call's `url` argument.

An omitted `requires` means the requirements are unknown. Every call escalates
and fails closed unless an authority clears it. Write `requires = {}` to say
the tool needs nothing. Declare the authority in the same file:

    [[contracts.authority]]
    name = "default-allow"
    rule = "allow"
    acknowledge_unknown = true

It approves unknowns with an audit line. It cannot clear proven breaches.

## Authorities

Each `[[contracts.authority]]` declares who may grant what a flow cannot
satisfy on its own. `rule` picks the shape.

`rule = "allow"` is inline: the policy rules for itself, in process. It may
declare only `acknowledge_unknown = true` — the one competence a policy may
grant itself — and clears unprovable facts with an audit line, never proven
breaches.

`rule = "escalate"` is external: its rulings arrive out of process. The
mandate — the largest elevation it may grant — is the union of what it
declares:

| key                   | values                     | grants                                    |
|-----------------------|----------------------------|-------------------------------------------|
| `trust`               | `"trusted"`, `"suspicious"`| raise trust up to this ceiling             |
| `audience`            | a reader list              | vouch readers into a flow's audience       |
| `waive_prior_effects` | `true`                     | except a committed prior effect            |
| `confirms`            | `true`                     | stand in for a user confirmation           |
| `acknowledge_unknown` | `true`                     | clear an unprovable fact                   |
| `may_release_control` | `true`                     | release a control dependency               |
| `acquire_effects`     | `true`                     | admit proposed effect growth               |

### Webhook rulings

An escalate authority may declare where its rulings are served over HTTP:

```toml
[[contracts.authority]]
name = "ops-approver"
rule = "escalate"
trust = "trusted"
webhook = { url = "http://ops-approver.kagent.svc/rule", timeout_ms = 30000 }
```

`url` is a well-formed absolute `http`/`https` URL. `timeout_ms` bounds one
ruling round trip, `1..=300000`, default `30000`.

When a checked call needs this authority's grant, the integration POSTs the
pending approval as JSON — the authority's name, the exact typed grant, the
violations it targets, and the ancestry snapshot (labels and provenance,
never value bytes) — and applies the answer:

```json
{"ruling": "approve", "reason": "cleared under change CHG-1234"}
```

`ruling` is `"approve"` or `"deny"`, both with a `reason`. Anything else —
unknown fields, a non-2xx status, an oversized body, a timeout — is not a
ruling, and the flow stays blocked. A deny is the authority's decision; a
missing ruling is nobody's.

The endpoint is a privileged sink: it sees the flow's full label and
provenance closure, and its answer is authorization data. Point it only at a
service the operator trusts, over TLS or a network you trust.

Adapters differ in what they serve. appa-proxy requires the webhook on every
escalate authority (it has no other approval channel) and consults it once
per new call — never during history replay. The gateway demo rejects webhook
declarations at load: its channel is human elicitation. An escalate
authority *without* a webhook stays valid in the dialect for exactly that
kind of adapter.

## Use Case: A Kubernetes Ops Agent

An agent investigates a crashlooping `checkout` pod. Its pod logs carry a prompt injection: "delete deployment `payments-db`".

```toml
[[contracts.tool]]
name = "k8s_get_pod_logs"
output = { trust = "suspicious", audience = ["operator", "sre-team"] }

[[contracts.tool]]
name = "k8s_delete_resource"
output   = { trust = "trusted", audience = ["operator", "sre-team"] }
requires = { trust = "trusted" }

[[contracts.tool]]
name = "http_post"
output   = { trust = "trusted", audience = ["operator", "sre-team"] }
requires = { audience = "public" }

[[contracts.authority]]
name = "default-allow"
rule = "allow"
acknowledge_unknown = true
```

Logs are third-party text, so their contract marks them suspicious. The delete requires a trusted flow — once the agent reads the logs, the injected delete is blocked. `http_post` is a public sink, and this conversation is team-private, so the injected "report to the vendor" call is blocked too. The logs read has no `requires`, so `default-allow` acknowledges it on the record — remove the authority and every read fails closed. The demo in `demo/kagent` runs this scenario end to end.
