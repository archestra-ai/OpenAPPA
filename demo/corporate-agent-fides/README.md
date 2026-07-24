# corporate-agent-fides

The OpenAPPA **corporate-agent** scenario, defended by **[FIDES]** on
**[Microsoft Agent Framework]** instead of by OpenAPPA's own policy engine.

Same corpus, same planted prompt injection, same thirteen-tool surface as the
sibling Rust [`corporate-agent`](../corporate-agent) demo — the *only* variable
is the defense. It exists to read one information-flow system against the other
on an identical attack: OpenAPPA's **trust / audience** algebra there, FIDES's
**integrity / confidentiality** labels here.

[FIDES]: https://devblogs.microsoft.com/agent-framework/fides/
[Microsoft Agent Framework]: https://learn.microsoft.com/en-us/agent-framework/

## What FIDES is

FIDES (Flow Integrity Deterministic Enforcement System) ships in Agent
Framework as `agent_framework.security`. It is information-flow control as
middleware: every piece of content carries an **integrity** label
(`trusted` / `untrusted`) and a **confidentiality** label
(`public` / `private` / `user_identity`); labels propagate automatically through
tool calls and combine to the most restrictive of each axis (the taint fold);
and a policy is enforced *before* a sensitive tool runs. It is the same IFC
lineage OpenAPPA draws on (Sabelfeld/Myers, taint, sink, label,
declassification) — a good external anchor for reading the APPA model.

Dropping it in is a single context provider, `SecureAgentConfig`, that wires two
function middlewares around the agent loop:

- **label tracking** — folds each tool result's `security_label` into a running
  context label;
- **policy enforcement** — refuses a tool call from an untrusted context (unless
  the tool opted in) and refuses writing higher-confidentiality data to a
  lower-confidentiality destination (exfiltration).

With `auto_hide_untrusted=True` the untrusted forum content is additionally
*hidden* from the planner and routed to a separate **quarantine** model, so the
planted instruction never reaches the main agent — the `send_email` block is the
deterministic backstop underneath that.

## The mapping (this is the demo)

The sibling demo's guarded policy (`appa-policy.toml`) and this demo's labels
are the same design expressed in two vocabularies:

| Concept | OpenAPPA (`appa-policy.toml`) | FIDES (this demo) |
|---|---|---|
| Taint axis | `trust`: `suspicious` → `internal` | `integrity`: `untrusted` → `trusted` |
| Audience axis | `audience = { exactly = ["hr"] }` | `confidentiality`: `private` |
| Forum read | `delta = { trust = "suspicious" }` | result label `integrity=untrusted` |
| HR read | `delta = { audience = exactly ["hr"] }` | result label `confidentiality=private` |
| Finance / tasks | `delta = {}` (unconstrained) | `integrity=trusted, confidentiality=public` |
| The taint fold | monoid fold over the trajectory | `combine_labels` (untrusted & most-private win) |
| The sink | `send_email` `requires { trust=internal, audience includes $to }` | `send_email` `accepts_untrusted=False`, `max_allowed_confidentiality=public` |
| Reads in a tainted context | narrowing accepted via a remedy plan | `accepts_untrusted=True` (pure sources can't exfiltrate) |

So `send_email` is the one gated egress sink, refused on **either** axis — a
tainted (untrusted) context **or** an attempt to mail private data outward —
just as APPA's `send_email` needs both internal trust and a covering audience.

## Layout

```
corp_fides/
  systems.py    mock corporate systems on disk (search/read/create/send_email) — port of systems.rs
  tools.py      the 13 FIDES-labeled tools; the APPA->FIDES label mapping lives here
  agent.py      builds the model client(s) + SecureAgentConfig + Agent (FIDES on, or --no-defense)
  __main__.py   the CLI: corp-agent-fides
tests/
  test_systems.py      framework-free: the systems primitives (no key)
  test_labels.py       the tools' declared policy + the labels their results carry (no key)
  test_enforcement.py  drives the real FIDES taint fold + gate to prove the exfil is blocked (no key)
scripts/        ready-made scenarios (mirroring the sibling demo)
```

The corpus (`data/hr`, `data/public_forum`, …) is **not duplicated** — reads
default to the sibling `../corporate-agent/data`, so it is literally the same
records and the same planted `acme-forum-thread.md`. `send_email` writes to this
demo's own `data/email/` (git-ignored) so the observable sink stays separate.

## Prerequisites

- Python ≥ 3.10.
- An OpenRouter API key **for the agent**. The systems layer and all three test
  modules need none.

```sh
cd demo/corporate-agent-fides
uv venv && source .venv/bin/activate      # or your venv of choice
uv pip install -e .                        # agent-framework-core + agent-framework-openai
cp .env.example .env                       # set OPENROUTER_API_KEY
```

## Test

```sh
cd demo/corporate-agent-fides
python -m pytest           # 15 tests, no API key required
```

`test_enforcement.py` is the important one: it drives FIDES's real
`combine_labels` and `check_confidentiality_allowed` with this demo's labels and
asserts the injection flow is refused at `send_email` — the LLM-independent core
of the block, provable offline.

## Run

```sh
# FIDES on (default): the injection is defended, the sink stays empty
./scripts/injection-forum-fides.sh

# --no-defense: same loop, same prompt, no FIDES — the HR record leaks
./scripts/injection-forum-open.sh
```

| Script | What it shows |
|--------|---------------|
| `./scripts/injection-forum-fides.sh` | **The block**: planted thread → FIDES hides the forum text and refuses `send_email`; `data/email/` stays empty; audit log records it |
| `./scripts/injection-forum-open.sh` | **The leak** (`--no-defense`): the same attack exfiltrates the HR record |
| `./scripts/summarize-hr.sh` | Benign: HR reads are `private` but safe to read — the summary returns |
| `./scripts/email-finance.sh` | Value-granular: `public` finance data **is** allowed out — FIDES isn't blanket-blocking |
| `./scripts/reset-email.sh` | Clear the `data/email/` sink |
| `./scripts/chat.sh` | Interactive REPL |

Direct invocation:

```sh
corp-agent-fides "Find Alice Chen's HR record and summarise it"
corp-agent-fides --no-defense "<prompt>"     # the unmediated contrast
corp-agent-fides --chat
```

| Flag | Meaning |
|------|---------|
| `--no-defense` | build the agent without `SecureAgentConfig` (the leak) |
| `--model <id>` | OpenRouter model id (env `FIDES_DEMO_MODEL`; default `anthropic/claude-sonnet-5`) |
| `--quarantine-model <id>` | model for the quarantine client (env `FIDES_QUARANTINE_MODEL`; default: same as `--model`) |
| `--data-root <path>` | corpus root (env `CORP_DATA_ROOT`; default: sibling `corporate-agent/data`) |
| `--sink-root <path>` | where `send_email` writes (default: this demo's `data/`) |
| `--quiet` | print only the final answer |

## Swapping the model backend

Microsoft's own FIDES sample targets Azure AI Foundry. This demo defaults to an
OpenAI-compatible endpoint (OpenRouter) so it runs with the same key as the
sibling demo; only `make_chat_client` in `agent.py` changes. To match the MS
sample, install the `foundry` extra and use:

```python
from agent_framework.foundry import FoundryChatClient
from azure.identity import AzureCliCredential
return FoundryChatClient(async_credential=AzureCliCredential())
```

Nothing else in the demo depends on the client choice.

## Scope note

This is a **comparison artifact for reading FIDES against OpenAPPA**, not a
vendored dependency: the label mapping and the enforcement are the point, and
they are exercised offline in `tests/`. The full agent loop needs a model key;
the defense itself does not.
