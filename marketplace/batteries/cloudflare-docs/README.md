# Cloudflare documentation battery

Rules for Cloudflare's documentation MCP server, hosted at
`https://docs.mcp.cloudflare.com/mcp`. Plain TOML rules, no helper
process and no provider credential. Add it to your root config with
`include`, or install it with `appa battery install cloudflare-docs
--server <host-server-name>`.

## Server version

The server is the `docs-ai-search` app of
[`cloudflare/mcp-server-cloudflare`](https://github.com/cloudflare/mcp-server-cloudflare),
tag `docs-ai-search@0.4.13`, commit
`0c51a6fbcf9a2fae80120287e8238fb947cdc2df`. The app registers its tools in
[`apps/docs-ai-search/src/docs-ai-search.app.ts`](https://github.com/cloudflare/mcp-server-cloudflare/blob/0c51a6fbcf9a2fae80120287e8238fb947cdc2df/apps/docs-ai-search/src/docs-ai-search.app.ts),
which calls `registerDocsTools` from
[`packages/mcp-common/src/shared-tools/docs-ai-search.tools.ts`](https://github.com/cloudflare/mcp-server-cloudflare/blob/0c51a6fbcf9a2fae80120287e8238fb947cdc2df/packages/mcp-common/src/shared-tools/docs-ai-search.tools.ts).
That file is the tool list.

Earlier releases named this app `docs-vectorize`. The hostname is the same
and the tool names did not change.

## How the server exposes its tools

The app is built with `createPublicMcpApp`, so the server requires no
OAuth and reaches no Cloudflare account. It registers two tools and one
prompt; the prompt is not a tool and the battery does not name it.

| Tool | Arguments | What it does |
| --- | --- | --- |
| `search_cloudflare_documentation` | `query` | Semantic search over developers.cloudflare.com; returns page chunks with their URLs |
| `migrate_pages_to_workers_guide` | none | Fetches `developers.cloudflare.com/workers/prompts/pages-to-workers.txt` |

## Rules

Both tools return pages of the public Cloudflare developer documentation.
That text is public and was written outside the trajectory, so each read
enters `suspicious` and leaves the audience unchanged.

`search_cloudflare_documentation` sends the agent's `query` to
Cloudflare's documentation index, which serves the public web, so the
query must be sharable with `public`:
`requires = { audience = { contains = ["public"] } }`. An agent that has
read internal data cannot search the documentation until that data is
released.

`migrate_pages_to_workers_guide` takes no argument and fetches one fixed
URL, so it carries nothing out and needs no audience bound.

## Root configuration

Nothing. The battery declares no write, no effect, and no review mark, so
your root config needs no Authority for it. The server needs no
credential, so there is no credential variable.

## Limits

The documentation index is a Cloudflare AI Search instance over
developers.cloudflare.com. The battery treats every result as public
because that corpus is public. A private corpus behind the same tool name
would need a different contract.

The tool list follows `docs-ai-search@0.4.13`. Edit the TOML rules when it
changes; a tool the policy does not name is blocked.

```sh
cargo test --locked -p appa --test cloudflare_policy --test marketplace
bash scripts/appa-marketplace.sh --check
```
