---
title: Add to your agent
category: Integrations
order: 7
description: Connect an agent harness to OpenAPPA, map its lifecycle to the runtime API, and enforce policy decisions.
---

An **integration** connects an agent harness (such as Claude Code, kagent, or a custom agent loop) to OpenAPPA. The harness intercepts the agent at key lifecycle points, submits proposed actions to OpenAPPA, and enforces policy decisions before execution proceeds.

Follow [Add an Integration](#add-an-integration) to wire your harness hooks, or see [Runtime Overview](#runtime-overview) for architecture details.

## Runtime Overview

`appa-runtime` is an HTTP service that runs alongside your agent as a local process, sidecar container, or centralized deployment.

:::fig-runtime-overview:::

### Endpoints

By default, `appa-runtime` listens on `http://127.0.0.1:8787` (`--listen`) and exposes HTTP and MCP endpoints:

- **`POST /hook`**: Primary lifecycle interception endpoint. Receives serialized harness events (e.g., `ToolCall`, `ToolResult`) and returns synchronous policy decisions (`HookDecision`).
- **`/mcp`**: Built-in Model Context Protocol endpoint. Exposes runtime tools like `execute_remedy_plan` and handles approvals or sanitization.
- **`GET /health` & `GET /status`**: Liveness probes and operational status for active trajectories.
- **`POST /reload`**: Hot-reloads policy configurations from disk without restarting the runtime process.
- **`GET /binary-fingerprint`**: Returns process ID, binary build digest, and config file path for tooling verification.
- **`GET /policy-key`**: Returns the active in-memory policy hash to detect configuration changes.

### Event Log

OpenAPPA reconstructs each trajectory from an append-only event log persisted to SQLite (`--db ./appa.db`). The log preserves state across turns: tool dispatches, child branches, authority decisions, and the active policy.

### Deployment Models

`appa-runtime` works with any agent that can send lifecycle events over HTTP and await decisions. The integration contract is identical across deployment topologies:

| Placement | Typical use |
|---|---|
| **Same-host process or sidecar** | Enforces policy beside a single agent (CLI agents, single-tenant agent pods). |
| **Shared internal service** | Gates multiple internal agents through a centralized runtime and shared policy. |
| **Application service** | Protects user-facing agents directly inside your application infrastructure. |

## Add an Integration

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

OpenAPPA's integration surface centers on the [`POST /hook`](#endpoints) endpoint. The runtime handles five core lifecycle events for single-agent workflows, plus three optional events for child agents (subagents).

Connecting an agent harness requires two steps:

1. Configure your [agent harness](#connect-the-agent-hooks) to intercept execution at these lifecycle events.
2. Implement or select an [Appa adapter](#implement-the-appa-adapter) that converts your harness's wire payloads into OpenAPPA events.

### Lifecycle Events

Every integration maps harness lifecycle hooks to OpenAPPA's typed events. The [Appa adapter](#implement-the-appa-adapter) accepts the harness's native payload, translates it into an OpenAPPA `HookEvent`, and renders the returned `HookDecision` into the format your harness expects.

Every payload must supply a stable session identifier for the root trajectory. If an event originates from a child agent, it must identify the child trajectory as well.

| # | Event | Trigger | Event Payload | Decision Handling |
|---|---|---|---|---|
| 1 | **`SessionStart`** | Root conversation or agent task initializes. | `SessionStart { root }` with the root trajectory ID. | `Ack`: Continue session startup.<br>`Refuse`: Abort session startup and report the `detail` message. |
| 2 | **`Prompt`** | User or system input starts an agent turn. | `Prompt { actor, text }` with the actor reference and input text. | `Ack`: Forward prompt to the model. |
| 3 | **`ToolCall`** | Model proposes a tool call, before execution. | `ToolCall { actor, call, spawn, ruling }` with tool name and arguments. Set `spawn: true` when delegating to a child agent (`false` otherwise). Set `ruling` if an external review decision exists; otherwise `None`. | `AllowCall`: Execute the tool. (If spawning a child, save the returned `spawn` token for `ChildStart`.)<br>`PassControl`: Call is `execute_remedy_plan`. Forward directly to `/mcp` without local execution.<br>`DenyCall`: Block execution. Return policy `feedback` and remedy plans to the model. |
| 4 | **`ToolResult`** | Tool execution finishes, before output reaches the model. | `ToolResult { actor, call, outcome }` with matching call and `ToolOutcome` (`Success`, `Failure`, or `Indeterminate`). | `Ack`: Deliver output to the model.<br>`ReplaceOutput`: Deliver substituted output instead.<br>`Block`: Withhold output from model context and trajectory history. |
| 5 | **`TurnEnd`** | Agent turn completes. | `TurnEnd { actor }` for the completed turn. | `Ack`: Finalize turn and settle pending dispatches. |

**`Prompt` handling**: `Prompt` marks the turn boundary without inspecting or filtering prompt text. OpenAPPA never records prompt text in trajectory history and always responds with `Ack`. Policy enforcement begins at `ToolCall`.

**Remedy execution (`execute_remedy_plan`)**: `execute_remedy_plan` is OpenAPPA's reserved tool for executing remedy plans. When the model selects a remedy plan, send `ToolCall` like any other tool call. The runtime validates the plan and responds with `PassControl`. Forward the call unmodified to [`/mcp`](#endpoints). Never give a harness tool this name—shadowing `execute_remedy_plan` bypasses policy enforcement.

If your agent framework supports child agents (subagents), handle these three additional events:

| # | Event | Trigger | Event Payload | Decision Handling |
|---|---|---|---|---|
| 6 | **`ChildStart`** | Child agent initializes. | `ChildStart { root, child, spawn }` linking the child trajectory to the parent root, including the `spawn` token returned from `AllowCall`. | `Ack`: Start child with inherited parent boundaries.<br>`Context`: Pass returned contract text to the child agent.<br>`Refuse`: Abort child launch. |
| 7 | **`ChildEnd`** | Child agent finishes, before returning data to parent. | `ChildEnd { root, child, value }` with the child's proposed return value. | `Ack`: Return original value to parent.<br>`ChildReturn`: Forward sanitized replacement `value` to parent.<br>`Block`: Withhold child output. |
| 8 | **`SpawnResult`** | Parent agent receives result from child agent. | `SpawnResult { actor, call, outcome, child, value }` in parent trajectory context. | `Ack`: Deliver child result into parent model context.<br>`ReplaceOutput`: Deliver substituted output instead.<br>`Block`: Withhold child result from parent context. |

Child delegation requires setting `spawn: true` on the initiating `ToolCall`. If sent with `spawn: false`, no fork is created; the runtime will return `Refuse` to subsequent `ChildStart` calls and block any following `ChildEnd`. If your agent framework does not support child agents, skip these three events.

### Connect the Agent Hooks

Integration hooks can live directly inside your agent or run as an external extension:

- **In-process**: Middleware, SDK wrappers, or tool dispatch callbacks in custom agent loops (Python, TypeScript, Go).
- **External plugin**: Client-side interceptors or shell scripts for closed agent runtimes (such as Claude Code).

At each hook point, the harness must:

1. **Pause** the pending action.
2. **Send** the event payload to the OpenAPPA [`POST /hook`](#endpoints) endpoint.
3. **Enforce** the returned `HookDecision` before resuming the agent.

**Fail closed on errors:** If `appa-runtime` is unreachable, times out, or returns an HTTP error, the harness must treat the response as `Block` and refuse the action. Never fail open.

### Implement the Appa Adapter

The adapter is the component inside `appa-runtime` that receives incoming requests at [`POST /hook`](#endpoints), translates the payload into an OpenAPPA `HookEvent`, and hands it to the core policy engine.

An adapter implements a two-function codec:

- **`parse(bytes)`**: Maps incoming JSON into a typed `HookEvent` (or returns a `ParseRefusal` on unreadable or malformed input).
- **`render(decision)`**: Serializes the engine's `HookDecision` into the response format expected by the harness (such as process exit codes, JSON hook outputs, or substituted tool arguments).

If your agent harness sends OpenAPPA's typed `HookEvent` JSON natively, no custom translation is needed. When integrating an agent with its own wire schema, compile an adapter codec into `appa-runtime`. Shipped adapters include `claude-code` and `kagent`. Select the active adapter at startup with `--adapter claude-code|kagent` (default: `claude-code`). Adapters are compiled into the binary, not loaded dynamically at runtime.

### Reference Implementations

Use the shipped source on GitHub as a reference:

- **Claude Code**: [Appa adapter](https://github.com/archestra-ai/OpenAPPA/tree/main/appa-adapter-claude-code) and [hooks plugin](https://github.com/archestra-ai/OpenAPPA/tree/main/integrations/claude-code).
- **kagent**: [Appa adapter](https://github.com/archestra-ai/OpenAPPA/tree/main/appa-adapter-kagent), [Python plugin](https://github.com/archestra-ai/OpenAPPA/tree/main/integrations/kagent/appa-kagent-adk), and [Go plugin](https://github.com/archestra-ai/OpenAPPA/tree/main/integrations/kagent/appa-kagent-adk-go).

### Smoke-Test Checklist

Verify your integration against these core behaviors:

- [ ] **Allowed tool calls execute**: When OpenAPPA returns `AllowCall`, the tool runs once with unmodified arguments.
- [ ] **Denied tool calls never execute**: When OpenAPPA returns `DenyCall`, the harness blocks execution and returns policy `feedback` and remedy plans to the model.
- [ ] **Remedy calls pass through**: When OpenAPPA returns `PassControl`, the harness forwards `execute_remedy_plan` unmodified to `/mcp` without intercepting or re-evaluating it.
- [ ] **Replaced outputs take effect**: When OpenAPPA returns `ReplaceOutput`, model context and trajectory history receive substituted content instead of raw tool output.
- [ ] **Blocked outputs are withheld**: When OpenAPPA returns `Block`, the tool result is withheld from model context and trajectory history.
- [ ] **Fails closed on connection failure**: Stopping [`appa-runtime`](#runtime-overview) causes subsequent prompts and tool calls to fail safely instead of running unprotected.
- [ ] **Subagents are bounded (if supported)**: Child trajectories inherit parent security labels, and unverified child returns are blocked at `ChildEnd`.
