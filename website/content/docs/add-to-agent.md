---
title: Add to your agent
nav_title: + Add to your agent
category: Works with
order: 8
description: Embed APPA in your own agent, connect a coding agent through hooks, or use it through the Archestra LLM proxy.
---

Your agent runs the tools. APPA checks whether your rules allow each tool call and the data returned. See [How it works](/how-it-works) for what happens when something is blocked and how the agent can continue.

:::fig-runtime-overview:::

Choose an integration path below, or give your coding agent a prompt to work out the details.

:::integration-paths:::

## Embed the APPA runtime in your agent's code

Use APPA with an agent written in Python, Rust, TypeScript, Java, Go, or another language. This includes agents built with LangGraph, PydanticAI, or the OpenAI Agents SDK. Keep your stack and add the integration to your existing code.

Start with the [Python binding](https://github.com/archestra-ai/OpenAPPA/tree/main/appa-agent-python) and [Rust example agent](https://github.com/archestra-ai/OpenAPPA/tree/main/appa-example-agent) as references.

Ask your coding agent to run [this prompt](#start-in-your-repository) to implement it.

## Connect a coding agent through hooks

Connect an existing agent through its lifecycle hooks. The hooks pause tool execution and result delivery, ask APPA for a decision, and apply it before the agent continues. This works with any harness whose hooks can block a call and withhold or replace its result.

APPA runs alongside the agent as a local process, sidecar, or shared service. For a new harness, your coding agent can build the hook integration and runtime adapter using the existing implementations as references.

[Claude Code](/claude-code) is a ready-made example for a coding agent; [kagent](/kagent) shows the same approach for Kubernetes agents. Their dedicated guides cover setup and configuration.

Ask your coding agent to run [this prompt](#start-in-your-repository) to implement it.

## Use APPA at the LLM proxy

If you manage agents through a shared gateway, use **[Archestra](/archestra)**. It integrates OpenAPPA at the LLM proxy layer and also provides an MCP gateway, so you can manage policy there instead of embedding the runtime in each agent.

## Connect to your observability stack

See what your agent was allowed to do, what was blocked, and why. OpenTelemetry export is in development so you can investigate decisions and policy-check times in your existing monitoring tools. See [Observability](/observability) for the planned metrics, logs, and setup.

## Validate policies in CI

Test policy changes before they reach your agents. `appa replay` checks sequences of tool calls against the decisions you expect, without running the tools. See [CI Validation](/validation) for a working example and how to run it in your pipeline.

## Self-improving policies

Agents can report confusing decisions through `appa yell`. The planned maintenance workflow uses those reports and telemetry to propose and test policy changes for your review. See [Self-improving policies](/self-improving-policies) for the maintenance agent's role and review process.

## Start in your repository

Give your coding agent access to your source and the [`appa-guide` skill](https://github.com/archestra-ai/OpenAPPA/blob/main/integrations/appa-guide/SKILL.md) for policy setup and [batteries](/batteries), then use this prompt:

```text
Integrate OpenAPPA with our agent.
Use the source at https://github.com/archestra-ai/OpenAPPA.

Inspect our language, framework, and agent loop. Use the
Python binding or Rust runtime where appropriate. For other
languages, build the HTTP client and runtime adapter needed
to connect our agent. Follow the reference implementations.

Connect call checks, result checks, and remedy handling.
Preserve lifecycle events and include subagents if used.

Use the appa-guide skill to initialize the configuration,
connect batteries for our tools, and configure policy.
Ask which existing approval, classification, redaction,
and directory services we want to use.
Ask me for permissions you cannot establish from the
repository. Validate the configuration and show me the
proposed policy changes before applying them.

Test that denied calls never execute, blocked results
never reach the model, and runtime errors stop the flow.
Report any paths our framework cannot intercept.
Show the changes and test results. Do not deploy.
```

## What will happen next

The prompt asks your coding agent to add hooks: code that pauses a tool call or result while APPA checks it. Your agent still runs the tools. The integration makes it wait for APPA's decision.

### Hooks pause calls and results for a decision

The hooks report each stage of the agent's work and apply APPA's response before continuing:

| Event | When it happens | What the integration does |
|---|---|---|
| `session_start` | The conversation starts. | Continues on `ack`. Stops on `refuse`. |
| `prompt` | A turn starts. | Marks the turn boundary. Does not check prompt content. |
| `tool_call` | Before a tool runs. | Runs on `allow_call`. Returns an explanation and remedy offers on `deny_call`. Sends `pass_control` to the remedy handler. |
| `tool_result` | Before the model reads the result. | Delivers on `ack`, uses `deliver_value` or `replace_output` instead of the original, or withholds on `block`. |
| `turn_end` | The turn finishes. | Settles the turn and any unexecuted calls. |

Replacement values reach the model unchanged. Blocked results stay out of its context. If the runtime cannot respond, the pending call or result stays blocked.

### How the hooks reach APPA

With a separate runtime, a hook sends the tool name and arguments as JSON to `POST /hook` and waits for the response. With an embedded runtime, it makes a function call inside your agent's process instead. Both paths return a decision for the hook to apply.

An adapter handles naming differences between your agent and APPA. For example, it identifies which policy tool a Claude Code `Read` call refers to. Your coding agent connects an existing adapter or writes one for your agent.

### One embedded runtime can serve many policies

A host that serves several policies from one embedded runtime, such as one per organization, prepares each policy's deployment with `Runtime::prepare_deployment` and dispatches each event through `runtime.pinned(&deployment)`. Every root that view opens, every decision it makes, and every consult it sends to an annotator, authority, or membership service uses the pinned deployment, whatever the shared runtime serves. A trajectory opened under an earlier policy still decides under its stored policy file.

A pin selects a policy, not whose trajectories a view can reach. When the policies belong to separate tenants, give each dispatch its tenant's store through `runtime.on(store)`, a store that reads and writes only that tenant's trajectory logs. Also keep root trajectory ids unique across the whole runtime, for example by including the tenant id, because the runtime keys some in-process state by root id alone.

A hosted document (`Config::hosted`, `Config::hosted_composed`, `Config::hosted_included`) resolves each `token_env` through a `lookup` function the host passes in, not through the process environment. Each policy's endpoints receive only the credentials the host supplies for that policy.

### How the agent uses a remedy

Suppose APPA blocks a call but offers a way to request approval. The integration returns that offer to the model. The model can then call `appa/execute_remedy_plan` to ask APPA to carry out the offered plan.

In an HTTP integration, the hook checks this request through `POST /hook`. A `pass_control` response tells it to send the request to APPA's `/mcp` endpoint. APPA contacts the approval service specified by the policy. The agent does not approve its own blocked call.

### Subagent results pass through APPA too

Each child has a `child_id`. Three additional events connect its work to the parent:

| Event | What APPA checks | What the integration does |
|---|---|---|
| `child_start` | The child belongs to an approved launching call, linked by `spawn_binding`. | Starts on `ack`, supplies returned text on `context`, or stops on `refuse`. |
| `child_end` | The child's proposed answer. | Keeps the answer on `ack`, uses the replacement on `child_return`, or withholds on `block`. |
| `spawn_result` | Delivery of that answer into the parent's context. | Delivers on `ack`, uses `deliver_value` or `replace_output`, or withholds on `block`. |

The [runtime API reference](https://github.com/archestra-ai/OpenAPPA/tree/main/appa-runtime-api) defines the full event payloads and responses.

### The runtime keeps state between turns

The separate runtime stores its trajectory event log in SQLite, at the path selected by `--db`. Durable storage preserves that log across restarts.

| Endpoint | Purpose |
|---|---|
| `GET /health` | Check whether the service responds. |
| `GET /status` | Inspect runtime status. |
| `POST /reload` | Reload policy from disk. |
| `GET /binary-fingerprint` | Identify the running binary and configuration path. |
| `GET /policy-key` | Read the active policy hash. |

Management endpoints accept local requests only.
