# Cloudflare battery

Rules for Cloudflare's hosted MCP servers: the documentation server at
`https://docs.mcp.cloudflare.com/mcp` and the Workers Observability server
at `https://observability.mcp.cloudflare.com/mcp`. Plain TOML rules, no
helper process and no provider credential of its own. One namespace,
`cloudflare`, is bound to each server you run:

```sh
appa battery install cloudflare --server <docs-server> --server <observability-server>
```

A deployment with one of the two servers binds only that one.

## Server versions

Both servers are apps of
[`cloudflare/mcp-server-cloudflare`](https://github.com/cloudflare/mcp-server-cloudflare)
at commit `0c51a6fbcf9a2fae80120287e8238fb947cdc2df`.

The documentation server is the `docs-ai-search` app, tag
`docs-ai-search@0.4.13`. It registers its tools in
[`apps/docs-ai-search/src/docs-ai-search.app.ts`](https://github.com/cloudflare/mcp-server-cloudflare/blob/0c51a6fbcf9a2fae80120287e8238fb947cdc2df/apps/docs-ai-search/src/docs-ai-search.app.ts),
which calls `registerDocsTools`. Earlier releases named this app
`docs-vectorize`; the hostname and the tool names did not change.

The Workers Observability server is the `workers-observability` app, tag
`workers-observability@0.5.5`. Its
[`apps/workers-observability/src/workers-observability.app.ts`](https://github.com/cloudflare/mcp-server-cloudflare/blob/0c51a6fbcf9a2fae80120287e8238fb947cdc2df/apps/workers-observability/src/workers-observability.app.ts)
names three registrations, and together they are its tool list:

- `registerObservabilityTools` in
  [`apps/workers-observability/src/tools/workers-observability.tools.ts`](https://github.com/cloudflare/mcp-server-cloudflare/blob/0c51a6fbcf9a2fae80120287e8238fb947cdc2df/apps/workers-observability/src/tools/workers-observability.tools.ts)
  (3 tools);
- `registerWorkersTools` in
  [`packages/mcp-common/src/shared-tools/worker.tools.ts`](https://github.com/cloudflare/mcp-server-cloudflare/blob/0c51a6fbcf9a2fae80120287e8238fb947cdc2df/packages/mcp-common/src/shared-tools/worker.tools.ts)
  (3 tools);
- `registerDocsTools` in
  [`packages/mcp-common/src/shared-tools/docs-ai-search.tools.ts`](https://github.com/cloudflare/mcp-server-cloudflare/blob/0c51a6fbcf9a2fae80120287e8238fb947cdc2df/packages/mcp-common/src/shared-tools/docs-ai-search.tools.ts)
  (2 tools).

Both servers take the documentation tools from that one shared file, so a
tool name means the same on each server that lists it, and one rule per
name covers both. Each app also registers a prompt; a prompt is not a tool
and the battery does not name it.

## How the servers expose their tools

The documentation server is built with `createPublicMcpApp`: it requires
no OAuth and reaches no Cloudflare account. The Workers Observability
server is built with `createAuthenticatedMcpApp`; a caller connects with
OAuth or a Cloudflare API token carrying the `workers_observability:read`,
`workers:read`, and `account:read` scopes.

| Tool | Servers | Arguments | Reads |
| --- | --- | --- | --- |
| `query_worker_observability` | observability | `query` | Workers logs, metrics, and invocations |
| `observability_keys` | observability | `keysQuery` | Filter keys present in the log data |
| `observability_values` | observability | `valuesQuery` | Filter values present in the log data |
| `workers_list` | observability | none | Every Worker in the account |
| `workers_get_worker` | observability | `scriptName` | One Worker's name and script tag |
| `workers_get_worker_code` | observability | `scriptName` | One Worker's deployed source bundle |
| `search_cloudflare_documentation` | both | `query` | developers.cloudflare.com |
| `migrate_pages_to_workers_guide` | both | none | One fixed public page |

The six observability tools are registered with `accountTool`, which adds
an `account_id` argument when the credential covers more than one account.
The battery matches on the tool name only, so either shape is covered.

## Rules

**Logs and telemetry are internal and untrusted.** Workers logs carry
whatever the account's Workers wrote and whatever their callers sent:
request paths, headers, bodies, error strings. That reaches an attacker,
so `query_worker_observability`, `observability_keys` and
`observability_values` enter `suspicious`. A Cloudflare account shows its
members the same logs, so the audience is `internal`, and a query's input
must be sharable with `internal`.

**The Workers themselves are internal and untrusted.** `workers_list`,
`workers_get_worker` and `workers_get_worker_code` read the account's own
Workers. The code download is the deployed bundle, which carries
third-party dependencies the policy cannot vouch for, so it is
`suspicious` too.

**The documentation tools return public pages.** Their results were
written outside the trajectory, so they enter `suspicious` and leave the
audience alone. `search_cloudflare_documentation` sends the agent's
`query` to Cloudflare's public documentation index, so the query must be
sharable with `public`: an agent that has read internal data cannot
search the documentation until that data is released.
`migrate_pages_to_workers_guide` takes no argument and fetches one fixed
URL, so it carries nothing out and needs no audience bound.

## Root configuration

No Authority. Every tool on these servers reads, so the battery declares
no write, no effect, and no review mark.

A deployment that runs the Workers Observability server must map the
`internal` audience onto its organization's audience sources, because the
account-scoped reads narrow onto it. The documentation server needs no
credential and no audience source.

## Limits

Cloudflare exposes no per-Worker or per-dataset readers to a policy. An
API token can be scoped to fewer permissions, but no tool reports which
people may read a given Worker's logs. Every account-scoped read is
therefore `internal`: map it to the people who may see everything this
token can reach. Narrow one Worker in a root rule by argument
(`workers_get_worker_code(scriptName:payments-api)`); root rules run
first.

Log content is unsanitized. An attacker who can reach one of the
account's Workers can write text into the logs the agent reads. The
battery labels that text `suspicious`, which stops it from reaching a
tool that needs trusted input, but no Sanitizer strips it.

The battery treats every documentation result as public because the
corpus behind it, developers.cloudflare.com, is public. A private corpus
behind the same tool name would need a different contract.

The tool lists follow `docs-ai-search@0.4.13` and
`workers-observability@0.5.5`. Edit the TOML rules when they change; a
tool the policy does not name is blocked.

```sh
cargo test --locked -p appa --test cloudflare_policy --test marketplace
bash scripts/appa-marketplace.sh --check
```
