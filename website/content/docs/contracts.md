---
title: Policy configuration
category: Deep Dive
order: 3
description: TOML configuration for tool contracts, restrictions, annotations, and remedy plans.
---

An OpenAPPA policy defines restrictions on tool results and requirements for tool calls. It also defines the approvals and data transformations available when a call is blocked.

This page specifies the configuration format. For the concepts behind these rules, see [How it works](/how-it-works#the-core-concepts).

## Policy file

OpenAPPA reads its configuration from an `appa.toml` file.

The file defines rules for the agent's tools. For example, a rule can restrict customer records to company members. These rules belong under `[policy]`.

Some rules need an external component to resolve group membership or remove private data. The `[externals]` section configures how OpenAPPA calls these components and other deployment settings, such as response size limits and timeouts.

```toml
[policy]
version = 2

[[policy.tool]]
name = "get_ticket_from_crm"
delta = { audience = ["internal"] }

[externals]
timeout_ms = 2000
max_body_bytes = 65536
```

## Tool contracts

Each `[[policy.tool]]` entry is a tool contract. It answers three questions:

| Field | What to write | What OpenAPPA does |
|---|---|---|
| `delta` | Restrictions carried by the tool's result. | Applies those restrictions when the agent receives the result. |
| `requires` | Conditions the call must satisfy. | Checks them before allowing the call. |
| `effects` | Side effects of a successful call. | Records them in the trajectory's history. |

In the example below, the `get_ticket_from_crm` tool contract restricts the trajectory to `internal` and lowers its trust to `suspicious`. The `send_email` tool contract requires the email recipient to belong to the current audience.

```toml
[[policy.tool]]
name = "get_ticket_from_crm"
description = "Reads a customer support ticket."
delta = { trust = "suspicious", audience = ["internal"] }

[[policy.tool]]
name = "send_email"
parameters = { type = "object", properties = { recipient = { type = "string" }, body = { type = "string" } }, required = ["recipient", "body"] }
requires = { audience = { contains = ["$recipient"] } }
delta = {}
effects = ["egress"]
```

| Field | Configuration rule |
|---|---|
| `name` | The tool name, optionally with an argument selector. |
| `description` | Optional description of the tool. Annotators can receive this description. |
| `parameters` | JSON Schema for the tool arguments. Some input mappings require it. |
| `tags` | Names used to select applicable authorities and sanitizers. See [Tags](#tags). |
| `delta` | Can restrict the audience or lower trust. It cannot make the trajectory less restricted. |
| `requires` | Audience, trust, effects, or attention requirements for the call. |
| `effects` | Effect names recorded after successful execution. Declare all relevant side effects. |
| `annotator` | A registered component that supplies the complete annotation for each call. |

An omitted `delta` adds no restriction. An omitted `requires` adds no requirement.

Only one source of contract rules is allowed: static `delta`, `requires`, and `effects` fields, or an `annotator`. Combining them causes a load error.

OpenAPPA checks both `delta` and `requires` before allowing the tool call.

### Tool names

A policy can use a host-native name or a canonical tool id. Canonical ids have three segments: `<family>/<namespace>/<tool>`. The family is `mcp`, `host`, or `agent`. Each segment matches `[A-Za-z0-9_.-]+`. A namespace segment never contains `__`.

| Family | Names | Example |
|---|---|---|
| `mcp` | One tool of one MCP server. The namespace is the server. | `mcp/github/create_issue` |
| `host` | A tool the host itself provides. The namespace is the host. | `host/claude-code/Bash` |
| `agent` | An agent called as a tool. The namespace is where the agent lives. | `agent/kagent/log-analyst` |

`appa/execute_remedy_plan` is the runtime's own control tool, the one member of the `appa` family. A policy cannot declare it: the runtime recognizes it before any contract, and a `[[policy.tool]]` entry that names it refuses the policy at load. The wildcard entry `name = "*"` is not a canonical id; it covers every tool the policy does not name (see [the wildcard](#handling-undeclared-tools)).

The agent keeps using its host's tool names. A plugin implements the host lifecycle; the runtime's adapter translates tool identities and events. The runtime records canonical ids, even when the policy uses native names. Where it tells the model to run a tool, it uses the host's dispatch spelling. Claude Code and kagent have these mappings:

| Adapter | Raw tool spelling | Canonical tool id |
|---|---|---|
| Claude Code | `mcp__<server>__<tool>` — split at the first `__` after `mcp__` | `mcp/<server>/<tool>` |
| Claude Code | A built-in tool: `Bash`, `Read`, `Edit`, `Agent`, … | `host/claude-code/<name>` |
| Claude Code | `mcp__appa__execute_remedy_plan`, the remedy tool of the runtime's own `appa` MCP server | `appa/execute_remedy_plan` |
| kagent | A tool discovered from a configured MCP endpoint | `mcp/<source-id>/<tool>` |
| kagent | An agent called as a tool | `agent/<namespace>/<agent>` |
| kagent | A kagent built-in, such as `ask_user`, `load_memory`, `save_memory`, `prefetch_memory`, or a skill tool | `host/kagent/<name>` |
| kagent | The entrypoint gates | `host/kagent-gate/code_execution`, `host/kagent-gate/memory_persist` |
| kagent | The remedy tool | `appa/execute_remedy_plan` |

Claude Code names such as `Bash` and `mcp__github__create_issue` identify precise tools. An unqualified kagent rule such as `read_secret` applies to that native name across MCP servers and kagent's own tools. It does not cover remote-agent delegation. A newly discovered tool can use an existing rule without restarting the trajectory or changing its opening policy.

Use `server` when a rule should apply to one MCP connection:

```toml
[[policy.tool]]
name = "read_secret"
server = "demo-tools"
delta = { trust = "suspicious" }
```

This rule identifies `mcp/demo-tools/read_secret`. Deployment-level `[server_aliases]` can map a policy's server name to a configured connection identity. APPA does not infer a provider from a hostname or server metadata. A server qualifier cannot repair duplicate host dispatch names; the host must distinguish those tools.

kagent derives a default source ID from the exact configured endpoint URL. Native rules do not require that ID. Discovery supplies evidence, not permission: known tools need policy coverage, unavailable sources remain unknown, and later calls still pass runtime enforcement. A trajectory retains its opening policy and accepted identities.

Native names also work in `confined_results`, `assumed_tools`, and `provider_run_tools`. Coverage reports distinguish a rule spanning servers from an observed concrete tool. Overlapping declarations with incompatible provider-run execution settings are rejected.

Which calls start a child trajectory is the runtime's derivation from the adapter (Claude Code's `Agent`, a kagent agent called as a tool); a policy does not declare it.

### Tags

Tags connect tools to the authorities that can review their calls and the sanitizers that can transform their data. For example, this contract gives a ticket tool the `support` tag:

```toml
[[policy.tool]]
name = "get_ticket_from_crm"
tags = ["support"]
delta = { audience = ["internal"] }
```

An authority or sanitizer with `tags = ["support"]` applies to tools with that tag. If it lists several tags, one matching tag is enough. Without tags, an authority or sanitizer is not restricted to a particular set of tools. Its permissions still limit what it can approve or transform.

Attention approvals use a different rule: OpenAPPA selects authorities by `permits.attention`, regardless of their tags. See [Attention](#attention).

### Pattern matching

A policy can declare several contracts for one tool. OpenAPPA checks them in declaration order and selects the first matching contract. This includes overlapping native and canonical names; canonical spelling does not give a rule priority.

In the example below, the first contract matches a `path` string that starts with `/docs/` and declares its result as `public`. The second contract covers all other calls to `read_file` and declares their results as `internal`.

```toml
[[policy.tool]]
name = "read_file(path:/docs/*)"
delta = { audience = ["public"] }

[[policy.tool]]
name = "read_file"
delta = { audience = ["internal"] }
```

A selector checks top-level string arguments. Put it in parentheses after the tool name, with conditions written as `argument:pattern` and separated by commas. List each argument only once, in any order.

Every condition must match the full value of its argument. If an argument is missing or is not a string, the selector does not match.

```toml
[[policy.tool]]
name = "mcp__github__fork_repository(owner:archestra-ai,repo:website)"
requires = { trust = "trusted" }
delta = {}
```

Use `*` to match any sequence of characters, including an empty sequence:

| Pattern | Matches | Does not match |
|---|---|---|
| `report.txt` | `report.txt` | `old-report.txt` |
| `/docs/*` | `/docs/guide.md`, `/docs/setup/install.md` | `/private/guide.md` |
| `*.md` | `guide.md`, `/docs/guide.md` | `guide.txt` |

#### Match special characters literally

Some characters have a special meaning in a selector. For example, `*` matches any text, and a comma separates argument conditions.

To match the character itself, put a backslash before it:

| Character to match | Write in the pattern |
|---|---|
| Asterisk (`*`) | `\*` |
| Closing parenthesis (`)`) | `\)` |
| Comma (`,`) | `\,` |
| Backslash (`\`) | `\\` |

For example, this contract matches a `search` call whose `query` argument is exactly `a,b`:

```toml
[[policy.tool]]
name = 'search(query:a\,b)'
delta = {}
```

The backslash tells OpenAPPA that the comma belongs to the query value. It does not separate two argument conditions.

Use single quotes around the name to preserve backslashes as written. If you use double quotes, TOML requires each backslash to be doubled. These two lines mean the same thing:

```toml
[[policy.tool]]
# Single quotes:
name = 'search(query:a\,b)'
```

```toml
[[policy.tool]]
# Equivalent form with double quotes:
name = "search(query:a\\,b)"
```

Only the four escapes listed above are supported. Other escapes cause a policy load error.

OpenAPPA selects a contract before it validates the contract's `parameters` schema. A schema error does not select a later contract. Rewritten arguments select their own matching contract. See [Sanitizers](#sanitizers) for rewrite rules.

### Handling undeclared tools

The tool name `"*"` covers tool names that the policy does not declare. The current format requires an annotator for this entry:

```toml
[[policy.annotator]]
name = "classify_unknown_tool"
ranks = ["suspicious"]

[[policy.tool]]
name = "*"
annotator = "classify_unknown_tool"
```

Declare and bind `classify_unknown_tool` as shown in [Annotators](#annotators). The wildcard cannot contain static `delta`, `requires`, or `effects` fields. It also cannot contain metadata or argument selectors.

A policy can contain one wildcard entry. An exact tool declaration takes precedence over it. If no declaration covers a call, OpenAPPA refuses the call before execution.

## Restrictions and requirements

Audience and trust form the security label. Effects record completed actions. Attention requires approval for a specific call. The examples below show how to declare each one.

### Audiences

An audience identifies who can receive data. Reading restricted data limits where the trajectory can send data later.

Use `delta.audience` to restrict who can receive a tool's result. Use `requires.audience` to check the current audience before a tool call:

| Field | What OpenAPPA does |
|---|---|
| `delta.audience` | Restricts the result to the specified readers. |
| `requires.audience.contains` | Checks that the current audience includes all specified readers. |
| `requires.audience.within` | Checks that every reader in the current audience belongs to the specified audience. |

In the example below, reading a ticket restricts the trajectory to `internal`. The `publish_update` call requires unrestricted sharing. The `process_internal_data` call requires the trajectory's audience to be within `internal`.

```toml
[[policy.tool]]
name = "get_ticket_from_crm"
delta = { audience = ["internal"] }

[[policy.tool]]
name = "publish_update"
requires = { audience = { contains = ["public"] } }

[[policy.tool]]
name = "process_internal_data"
requires = { audience = { within = ["internal"] } }
```

Multiple entries combine readers from all entries. For example:

| Declaration | Meaning |
|---|---|
| `delta = { audience = ["@finance", "@support"] }` | The result is for readers who belong to either group. |
| `requires = { audience = { contains = ["@finance", "@support"] } }` | The current audience must include every member of both groups. |
| `requires = { audience = { within = ["@finance", "@support"] } }` | Every current reader must belong to at least one of the two groups. |

`within` checks the trajectory's audience, not the recipient of one call. Only `contains` and `within` are allowed under `requires.audience`. Other keys cause a load error.

Each audience entry can be one of the following:

| Entry | Meaning |
|---|---|
| `"public"`, `"internal"`, `"self"` | Built-in audiences: everyone (`public`), the organization (`internal`), or the identity OpenAPPA acts for (`self`). |
| `"@name"`, `"@provider:selector"` | A configured group, such as `"@finance"`, or a group read directly from a source, such as `"@slack:user-group/oncall"`. |
| Other strings, such as `"alice@example.com"` | A literal [reader ID](#reader-ids), compared exactly. For example, `"finance"` is a reader ID; `"@finance"` refers to a group. |

For example, `audience = ["@finance", "alice@example.com"]` includes all members of `finance` and the individual reader `alice@example.com`.

#### Built-in audiences

The built-in chain is `self` ⊆ `internal` ⊆ `public`: `internal` includes `self`, and `public` includes everyone. Policies cannot add levels to this chain. Use named audiences for other groups of readers.

Within one set of square brackets, use at most one of `self` and `internal`. Do not combine `public` with other entries. `contains = ["public"]` requires unrestricted sharing, as shown by `publish_update` above.

#### Read an audience from a tool argument

Under `contains`, use `$<argument_name>` to read an audience from a tool argument. The name is the tool's argument name; `recipient` below is one example:

```toml
[[policy.tool]]
name = "send_email"
parameters = { type = "object", properties = { recipient = { type = "string" } }, required = ["recipient"] }
requires = { audience = { contains = ["$recipient"] } }
```

OpenAPPA reads the proposed call's `recipient` argument and checks that the current audience includes its readers. The tool's `parameters` schema must declare the argument as a required top-level string. A whole-entry argument placeholder is allowed only under `contains`.

The argument can contain a literal reader, `public`, `self`, `internal`, or an `@` mention. An unresolved dynamic mention stops the call with an operational error.

#### Read a source collection from a tool argument

A selector placeholder names a source collection whose selector takes one or more segments from the call. Write `@<provider>:<selector>` and put `$<argument_name>` in place of a segment:

```toml
# Reading a channel restricts the result to that channel's members.
[[policy.tool]]
name = "mcp/claude_ai_Slack/slack_read_channel"
parameters = { type = "object", properties = { channel_id = { type = "string" } }, required = ["channel_id"] }
delta = { audience = ["@slack:channel/$channel_id"] }

# Posting to a channel requires that its members can already read the data.
[[policy.tool]]
name = "mcp/claude_ai_Slack/slack_send_message"
parameters = { type = "object", properties = { channel_id = { type = "string" }, message = { type = "string" } }, required = ["channel_id", "message"] }
requires = { trust = "trusted", audience = { contains = ["@slack:channel/$channel_id"] } }
```

A selector placeholder MAY be the only entry of `delta.audience` or of `requires.audience.contains`. It MAY also be an entry of an annotator's `audiences` mandate; see [Permits and hint](#permits-and-hint). It MUST NOT appear under `within`, beside other entries in one list, or in an annotator's answer.

Each `$<argument_name>` MUST name a required top-level string argument of the tool's `parameters`. The spelling, with each `$<argument_name>` read as a variable segment, MUST match one template the provider declares under `selectors`; see [Declare selector templates](#declare-selector-templates). A `$<argument_name>` segment matches only a `<variable>` segment of the template.

At check time OpenAPPA replaces each `$<argument_name>` with the call's argument value. The result is an ordinary `@provider:selector` mention: OpenAPPA reads its members from the provider's service, records the answer with the decision, and checks the call exactly as for a static mention. In the example above, reading channel `C0123` restricts the result to the members of `C0123`, and posting to `C0123` requires that every member of `C0123` is already a reader.

A static mention cannot contain a segment that starts with `$`. There is no escape: a collection whose selector begins with `$` cannot be written in a policy.

#### Configure audience membership

Membership answers: who belongs to this audience? OpenAPPA asks an external membership service for the members. For example, that service can read the members of a Slack group.

Under `[policy.audience]`, `self` and `internal` list the selectors that supply their members. `[policy.audience.group.<name>]` declares a named group with `within` and `from`.

Each selector entry has the form `provider:selector`. The provider identifies the service configured under `[externals.audience.<provider>]`. The selector tells that service which reader or group to read. The service's `selectors` declaration lists the selector templates it understands.

The example below uses a Google Workspace membership service to define `self`, `internal`, and `@finance`:

```toml
# Use the Google Workspace viewer as self and organization members as internal.
[policy.audience]
self = ["google-workspace:viewer"]
internal = ["google-workspace:full-members"]

# Define @finance from a Workspace group and declare it part of internal.
[policy.audience.group.finance]
within = "internal"
from = ["google-workspace:group/finance@corp.com"]

# Set the service that supplies Google Workspace membership and declare what it serves.
[externals.audience.google-workspace]
url = "https://audience.corp/google-workspace"
selectors = [
  { template = "viewer", feeds = "self" },
  { template = "full-members", feeds = "internal" },
  { template = "group/<group-address>" },
]
```

For `google-workspace:group/finance@corp.com`, OpenAPPA sends `group/finance@corp.com` as the selector to the membership service configured under `[externals.audience.google-workspace]`. The service reads the group's members and returns them to OpenAPPA.

Configuring a provider does not connect OpenAPPA directly to Google Workspace, Slack, or GitHub; you must supply the service that makes that connection. Each shipped battery supplies the service for the provider it covers and declares that service's templates; see [What is a battery](/batteries#audience-sources).

##### Declare selector templates

`selectors` on `[externals.audience.<provider>]` declares the selector templates the service understands. Each entry has a `template` and an optional `feeds`:

| Field | Meaning |
|---|---|
| `template` | One selector format: literal segments and `<variable>` segments separated by `/`, such as `viewer`, `full-members`, `group/<group-address>`, or `channel/<id>`. A `<variable>` segment matches one non-empty segment. |
| `feeds` | `self` or `internal`: the built-in audience this template may supply. Omit it for a template that supplies only named groups and `@provider:selector` mentions. |

A provider exists in a policy when a `[policy.audience]` selector references it. Every selector the policy writes MUST match one declared template of its provider, with the role its position needs:

| Audience key | Templates you can use |
|---|---|
| `self` | A template with `feeds = "self"`, usually `viewer`: the identity OpenAPPA acts for. |
| `internal` | A template with `feeds = "internal"`, such as `full-members` or `org/<org>/members`. For example, `github:org/acme/members` makes members of `acme` internal. Members of other GitHub organizations are not included by this source. |
| `group.<name>.from`, `@provider:selector` mentions, and selector placeholders | Any declared template except one with `feeds = "self"`. |

OpenAPPA rejects the configuration if you use a selector in the wrong key. For example, `slack:viewer` cannot define `internal`. The load error for a selector that matches no template lists the templates the provider declares. The declared templates enter the policy identity; the URL, command, and credentials do not.

If a key lists several sources, the audience includes members from any of them. For example, `internal = ["google-workspace:full-members", "slack:full-members"]` includes members returned by either service.

In the `finance` example, `within = "internal"` declares that every member of `finance` is internal. OpenAPPA trusts this declaration; it does not check each member against the sources for `internal` or check their email domain. A group can declare `within = "self"` or `within = "internal"`.

You can use `"@slack:user-group/oncall"` directly instead of declaring a named group such as `@oncall` under `[policy.audience.group.<name>]`. It refers to the Slack group `oncall`. To use this reference, at least one selector must use the `slack` provider:

```toml
# Use Slack workspace members as internal.
[policy.audience]
internal = ["slack:full-members"]

# Restrict incident details to the Slack oncall group.
[[policy.tool]]
name = "get_incident"
delta = { audience = ["@slack:user-group/oncall"] }

# Set the service that supplies Slack membership and declare what it serves.
[externals.audience.slack]
url = "https://audience.corp/slack"
selectors = [
  { template = "viewer", feeds = "self" },
  { template = "full-members", feeds = "internal" },
  { template = "user-group/<handle>" },
]
```

OpenAPPA rejects policy references to undeclared named audiences, providers that no `[policy.audience]` selector references, or selectors that match no declared template.

##### Reader IDs

A reader ID is a string that OpenAPPA compares exactly. The membership service decides the reader ID for each member: the email address the provider verified for the account, otherwise `<provider>:<id>`, such as `slack:U012345` or `github:alice`. The shipped Slack, GitHub, and Google Workspace services do this. The GitHub service reports organization and team members by profile email where the member publishes one, otherwise as `github:<login>`.

The membership service is responsible for verifying who owns an address. OpenAPPA trusts the service; it does not verify ownership itself. Two services that report the same verified address name the same reader. A member reported by provider ID merges with nothing else.

OpenAPPA converts the domain of an address to lowercase. It leaves the part before `@` unchanged, including dots and `+suffix` values. It does not merge aliases or treat personal and corporate addresses as the same reader.

Every reader ID in an answer must be a well-formed email address or `<provider>:<non-empty>` under the answering provider. An ID with a `:` before its `@` is a qualified ID, not an address. Any other value, such as `"finance"`, refuses the whole answer as an operational failure that names the provider and selector. OpenAPPA records no decision for a refused answer.

A service that reports one account under two different addresses within one operation yields two reader IDs for one person. The result is a narrower audience, never a wider one.

##### Membership request protocol

Audience providers support HTTP endpoints and local commands.

OpenAPPA can ask the membership service for a group's members or for the reader ID behind one member. The examples below show the request data and response data. For the complete JSON request and response format, see [The consult request](#the-consult-request).

Every request carries `declaration.templates`: the templates the policy declares for the provider. The service MUST compare that list with the templates it serves and MUST refuse the request when the lists differ, before it reads its credential or calls the provider. OpenAPPA treats the refusal as an operational failure of the service that names the provider; it records no decision.

To read the members of the Slack `oncall` group, OpenAPPA sends:

```json
{"selector": "user-group/oncall"}
```

The service returns the members as reader IDs:

```json
{"members": ["a@corp.com", "slack:U2"]}
```

The service returns `{"members": []}` when a group has no members. This is a successful response.

To look up one member, OpenAPPA sends:

```json
{"member": "slack:U1"}
```

The service returns that member's reader ID under `principal`:

```json
{"principal": "a@corp.com"}
```

If the service cannot find the member, it returns `{"principal": null}`. The member then keeps its ID as written. A non-null `principal` must satisfy the [reader ID rule](#reader-ids).

If the service fails, takes too long to respond, or returns an invalid answer, OpenAPPA cannot complete the audience check and stops the operation. OpenAPPA records membership responses with the decision that requested them.

##### Map members with `lookup` and `readers`

By default, a provider's own service answers member lookups. Set `lookup = "<name>"` on a provider to send its member lookups to another `[externals.audience.<name>]` entry. That entry has exactly one of `readers`, `command`, or `url`. A `readers` entry is an inline table from `<provider>:<id>` to reader ID. OpenAPPA answers from that table without calling a service.

The example below maps GitHub members to corporate addresses with a `readers` table:

```toml
[policy.audience]
self = ["github:viewer"]
internal = ["github:org/acme/members"]

[externals.audience.github]
command = ["python3", "batteries/github/audience-source.py"]
token_env = "APPA_PROVIDER_GITHUB_TOKEN"
selectors = [
  { template = "viewer", feeds = "self" },
  { template = "org/<org>/members", feeds = "internal" },
  { template = "org/<org>/team/<team>" },
]
lookup = "people"

[externals.audience.people]
readers = { "github:alice" = "alice@corp.com" }
```

For a provider with `lookup`, OpenAPPA also looks up every group member that is not an email address. With the configuration above, the member `github:alice` reported for `org/acme/members` becomes the reader `alice@corp.com`. A `null` answer, or a member absent from a `readers` table, leaves the member as written. OpenAPPA records lookup answers with the decision like other membership responses.

A battery binds the membership service it ships in its own `appa.toml`, with the service's `selectors`. The root config keeps the `[policy.audience]` mappings. To route such a provider's member lookups, the root MAY add `[externals.audience.<provider>]` with `lookup` as its only key; OpenAPPA attaches that routing to the battery's binding. A root entry with any other key beside a battery's binding is a duplicate binding, and OpenAPPA rejects the configuration.

##### Source probe at start and reload

Before it serves a configuration, and before it switches to a reloaded one, OpenAPPA reads every selector the policy references once, asks each configured `lookup` entry for one member those answers owe, and applies the reader ID rule to each answer. A service that fails or returns a malformed reader ID stops the start or the reload with the provider, the selector or member, and the reason. A service that refuses its declared templates fails this probe, so a policy whose `selectors` disagree with the service never serves. A failed reload leaves the previous configuration serving. Replay does not probe.

### Trust

Trust describes how much OpenAPPA can rely on the data the agent has received. A trust rank is a named level, such as `suspicious` or `trusted`. Reading data at a lower rank lowers the trajectory's trust. Tools can require a minimum rank before they run.

Use these fields to declare trust restrictions and requirements:

| Field | Meaning | Example |
|---|---|---|
| `delta.trust` | Declares the result's trust rank. Reading it can lower the trajectory's trust. | `delta = { trust = "suspicious" }` |
| `requires.trust` | Sets the minimum trust rank needed to allow the call. | `requires = { trust = "trusted" }` |

Both fields must use a name from the policy's trust chain: the ordered list of ranks from least trusted to most trusted.

#### Trust ranks

If you omit `trust_chain`, the ranks are `suspicious` followed by `trusted`, from least trusted to most trusted.

To define your own ranks, set `trust_chain` in `[policy]`. For example, `trust_chain = ["untrusted", "reviewed", "trusted"]` defines three ranks in increasing order. This replaces the default ranks. A trust rank used elsewhere in the policy must appear in this list, or the policy does not load.

#### Declare tool restrictions and requirements

In the example below, `trust_chain` explicitly sets the default ranks. The `read_web_page` contract marks its result as `suspicious`. Once the agent receives that result, OpenAPPA blocks `apply_db_migration` because it requires `trusted` data.

```toml
[policy]
version = 2
trust_chain = ["suspicious", "trusted"]

[[policy.tool]]
name = "read_web_page"
delta = { trust = "suspicious" }

[[policy.tool]]
name = "apply_db_migration"
requires = { trust = "trusted" }
```

Reading a later result marked `trusted` does not undo the earlier drop to `suspicious`. A tool's `delta.trust` can lower the trajectory's trust, but cannot raise it. An [authority](#authorities) with the required permission can approve a blocked call without changing the trajectory's trust.

### Effects

Effects record successful actions. List the tool's side effects in `effects` so later calls can check whether they occurred.

| Field | Meaning | Example |
|---|---|---|
| `effects` | Records the listed effects when the tool succeeds. | `effects = ["backup.completed"]` |
| `requires.effects.contains` | Requires the listed effects to have been recorded in the trajectory. | `contains = ["backup.completed"]` |
| `requires.effects.excludes` | Blocks the call if a listed effect is already recorded or declared by another call that has been allowed but has not finished. | `excludes = ["migration.applied"]` |

Only `contains` and `excludes` are allowed under `requires.effects`.

In the example below, `backup_database` records `backup.completed` when it succeeds. The migration requires that backup and cannot run if a migration has already succeeded or another migration call has been allowed but has not finished:

```toml
[[policy.tool]]
name = "backup_database"
delta = {}
effects = ["backup.completed"]

[[policy.tool]]
name = "apply_db_migration"
delta = {}
effects = ["migration.applied", "mutation"]

[policy.tool.requires.effects]
contains = ["backup.completed"]
excludes = ["migration.applied"]
```

### Attention

Attention requires fresh approval for each call. A previous approval or recorded effect cannot satisfy it.

| Field | Meaning | Example |
|---|---|---|
| `requires.attention` | Lists the approvals required before the tool can run. | `requires = { attention = ["sre-signoff"] }` |
| `permits.attention` | Lists the approvals an authority is allowed to give. | `permits = { attention = ["sre-signoff"] }` |

In the example below, each `apply_db_migration` call requires `sre-signoff`. The `sre-reviewer` authority has permission to give that approval and uses the built-in human approval handler, `hitl`.

```toml
[[policy.tool]]
name = "apply_db_migration"
requires = { attention = ["sre-signoff"] }
delta = {}
effects = ["migration.applied"]

[[policy.authority]]
name = "sre-reviewer"
permits = { attention = ["sre-signoff"] }

[externals.authorities.sre-reviewer]
builtin = "hitl"
```

An attention mark is the name of an approval requirement, such as `sre-signoff`. OpenAPPA can ask any authority whose `permits.attention` includes that name, regardless of its tags. See [Authorities](#authorities) for other approval permissions.

## Annotators

An annotator classifies a tool call to determine its output restrictions (`delta`), requirements (`requires`), and effects. OpenAPPA checks the resulting contract before allowing the call.

Use an annotator when a script or service must determine the rules for a call. For example, a script can classify files by directory: files in `/srv/public-docs` can be shared publicly, while files in `/srv/customer-records` are restricted to internal users.

A tool selects one annotator with `annotator = "<name>"`. The annotator supplies `delta`, `requires` (including attention marks), and emitted effects. Do not also declare these fields on that tool.

### Example: annotate a tool call with Claude Code

The example below uses the built-in Claude Code classifier to annotate calls to `Bash`. It receives the complete tool call and uses the `hint` to determine its restrictions and requirements. The `ranks`, `audiences`, `marks`, and `effects` fields limit what it can return.

```toml
[[policy.annotator]]
name = "classify-command"
builtin = "claude-code"
ranks = ["suspicious", "trusted"]
audiences = ["internal"]
marks = []
effects = []
hint = "Use suspicious for data from unverified sources. Use trusted only for local computation over trusted inputs. Restrict private results to internal."

[[policy.tool]]
name = "Bash"
description = "Runs one shell command and returns its output."
annotator = "classify-command"
```

This configuration sends the tool name, description, and arguments to Claude Code. It does not require a `parameters` schema. If the tool has no description, the request omits it.

### Inputs

By default, the annotator receives the complete tool call. Use `inputs` when it needs only specific parts of the call. In the example below, the annotator receives the `customer_id` argument under the name `subject`, instead of receiving the tool name, description, and all arguments:

```toml
[[policy.annotator]]
name = "classify-customer"
inputs = { subject = "$tool_call.arguments.customer_id" }
ranks = ["suspicious"]
audiences = ["internal"]
marks = []
effects = []
hint = "Classify customer records as internal and suspicious."

[[policy.tool]]
name = "get_customer"
parameters = { type = "object", properties = { customer_id = { type = "string" } }, required = ["customer_id"] }
annotator = "classify-customer"

[externals.annotators.classify-customer]
url = "https://classifier.corp/label"
```

Selecting fewer inputs does not change the response requirements: the annotator still supplies the complete annotation. Each input can select one of the following:

| Input value | Selected data |
|---|---|
| `$tool_call` | Complete call: name, optional description, and arguments. |
| `$tool_call.name` | Tool name. |
| `$tool_call.description` | The tool's description. The tool contract must declare `description`. |
| `$tool_call.arguments` | Complete argument object. |
| `$tool_call.arguments.<name>` | One top-level argument. The tool's `parameters` schema must declare it as required. |

`$tool_call` is the only input source. A selected argument can contain any JSON value permitted by its schema.

### Permits and hint

An annotator's permits limit the values it can use in its answers. The following fields define these limits:

| Field | Allowed values in an answer | If omitted |
|---|---|---|
| `ranks` | Ranks used in `delta.trust` or `requires.trust`. | Every rank in the trust chain. |
| `audiences` | Built-in audiences, `@` references, selector placeholders, or literal reader IDs that the answer may use. | `self`, `internal`, named groups, and reader IDs declared in the policy. |
| `marks` | Required attention marks. | Every mark declared in an authority's `permits.attention`. |
| `effects` | Effects that the call may record or require. | Every effect name declared by the policy. |

`public` is always allowed in an answer, so it is not listed in `audiences`. Setting `audiences = []` allows only public answers.

A selector placeholder in `audiences`, such as `@github:repo/$owner/$repo/collaborators`, is instantiated for each call. Every `$<argument_name>` in it MUST name a required top-level string argument of every tool that uses the annotator, so the wildcard `*` tool cannot use such an annotator. The consult request and the answer schema list the concrete spelling for that call, such as `@github:repo/acme/api/collaborators`, and the answer MAY use only that spelling. The annotator can answer about the resource the call names and about no other. See [Read a source collection from a tool argument](#read-a-source-collection-from-a-tool-argument) for the placeholder rules.

An empty list and an omitted field have different meanings. For example, `marks = []` prevents the annotator from requiring attention. Omitting `marks` allows it to use any mark declared in an authority's `permits.attention`.

The optional `hint` tells the annotator how to classify the call. It can explain what to look for and give examples. It cannot allow values excluded by the permits and cannot exceed 512 characters. An annotator name must be non-empty and can contain dots.

### Implementing an annotator

An annotator can be an HTTP service or a local program on a Unix system. Configure its `url` or `command` under `[externals.annotators.<name>]`, where `<name>` matches the annotator's declaration.

Alternatively, use a built-in annotator. The available options are:

- `builtin = "claude-code"`: uses Claude Code to classify tool calls.
- `builtin = "llm"`: uses the model configured under `[externals.llm]` to classify tool calls.

Set `builtin` on `[[policy.annotator]]`, as in the Claude Code example above. An annotator with `builtin` cannot also have an `[externals.annotators.<name>]` section. Unlike sanitizers and authorities, annotators do not accept `builtin` under `[externals]`.

`claude-code` runs the local `claude` command and requires Claude Code on the Unix machine running OpenAPPA. `llm` requires model settings under `[externals.llm]`. OpenAPPA rejects a configuration with a missing implementation, an unknown implementation name, or an implementation unavailable on that system.

### Annotator protocol

OpenAPPA sends the selected call data and the annotator's instructions in a consult request with `kind = "annotation"`. `declaration` contains the instructions and permitted values; `artifact.args` contains the call data to classify.

For the customer example, the request is:

```json
{
  "version": 1,
  "kind": "annotation",
  "name": "classify-customer",
  "declaration": {
    "hint": "Classify customer records as internal and suspicious.",
    "inputs": ["subject"],
    "trust_ranks": ["suspicious"],
    "audiences": ["internal"],
    "attention_marks": [],
    "effects": []
  },
  "artifact": { "args": { "subject": "cust-7" } }
}
```

Without an `inputs` mapping, `declaration.inputs` is empty and `artifact.args` contains the complete call. For example, the `artifact` field contains:

```json
{
  "args": {
    "name": "Bash",
    "description": "Runs one shell command and returns its output.",
    "arguments": { "command": "cargo test" }
  }
}
```

The request does not include the trajectory's current audience, trust rank, or previous actions.

For the customer request, the service returns this response to classify the result as internal and suspicious, with no call requirements or effects:

```json
{
  "version": 1,
  "answer": {
    "delta": { "trust": "suspicious", "audience": ["internal"] },
    "requires": { "history": [], "attention": [] },
    "emits": []
  }
}
```

The response uses `emits` for effects and `requires.history` for history checks. These names differ from the policy TOML fields.

- `answer` must contain exactly `delta`, `requires`, and `emits`.
- `requires` must contain `history` and `attention` arrays, even when empty.
- Audience and trust fields inside `delta` and `requires` are optional. An omitted field adds no restriction or requirement.
- `requires.audience` can contain `contains`, `within`, or both.
- Each history entry is `{"contains":"<effect>"}` or `{"excludes":"<effect>"}`.
- JSON audience values use `"public"` or a list of permitted audiences. Do not put `public` inside a JSON audience list.
- A restricted list cannot repeat entries or contain both `self` and `internal`.

OpenAPPA rejects unknown keys, `null` values, empty audience objects, duplicate emitted effects, and values outside the permits. A built-in model returns only the contents of `answer`, without the surrounding `version` and `answer` fields.

OpenAPPA uses the annotation only for the call it classified. Changing the call requires a new annotation. Rechecking or replaying the same recorded call reuses its annotation and membership responses.

If the annotator fails or returns an invalid answer, the call does not run. The agent can propose it again. OpenAPPA checks that the answer uses permitted values; the annotator is responsible for classifying the call correctly.

## Sanitizers

A sanitizer cleans or validates data before an agent or tool receives it. Its `permits` section specifies which audience or trust rank OpenAPPA can assign to the result.

In the example below, the integration keeps the original ticket hidden from the agent while `remove_customer_details` removes private information. The policy allows the cleaned result to be shared publicly. The service must remove all information that cannot be shared publicly.

```toml
[[policy.tool]]
name = "get_ticket_from_crm"
tags = ["support"]
delta = { audience = ["internal"] }

[[policy.sanitizer]]
name = "remove_customer_details"
on = ["tool_output"]
tags = ["support"]
hint = "Remove customer identities and all other private details from the ticket."

[policy.sanitizer.permits]
audience = { from = ["internal"], to = ["public"] }

[policy.deployment]
confined_results = ["get_ticket_from_crm"]

[externals.sanitizers.remove_customer_details]
url = "https://sanitizer.corp/sanitize"
```

### Permitted transitions

A sanitizer can change either the audience or the trust rank of its result. Its `permits` section declares the allowed change with `from` and `to`. It can contain `audience` or `trust`, but not both.

| Transition | Meaning of `from` | Meaning of `to` |
|---|---|---|
| `audience` | Readers who must be included in the original data's audience. | Audience assigned to the cleaned result. |
| `trust` | Minimum trust rank of the original data. | Trust rank assigned to the result. |

For example, the following declaration permits a sanitizer to validate or clean suspicious data and return a trusted result:

```toml
[policy.deployment]
context_control = true

[[policy.sanitizer]]
name = "vouch-fetched-text"
on = ["tool_output"]

[policy.sanitizer.permits]
trust = { from = "suspicious", to = "trusted" }
```

The sanitizer implementation must perform the validation or cleaning. The integration must keep the original data hidden until the sanitizer finishes. Merely declaring the result `trusted` does not make its content trustworthy.

The optional `hint` tells the sanitizer what to remove or validate. It does not grant permission beyond `permits`.

### Tool outputs and inputs

The `on` field selects which data the sanitizer can transform:

| `on` value | Data to transform | What the integration must do |
|---|---|---|
| `tool_output` | A tool result or a child agent's answer. | Keep the original hidden from the receiving agent and deliver the transformed result. |
| `tool_input` | All arguments of one tool call. | Run the tool with exactly the arguments returned by the sanitizer. |

When a tool result would restrict the agent, OpenAPPA can offer a sanitizer whose permitted change reduces that restriction. If the agent selects it, the integration runs the sanitizer before delivering the result.

If the cleaned result still adds restrictions, the agent can accept them or select another compatible sanitizer. Cleaning a new result does not remove restrictions from data the agent has already read.

Changing tool arguments can satisfy an audience `contains` requirement. For example, a sanitizer could replace an external recipient with an allowed internal recipient. It cannot satisfy `within` or trust requirements, because changing arguments does not change the data the agent has already read.

OpenAPPA selects the contract that matches the new arguments and checks the call again, including its requirements, effects, and argument schema. If that contract uses an annotator, OpenAPPA requests a new annotation. Membership checks use the responses already recorded for this decision.

A sanitizer's [tags](#tags) select the tools whose data it can transform. When arguments change, the new matching contract must also have a matching tag. Only a sanitizer without tags can transform a child agent's answer, because that answer is not a tool result.

### Implementing a sanitizer

A sanitizer implementation receives data and returns a transformed version. For example, a program can remove customer names and account numbers from a ticket before the agent reads it. The implementation must perform the transformation described by `hint` and make the result suitable for the audience or trust rank allowed by `permits`.

Configure the implementation under `[externals.sanitizers.<name>]`, using the name from `[[policy.sanitizer]]`. Use `url` for an HTTP service or `command` for a local program.

For example, this configuration runs a local program for the `remove_customer_details` sanitizer:

```toml
[externals.sanitizers.remove_customer_details]
command = ["python3", "./remove_customer_details.py"]
```

You can also select a built-in implementation with `builtin` under `[externals.sanitizers.<name>]`. The available options are:

| Configuration | Behavior |
|---|---|
| `builtin = "redact-email"` | Replaces email addresses with a fixed placeholder. It does not remove other private information. |
| `builtin = "claude-code"` | Uses Claude Code to transform data according to the sanitizer's `hint`, `on`, and `permits`. |
| `builtin = "llm"` | Uses the model configured under `[externals.llm]` to transform data according to the sanitizer's `hint`, `on`, and `permits`. |

See [Externals](#externals) for implementation settings. The reserved `attest-schema` sanitizer has separate configuration for [structured child returns](#structured-child-returns).

### Sanitizer protocol

A consult request tells the sanitizer what to change and supplies the data in `artifact.body`. The sanitizer returns the transformed data in `answer.body`:

| Part | Fields |
|---|---|
| `declaration` | Instructions in `hint`, the permitted change in `permits`, and the data type in `on`. For `tool_input`, also the argument schema in `parameters`. |
| `artifact` | The data in `body`, and the tool name in `tool` when known. |
| `answer` | The transformed data in `body`. |

In a request, `on` is one string: `tool_input` or `tool_output`. OpenAPPA assigns the returned data's audience or trust rank from `permits`. See [The consult request](#the-consult-request) for the complete JSON format and response requirements.

## Authorities

An authority reviews a tool call that would otherwise be blocked. It can approve an exception only for requirements listed in its `permits` section.

In the example below, a person reviews requests to share data from tools tagged `support`. The reviewer can approve sharing to any audience, including public sharing:

```toml
[[policy.authority]]
name = "support-reviewer"
tags = ["support"]
hint = "Review whether this release of customer information is authorized."

[policy.authority.permits]
audience_missing = ["public"]

[externals.authorities.support-reviewer]
builtin = "hitl"
```

Approval applies to one call. It does not change the trajectory's label or approve later calls.

### Permissions, tags, and hints

Each field in `permits` allows the authority to approve a different type of requirement:

| `permits` field | What an approval can satisfy |
|---|---|
| `trust_below` | Allows a call whose required trust rank is not met, up to the rank specified here. |
| `audience_missing` | Allows sharing with readers outside the current audience, limited to the audience specified here. |
| `effects_containing` | Allows a call blocked by `excludes` because a listed effect has already occurred. |
| `attention` | The listed attention marks for this call. |

For example, these permissions let an authority approve a call that needs `trusted` data, a public audience, an exception for an earlier `email.sent` effect, or `finance-signoff`:

```toml
[[policy.authority]]
name = "finance-officer"

[policy.authority.permits]
trust_below = "trusted"
audience_missing = ["public"]
effects_containing = ["email.sent"]
attention = ["finance-signoff"]
```

An authority's [tags](#tags) select the tools it can review for unmet audience, trust, or effects requirements. Attention approvals are selected by `permits.attention` instead.

The optional `hint` explains what the authority reviews. It does not expand `permits`.

### Implementing an authority

An authority can use a built-in reviewer, an HTTP service, or a local program. Configure the implementation under `[externals.authorities.<name>]`, using the name from `[[policy.authority]]`:

| Implementation | Behavior |
|---|---|
| `builtin = "hitl"` | Asks a person to review the exact call and the requirements to be approved. |
| `builtin = "approve"` | Automatically approves every matching request within `permits`. |
| `builtin = "claude-code"` or `builtin = "llm"` | A model approves or denies using the declaration, call, and unmet requirements. |
| `builtin = "<module name>"` | Runs a module loaded from `--modules-dir` with the same system permissions as OpenAPPA. |
| `url` or `command` | Asks an external service or local program to approve or deny the call. |

Every implementation is limited by the authority's `permits`, including automatic approvers.

### Authority protocol

OpenAPPA sends the authority the proposed tool call and the requirements that need approval. The consult request puts `hint` and `permits` in `declaration`. The `artifact` field contains the tool name in `tool`, its `arguments`, and the unmet `requirements`.

Each entry in `requirements` uses one of these forms:

| Requirement | JSON form |
|---|---|
| Trust | `{"kind":"trust","required":"trusted"}` |
| Public audience | `{"kind":"audience","required":"public"}` |
| Restricted audience | `{"kind":"audience","required":2}`; the number is the required reader count. |
| Effect exclusion | `{"kind":"effect","excludes":"email.sent"}` |
| Attention | `{"kind":"attention","mark":"finance-signoff"}` |

The request describes the requirements to approve. It does not include the trajectory's current audience or trust rank, or the identities of its current readers.

The authority returns `ruling` as `approve` or `deny`, with an optional `reason`. For example:

```json
{
  "version": 1,
  "answer": {
    "ruling": "approve",
    "reason": "The user authorized this email."
  }
}
```

See [The consult request](#the-consult-request) for the full request format.

## Remedy plans and child returns

The authorities, sanitizers, and integration settings in the policy determine which remedy plans OpenAPPA can offer. These plans give the agent ways to continue when a call is blocked or a result would add restrictions.

For a blocked call, a plan can request approval or change the proposed arguments. For a restricted result, a plan can clean it before the agent reads it or let the agent accept its restrictions.

### Subagent Returns

A child agent can read data without exposing it to the parent agent. `context_control = true` declares that the integration keeps the child's data separate and can withhold its answer until OpenAPPA allows it. The parent chooses how the answer will be checked or cleaned before the child starts.

OpenAPPA first offers a plan that checks the child's answer without changing it. It then offers plans that use the registered `tool_output` sanitizers without tags.

The parent supplies `label` to specify the audience and trust limits for the child's answer. `label = {}` uses the parent's current audience and trust rank, so the answer cannot add restrictions to the parent. If a plan uses a sanitizer, the cleaned answer must meet those limits.

In the example below, a child can read internal customer data and pass its answer through `remove_customer_details`. The sanitizer must remove private details before the parent receives the answer:

```toml
[[policy.sanitizer]]
name = "remove_customer_details"
on = ["tool_output"]
hint = "Remove customer identities and all other private details from the return."

[policy.sanitizer.permits]
audience = { from = ["internal"], to = ["public"] }

[policy.deployment]
context_control = true

[externals.sanitizers.remove_customer_details]
url = "https://sanitizer.corp/sanitize"
```

The parent selects this sanitizer's plan by its `offer_id`. This request keeps the parent's current audience and trust rank as the limits for the cleaned answer:

```json
{ "offer_id": "<the sanitizer offer ID>", "label": {} }
```

The chosen limits also restrict what the child can read: it must still be able to return an answer that meets them, with the selected sanitizer if needed. The child receives its answer requirements when it starts and submits its answer when it finishes its turn. If the answer does not meet those requirements, OpenAPPA explains the problem so the child can revise it.

### Structured child returns

Use the reserved sanitizer `attest-schema` when the child must return structured data, such as a number of days, rather than free text. It checks the answer against the parent's JSON Schema without changing it. It can raise trust from `suspicious` to `trusted` only when all these conditions hold:

1. Every field limits what the child can return: a number, a boolean, a fixed list of choices, or a restricted format. Free text is not permitted.
2. The parent declares the schema before the child reads untrusted data.
3. The parent is trusted when it starts the child.

The following declaration allows `attest-schema` to return a trusted answer after these checks:

```toml
[[policy.sanitizer]]
name = "attest-schema"
on = ["tool_output"]
hint = "Validate the child's structured return against the declared schema."

[policy.sanitizer.permits]
trust = { from = "suspicious", to = "trusted" }

[policy.deployment]
context_control = true
```

The parent supplies `return_schema` when it selects the plan. This example requires one non-negative integer, `days_allowed`, and rejects extra fields:

```json
{
  "offer_id": "<the attest-schema offer ID>",
  "label": {},
  "return_schema": {
    "type": "object",
    "properties": { "days_allowed": { "type": "integer", "minimum": 0 } },
    "required": ["days_allowed"],
    "additionalProperties": false
  }
}
```

OpenAPPA runs `attest-schema` itself. An `[externals.sanitizers.attest-schema]` section is not allowed. The schema checks the answer's format; it cannot check whether the number of days is factually correct.

### Example: Customer Ticket Policy

This complete configuration accompanies the [customer-ticket example](/how-it-works#example-sharing-information-from-a-private-customer-ticket).

The integration must be able to keep original ticket results hidden and keep a child's data separate from its parent's. The example uses external services and an `APPA_PII_TOKEN` environment variable to authenticate requests to the sanitizer.

```toml
[policy]
version = 2

[policy.deployment]
context_control = true
confined_results = ["get_ticket_from_crm"]

[policy.audience]
internal = ["google-workspace:full-members"]

[[policy.tool]]
name = "get_ticket_from_crm"
delta = { audience = ["internal"] }

[[policy.tool]]
name = "send_email"
parameters = { type = "object", properties = { recipient = { type = "string" }, body = { type = "string" } }, required = ["recipient", "body"] }
requires = { audience = { contains = ["$recipient"] } }
delta = {}
effects = ["egress"]

[[policy.tool]]
name = "file_github_issue"
requires = { audience = { contains = ["public"] } }
delta = {}
effects = ["egress", "mutation"]

[[policy.sanitizer]]
name = "remove_customer_details"
on = ["tool_output"]
hint = "Remove customer identities and all other private details from the ticket."

[policy.sanitizer.permits]
audience = { from = ["internal"], to = ["public"] }

[[policy.authority]]
name = "user"

[policy.authority.permits]
audience_missing = ["public"]

[externals]
timeout_ms = 2000
max_body_bytes = 65536

[externals.sanitizers.remove_customer_details]
url = "https://sanitizer.corp/sanitize"
token_env = "APPA_PII_TOKEN"

[externals.authorities.user]
builtin = "hitl"

[externals.audience.google-workspace]
url = "https://audience.corp/google-workspace"
selectors = [
  { template = "viewer", feeds = "self" },
  { template = "full-members", feeds = "internal" },
  { template = "group/<group-address>" },
]
```

After the agent reads the original ticket, it can email readers identified as company members by the membership service. External email and public issue creation require approval. Approval permits one call and leaves the trajectory internal.

If the agent receives only the cleaned, public version of the ticket, that result does not restrict it to internal readers. The same applies to a cleaned answer from a child agent.

The example authority has no tags, so it can approve sharing to any audience for any tool. See [Validation](/validation) for ways to check policy behavior.

## Include policy files

Use `include` to reuse configuration from other files. It belongs at the start of the file, outside `[policy]` and `[externals]`. This example loads declarations from `battery.toml` alongside the root file's settings:

```toml
include = ["battery.toml"]

[policy]
version = 2

[externals]
timeout_ms = 2000
max_body_bytes = 65536
```

OpenAPPA checks declarations in the root file first, then declarations from included files in the order listed by `include`. The following rules apply:

- An included file cannot include another file.
- An included file cannot replace settings that apply to the whole deployment.
- A root `[[policy.annotator]]` replaces an included annotator with the same name. Fields omitted from the replacement are not inherited.
- Two included files cannot declare the same annotator.
- Two files cannot configure the same external component name within the same kind, such as two `[externals.sanitizers.clean]` sections.

See [Batteries](/batteries) for reusable policy files.

## Deployment coverage

These settings describe what the agent integration can control: which tool results it can withhold and whether it can keep a child agent's data separate from the parent's:

```toml
[policy.deployment]
confined_results = ["get_ticket_from_crm"]
context_control = true

[[policy.tool]]
name = "get_ticket_from_crm"
delta = { audience = ["internal"] }
```

`confined_results` lists tools whose results the integration can withhold from the agent.

`context_control = true` declares that the integration can keep a child agent's data hidden from the parent and withhold the child's answer until OpenAPPA allows it. This lets OpenAPPA check or clean the answer before the parent reads it. The integration must implement this behavior; the setting alone does not provide it.

- A `tool_output` sanitizer requires either a tool listed in `confined_results` or a child agent's answer controlled through `context_control`.
- Every tool in `confined_results` must have a policy contract. A wildcard contract also satisfies this requirement.
- Some tools run inside the model provider's service. The integration cannot intercept their results before the model reads them. These tools can declare only static `delta` fields. They cannot declare requirements, annotators, or argument selectors, and cannot appear in `confined_results`.

OpenAPPA rejects configurations that require controls the integration does not support. See [integration configuration](/writing-an-integration) for the integration's responsibilities.

## Externals

The `[externals]` sections tell OpenAPPA how to call the components declared in the policy. Each component uses `[externals.<kind>.<name>]`, where `kind` identifies its role and `name` matches its policy declaration. This connection is called a binding.

In the example below, OpenAPPA sends approval requests for `support-reviewer` to an HTTP service. The shared settings limit requests to two seconds and responses to 65,536 bytes:

```toml
[externals]
timeout_ms = 2000
max_body_bytes = 65536

[externals.authorities.support-reviewer]
url = "https://approver.corp/rule"
token_env = "APPA_APPROVER_TOKEN"
```

`timeout_ms` limits the time an endpoint or command has to answer one request. `max_body_bytes` limits the accepted response size. These settings apply to the whole deployment.

The available settings depend on the component's role:

| Component kind | Implementation setting | Requirement |
|---|---|---|
| `authorities` | Exactly one of `url`, `command`, or `builtin`. | Optional. Without a binding, the authority returns no answer. |
| `sanitizers` | Exactly one of `url`, `command`, or `builtin`. | Required, except for `attest-schema`. |
| `annotators` | Exactly one of `url` or `command`. | Required unless the declaration specifies a builtin. |
| `audience` | Exactly one of `url`, `command`, or `readers`; `selectors` on a `url` or `command` entry; optional `lookup`. | Required for each referenced provider and each `lookup` target. `readers` is allowed only on a `lookup` target. A root entry whose only key is `lookup` routes a provider that an included file binds. |

OpenAPPA rejects an external component name that the policy does not declare, or a component that is missing its required implementation. For annotators, `builtin` belongs on `[[policy.annotator]]`, not under `[externals]`.

Included files can add bindings and annotator builtins. They cannot replace root settings: `timeout_ms`, `max_body_bytes`, `review_timeout_ms`, `[externals.claude_code]`, or `[externals.llm]`.

### HTTP services

Set `url` to the service endpoint. Use HTTPS for remote services. Plain HTTP is allowed only for a service on the same machine, at a loopback address such as `127.0.0.1`. Do not put a username or password in the URL.

If the service requires authentication, set `token_env` to an environment variable such as `APPA_SANITIZER_TOKEN`. OpenAPPA reads its value and sends it as a bearer token. The variable name must start with `APPA_`, but cannot start with `APPA_PROVIDER_`.

### Local programs

Set `command` to a list containing the executable and its arguments, such as `command = ["python3", "./sanitize.py"]`. OpenAPPA runs the program on the same Unix machine, without a shell. It starts the program in the directory containing the configuration file.

The program reads one JSON consult request from standard input and writes one JSON response to standard output. It must respond within `timeout_ms`, and its response must fit within `max_body_bytes`. Each OpenAPPA instance runs at most eight such programs at once.

If the program needs a credential, set `token_env` to an environment variable whose name starts with `APPA_PROVIDER_`. OpenAPPA passes that variable to the program. It does not pass other `APPA_*` variables, including its own credentials.

OpenAPPA does not require this variable when loading the policy. The program must handle a missing credential when it runs.

### The consult request

A consult request is a JSON request that OpenAPPA sends to an external component. HTTP services and local programs receive the same format. This example asks `support-reviewer` to approve an email whose recipient is outside the current audience:

```json
{
  "version": 1,
  "kind": "authority",
  "name": "support-reviewer",
  "declaration": {
    "hint": "Review whether this release of customer information is authorized.",
    "permits": { "audience_missing": ["public"] }
  },
  "artifact": {
    "tool": "send_email",
    "arguments": { "recipient": "auditor@external.com", "body": "Ticket summary" },
    "requirements": [{ "kind": "audience", "required": 1 }]
  }
}
```

| Key | Meaning |
|---|---|
| `version` | Protocol version. Must be `1`. |
| `kind` | `authority`, `sanitizer`, `annotation`, or `audience`. |
| `name` | The component name declared in the policy. |
| `declaration` | Policy instructions and limits for the component. The agent does not supply them. |
| `artifact` | Request data: the tool call to review, data to clean, or the selector or member to look up. |

Each component uses these fields differently:

| Kind | `declaration` | `artifact` | `answer` |
|---|---|---|---|
| `authority` | `hint`, `permits` | `tool`, `arguments`, `requirements` | `ruling`, optional `reason` |
| `sanitizer` | `hint`, `on`, `permits`; `parameters` for input rewrites | `tool` when known, `body` | `body` |
| `annotation` | `hint`, `inputs`, `trust_ranks`, `audiences`, `attention_marks`, `effects` | `args` | `delta`, `requires`, `emits` |
| `audience` | `templates` | `selector` or `member` | `members` or `principal` |

For an audience request, `declaration.templates` lists the selector templates the policy declares for the provider under `selectors`, such as `viewer` and `user-group/<handle>`. The service MUST refuse a request whose templates differ from the ones it serves. It reads the requested selector or member ID from `artifact` and returns its result under `answer`.

OpenAPPA records membership responses with the decision that requested them. If that decision requires an approval or remedy, OpenAPPA reuses those responses when it continues the decision. A new decision can request updated membership. Replaying a recorded decision uses its saved responses without calling the membership service. Responses from unrelated decisions cannot be substituted.

A consult request does not include the agent's current audience, trust rank, previous actions, or user message. The component processes the request data in `artifact` using the instructions and limits in `declaration`.

The service or program returns `{"version":1,"answer":{...}}`. The fields inside `answer` must match the component's response format. Extra fields in the surrounding response object are not allowed.

OpenAPPA rejects a response if the HTTP service reports an error, the program exits with a non-zero status, the request times out, or the response exceeds the size limit or has an invalid format. It cannot use that response to approve a call, deliver cleaned data, or annotate a tool.

### Model implementations

The `claude-code` and `llm` implementations send the component's instructions and request data to a model. OpenAPPA puts fixed instructions and `declaration` in the system prompt. It sends `artifact` as the user message, to be processed as data.

OpenAPPA builds the expected response format from the declaration. The model returns only the contents of `answer`, without the surrounding `version` and `answer` fields.

OpenAPPA checks authority and annotator answers against their permits and assigns sanitized data the declared audience or trust rank. The model is responsible for making the correct judgment or removing the required content.

`[externals.claude_code]` configures the local Claude Code implementation:

| Field | Purpose |
|---|---|
| `command` | Selects the executable. |
| `model` | Selects the model. |
| `timeout_ms` | Sets the timeout for one request. |

Each request starts a new `claude -p` process. It cannot use tools, load project settings, or reuse a previous conversation. It runs in a new temporary directory with optional background traffic disabled and receives no `APPA_*` environment variables. Each OpenAPPA instance runs at most four of these requests at once.

`[externals.llm]` selects the model used by all `builtin = "llm"` components. This example uses an Anthropic model, a token from `APPA_LLM_TOKEN`, a 30-second timeout, and up to four concurrent requests:

```toml
[externals.llm]
provider = "anthropic"
model = "claude-sonnet-4-5"
token_env = "APPA_LLM_TOKEN"
timeout_ms = 30000
max_concurrent = 4
# Optional endpoint override:
# url = "https://gateway.corp/v1"
```

Supported providers are `anthropic`, `openai`, `gemini`, and `ollama`. `token_env` is required except for `ollama`. An optional `url` selects a custom endpoint and follows the same URL rules as [HTTP services](#http-services).

`openai` uses the Chat Completions API, including when `url` points to a compatible service. `ollama` uses `http://localhost:11434` unless `url` specifies another endpoint, and requires no token.
