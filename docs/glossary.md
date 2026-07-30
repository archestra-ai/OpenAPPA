# Glossary

Two tiers. **Surface terms** appear in the TOML or the API, so you will type
them. **Model terms** exist only in prose, and each has to earn its place.

## Surface terms

### Tool contracts

| key | means |
|---|---|
| `[[tool]]` | one tool's contract |
| `name` | the tool's identifier, matched against the proposed call |
| `delta` | what a call's admitted result folds into the run's label. Always restrictive |
| `requires` | what must hold before the call may run: label, history and attention conditions |
| `trust` | inside `delta`, the rank a result carries; inside `requires`, the floor a call demands |
| `audience` | inside `delta`, the readers a result is limited to; inside `requires`, a condition on the run's readers |
| `exactly` | the reader set is precisely this list |
| `includes` | the run's readers must contain these — `audience ⊇ recipients` |
| `cap` | the run's readers must stay within these — `audience ⊆ C` |
| `effects` | as a list, the tool's `emits`: what a successful call appends to the log |
| `effects.has` | `prior(k)` — a matching effect must already exist |
| `effects.has_no` | `no_prior(k)` — no matching effect may exist |
| `attention` | named marks demanding a fresh ruling on every dispatch |
| `tags` | routing names with no algebraic life; the only currency of authority scope |

### Authorities

| key | means |
|---|---|
| `[[authority]]` | one registered judge |
| `[authority.mandate]` | what its rulings may cover. An empty mandate is a load error |
| `can_raise_trust_to` | the ceiling of a trust cover |
| `can_add_readers` / `may_add` | the ceiling of an audience cover |
| `can_waive` | the effect kinds a ruling may waive for one dispatch |
| `attends` | the attention marks this authority's rulings satisfy |
| `[authority.scope]` / `tags` | jurisdiction. Omitted scope covers every call |
| `[authority.implementation]` | `builtin` for in-process, `resolver` for dynamic; `builtin = "hitl"` is human elicitation hosted by the harness |

### Sanitizers and casts

| key | means |
|---|---|
| `[[sanitizer]]` | a registered transformer that derives a new value under a mandated label |
| `on` | where it may apply: `tool_output` is live, `tool_input` is refused at load |
| `[sanitizer.mandate]` / `from` / `to` | the one transition it may claim, on either dimension |
| `[[cast]]` | the registered resolution of an Unknown dimension |
| `constant` | every Unknown on the dimension resolves to one declared state |
| `resolver` / `may_cast` | decided per value by a service, bounded by a declared ceiling of targets |

### Run configuration and API

| key | means |
|---|---|
| `version` | the configuration dialect version |
| `[child]` / `return_sanitizer` | every child return crosses only as this sanitizer's derivation |
| `resolver.url` / `timeout_ms` | how a dynamic external is reached |
| `requirement_gaps` | unmet entries of `requires`, returned on a refusal |
| `narrowing` | the loss of reach a call would commit, returned on a refusal |
| `remedy_plans` | the ways out, returned on a refusal |

## Model terms

| term | means |
|---|---|
| **trajectory** | one agent run: its label plus its event log |
| **value** | one tool call and its result, carried with its label and never separated from it |
| **label** | who may read the run's information, and how far it can be trusted |
| **fold** | folding a value's label into the run's: intersect readers, take the lower trust |
| **effect** | a recorded fact about what the run did outside, such as `egress` or `mutation` |
| **requirement gap** | an unmet entry of `requires`. Distinct from a narrowing, which fails no requirement |
| **narrowing** | a strict loss of reach a proposed flow would commit. Soft-blocked until the agent accepts it, and never terminal |
| **acceptance** | the agent's own step acknowledging a narrowing. No authority, no security power, clears no requirement. Always informed: it executes only in a round after the one that offered it |
| **remedy plan** | a way out of a refusal. One with an engine-side step is an executable object with an id, run through `execute_remedy_plan(plan_id)`; one without names a call the agent makes for itself and carries no id (`RMD-2`) |
| **ruling** | one act of judgment by an authority, admitting one rendered call. Call-scoped, consumed by its dispatch, never touches the label |
| **mandate** | the declared bound on a registered component's power: for an authority, what its rulings may cover; for a sanitizer, the one transition it may claim |
| **scope** | the tags an authority has jurisdiction over |
| **attention mark** | a per-call demand for a fresh ruling, through which tools and authorities reference each other without naming each other |
| **tag** | a routing-only name. Never folded, checked, or logged |
| **branch** | one concurrently executing thread of the run. All branches append to one shared log |
| **boundary event** | punctuation in the log — turn end, fork, merge. It marks and never gates |
| **confining deployment** | one that can hold a raw tool result out of the model's context. Required for quarantined branches and confined pending-cast results |
| **context-controlling deployment** | one that chooses what a child branch sees and receives what it returns. The weaker capability branching requires (`POS-5`) |
| **Unknown** | "this label has not been established yet." Not a rank; absorbing under the fold |
| **resolver** | the dynamic implementation of a registered external: authority rulings, cast decisions, sanitizer derivations, membership questions |
| **staged review** | what an authority actually sees: the call's identity, its rendered arguments, and typed context, persisted verbatim on the ruling |
| **release frontier** | what the run may still release, and to whom, without a further ruling. What a narrowing shrinks |

## Prose and wire

Some words differ between the docs and the implementation, on purpose.

| wire | prose |
|---|---|
| `outcome: "block"` | the engine refuses the call and names what would make it pass |
| soft block | the engine stops the call and offers the acceptance |
| `emits` | `effects = [...]` in the TOML |
