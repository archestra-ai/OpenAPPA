# OpenAPPA for Agent Substrate

This directory holds the OpenAPPA integration for
[Agent Substrate](https://github.com/agent-substrate/substrate): an actor
image that runs Claude Code under OpenAPPA, the Substrate manifests that
deploy it, and a deploy script for a local cluster.

- **Public documentation**: [website/content/docs/substrate.md](../../website/content/docs/substrate.md)

This project is not affiliated with Agent Substrate. Trademarks belong to
their respective owners.

## Components

```text
integrations/substrate/
├── workload/                  # The actor image
│   ├── Dockerfile             # node + Claude Code + install.sh + appa plugin install claude-code
│   ├── entrypoint.sh          # starts the loopback runtime, then the actor server
│   ├── actor-server.mjs       # GET / console, GET /healthz wakeup probe, POST /run (NDJSON)
│   ├── ui/index.html          # the console page
│   └── demo.appa.toml         # the demo policy, baked in as ~/.config/appa/appa.toml
├── manifests/                 # WorkerPool, ActorTemplate, EgressPolicy templates
├── tests/demo-policy.appa     # trace replay of the flows the demo policy decides
└── deploy.sh                  # build, push, and create everything; `delete` removes it
```

## Design

- The runtime runs inside the actor. Claude Code's hooks post only to a
  loopback runtime, and a memory snapshot keeps the runtime's trajectory
  store across suspend and resume.
- The actor holds placeholder tokens. Its `EgressPolicy` replaces each
  `Authorization` header with a Kubernetes Secret through Substrate's
  credential provider.
- The actor server runs each task as `clappa -p` with `--strict-mcp-config`,
  no built-in tools, and an allow-list that leaves out the public write. A
  headless session refuses an unallowed tool, so a policy mistake cannot
  post. OpenAPPA's `PreToolUse` decision runs first.

## Test

```sh
appa replay --config workload/demo.appa.toml tests/demo-policy.appa
```

The replay needs no cluster and no model. It checks that a fresh session
may comment, that a Grain read narrows the session to `internal`, and that
the comment is refused after the read.
