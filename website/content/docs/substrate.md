---
title: Agent Substrate
nav_title: Agent Substrate
category: Works with
order: 7.7
description: Run OpenAPPA-protected Claude Code agents as Agent Substrate actors.
---

[Agent Substrate](https://github.com/agent-substrate/substrate) runs agents as sandboxed, suspendable actors on Kubernetes. Each actor runs in a gVisor or microVM sandbox. Its egress gateway decides which hosts the actor can reach, and can inject credentials into the actor's requests after they leave the sandbox.

OpenAPPA adds the decision that a network policy cannot make: which data may flow to which tool. One hostname often serves both a private source and a public destination. An MCP gateway is one example: a meeting read and a public GitHub comment go to the same host. The egress gateway must allow that host for both calls or for neither. OpenAPPA checks each tool call against the data the session already holds and refuses the comment after the read.

## How it works

The integration packages Claude Code and the OpenAPPA runtime into one actor image:

- **Installed like a workstation.** The image runs `install.sh` and `appa plugin install claude-code`. The hooks, the runtime binary, and `clappa` are the released ones. Only the policy belongs to the deployment.
- **The runtime runs inside the sandbox.** Claude Code's hooks post only to a loopback runtime. The actor starts the runtime on `127.0.0.1:8787` before it serves any request, and its wakeup probe requires the runtime to answer.
- **Labels survive suspension.** The actor template takes a memory snapshot. When Substrate suspends the actor, the snapshot keeps the runtime process and its trajectory store. A resumed actor continues every session with the label it held.
- **No credentials in the sandbox.** The actor holds placeholder tokens. Its `EgressPolicy` names the real tokens, and the egress gateway puts them into the `Authorization` header outside the sandbox.
- **Two independent boundaries.** Substrate limits the hosts, the protocols, and the credentials. OpenAPPA limits the data that flows to each tool. Neither boundary depends on the other.

| Boundary | Enforced by | Decides |
|---|---|---|
| Sandbox | gVisor or microVM | Which syscalls, files, and devices the agent can use |
| Network | Substrate egress gateway | Which hosts the actor can reach, and which credential each request carries |
| Information flow | OpenAPPA runtime | Which data may flow into each tool call |

## Quickstart

The quickstart deploys one protected actor onto a local Substrate cluster. The actor reaches an [Archestra MCP gateway](/archestra) that serves Grain and GitHub tools. The demo policy marks a Grain read as `internal` and requires `public` data for a GitHub comment.

#### Prerequisites

- A local Substrate install: follow Substrate's [quickstart](https://github.com/agent-substrate/substrate#quickstart-development) with `KIND_CLUSTER_NAME=substrate`, and install the credential provider with `--credential-provider='{"name":"k8s.io"}'`.
- [`kubectl-ate`](https://github.com/agent-substrate/substrate/blob/main/docs/reference/kubectl-ate.md), Docker, and `envsubst`.
- An Archestra MCP gateway with the Grain `list_meetings` and `fetch_meeting` tools and the GitHub `add_issue_comment` tool assigned, and an Archestra token that the gateway accepts.
- A Claude subscription token from `claude setup-token`.

#### 1. Build the worker image

From the Substrate checkout, build the worker image of the installed version:

```sh
ATE_INSTALL_KIND=true go run ./cmd/ate-setup publish worker-images --kind --context kind-substrate
```

The command prints an `ateworker-gvisor` image reference.

#### 2. Deploy the actor

From the OpenAPPA checkout, save each token to a file, then deploy:

```sh
WORKER_IMAGE=<the ateworker-gvisor reference> \
ARCHESTRA_MCP_URL=https://<archestra-host>/v1/mcp/<gateway-id> \
CLAUDE_TOKEN_FILE=~/.config/appa-substrate-demo/claude-oauth-token \
ARCHESTRA_TOKEN_FILE=~/.config/appa-substrate-demo/archestra-token \
  integrations/substrate/deploy.sh
```

The script builds and pushes the actor image, creates the worker pool, stores the tokens in one Kubernetes Secret, and grants the demo atespace access to that Secret. It then creates the actor template, the actor, and the actor's `EgressPolicy`. The actor starts suspended.

#### 3. Open the console

Proxy the actor through Substrate's router:

```sh
kubectl ate --context kind-substrate proxy actor claude-appa 8000:8080 -a appa-substrate-demo
```

Open [http://localhost:8000](http://localhost:8000). The first request resumes the actor.

## Demonstration scenario

The console's default task asks the agent to read a meeting and post it publicly:

```text
Find my most recent Grain meeting titled "Core Team Daily Sync" and post its title and date as a comment on <public issue URL>.
```

1. The agent calls `grain__fetch_meeting`. OpenAPPA offers to narrow the session to `internal`, and the agent accepts the remedy plan. The read then runs.
2. The agent calls `github_copilot__add_issue_comment`. The comment requires data that `public` may read, and the session holds `internal` data. OpenAPPA refuses the call before it reaches the gateway.
3. The policy offers no sanitizer and no authority for this flow, so the refusal is final. The agent reports the block.

The egress gateway allowed both calls, because both go to the gateway's host.

As a second barrier, the actor server does not allow `add_issue_comment` at Claude Code's permission step. A headless session refuses an unallowed tool. If the policy admitted the comment by mistake, Claude Code would still refuse it. OpenAPPA's `PreToolUse` decision runs first, so when the policy holds, the agent sees OpenAPPA's refusal.

## Write a policy for your actors

The demo policy is `integrations/substrate/workload/demo.appa.toml`. Claude Code names an MCP gateway's tool `mcp__<server>__<gateway-tool>`. The adapter splits that name at the first `__`, so a rule names `mcp/<server>/<gateway-tool>`:

```toml
[[policy.tool]]
name = "mcp/archestra/grain__fetch_meeting"
delta = { audience = ["internal"] }

[[policy.tool]]
name = "mcp/archestra/github_copilot__add_issue_comment"
requires = { audience = { contains = ["public"] } }
delta = {}
```

A tool without a rule is refused, so the gateway's other tools stay closed. Check a policy change with a trace replay before you rebuild the image:

```sh
appa replay --config integrations/substrate/workload/demo.appa.toml \
  integrations/substrate/tests/demo-policy.appa
```

## Limits

- **One runtime per actor.** Each actor holds its own runtime and trajectory store. Two actors do not share a session label.
- **Claude Code only.** The actor image packages the Claude Code adapter. Another harness needs its own adapter in the image.
- **The policy is in the image.** A policy change requires a new image and a new actor template.
- **The network boundary sees no arguments.** Substrate's egress `ext_proc` callout receives request headers, not bodies, so a check at the gateway cannot read tool arguments. OpenAPPA checks them inside the sandbox.

## Uninstall

```sh
integrations/substrate/deploy.sh delete
```

The command deletes the actor, its template, and the demo namespace. The atespace and its Secret grant remain.
