---
title: Add to your agent · v2
category: Works with
order: 8.1
description: Embed APPA in your own agent, connect a coding agent through hooks, or use it through the Archestra LLM proxy.
---

Your agent runs the tools. APPA checks whether your rules allow each tool call and the data returned. See [How it works](/how-it-works) for what happens when something is blocked and how the agent can continue.

:::fig-runtime-overview-v2:::

Choose how to connect: add APPA to your own agent's code, use an existing agent's hooks, or route model requests through Archestra.

:::integration-paths:::

## Embed the SDK in your agent

Use APPA with an agent written in Python, Rust, TypeScript, Java, Go, or another language. This includes agents built with LangGraph, PydanticAI, or the OpenAI Agents SDK. Keep your stack and add the integration to your existing code.

Start with the [Python binding](https://github.com/archestra-ai/OpenAPPA/tree/main/appa-agent-python) and [Rust example agent](https://github.com/archestra-ai/OpenAPPA/tree/main/appa-example-agent) as references. Give your coding agent the [integration prompt below](#start-in-your-repository) to build and test the integration for your agent.

See [What changes in your agent](#what-changes-in-your-agent) for implementation details.

## Connect a coding agent through hooks

Connect an existing agent through its lifecycle hooks. The hooks pause tool execution and result delivery, ask APPA for a decision, and apply it before the agent continues. This works with any harness whose hooks can block a call and withhold or replace its result.

APPA runs alongside the agent as a local process, sidecar, or shared service. For a new harness, your coding agent can build the hook integration and runtime adapter using the existing implementations as references.

[Claude Code](/claude-code) is a ready-made example for a coding agent; [kagent](/kagent) shows the same approach for Kubernetes agents. Their dedicated guides cover setup and configuration.

See [What changes in your agent](#what-changes-in-your-agent) for the events and decisions your integration needs to handle.

## Use APPA at the LLM proxy

If you manage agents through a shared gateway, use **[Archestra](/archestra)**. It integrates OpenAPPA at the LLM proxy layer and also provides an MCP gateway, so you can manage policy there instead of embedding the runtime in each agent.

## What changes in your agent

Whether you embed APPA or connect over HTTP, the integration has three jobs:

:::integration-checkpoints:::

APPA can check only the flows your integration submits and controls. Test that denied calls never run and blocked results never reach the model, including through alternate tool paths and subagents. [Policy replay in CI](/validation-v2) tests the policy; integration tests check that your agent enforces the decisions.

:::integration-details:::

## Connect to your observability stack

See what your agent was allowed to do, what was blocked, and why. OpenTelemetry export is in development so you can investigate decisions and policy-check times in your existing monitoring tools. See [Observability](/observability-v2) for the planned metrics, logs, and setup.

## Validate policies in CI

Test policy changes before they reach your agents. `appa replay` checks sequences of tool calls against the decisions you expect, without running the tools. See [CI Validation](/validation-v2) for a working example and how to run it in your pipeline.

## Self-improving policies

Agents can report confusing decisions through `appa yell`. The planned maintenance workflow uses those reports and telemetry to propose and test policy changes for your review. See [Self-improving policies](/self-improving-policies-v2) for the workflow and an example skill.

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
