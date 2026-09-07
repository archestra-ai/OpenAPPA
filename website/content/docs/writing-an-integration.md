---
title: Add to your agent
category: Integrations
order: 7
description: Connect an agent harness to OpenAPPA, map its lifecycle to the runtime API, and add optional capabilities such as subagents.
---

An **integration** connects an agent harness (such as Claude Code, kagent, or a custom agent loop) to OpenAPPA. It intercepts the agent at key lifecycle points, submits proposed actions to OpenAPPA, and enforces the engine's policy decision before execution proceeds.

Follow [Add an Integration](#add-an-integration) to wire your harness hooks, or see [Runtime Overview](#runtime-overview) for architecture details.

## Runtime Overview

A host reaches the runtime through an adapter, which speaks one hook protocol: a versioned wire envelope posted to `/hook`. Claude Code and kagent are the initial adapters.

`appa-runtime` is an HTTP service that runs alongside your agent as a local process, sidecar container, or centralized deployment.

:::fig-runtime-overview:::

### Endpoints

By default, `appa-runtime` listens on `http://127.0.0.1:8787` (`--listen`) and exposes HTTP and MCP endpoints:

- **`POST /hook`**: Primary lifecycle interception endpoint. Receives one hook protocol envelope per event (`tool_call`, `tool_result`, …) and returns the synchronous gating decision in the same protocol.
- **`/mcp`**: Built-in Model Context Protocol endpoint. Exposes the runtime's control tool, `appa/execute_remedy_plan`, and handles human-in-the-loop (HITL) reviews when a blocked action requires approval or sanitization.
- **`GET /health` & `GET /status`**: Liveness probes and operational status for active trajectories.
- **`POST /reload`**: Hot-reloads policy configurations from disk without restarting the runtime process.
- **`GET /binary-fingerprint`**: Deployment check. Returns the process ID, binary build digest, and config file path so CLI tools (such as `appa init`) can verify process ownership.
- **`GET /policy-key`**: Policy synchronization check. Returns the hash of the active in-memory policy to detect disk-policy changes.

### Event Log

To keep track of agent actions and enforce policies, OpenAPPA reconstructs each trajectory from an append-only event log. The log preserves state across turns: tool dispatches, child branches, authority decisions, and the initial policy.

`appa-runtime` persists this log to a local SQLite database (`--db ./appa.db`). Use durable storage when trajectories must resume across runtime restarts.

### Deployment Models

`appa-runtime` works with any agent able to send lifecycle events over HTTP and wait for a decision before continuing—including background running agents or interactive chat agents. The integration contract remains identical across deployment models:

| Placement | Typical use |
|---|---|
| **Same-host process or sidecar** | Enforces policy beside a single agent. Typical for local CLI agents and single-tenant agent pods. |
| **Shared internal service** | Gates multiple internal agents through a centralized runtime and shared policy configuration. |
| **SaaS-managed service** | Protects user-facing agents directly inside your application infrastructure and private network. |

See [Add an Integration](#add-an-integration) for the required events and decision handling.

### Use Your Existing Security Controls

OpenAPPA can work with the approval processes, data-cleaning services, and company directories you already use. Connect them to OpenAPPA and define when the agent must use them:

| What you already use | OpenAPPA component | How it works |
|---|---|---|
| **Human review & automated approval services** | [Authority](/contracts#authorities) | Ask a person, webhook, or LLM evaluator to approve or deny a specific action before the agent proceeds. |
| **Data and Action Classification** | [Annotator](/contracts#annotators) | Use a scanner or classifier to determine who may see data, how much it can be trusted, or whether an action needs review. |
| **PII redactors & sanitizers** | [Sanitizer](/contracts#sanitizers) | Remove sensitive information before the agent receives data or shares it with another tool. |
| **IAM Groups based access control** | [Audience sources](/contracts#audience-sources) | Use membership from Google Workspace, Slack, or GitHub to check who is allowed to access data. |

See the [Policy reference](/contracts) for configuration examples.

## Why Add OpenAPPA?

### Benefits for a SaaS Product

OpenAPPA enforces policy independently of the LLM. If your product lets users connect custom MCP servers, you can let them control where their data may flow. If your product performs agentic work behind the scenes, OpenAPPA prevents the agent from sending that data to destinations the policy does not allow.

### Benefits for an Enterprise Agent

OpenAPPA lets an enterprise apply centralized security policies across its fleet of agents. A policy defines where data may go, how it must be cleaned before it is sent, and who must approve sensitive actions.

## Add an Integration

OpenAPPA's integration surface centers on the [`POST /hook`](#endpoints) endpoint. The runtime handles five core lifecycle events for single-agent workflows, plus three optional events for child agents (subagents).

Connecting an agent harness requires two steps:

1. Configure your [agent harness](#connect-the-agent-hooks) to intercept execution at these lifecycle events.
2. Provide an [adapter](#the-adapter-and-the-hook-protocol): the runtime serves one adapter, and it derives the canonical tool id of every call from that adapter and the host's raw tool spelling.

> **Ask your coding agent**
>
> Copy this prompt into the coding agent that has access to your agent's source code:
>
> ```text
> Integrate OpenAPPA with the agent in this repository.
>
> Follow the technical integration guide:
> https://openappa.com/writing-an-integration#add-an-integration
>
> Load the OpenAPPA documentation in one of these ways:
> - Add https://openappa.com/mcp as a remote MCP server named openappa-docs.
> - Or run: curl -s https://openappa.com/llms.txt
>
> Inspect the agent harness, connect every lifecycle hook required by the guide,
> and enforce every decision returned by OpenAPPA. Then run the smoke-test
> checklist and the repository's available checks. Explain what you changed,
> what you verified, and any required hook the harness cannot expose.
> ```

### Lifecycle Events

Every integration maps harness lifecycle hooks to OpenAPPA's hook events. The harness posts each event as the hook protocol's wire envelope: one JSON object with `protocol: 1`, the `adapter` name, the `event`, and the fields that event needs. The runtime answers with a decision envelope of the same protocol.

Every event must supply a stable session identifier as `root_id`, the host's own id for the root trajectory. If an event originates from a child agent, it must identify the child trajectory as `child_id`. Ids cross unprefixed; the runtime prefixes them with the adapter's prefix (`cc:` for Claude Code, `kagent:` for kagent), so no caller can speak for another adapter's trajectories.

| # | Event | Trigger | Event Payload | Decision Handling |
|---|---|---|---|---|
| 1 | **`session_start`** | Root conversation or agent task initializes. | `root_id`, a stable root id from the harness session id. | `ack`: Continue session startup.<br>`refuse`: Abort session startup and display the returned `detail` message. |
| 2 | **`prompt`** | User or system input starts an agent turn. | `root_id`, `child_id` when a child speaks, and the exact input `text`. | `ack`: Forward the prompt to the model. |
| 3 | **`tool_call`** | Model proposes a tool call, before execution. | The `tool` in the host's raw tool spelling and its raw `arguments`. A harness that reviews a remedy through its own channel sets `ruling` on the control call that quotes the reviewed offer. Every other call carries no `ruling`, and the runtime refuses an event that asserts one under a host that reviews through no channel of its own. The runtime derives the canonical tool id and whether the call starts a child trajectory; the envelope asserts neither. | `allow_call`: Execute the tool call. A released spawn also returns a `spawn_binding`. Keep it for the `child_start` of the child it releases.<br>`pass_control`: The call names the control tool OpenAPPA owns (`appa/execute_remedy_plan`). Execute it untouched. The runtime runs the remedy the call quotes, and the harness must not gate it.<br>`deny_call`: Refuse tool execution. Feed the policy `feedback` and remedy `offers` back to the model. |
| 4 | **`tool_result`** | Tool execution finishes, before output reaches the model. | The same `tool` and `arguments`, and the `outcome`: `success` with its `body` (the JSON value as spelled, `null` included), `success_without_body` for a success whose body the harness does not carry, `failure` with its `message`, or `indeterminate`. | `ack`: Supply output to the model.<br>`deliver_value`: Supply the admitted `value` instead, byte for byte.<br>`replace_output`: Supply the runtime's own `output` text instead.<br>`block`: Withhold output completely from model context and trajectory history. |
| 5 | **`turn_end`** | Agent turn completes. | `root_id` and, for a child's turn, `child_id`. | `ack`: Finalize the turn. Settles any unexecuted tool dispatches. |

A decision that stands in for a result says which of two contents it carries. `deliver_value` and `child_return` carry a `value` OpenAPPA admitted: a confined result the check let through, or a sanitizer's derivation. Deliver those bytes as they are. `replace_output`, `deny_call`, `block` and `refuse` carry text the runtime authored, which names tools by the raw spelling your harness sent. A host whose model dispatches other names rewrites the spellings in that text, and never in an admitted value.

The `prompt` event gates nothing. The runtime notes it as the boundary that ends the previous turn. The prompt text reaches no engine check and enters no trajectory record, and the answer is always `ack`. Enforcement starts at `tool_call`.

`appa/execute_remedy_plan` is the canonical tool id OpenAPPA owns. When the model takes a remedy offer, send the `tool_call` for it like any other call, in the host's raw spelling. The runtime recognizes the control tool from the adapter's mapping, records the offer the call quotes, and answers `pass_control`. The call must then reach the [`/mcp`](#endpoints) endpoint unmodified.

That endpoint refuses a remedy call that no `tool_call` preceded. A call that quotes an offer this trajectory no longer pursues comes back as `deny_call`. A harness tool cannot take the id: only the raw spelling the adapter maps to `appa/execute_remedy_plan` is the control tool, and a lookalike on another server is an ordinary checked call.

If your agent framework supports child agents (subagents), handle these three additional events:

| # | Event | Trigger | Event Payload | Decision Handling |
|---|---|---|---|---|
| 6 | **`child_start`** | Child agent initializes. | `root_id` and `child_id`, linking the child trajectory to the parent root, and the `spawn_binding` that the delegating `allow_call` returned. A harness whose start signal names no such call omits it. The runtime then binds the single outstanding spawn of that family. | `ack`: Start child with inherited parent boundaries.<br>`context`: Pass the returned contract `text` to the child agent.<br>`refuse`: Abort child launch. |
| 7 | **`child_end`** | Child agent finishes, before returning data to the parent. | `root_id`, `child_id`, and the child's proposed return `value`. | `ack`: Return original value to parent.<br>`child_return`: Forward the replacement `value` to parent.<br>`block`: Withhold child output. |
| 8 | **`spawn_result`** | Parent agent receives the result from a child agent. | The delegating `tool` and `arguments`, the `outcome`, the `spawned_id` of the child, and its `value`, in parent trajectory context. | `ack`: Deliver child result into parent model context.<br>`deliver_value`: Deliver the admitted `value` instead of what the child returned, byte for byte.<br>`replace_output`: Deliver the runtime's own `output` text instead.<br>`block`: Withhold child result from parent context. |

Which calls start a child trajectory is the adapter's derivation (Claude Code's `Agent`, a kagent agent called as a tool), never a claim on the wire. A delegating call the adapter does not recognize as a spawn releases no fork: the runtime then answers `refuse` to that `child_start`, and blocks the `child_end` that follows. If your agent framework does not support child agents, skip these three events.

### Connect the Agent Hooks

Integration hooks can live directly inside your agent or run as an external extension:

- **In-process inside the agent**: For custom agent loops (Python, TypeScript, Go), hook calls run directly inside the agent—as middleware, SDK wrappers, or tool dispatch callbacks around model turns and tool execution.
- **External plugin**: For closed agent runtimes (such as Claude Code), hooks run as client-side interceptors or shell scripts configured in the harness.

At each hook point, the harness must:

1. **Pause** the pending action.
2. **Send** the wire envelope to the OpenAPPA [`POST /hook`](#endpoints) endpoint.
3. **Enforce** the returned decision before resuming the agent.

**Fail closed on errors:** If `appa-runtime` is unreachable, times out, or returns an HTTP error, the harness must treat the response as `block` and refuse the action. Never fail open.

### The adapter and the hook protocol

An adapter connects one host to APPA. On the wire it is a name: the `adapter` field of every envelope. In the runtime it is a crate that provides `adapter()`, the name plus one derivation: from a host's raw tool spelling, the canonical tool id (`<family>/<namespace>/<tool>`, or `appa/execute_remedy_plan`), whether the call starts a child trajectory, and the family children the arguments name. The runtime keys every fact on the canonical id; the raw spelling stays in the trajectory record for host dispatch, diagnostics, and replay. The [Policy reference](/contracts#tool-names) has the mapping tables of the initial adapters.

The runtime serves one adapter, selected at startup with `--adapter claude-code|kagent`; the default is `claude-code`. The adapter is a build-time choice and a startup flag, never configuration. An envelope that names another adapter, or a protocol other than `1`, is refused with `409` and blocks the action. So is an envelope carrying a field its own event does not read: the envelope is flat, one event makes one claim, and a result reported under another event's name would settle nothing.

The two initial adapters reach the wire differently:

- **Claude Code** posts through `appa hook`, run by the plugin's hooks. The Claude Code adapter crate keeps a client-side codec for it: the command reads Claude Code's hook JSON (`PreToolUse`, `PostToolUse`, `SubagentStart`, `SubagentStop`, …) on standard input, translates it to the envelope, posts it, and translates the decision back into the hook answer Claude Code reads.
- **kagent** posts the envelope directly: the Python and Go ADK plugins build it inside the agent pod and read the decision back. The kagent adapter crate is `adapter()` alone.

A new host chooses one of the two shapes: build the envelope in-process, as kagent does, or translate a host's hook format in a client, as `appa hook` does. Either way it needs an adapter crate the runtime is built with, because the runtime derives the tool identity of every call from it. For a complete reference, see [`appa-adapter-claude-code`](https://github.com/archestra-ai/OpenAPPA/tree/main/appa-adapter-claude-code) and [`appa-adapter-kagent`](https://github.com/archestra-ai/OpenAPPA/tree/main/appa-adapter-kagent); the envelope and decision types are in [`appa-runtime-api`](https://github.com/archestra-ai/OpenAPPA/tree/main/appa-runtime-api).

### Reference Implementations

Use the shipped source on GitHub as a reference:

- **Claude Code**: [Appa adapter](https://github.com/archestra-ai/OpenAPPA/tree/main/appa-adapter-claude-code) and [hooks plugin](https://github.com/archestra-ai/OpenAPPA/tree/main/integrations/claude-code).
- **kagent**: [Appa adapter](https://github.com/archestra-ai/OpenAPPA/tree/main/appa-adapter-kagent), [Python plugin](https://github.com/archestra-ai/OpenAPPA/tree/main/integrations/kagent/appa-kagent-adk), and [Go plugin](https://github.com/archestra-ai/OpenAPPA/tree/main/integrations/kagent/appa-kagent-adk-go).

### Smoke-Test Checklist

Verify your integration against these core behaviors:

- [ ] **Allowed tool calls execute**: When OpenAPPA returns `allow_call`, the tool runs once with unmodified arguments.
- [ ] **Denied tool calls never execute**: When OpenAPPA returns `deny_call`, the harness prevents execution and feeds policy `feedback` and remedy `offers` back to the model.
- [ ] **Remedy calls pass through**: When OpenAPPA returns `pass_control`, the harness runs the remedy tool unmodified and does not re-gate it.
- [ ] **Replaced outputs take effect**: When OpenAPPA returns `deliver_value` or `replace_output`, model context and trajectory history receive the substituted content, never the raw tool output. The `value` of a `deliver_value` reaches the model unchanged; only the runtime's own text is rewritten into the host's spellings.
- [ ] **Blocked outputs are withheld**: A blocked result is completely dropped from model attention and trajectory history.
- [ ] **Fails closed on connection failure**: Stopping [`appa-runtime`](#runtime-overview) causes subsequent prompts and tool calls to fail safely instead of running unprotected.
- [ ] **Subagents are bounded (if supported)**: Child trajectories inherit parent security labels, and unverified child returns are blocked at `child_end`.
