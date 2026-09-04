# The kagent integration suite

The real gated path, without a cluster, a dashboard, a model or an API
key. One pytest session starts every component the demo deployment
runs, in one process on loopback ports, and drives the fleet over
kagent's A2A endpoint:

- **the runtime** — a real `appa runtime --adapter kagent` on
  [policy.appa.toml](policy.appa.toml), the demo policy with the
  sanitizers rebound (below);
- **the tools** — the real demo MCP server
  ([../demo/demo_tools.py](../demo/demo_tools.py)), serving the
  real hazards: live secret material, crash logs carrying an injection,
  a status page carrying another;
- **the externals** — the real mock service
  ([../demo/mocks/](../demo/mocks/)): the runbook Annotator, the
  release-window authority, the change board, and the deterministic
  `/sanitize` derivation;
- **the agents** — a parent and the child it delegates to, each built
  by the real `appa_kagent_adk.entrypoint.build_server` from a rendered
  `config.json` and `agent-card.json`, each with its own
  `AppaPluginKagent` and reserved toolset, each served by uvicorn on its
  own port. The parent's remote agents are the declared child and one
  the policy never names;
- **the client** — plain JSON-RPC `message/send`, the same wire the
  kagent dashboard speaks, including the `decision_type` data part a
  person's ruling rides on.

Only the model is scripted. Every APPA decision — the deny, the offer,
the plan, the substitution, the human review — is the runtime's.

This is the deterministic twin of the model-driven matrix in
[../a2a/](../e2e/a2a/). That matrix proves the same substance against a
real model on a real cluster and needs both; this suite runs in about
twenty seconds and is meant to gate every pull request.

## Run

Needs a compiled `appa` binary and the kagent v0.9.12 lane.

```sh
APPA_INTEGRATION=1 uv run --project integrations/kagent/appa-kagent-adk \
  --with "kagent-adk @ git+https://github.com/kagent-dev/kagent@v0.9.12#subdirectory=python/packages/kagent-adk" \
  --with "a2a-sdk>=0.3.23,<0.4" --with "google-adk==1.31.1" --with "mcp>=1.25,<2" --with "pytest>=8" \
  pytest integrations/kagent/tests
```

`APPA_BIN` names the binary; otherwise `target/release/appa` and
`target/debug/appa` are tried, then `appa` on the PATH; with none of
them the suite skips. `APPA_INTEGRATION_TIMEOUT` (seconds, default 180)
bounds one A2A request.

Without `APPA_INTEGRATION=1`, or without the lane installed, collection
stops with the reason on the last line and a non-zero exit — the same
shape [../e2e/a2a/conftest.py](../e2e/a2a/conftest.py) and
[../e2e/ui/conftest.py](../e2e/ui/conftest.py) produce for their own gates.
So the suite spawns nothing on a bare unit run, and CI must set the
variable rather than rely on a skip.

Every component
writes its log into the session's pytest temp directory —
`runtime.log`, `mocks.log`, `demo-tools.log`, and the rendered
`policy.appa.toml` and config dirs beside them. The mock runs
`--verbose`, so `mocks.log` carries every consult envelope verbatim.

## Cases

[test_core.py](test_core.py) — one conversation each:

| case | what it pins |
|---|---|
| ordinary read | `list_pods` carries no narrowing, so the real pod data reaches the model ungated |
| exfiltration | `read_secret` would narrow a public trajectory to the ops readers, so the read is denied with runnable offers and no secret material enters the agent |
| sanitized default | the denied read offers both the narrowing and the sanitizer; taking the sanitizer's plan authorizes the call, the re-proposed read runs, and the gate replaces its result with the derivation — key names legible, values `[redacted]` |
| forged offer | an offer id the trajectory never pursued is refused at the vouching hook, so no plan runs and no person is asked |
| untrusted ingress | `get_pod_logs` enters suspicious, so the read is denied and the instruction inside the logs never reaches the model |
| third-party ingress | `check_status_page` enters suspicious too, so the read is denied and the instruction embedded in the page never reaches the model |

[test_remedies.py](test_remedies.py) — every authority that rules on a
blocked call, and every refusal: the operator steering the agent to the
narrowing and to no remedy at all, the human-review authority approving
and rejecting through kagent's own confirmation, the per-call Annotator
on a public and on an ops runbook, the release window in and out of
window, and the remote change board approving, denying, and staying
silent.

[test_delegation.py](test_delegation.py) — the child's own stop, the
parent's replay, and the fork that binds:

| case | what it pins |
|---|---|
| clean crossing | the child reads nothing, so its branch stands at the floor; its message crosses at its own stop and the parent's spawn result replays it byte for byte, ungated |
| the floor binds the child | the bare floor the plugin declares binds every narrowing the child proposes: its crash-log read is offered the sanitizer and not the change, and its summary then crosses as spoken |
| a void return | a child that returns nothing crosses the void at its own stop and reads the runtime's answer as an ordinary tool result; the plugin replays that crossing at every later stop, so what the child says next reaches neither the runtime nor the parent |
| undeclared delegation | an agent no contract names is denied at the spawn, so no return menu is routed, no fork is bound, and the child app is never entered |
| one child, two parents | a child opens once per (root, child) pair, so a second parent binds its own fork into the session the first left behind and both returns cross |

In every delegation case the parent's model reads one tool call and one
result. The return declaration a marked spawn is blocked with never
reaches it: the plugin picks the bare-floor offer, runs it through the
runtime, and re-proposes the call, so no case scripts a declaration
turn and no agent instruction teaches one.

## How a case is written

A case registers the turns its agent plays, sends one message, and
asserts on the task's function calls and their responses:

```python
def test_an_ordinary_read_flows_real_data(stack):
    task = stack.say(
        PODS,
        [
            {"tool": "list_pods", "args": {"namespace": "shop"}},
            {"text": "checkout-api-b2k1 is in CrashLoopBackOff with 14 restarts."},
        ],
    )
    assert "checkout-api-b2k1" in json.dumps(task.responses("list_pods")[0])
```

A turn is a tool call `{"tool", "args"}`, a final `{"text"}`, or
`{"remedy": "<action>"}` — take the offer APPA last quoted whose action
line names `<action>`, the way a model reads the offers out of the
blocking feedback. An empty `{"text": ""}` is a stop with no message,
which a gated child returns as no value. `stack.script_child(request,
turns)` registers the child's turns under the `request` its parent will
send.

Assertions read `task.responses(tool)`, `task.calls(tool)`,
`task.confirmation()` and `task.everything()` — never the model's
wording, which is the script's own.

A delegated child answers in its own A2A task on its own port, which
the parent's task never carries, so what the child's model read is read
from the harness instead: `stack.child_read()` gives every tool result
it saw as `(tool, response)` pairs, `stack.child_results(tool)` those of
one tool, `stack.child_saw(text)` the ones quoting a text, and
`stack.child_turns()` the script positions it played. The runtime's
answer to the child's own stop is among those results, because the
child stops through an APPA-owned tool (`appa_return`).

## Two construction facts

`entrypoint.build_server` always builds the kagent app non-local, which
wants a kagent controller for its session store, task store and
service-account token. The suite forces `KAgentApp.build(local=True)`
at that one call, which swaps in the in-memory services — the same ones
the HITL resume needs to find its pending confirmation. Nothing else
about the construction changes: the config guard, the plugin order, the
reserved toolset and the gates are the production ones.

kagent resolves the model inside `AgentConfig.to_agent`, through the
module global `kagent.adk.types._create_llm_from_model_config`. The
suite replaces that global with a factory returning the scripted model,
and each agent's rendered config names the model it asks for
(`scripted-parent`, `scripted-child`), so one factory serves both.
kagent also builds a fresh Runner and a fresh agent for every A2A
request, so the scripted model keeps no cursor: it finds its script by
the first user text in the request and its position by counting the
model turns already in the transcript. That is why a resumed task plays
the right turn.

## The policy

[policy.appa.toml](policy.appa.toml) is
[../demo/chart/files/demo.appa.toml](../demo/chart/files/demo.appa.toml)
with three differences and no others, each commented in the file: the
agent-tool contract names this suite's one child, both sanitizers bind
to the mock's `/sanitize` instead of `builtin = "llm"` (and
`[externals.llm]` is gone), and the consult timeouts are the harness's.
`@@MOCK_PORT@@` is rendered with the mock's loopback port before the
runtime starts.
