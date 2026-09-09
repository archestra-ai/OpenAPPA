---
title: OpenAPPA vs OPA
category: Comparison
order: 10.7
---

[Open Policy Agent (OPA)](https://www.openpolicyagent.org/docs) is a general-purpose policy engine used across applications and infrastructure. For AI agents, OPA can check [which tools an agent may call and which arguments are allowed](https://play.openpolicyagent.org/?state=eyJpIjoie1xuICBcInRvb2xfY2FsbHNcIjogW1xuICAgIHtcbiAgICAgIFwidG9vbFwiOiBcIkJhc2hcIixcbiAgICAgIFwicGFyYW1zXCI6IHtcbiAgICAgICAgXCJjb21tYW5kXCI6IFwibHMgLWxhXCIsXG4gICAgICAgIFwidGltZW91dFwiOiA1MDAwXG4gICAgICB9XG4gICAgfSxcbiAgICB7XG4gICAgICBcInRvb2xcIjogXCJXZWJGZXRjaFwiLFxuICAgICAgXCJwYXJhbXNcIjoge1xuICAgICAgICBcInVybFwiOiBcImh0dHA6Ly9leGFtcGxlLmNvbVwiLFxuICAgICAgICBcInRpbWVvdXRcIjogNTAwMFxuICAgICAgfVxuICAgIH1cbiAgXVxufSIsImQiOiJ7fSIsInAiOiJwYWNrYWdlIGNvZGluZy50b29sc1xuXG5kZW55IGNvbnRhaW5zICRcIlRvb2wge3RjLnRvb2x9IGlzIG5vdCBhbGxvd2VkXCIgaWYge1xuXHRzb21lIHRjIGluIGlucHV0LnRvb2xfY2FsbHNcblx0dGMudG9vbCBpbiBfZGlzYWxsb3dlZF90b29sc1xufVxuXG5kZW55IGNvbnRhaW5zICRcIldlYkZldGNoIGNhbiBvbmx5IGxvYWQgZnJvbSBIVFRQUyBVUkxzXCIgaWYge1xuXHRzb21lIHRjIGluIGlucHV0LnRvb2xfY2FsbHNcblx0dGMudG9vbCA9PSBcIldlYkZldGNoXCJcblx0bm90IHN0YXJ0c3dpdGgodGMucGFyYW1zLnVybCwgXCJodHRwczovL1wiKVxufVxuXG5kZW55IGNvbnRhaW5zICRcIldlYlNlYXJjaCBjYW5ub3QgbG9hZCBtb3JlIHRoYW4ge19tYXhfcmVzdWx0c30gcmVzdWx0c1wiIGlmIHtcblx0c29tZSB0YyBpbiBpbnB1dC50b29sX2NhbGxzXG5cdHRjLnRvb2wgPT0gXCJXZWJTZWFyY2hcIlxuXHR0Yy5wYXJhbXMubnVtX3Jlc3VsdHMgPiBfbWF4X3Jlc3VsdHNcbn1cblxuZGVueSBjb250YWlucyAkXCJUb29sIHRpbWVvdXQgY2Fubm90IGJlIG1vcmUgdGhhbiAxMHNcIiBpZiB7XG5cdHNvbWUgdGMgaW4gaW5wdXQudG9vbF9jYWxsc1xuXHR0Yy5wYXJhbXMudGltZW91dCA). But OPA does not automatically know what the agent has read or done earlier. Those actions affect a decision only if your application makes that history available and your policies use it. Your application then allows or blocks the tool call based on the decision.

OpenAPPA is an agentic security framework. It supplies action-history tracking, persistent security state, and recovery plans. Reading internal data limits later sharing. Reading an untrusted source can prevent later use of tools that require trusted input. OpenAPPA carries those restrictions forward and combines them as the agent reads more data.

| | OPA | OpenAPPA |
|---|:---:|:---:|
| Policy language and evaluation engine | ✓ | ✓ |
| Express custom business rules directly in policy | ✓ | ✕ |
| Tracks action history to check later actions | ✕ | ✓ |
| Suggests ways to unblock an action | ✕ | ✓ |

## Deployment and integration

| | OPA | OpenAPPA |
|---|---|---|
| Deployment | [HTTP service, embedded Go library, or compiled WebAssembly policies](https://www.openpolicyagent.org/docs/integration) | Run alongside your agent process or deploy as a shared Kubernetes service |
| Agent connection | Your application or an integration requests and enforces decisions | Use supplied [Claude Code](/claude-code) and [kagent](/kagent) integrations, or [connect your existing agent](/writing-an-integration) |

For an agent, your integration supplies the facts each OPA policy needs and enforces the decision. OPA can [hold or retrieve policy data](https://www.openpolicyagent.org/docs/external-data), but it does not automatically track what the agent has read. Your application keeps that history up to date and makes it available when checking later actions.

OpenAPPA's integration connects tool calls and results to its security checks. When a call is blocked, the agent can use a remedy plan to clean the outgoing data or request approval, if your policy permits it. With OPA, your application defines those next steps and connects the services that carry them out.
