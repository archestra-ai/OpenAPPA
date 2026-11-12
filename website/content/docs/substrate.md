---
title: Agent Substrate
nav_title: Agent Substrate
category: Works with
order: 7.7
description: Enforce OpenAPPA at Agent Substrate's egress gateway, for every actor and any agent harness.
---

[Agent Substrate](https://github.com/agent-substrate/substrate) runs agents as sandboxed, suspendable actors on Kubernetes. Every request an actor makes leaves through Substrate's egress gateway. The gateway decides which hosts the actor may reach and injects credentials outside the sandbox.

A host allowlist can't tell two calls to the same host apart. A read of a private repository and a comment on a public issue both go to `api.githubcopilot.com`. OpenAPPA runs at the same gateway and checks each MCP tool call against the data the actor already holds. After the private read, it refuses the public comment before the request leaves the cluster.

Nothing runs inside the sandbox, so the agent image needs no plugin, hook, or SDK. Claude Code, another harness, or `curl` gets the same checks.

## How it works

- **The gateway calls OpenAPPA.** Substrate's egress gateway calls an additional `ext_proc` service, `appa-extproc`, for each request. For a host configured as an MCP server, the processor asks for the request body and the response. Other traffic passes on its headers.
- **One label per actor.** The gateway passes the actor's SPIFFE ID, taken from the verified certificate of the actor's tunnel. The processor keys the trajectory on it. Every process and MCP session in one actor shares the label, and nothing inside the sandbox can name another actor.
- **Each `tools/call` is checked.** The processor asks the OpenAPPA runtime whether the call may run. A refused call gets a tool error that carries the runtime's reason, and the request never reaches the server. After an allowed call, the processor reports the result, and the runtime updates the label.
- **Narrowing is taken for the agent.** The agent doesn't know about OpenAPPA, so the processor takes a remedy that only narrows the label and needs no review, then runs the call. A remedy that asks a person is refused.
- **Fail closed.** A body the processor can't read, or a runtime that doesn't answer, blocks the call. The gateway refuses requests while the processor is down.

| Boundary | Enforced by | Decides |
|---|---|---|
| Sandbox | gVisor or microVM | Which syscalls, files, and devices the agent can use |
| Network | Substrate egress gateway | Which hosts the actor can reach, and which credential each request carries |
| Information flow | OpenAPPA at the egress gateway | Which data may flow into each tool call |

The processor and the runtime run in one pod in `appa-system`. The runtime binds loopback, so nothing else in the cluster reaches it.

## Requirements

The processor needs Substrate's egress gateway to pass request and response bodies. That support is proposed in [agent-substrate/substrate#3817](https://github.com/agent-substrate/substrate/pull/3817). Until it merges, run `ate-setup` from that branch.

Install Substrate with the processor configured:

```sh
ATE_INSTALL_KIND=true go run ./cmd/ate-setup deploy ate-system --context kind-substrate \
  --credential-provider='{"name":"k8s.io"}' \
  --ingress-allowed-spiffe-ids=spiffe://cluster.local/ns/ate-system/sa/atenet-client \
  --experimental-additional-egress-extproc-service=appa-system/appa-extproc:9443 \
  --experimental-additional-egress-extproc-allow-mode-override \
  --experimental-additional-egress-extproc-timeout=60s
```

You also need [`kubectl-ate`](https://github.com/agent-substrate/substrate/blob/main/docs/reference/kubectl-ate.md), Docker, and `envsubst`.

## Quickstart

The quickstart deploys the enforcement point and one actor running stock Claude Code against GitHub's MCP server.

#### 1. Build the worker image

From the Substrate checkout:

```sh
ATE_INSTALL_KIND=true go run ./cmd/ate-setup publish worker-images --kind --context kind-substrate
```

The command prints an `ateworker-gvisor` image reference.

#### 2. Deploy

Save a Claude subscription token from `claude setup-token` and a GitHub token to files. Then run the deploy script from the OpenAPPA checkout:

```sh
WORKER_IMAGE=<the ateworker-gvisor reference> \
CLAUDE_TOKEN_FILE=<claude token file> \
GITHUB_TOKEN_FILE=<github token file> \
  integrations/substrate/deploy.sh
```

The script does the following:

- Builds `appa-extproc` and the agent image.
- Deploys the processor and the runtime to `appa-system`, with the policy in `integrations/substrate/policy/appa.toml`.
- Creates the actor and its `EgressPolicy`.

The GitHub token has two uses. The egress gateway injects it into the actor's MCP requests, and the policy uses it to look up who may read a repository.

#### 3. Open the console

```sh
kubectl ate --context kind-substrate proxy actor agent 8000:8080 -a appa-substrate-demo
kubectl --context kind-substrate -n appa-system port-forward deploy/appa-extproc 8001:8081
```

Open `http://localhost:8000/?log-port=8001`. The left side runs the agent. The right side is the processor's decision log.

## Scenario

Ask the agent to read a file in a private repository and post a summary on a public issue:

1. The agent calls `get_file_contents`. The read would narrow the actor's label to the repository's collaborators. The processor takes the narrowing, and the read runs.
2. The agent calls `add_issue_comment` on the public issue. A public issue's readers are everyone, and the actor holds data only the collaborators may read. The processor answers the call with OpenAPPA's refusal, and the agent reports it.
3. **Same comment with curl** sends that comment with `curl` from the same actor. The request is refused the same way, because the label belongs to the actor, not to the client.

## Policy

The policy includes the GitHub [battery](/docs/battery-github). A tool crosses the wire as `mcp:<server>/<tool>`, and a rule names it `mcp/<server>/<tool>`. The processor's `--mcp-server` flag maps each MCP host to a server name; the manifest maps `api.githubcopilot.com` to `github`. Add rules for other MCP servers below the include, and add their hosts with `--mcp-server`.

Start a fresh label for an actor:

```sh
curl -X POST 'localhost:8001/reset?actor=spiffe://cluster.local/worker-for-actor/<atespace>/<actor>'
```

## Limits

- **MCP over streamable HTTP only.** The processor reads JSON-RPC `tools/call` requests sent as HTTP POST. Other traffic to an allowed host, such as GitHub's REST API, isn't checked, so allow only the MCP hosts in the actor's `EgressPolicy`.
- **HTTPS rules only.** The gateway terminates TLS only for hosts under an `https` rule. A `tls_passthrough` host never reaches the processor.
- **Buffered bodies.** The gateway buffers each gated request and its response. A response larger than the gateway's buffer limit fails.
- **One replica.** The trajectory store is the runtime's local database on an `emptyDir`, so a pod restart resets every label.
- **No reviewed remedies.** A remedy that asks a person is refused, because the agent has no channel to the reviewer.

## Uninstall

```sh
integrations/substrate/deploy.sh delete
```

Reinstall Substrate without the `--experimental-additional-egress-extproc-*` flags. While they stay set, the gateway refuses requests that the processor doesn't answer.
