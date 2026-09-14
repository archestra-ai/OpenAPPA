# Cloudflare Workers Observability battery

Rules for Cloudflare's Workers Observability MCP server, hosted at
`https://observability.mcp.cloudflare.com/mcp`. Plain TOML rules, no
helper process and no provider credential of its own. Add it to your root
config with `include`, or install it with `appa battery install
cloudflare-observability --server <host-server-name>`.

## Server version

The server is the `workers-observability` app of
[`cloudflare/mcp-server-cloudflare`](https://github.com/cloudflare/mcp-server-cloudflare),
tag `workers-observability@0.5.5`, commit
`0c51a6fbcf9a2fae80120287e8238fb947cdc2df`. The app registers its tools in
[`apps/workers-observability/src/workers-observability.app.ts`](https://github.com/cloudflare/mcp-server-cloudflare/blob/0c51a6fbcf9a2fae80120287e8238fb947cdc2df/apps/workers-observability/src/workers-observability.app.ts).
That file names three registrations, and together they are the tool list:

- `registerObservabilityTools` in
  [`apps/workers-observability/src/tools/workers-observability.tools.ts`](https://github.com/cloudflare/mcp-server-cloudflare/blob/0c51a6fbcf9a2fae80120287e8238fb947cdc2df/apps/workers-observability/src/tools/workers-observability.tools.ts)
  (3 tools);
- `registerWorkersTools` in
  [`packages/mcp-common/src/shared-tools/worker.tools.ts`](https://github.com/cloudflare/mcp-server-cloudflare/blob/0c51a6fbcf9a2fae80120287e8238fb947cdc2df/packages/mcp-common/src/shared-tools/worker.tools.ts)
  (3 tools);
- `registerDocsTools` in
  [`packages/mcp-common/src/shared-tools/docs-ai-search.tools.ts`](https://github.com/cloudflare/mcp-server-cloudflare/blob/0c51a6fbcf9a2fae80120287e8238fb947cdc2df/packages/mcp-common/src/shared-tools/docs-ai-search.tools.ts)
  (2 tools).

Eight tools. The app also registers one prompt, `workers-prompt-full`; a
prompt is not a tool and the battery does not name it.

## How the server exposes its tools

The app is built with `createAuthenticatedMcpApp`. A caller connects with
OAuth or a Cloudflare API token carrying the `workers_observability:read`,
`workers:read`, and `account:read` scopes.

| Tool | Arguments | Reads |
| --- | --- | --- |
| `query_worker_observability` | `query` | Workers logs, metrics, and invocations |
| `observability_keys` | `keysQuery` | Filter keys present in the log data |
| `observability_values` | `valuesQuery` | Filter values present in the log data |
| `workers_list` | none | Every Worker in the account |
| `workers_get_worker` | `scriptName` | One Worker's name and script tag |
| `workers_get_worker_code` | `scriptName` | One Worker's deployed source bundle |
| `search_cloudflare_documentation` | `query` | developers.cloudflare.com |
| `migrate_pages_to_workers_guide` | none | One fixed public page |

The first six are registered with `accountTool`, which adds an
`account_id` argument when the credential covers more than one account.
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

**The documentation tools return public pages.** This server also carries
the two tools of Cloudflare's documentation server. They leave the
audience alone, and `search_cloudflare_documentation` sends the agent's
`query` to Cloudflare's public documentation index, so that query must be
sharable with `public`.

## Root configuration

No Authority. Every tool on this server reads, so the battery declares no
write, no effect, and no review mark.

Your root config must map the `internal` audience onto your
organization's audience sources, because the account-scoped reads narrow
onto it.

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

The tool list follows `workers-observability@0.5.5`. Edit the TOML rules
when it changes; a tool the policy does not name is blocked.

```sh
cargo test --locked -p appa --test cloudflare_policy --test marketplace
bash scripts/appa-marketplace.sh --check
```
