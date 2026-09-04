---
title: Add to your agent
category: Integrations
order: 7
description: Connect an agent harness to OpenAPPA, map its lifecycle to the runtime API, and add optional capabilities such as subagents.
---

**OpenAPPA is a deterministic security and policy engine for AI agents.**

When an LLM agent runs, it ingests data (user prompts, files, web pages, APIs) and takes actions (executes commands, calls external services, writes to databases). OpenAPPA sits between your agent framework (the **harness**) and those tools. Before any action runs or new data enters model context, OpenAPPA evaluates one question: *Can this data, given where it originated, legally flow into this destination?*

An **integration** connects an agent harness (such as Claude Code, Hermes, or a custom agent loop) to OpenAPPA. It intercepts the agent at key lifecycle points, submits proposed actions to OpenAPPA, and enforces the engine's policy decision before execution proceeds.

Follow [Add an Integration](#add-an-integration) to wire your harness hooks, or see [Appa Overview](#appa-overview) for runtime architecture details.

---

## Add an Integration

OpenAPPA's integration surface centers on the [`POST /hook`](#endpoints-openappa-exposes) endpoint. The runtime handles five core lifecycle events for single-agent workflows, plus three optional events for child agents (subagents).

Connecting an agent harness requires two steps:
1. Configure your [agent harness](#connect-the-agent-hooks) to intercept execution at these lifecycle events.
2. Implement or select an [Appa adapter](#implement-the-appa-adapter) that converts your harness's wire payloads into OpenAPPA events.

### Lifecycle Events

Every integration maps harness lifecycle hooks to OpenAPPA's typed events. The agent harness does not need to send OpenAPPA's schema directly: the [Appa adapter](#implement-the-appa-adapter) accepts the harness's native payload, translates it into an OpenAPPA `HookEvent`, and renders the returned `HookDecision` into the format your harness expects.

Every payload must supply a stable session identifier to represent the root trajectory. If an event originates from a child agent, it must identify the child trajectory as well.

| # | Event | Trigger | Event Payload | Decision Handling |
|---|---|---|---|---|
| 1 | **`SessionStart`** | Root conversation or agent task initializes. | `SessionStart { root }` with the session ID for the root trajectory. | `Ack`: Continue session startup.<br>`Refuse`: Abort session startup and show the `detail` message. |
| 2 | **`Prompt`** | User or system input starts an agent turn. | `Prompt { actor, text }` with the actor reference and input text. | `Ack`: Forward prompt to the model. |
| 3 | **`ToolCall`** | Model proposes a tool call, before execution. | `ToolCall { actor, call, spawn, ruling }` with tool name and arguments. Set `spawn: true` when delegating to a child agent (`false` otherwise). Set `ruling` if an external review decision exists; otherwise `None`. | `AllowCall`: Execute the tool. (If spawning a child, save the returned `spawn` token for `ChildStart`.)<br>`PassControl`: Call is `execute_remedy_plan`. Forward directly to `/mcp` without local execution.<br>`DenyCall`: Block execution. Return policy `feedback` and remedy plans to the model. |
| 4 | **`ToolResult`** | Tool execution finishes, before output reaches the model. | `ToolResult { actor, call, outcome }` with matching call and `ToolOutcome` (`Success`, `Failure`, or `Indeterminate`). | `Ack`: Deliver output to the model.<br>`ReplaceOutput`: Deliver substituted output instead.<br>`Block`: Withhold output from model context and trajectory history. |
| 5 | **`TurnEnd`** | Agent turn completes. | `TurnEnd { actor }` for the completed turn. | `Ack`: Finalize turn and settle pending dispatches. |

`Prompt` does not evaluate policy or filter text. The runtime uses it only to mark the turn boundary. OpenAPPA does not inspect prompt text or record it in trajectory history, and always responds with `Ack`. Policy enforcement begins at `ToolCall`.

`execute_remedy_plan` is a reserved tool provided by OpenAPPA. When the model selects a remedy plan, send `ToolCall` like any other tool call. The runtime validates the selected plan and responds with `PassControl`. Forward the call unmodified to the [`/mcp`](#endpoints-openappa-exposes) endpoint.

The `/mcp` endpoint executes the remedy. It rejects any remedy call that did not pass through `ToolCall` first, or that references an expired or invalid plan (returning `DenyCall`). Never give a harness tool this name—shadowing `execute_remedy_plan` bypasses policy enforcement.

If your agent framework supports child agents (subagents), handle these three additional events:

| # | Event | Trigger | Event Payload | Decision Handling |
|---|---|---|---|---|
| 6 | **`ChildStart`** | Child agent initializes. | `ChildStart { root, child, spawn }` linking the child trajectory to the parent root, including the `spawn` token returned from `AllowCall`. | `Ack`: Start child with inherited parent boundaries.<br>`Context`: Pass returned contract text to the child agent.<br>`Refuse`: Abort child launch. |
| 7 | **`ChildEnd`** | Child agent finishes, before returning data to parent. | `ChildEnd { root, child, value }` with the child's proposed return value. | `Ack`: Return original value to parent.<br>`ChildReturn`: Forward sanitized replacement `value` to parent.<br>`Block`: Withhold child output. |
| 8 | **`SpawnResult`** | Parent agent receives result from child agent. | `SpawnResult { actor, call, outcome, child, value }` in parent trajectory context. | `Ack`: Deliver child result into parent model context.<br>`ReplaceOutput`: Deliver substituted output instead.<br>`Block`: Withhold child result from parent context. |

The `spawn` flag prepares the child agent fork. A delegating call sent with `spawn: false` creates no fork; the runtime will answer `Refuse` to subsequent `ChildStart` calls and block any following `ChildEnd`. If your agent framework does not support child agents, skip these three events.

### Connect the Agent Hooks

Integration hooks can live directly inside your agent or run as an external extension:

- **In-process inside the agent**: For custom agent loops (Python, TypeScript, Go), hook calls run directly inside the agent—as middleware, SDK wrappers, or tool dispatch callbacks around model turns and tool execution.
- **External plugin**: For closed agent runtimes (such as Claude Code), hooks run as client-side interceptors or shell scripts configured in the harness.

At each hook point, the harness must:

1. **Pause** the pending action.
2. **Send** the event payload to the OpenAPPA [`POST /hook`](#endpoints-openappa-exposes) endpoint.
3. **Enforce** the returned `HookDecision` before resuming the agent.

**Fail closed on errors:** If `appa-runtime` is unreachable, times out, or returns an HTTP error, the harness must treat the response as `Block` and refuse the action. Never fail open.

### Implement the Appa Adapter

The adapter is the component inside `appa-runtime` that receives incoming requests at [`POST /hook`](#endpoints-openappa-exposes), translates the payload into an OpenAPPA `HookEvent`, and hands it to the core policy engine.

An adapter implements a two-function codec:

- **`parse(bytes)`**: Extracts session and call context from the incoming JSON payload and maps it to a typed `HookEvent` (or returns a `ParseRefusal` on unreadable or malformed input).
- **`render(decision)`**: Serializes the engine's `HookDecision` into the response format expected by the harness (such as process exit codes, JSON hook outputs, or substituted tool arguments).

If your agent harness sends OpenAPPA's typed `HookEvent` JSON natively, no custom translation is needed. When integrating an agent with its own wire schema, compile an adapter codec into `appa-runtime`. The shipped Claude Code and kagent codecs sit beside it. Select it at startup with `--adapter claude-code|kagent`. The default is `claude-code`. The adapter is a build-time choice and a startup flag, never configuration: `appa-runtime` loads no codec at runtime. For a complete reference, see [`appa-adapter-claude-code`](https://github.com/archestra-ai/OpenAPPA/tree/main/appa-adapter-claude-code).

### Reference Implementation

Inspect the Claude Code integration on GitHub for a complete reference:

- **Appa Adapter**: [`appa-adapter-claude-code`](https://github.com/archestra-ai/OpenAPPA/tree/main/appa-adapter-claude-code) — maps Claude Code's native JSON hooks (`PreToolUse`, `PostToolUse`, `SubagentStart`, `SubagentStop`) to typed `HookEvent` variants, and renders `HookDecision` values into Claude Code responses.
- **Claude Code Hooks Plugin**: [`integrations/claude-code`](https://github.com/archestra-ai/OpenAPPA/tree/main/integrations/claude-code) — client-side harness configuration (`hooks.json`, shell interceptors, and MCP registration).

### Smoke-Test Checklist

Verify your integration against these core behaviors:

- [ ] **Allowed tool calls execute**: When OpenAPPA returns `AllowCall`, the tool runs once with unmodified arguments.
- [ ] **Denied tool calls never execute**: When OpenAPPA returns `DenyCall`, the harness prevents execution and feeds policy `feedback` and remedy plans back to the model.
- [ ] **Remedy calls pass through**: When OpenAPPA returns `PassControl`, the harness forwards `execute_remedy_plan` unmodified to `/mcp` without intercepting or re-evaluating it.
- [ ] **Replaced outputs take effect**: When OpenAPPA returns `ReplaceOutput`, model context and trajectory history receive substituted content, never the raw tool output.
- [ ] **Blocked outputs are withheld**: A blocked result is completely dropped from model attention and trajectory history.
- [ ] **Fails closed on connection failure**: Stopping [`appa-runtime`](#appa-overview) causes subsequent prompts and tool calls to fail safely instead of running unprotected.
- [ ] **Subagents are bounded (if supported)**: Child trajectories inherit parent security labels, and unverified child returns are blocked at `ChildEnd`.

---

## Appa Overview

OpenAPPA runs as a standalone daemon written in Rust (`appa-runtime`). In production or local development, it runs alongside your agent as a local background process or sidecar container.

:::fig-runtime-overview:::

### Endpoints OpenAPPA Exposes

By default, `appa-runtime` listens on `http://127.0.0.1:8787` (`--listen`) and exposes HTTP and MCP endpoints:

- **`POST /hook`**: Primary lifecycle interception endpoint. Receives serialized harness events (e.g., `ToolCall`, `ToolResult`) and returns synchronous policy decisions (`HookDecision`).
- **`/mcp`**: Built-in Model Context Protocol endpoint. Exposes runtime tools like `execute_remedy_plan` and handles human-in-the-loop (HITL) review elicitation when a blocked action requires approval or sanitization.
- **`GET /health` & `GET /status`**: Liveness probes and operational status for active trajectories.
- **`POST /reload`**: Hot-reloads policy configurations from disk without restarting the runtime process.
- **`GET /binary-fingerprint`**: Deployment check. Returns the process ID, binary build digest, and config file path so CLI tools (such as `appa init`) can verify process ownership.
- **`GET /policy-key`**: Policy synchronization check. Returns the hash of the active in-memory policy to detect disk-policy changes.

### Persistence: SQLite by Default, Pluggable for Any Storage

OpenAPPA records an append-only log of every trajectory, tool dispatch, authority approval, and policy decision.

- **SQLite by default**: Out of the box, `appa-runtime` persists state to a local SQLite database (`--db ./appa.db`).
- **Pluggable storage mechanism**: The storage layer (`appa-eventlog`) abstracts durability behind an append-only event log interface. SQLite is the only shipped backend today, but the storage architecture is pluggable so alternative backends can be implemented as needed.

---

## Existing Integrations

Explore working integrations in this repository:

- **[Claude Code](/claude-code)**: Anthropic's terminal agent, integrated via client-side shell hooks and an Appa adapter.
- **[kAgent](/kagent)**: Kubernetes agents with in-pod policy enforcement through the Google Agent Development Kit (ADK) plugin API, in both Python and Go.
