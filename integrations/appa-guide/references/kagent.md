# kagent

You run as a kagent declarative agent. OpenAPPA gates your own tool
calls through the shared runtime (`APPA_RUNTIME_URL` in your
environment). If a call is blocked, read the returned feedback and
follow it: a block is a decision, not an error. Do not retry the call
and do not route around it.

## Tools

- Read: `k8s_get_resources`, `k8s_get_resource_yaml`.
- Write and exec: `k8s_apply_manifest`, `k8s_execute_command`.
- Files: `skills` (this guide runs as `skills({"command": "appa-guide"})`)
  and `read_file`.

Use only these. Never kubectl, never bash, never a file write to change
policy — the policy crosses through `k8s_apply_manifest` or not at all.

### Read-only fallback

If the write tools are missing, or no appa-runtime runs in this cluster,
say so first: you can inspect and draft, not apply. Inspect with the
read tools and put the complete TOML policy in chat for the operator to
apply by hand. Do not treat this as an error.

## Rules on this host

- Batteries are not shipped for kagent yet. If asked, say so and offer
  only root rules. Never invent a battery.
- Do not configure this agent: skip the agent named `appa-guide`. The
  router's rule on the reserved `appa/execute_remedy_plan` applies the
  same way here.

## Find the live config

1. Find the appa-runtime pod with `k8s_get_resources` (the demo labels
   it `app=appa-runtime`). From its container command record the
   `--config` path, the `--listen` port, and the ConfigMap volume
   mounted at that path. No such pod: read-only fallback.
2. Read that ConfigMap with `k8s_get_resource_yaml`. The ConfigMap is
   the source of truth — never the mounted file, which kubelet syncs
   late.
3. If exec is available, `appa describe --config <path>` in the runtime
   container reports the config state, battery list, and policy tools.
   If it disagrees with the ConfigMap, stop and report the mismatch
   instead of guessing.

## Inventory

1. List every `RemoteMCPServer`. Each `status.discoveredTools` entry is
   one tool: its exact name and description. The policy names it
   `mcp/<server name>/<tool>`. A server with no discovered tools is
   uninspected — never invent its tool list.
2. List every `Agent`. From `spec.declarative.tools` record each
   `McpServer` reference and its `toolNames`, and each `type: Agent`
   delegation. Note agents with skills or `executeCodeBlocks`: they add
   the skill tools and code execution.
3. Record each `Agent`'s `spec.declarative.deployment.env`. An agent
   runs gated only when `APPA_ENABLED` reads `true` there. Unset, empty
   or `false` serves the stock kagent runtime, and no policy applies to
   that agent, whatever `APPA_RUNTIME_URL` says. Any other value
   refuses the start. A gated agent reaches this policy only when its
   `APPA_RUNTIME_URL` names the runtime you found. One that names
   another runtime runs on that runtime's policy.
4. Cross-check. A `toolNames` entry no server discovered has a name but
   no description; if its boundary is unclear, it belongs in the one
   ambiguity question below.
5. Count these as installed tools: kagent's built-in `host/kagent/ask_user`,
   and the entrypoint's gates `host/kagent-gate/code_execution` and
   `host/kagent-gate/memory_persist`. An agent with
   `spec.declarative.memory` adds the memory tools
   `host/kagent/load_memory` and `host/kagent/save_memory`. Its memory
   prefetch hands the model no function to call: no rule covers it, and
   the memories it appends cross no gate.
6. Compare the installed tools with the root rules. Existing rules stay
   in control, including rules for tools a battery would also cover.

## Tool names

A rule names a tool by its canonical tool id:

- A tool of the `RemoteMCPServer` or `ToolServer` served at
  `<toolset>`, the first label of the server host in `params.url`:
  `mcp/<toolset>/<tool>`. The same tool name on two servers is two
  contracts. A gated agent reaches that toolset only at the Kubernetes
  service forms of the same name (`<service>`,
  `<service>.<namespace>.svc`,
  `<service>.<namespace>.svc.cluster.local`) or at loopback, so the
  contract names one endpoint.
- An agent called as a tool: `agent/<namespace>/<name>`. The wildcard
  covers no spawn: a delegation needs a contract that names the agent,
  or it stays blocked.
- A kagent built-in: `host/kagent/<name>`. The entrypoint gates:
  `host/kagent-gate/code_execution` and `host/kagent-gate/memory_persist`.
- The reserved `appa/execute_remedy_plan` takes no rule.

## Cover the remaining tools

Create root rules only for installed tools the root config does not
cover.

- A tool that reads personal or authenticated data may return data for a
  configured `@self` or `@internal`. If the suitable group was not
  reported by `appa describe`, leave the tool blocked and explain the
  missing resolver. Never substitute `"private"`, `@company`, or
  another plausible reader or group.
- A tool that publishes, posts, sends, shares, or uploads requires data
  that may be public: `requires = { audience = { contains = ["public"] } }`.
- A tool that brings outside text into the session — logs, tickets,
  pages — uses `delta = { trust = "suspicious" }`.
- A state-changing action requires a person:
  `requires = { attention = ["human-approval"] }`, unless the operator
  asks otherwise.
- A clearly public read or a tool whose result carries no data uses
  `delta = {}`. Every tool entry needs `delta`, including entries with
  `requires`.
- A delegation stays blocked until the operator names it.

## Ask about ambiguity

Use tool names and descriptions when their behavior is clear. If you
still cannot tell which servers can return data that should stay
private, ask the operator once, every unclear server in one grouped
question. Wait for the answer before proposing. If nothing is unclear,
do not ask.

## Propose, then apply

Group the proposal by server and list the agents each group affects.
Show:

- existing behavior that stays unchanged, but only when it affects the
  result;
- how the remaining installed tools will behave;
- installed tools the proposal leaves undeclared: covered by a wildcard
  entry when the config has one, refused otherwise;
- blocked delegations;
- every ungated agent: "`<agent>` runs ungated, and nothing in this
  policy applies to it.";
- every uninspected server: "`<server>` is configured, but the cluster
  has not discovered its tools.";
- one short **OpenAPPA pieces** line;
- **Needed for this to work** at the end, when support is missing —
  group every missing requirement there with the concrete fix. An
  ungated agent belongs there: the fix adds `APPA_ENABLED` with the
  value `"true"` beside `APPA_RUNTIME_URL` in that agent's
  `spec.declarative.deployment.env`. Give the operator that change. Do
  not apply it yourself.

In read-only fallback, put the complete TOML in chat instead. Otherwise
end with: **Approve, or tell me what to change.** Wait for the reply.

After approval:

1. Re-read the ConfigMap. If it changed since the proposal, revise and
   ask again.
2. Merge the approved rules into the policy key, preserving comments
   and unrelated entries. Apply with `k8s_apply_manifest`, updating
   only that key. Tell the operator the kagent Approve/Reject card will
   appear — chat approval came first, the card is the enforced
   sign-off, and the apply runs only on Approve.
3. The runtime reads the mounted file, and kubelet syncs ConfigMap
   mounts on a delay. With `k8s_execute_command`, `cat <config path>`
   in the runtime container until it matches the applied policy — wait
   up to two minutes — then reload:

   ```sh
   curl --fail-with-body -sS -X POST "http://127.0.0.1:<listen-port>/reload"
   ```

4. A refused reload keeps the previous config serving. Explain the
   error plainly, fix it, and ask again if the fix changes the behavior
   the operator approved.

## Adjust

Start from the operator's requested outcome, not a full rescan. If it
is ambiguous, ask one focused question and wait.

1. Read the ConfigMap (and `appa describe` when exec helps). Explain
   what happens now, what you propose, and the practical effect, with
   the **OpenAPPA pieces** line.
2. For several rules with the same tool name, put a narrow
   argument-specific rule before its general fallback. Do not reorder
   unrelated rules.
3. Propose and apply as in `init`.

## Reload and finish

After a successful reload, give a one-to-three-sentence summary of the
behavior now in effect: what information is private or suspicious, and
where private information can or cannot go. Do not lead with rule
counts, file paths, or TOML. Mention one important remaining limitation
in one short sentence when needed. Agents left ungated are such a
limitation: name them, because the behavior you just summarized does not
reach them.

If the config changed, add:

> Start a new chat with a gated agent to use the updated policy; this
> chat keeps the policy it started with.
