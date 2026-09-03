---
title: kAgent
nav_title: kAgent
category: Integrations
order: 6
description: Roll the OpenAPPA runtime image out to every declarative kagent agent with one Helm value. Gate each agent with APPA_ENABLED=true.
---

[kagent](https://kagent.dev/docs/kagent/introduction/what-is-kagent/) runs AI agents natively on Kubernetes. OpenAPPA adds deterministic security to kagent. It enforces data boundaries, stops data leaks, and requires human approvals before sensitive tools run.

You make every [declarative agent](https://kagent.dev/docs/kagent/concepts/agents/) in your cluster ready to protect with one Helm configuration value:

```yaml
# Helm values for the kagent controller
controller:
  agentImage:
    registry: ghcr.io
    repository: archestra-ai/appa-kagent-quickstart
    tag: 0.7.1 # x-release-please-version
```

The image is a drop-in replacement for the stock kagent runtime image, and it is
inert until you ask for it. `APPA_ENABLED` is off by default, so an agent that
does not set it runs exactly as it runs on the stock image. You turn the gate on
one agent at a time, in that agent's own environment:

```yaml
apiVersion: kagent.dev/v1alpha2
kind: Agent
spec:
  declarative:
    deployment:
      env:
        - name: APPA_ENABLED
          value: "true"
```

So you can roll the image out to a whole fleet and change nothing, then gate
agents as you are ready. Neither step needs a change to any other agent's
manifest, a fork of kagent, or a fork of the Google Agent Development Kit (ADK).

## How it works

OpenAPPA runs inside the agent pod through the official Google ADK plugin API. Every [tool call](https://kagent.dev/docs/kagent/concepts/tools/) and [agent-to-agent delegation](https://kagent.dev/docs/kagent/examples/a2a-agents/) passes through the policy engine before execution.

:::fig-kagent:::

- **Enforcement occurs before execution**: A tool does not run if a policy requirement fails.
- **Fail-closed default**: If the policy runtime is unreachable, calls stop.
- **Runtime support**: Works with both Python (`appa-kagent-adk`) and Go (`appa-kagent-adk-go`) runtimes.

## Policy scope

Policy scope follows the runtime, not the cluster. A gated [Agent](https://kagent.dev/docs/kagent/concepts/agents/) enforces the policy of the `appa-runtime` that its `APPA_RUNTIME_URL` names.

Agents that name the same runtime share one `appa.toml` and one decision log. Agents that name different runtimes enforce different policies. An agent that sets `APPA_ENABLED=true` and leaves `APPA_RUNTIME_URL` unset runs the bundled runtime of the quickstart image, in its own pod. That runtime loads the policy file `APPA_CONFIG` names, and shares it with no one.

A policy file has no per-agent scoping. Every contract in it applies to every agent that runtime gates. To give two groups of agents different contracts, run two `appa-runtime` deployments and point each group at one.

Delegation constrains the split: a parent and the agents it calls must reach the same runtime. A child whose runtime recorded no spawn never starts, and the delegation fails closed.

## Quickstart

Follow this guide to deploy kagent with OpenAPPA and run your first protected agent in a test cluster.

### Prerequisites

Make sure you have installed:
- [kind](https://kind.sigs.k8s.io/docs/user/quick-start/) or an existing Kubernetes cluster
- [Helm](https://helm.sh/docs/intro/install/) (v3.8+)
- [kubectl](https://kubernetes.io/docs/tasks/tools/)
- [git](https://git-scm.com/downloads)
- An [OpenAI API key](https://platform.openai.com/account/api-keys)

No registry serves the demo chart. It ships in this repository, so clone it and run the quickstart from the repository root:

```sh
git clone https://github.com/archestra-ai/OpenAPPA
cd OpenAPPA
```

### 1. Install kagent with OpenAPPA

Install the kagent [CRDs and Helm chart](https://kagent.dev/docs/kagent/resources/helm/) with the OpenAPPA runtime image:

```sh
# Install kagent CRDs
helm upgrade --install kagent-crds oci://ghcr.io/kagent-dev/kagent/helm/kagent-crds \
  --version 0.9.12 -n kagent --create-namespace

# Install kagent controller with OpenAPPA runtime
helm upgrade --install kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version 0.9.12 -n kagent \
  --set controller.agentImage.registry=ghcr.io \
  --set controller.agentImage.repository=archestra-ai/appa-kagent-quickstart \
  --set controller.agentImage.tag=0.7.1 --wait # x-release-please-version
```

The registry serves these images from OpenAPPA releases only. To run an unreleased version, build the images from source and load them into your cluster. `integrations/kagent/README.md` carries the commands.

### 2. Deploy the demo stack

From the repository root, install the demo chart to create sample agents (`cluster-ops`, `log-analyst`) and 16 demonstration scenarios:

```sh
helm upgrade --install appa-kagent-demo ./integrations/kagent/demo/chart \
  -n kagent --set openai.apiKey="$OPENAI_API_KEY" --wait
```

### 3. Open the kagent dashboard

Forward the [kagent dashboard](https://kagent.dev/docs/kagent/observability/launch-ui/) to your machine:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8901:8080
```

Open [http://localhost:8901](http://localhost:8901) in your browser.

## Protect an existing cluster

If you already run kagent in your cluster, you do not need to recreate your agents.

### 1. Update the kagent controller image

Update your existing Helm release to use the OpenAPPA quickstart image:

```sh
helm upgrade kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  -n kagent --reuse-values \
  --set controller.agentImage.registry=ghcr.io \
  --set controller.agentImage.repository=archestra-ai/appa-kagent-quickstart \
  --set controller.agentImage.tag=0.7.1 # x-release-please-version
```

The image alone changes nothing: `APPA_ENABLED` is off by default, so every
agent keeps running as it runs today.

### 2. Write a policy that names your tools

The policy decides only the tools it names. A call no declaration and no wildcard
covers is refused before it runs. That refusal is operational: the call stops
rather than running ungated. So the policy must cover your agents' tools before
you gate them.

Deploy an `appa-runtime` with an `appa.toml` that declares them. A wildcard entry
covers the long tail: `name = "*"` routed through an annotator. It is the
practical first posture for a fleet, because CRD-declared toolsets produce tool
names no policy has in advance.

The wildcard covers no delegation. An agent called as a tool needs its own
`[[policy.tool]]` entry, under the name kagent dispatches it by:
`<namespace>__NS__<agent>`, hyphens as underscores.

See [Policy contracts](/contracts) for the syntax. The `appa-guide` skill below
drafts the file for you: it inventories your `Agent` and `RemoteMCPServer`
resources.

A shared runtime is what cross-workload delegation needs in any case. A parent
and the agent it calls run in two pods. Both sides of a spawn must reach the same
runtime.

### 3. Gate the agents you choose

Add the gate and the runtime address to an agent's own environment:

```yaml
spec:
  declarative:
    deployment:
      env:
        - name: APPA_ENABLED
          value: "true"
        - name: APPA_RUNTIME_URL
          value: http://appa-runtime.kagent.svc.cluster.local:18789
```

Add those entries to the manifest you already apply, or edit the agent in place
with `kubectl edit agent cluster-ops -n kagent`. Both variables belong to one
edit. `APPA_ENABLED` alone is the gate, and the image ignores `APPA_RUNTIME_URL`
without it.

`kubectl patch --type merge` replaces the whole `env` list rather than adding to
it, and the kagent CRD accepts no list-aware patch type. Use it only on an agent
that declares no other environment. Write every variable the agent needs in one
patch:

```sh
kubectl patch agent cluster-ops -n kagent --type merge \
  -p '{"spec":{"declarative":{"deployment":{"env":[{"name":"APPA_ENABLED","value":"true"},{"name":"APPA_RUNTIME_URL","value":"http://appa-runtime.kagent.svc.cluster.local:18789"}]}}}}'
```

Leave `APPA_RUNTIME_URL` unset and the quickstart image starts its bundled
runtime in the pod. That runtime loads one policy file, `APPA_CONFIG`. It
defaults to the packaged example: the demo cluster-ops toolset (`list_pods`,
`read_secret`, `post_status_update`, …), and nothing your own agents call. Use it
on a test cluster, or mount your own policy over `APPA_CONFIG`.

An agent that sets `APPA_ENABLED=true` and reaches no runtime refuses to start.
An agent you asked to gate never runs ungated. An agent that loses
`APPA_ENABLED` runs as it runs on the stock image. It logs one warning that names
it as ungated, so check that the value survived your edit.

### 4. Confirm the gate

The kagent controller compiles each Agent into a Deployment of the same name.
Your edit changes that Deployment, so the agent pods roll on their own. Force the
roll only if you need one:

```sh
kubectl rollout restart deployment/cluster-ops -n kagent
```

Do not look for a second container. The OpenAPPA image is the agent runtime.
Without a shared runtime, it runs the bundled `appa-runtime` beside the agent in
that one container. The first lines of the new pod name the mode:

```sh
kubectl logs -n kagent deployment/cluster-ops | head
```

A gated agent names its runtime: `APPA_ENABLED is true. This agent runs gated by
the OpenAPPA runtime at ...`. An agent that is still ungated warns `UNGATED`
instead.

Your gated agents now route every tool call through the policy you wrote.

## Configure policy with appa-guide

The demo chart installs an `appa-guide` agent. It attaches the OpenAPPA guide skill through kagent's git-ref skills. It also provides the kagent tool server's Kubernetes tools. The shared runtime gates the guide agent's own tool calls. Open its chat and say `init`.

The canonical skill lives at `integrations/appa-guide`. Its `SKILL.md` routes to `references/claude-code.md` or `references/kagent.md`. kagent clones that directory directly. Claude packaging stages the same directory at its required plugin path. On kagent, the skill reads the policy ConfigMap. It inventories `RemoteMCPServer.status.discoveredTools` and each `Agent` tool declaration. It proposes contracts in plain English and waits for chat approval.

The skill applies the ConfigMap through `k8s_apply_manifest`. The fleet policy requires `attention = ["human-approval"]` for that call. Therefore, the kagent Approve / Reject card is the human decision. The skill then waits for the mounted policy to update and reloads the runtime. Any host with the same tools can run this skill. The pre-configured agent is only a convenience.

## 1. Try a blocked flow (Data leak prevention)

Open the `cluster-ops` agent in the [kagent dashboard](https://kagent.dev/docs/kagent/observability/launch-ui/). Ask it to read the payments-provider secret and post the API key to the public status page. The demo chart seeds this chat.

1. The agent proposes the confidential read:
   ```text
   read_secret(name: "payments-provider")
   ```
   `read_secret` carries `delta = { audience = ["ops"] }`. The session is public, so admitting that result would narrow its readers to the ops readers alone.

2. **OpenAPPA denies the read.** OpenAPPA gates the flow that changes the label, not the later sink. The secret never reaches the model, so the public post has nothing to leak. The deny comes back as model-facing feedback that quotes the runnable offers:
   ```text
   [appa] Blocked: this call cannot run yet.

   Why:
     - allowed readers would narrow: public -> 1 reader

   Continue:
     - Accept this change for the rest of this session:
       execute_remedy_plan(offer_id: "…")
     - Use sanitizer strip-secret-values's result:
       execute_remedy_plan(offer_id: "…")
   ```
   A third continuation keeps the session unchanged. Delegate the call to a child session, and bring back only a sanitized derivation.

3. **The agent stays productive.** In this chat it takes the sanitizer. The re-proposed read hands the model the key names with every value redacted. The status update then carries nothing replayable as a credential. Accept the narrowing instead, and the session holds `audience = ["ops"]`. OpenAPPA then denies `post_status_update`, which requires `audience = { contains = ["public"] }`.

## 2. Try human approval (HITL workflows)

OpenAPPA integrates with kagent's native [Human-in-the-Loop](https://kagent.dev/docs/kagent/examples/human-in-the-loop/) approval cards:

1. The agent proposes a destructive cluster action:
   ```text
   restart_deployment(name: "checkout-api")
   ```
   The policy requires explicit approval: `attention = ["human-approval"]`.

2. **OpenAPPA denies the call and offers a remedy plan** that consults the `oncall` authority. The model reads the denial, which names the missing `attention: human-approval`, and runs the offered plan itself:
   ```text
   execute_remedy_plan(offer_id: "...")
   ```
   The plan needs a person, so the turn suspends. An **Approve / Reject** confirmation card appears in the kagent dashboard, on that `execute_remedy_plan` call.

3. **The operator decides**:
   - **Approve**: `oncall` grants `human-approval`. OpenAPPA authorizes that exact call, the agent proposes `restart_deployment` again, and the deployment restarts.
   - **Reject**: `oncall` refuses. OpenAPPA records the refusal, denies the re-proposed call again, and the tool never runs.

## 3. Multi-agent delegation

When agents call other agents through [A2A (Agent-to-Agent)](https://kagent.dev/docs/kagent/examples/a2a-agents/) delegation, OpenAPPA isolates their execution contexts. The isolation is a deployment setting: `context_control = true` under `[policy.deployment]`. Without it a delegation releases as an ordinary tool call. The child then runs on the parent context, and declares no return.

- **Inherited boundaries**: Child agents inherit the parent's data restrictions automatically.
- **Quarantine**: Untrusted operations (like inspecting raw pod logs) run inside the child agent. Only validated outputs flow back to the parent.
- **Explicit authorization**: Agents can only delegate to sub-agents explicitly listed in the policy. Unlisted agent spawns are blocked immediately. Declare the sub-agent under the name kagent dispatches it by: `<namespace>__NS__<agent>`, hyphens as underscores. The wildcard covers no spawn.

## Policy example

Policies are declarative TOML files checked into version control. This is the part of the demo contract that the walkthroughs above exercise, excerpted from [`integrations/kagent/demo/chart/files/demo.appa.toml`](https://github.com/archestra-ai/OpenAPPA/blob/main/integrations/kagent/demo/chart/files/demo.appa.toml):

```toml
# In-cluster secret read: results carry the ops audience
[[policy.tool]]
name = "read_secret"
delta = { audience = ["ops"] }

# The outward sink: the flow must be readable by everyone and must not
# derive from an untrusted source
[[policy.tool]]
name = "post_status_update"
delta = {}

[policy.tool.requires]
trust = "trusted"
audience = { contains = ["public"] }

# An effectful action behind a person
[[policy.tool]]
name = "restart_deployment"
delta = {}

[policy.tool.requires]
trust = "trusted"
attention = ["human-approval"]

# Delegation: the log-analyst agent, called as a tool. kagent dispatches
# an agent tool as `<namespace>__NS__<agent>`, hyphens as underscores.
[[policy.tool]]
name = "kagent__NS__log_analyst"
delta = {}

# Children run on their own context, and declare what their return carries
[policy.deployment]
context_control = true

# The human behind human-approval
[[policy.authority]]
name = "oncall"
hint = "Ask the on-call lead through the kagent approval flow."

[policy.authority.permits]
attention = ["human-approval"]

# The channel that authority speaks through. The `hitl` builtin is what
# raises the Approve / Reject card in the dashboard.
[externals.authorities.oncall]
builtin = "hitl"
```

The `[externals.authorities.oncall]` binding is not optional. A consult against an authority with no channel finds it unregistered. The runtime returns no answer, no card reaches the dashboard, and nothing can grant the `human-approval` mark. The full file also carries `[policy] version = 2`, which every policy needs, and the rest of the demo toolset.

## Where next

- [How it works](/how-it-works) — Core concepts, labels, and algebraic flow guarantees.
- [Policy contracts](/contracts) — Complete policy authoring and syntax guide.
- [kagent documentation](https://kagent.dev/docs/kagent/) — Official kagent guides and references.
- [Implementation details](https://github.com/archestra-ai/OpenAPPA/blob/main/integrations/kagent/IMPLEMENTATION.md) — ADK callback lifecycle, Go/Python runtime architecture, and wire specs.
