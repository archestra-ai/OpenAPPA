# Cloudflare Radar battery

Rules for Cloudflare's Radar MCP server, hosted at
`https://radar.mcp.cloudflare.com/mcp`. Plain TOML rules, no helper
process and no provider credential of its own. Add it to your root config
with `include`, or install it with `appa battery install cloudflare-radar
--server <host-server-name>`.

## Server version

The server is the `radar` app of
[`cloudflare/mcp-server-cloudflare`](https://github.com/cloudflare/mcp-server-cloudflare),
tag `cloudflare-radar-mcp-server@0.2.5`, commit
`0c51a6fbcf9a2fae80120287e8238fb947cdc2df`. The app registers its tools in
[`apps/radar/src/radar.app.ts`](https://github.com/cloudflare/mcp-server-cloudflare/blob/0c51a6fbcf9a2fae80120287e8238fb947cdc2df/apps/radar/src/radar.app.ts),
which calls `registerRadarTools` from
[`apps/radar/src/tools/radar.tools.ts`](https://github.com/cloudflare/mcp-server-cloudflare/blob/0c51a6fbcf9a2fae80120287e8238fb947cdc2df/apps/radar/src/tools/radar.tools.ts)
(61 tools) and `registerUrlScannerTools` from
[`apps/radar/src/tools/url-scanner.tools.ts`](https://github.com/cloudflare/mcp-server-cloudflare/blob/0c51a6fbcf9a2fae80120287e8238fb947cdc2df/apps/radar/src/tools/url-scanner.tools.ts)
(5 tools). Those two files are the tool list: 66 tools.

## How the server exposes its tools

The app is built with `createAuthenticatedMcpApp`. A caller connects with
OAuth or a Cloudflare API token carrying the `radar:read`,
`account:read`, and `url_scanner:write` scopes.

The 61 Radar tools are registered with `registerTool` and read
`api.cloudflare.com/client/v4/radar`, the endpoint behind
radar.cloudflare.com. They cover traffic, outages and traffic anomalies,
BGP (hijacks, leaks, MOAS, RPKI ASPA, routing tables), DNS, layer 3 and
layer 7 attacks, bots and crawlers, `robots.txt`, certificate
transparency, netflows, email routing and security, Internet speed and
quality, AI traffic, leaked-credential trends, rankings, and the ASN, IP,
TLD, origin, bot and geolocation entity lookups.

The 5 URL Scanner tools are registered with `accountTool` and read or
write `accounts/<account>/urlscanner/v2`. `accountTool` adds an
`account_id` argument when the credential covers more than one account;
the battery matches on the tool name only, so either shape is covered.

## Rules

**Radar reads return public data.** Radar publishes the same numbers to
every caller on radar.cloudflare.com. The data is measured outside the
trajectory, so each read enters `suspicious` and leaves the audience
alone: `delta = { trust = "suspicious" }`. Their arguments are filters -
an ASN, a prefix, a date range, a location - sent to the account's own
authenticated Cloudflare API, which only reads, so no new reader sees
them and these reads carry no audience bound.

**URL Scanner reads are internal.** `search_url_scans`, `get_url_scan`,
`get_url_scan_screenshot` and `get_url_scan_har` read scans the token's
account owns. Their content is whatever page was scanned, published by a
third party, so they enter `suspicious` restricted to `internal`, and
their input must be sharable with `internal`.

**One write.** `create_url_scan` makes Cloudflare fetch the `url` the
agent supplies, and the resulting scan, screenshot and HAR stay visible
to everyone unless the call sets `visibility` to `Unlisted`. The URL
leaves for the public Internet and the scan record is published, so the
call needs trusted data that `public` may see and the
`cloudflare-radar-review` mark, and records
`cloudflare-radar.sensitive`.

Your root config must define an authority permitting
`cloudflare-radar-review`:

```toml
[[policy.authority]]
name = "cloudflare-radar-operator"
hint = "Review the URL Cloudflare is about to fetch and publish a scan of."
permits = { trust_below = "trusted", attention = ["cloudflare-radar-review"] }

[externals.authorities.cloudflare-radar-operator]
builtin = "hitl"
```

The `internal` audience needs a mapping onto your organization's audience
sources, because the URL Scanner reads narrow onto it.

## Limits

Cloudflare exposes no per-scan readers to a policy. A scan submitted as
`Unlisted` is visible only to the account, and a scan submitted as
`Public` is visible to everyone, but no tool reports which a stored scan
is. Every URL Scanner read is therefore `internal`: map it to the people
who may see everything this token can list. Narrow one account in a root
rule by argument (`search_url_scans(account_id:<id>)`); root rules run
first.

`create_url_scan` carries one contract for both visibilities. The battery
requires `public` for either, because the default is `Public` and the
battery does not read the `visibility` argument. A deployment that always
scans unlisted can override the rule in its root config with an
`internal` bound.

The pinned release marks this server deprecated in its own server
instructions and points callers at the unified `https://mcp.cloudflare.com/mcp`,
whose two generic `search` and `execute` tools run arbitrary Cloudflare
API code. That server is not covered here; a generic code-execution tool
needs its own contract.

The tool list follows `cloudflare-radar-mcp-server@0.2.5`. Edit the TOML
rules when it changes; a tool the policy does not name is blocked.

```sh
cargo test --locked -p appa --test cloudflare_policy --test marketplace
bash scripts/appa-marketplace.sh --check
```
