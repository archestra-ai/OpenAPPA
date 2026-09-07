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
| `tags` | Names used to select applicable authorities and sanitizers. |
| `delta` | Can restrict the audience or lower trust. It cannot make the trajectory less restricted. |
| `requires` | Audience, trust, effects, or attention requirements for the call. |
| `effects` | Effect names recorded after successful execution. Declare all relevant side effects. |
| `annotator` | A registered component that supplies the complete annotation for each call. |

An omitted `delta` adds no restriction. An omitted `requires` adds no requirement.

Only one source of contract rules is allowed: static `delta`, `requires`, and `effects` fields, or an `annotator`. Combining them causes a load error.

OpenAPPA checks both `delta` and `requires` before allowing the tool call.

### Pattern matching

A policy can declare several contracts for one tool. OpenAPPA checks them in declaration order and selects the first matching contract.

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
# Single quotes:
name = 'search(query:a\,b)'
```

```toml
# Equivalent form with double quotes:
name = "search(query:a\\,b)"
```

Only the four escapes listed above are supported. Other escapes cause a policy load error.

OpenAPPA selects a contract before it validates the contract's `parameters` schema. A schema error does not select a later contract. Rewritten arguments select their own matching contract. See [Sanitizers](#sanitizers) for rewrite rules.

### Handling undeclared tools

The tool name `"*"` covers tool names that the policy does not declare. The current format requires an annotator for this entry:

```toml
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
| Other strings, such as `"alice@example.com"` | A literal reader ID, compared exactly after [identity resolution](#identity-resolution). For example, `"finance"` is a reader ID; `"@finance"` refers to a group. |

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

OpenAPPA reads the proposed call's `recipient` argument and checks that the current audience includes its readers. The tool's `parameters` schema must declare the argument as a required top-level string. Argument placeholders are allowed only under `contains`.

The argument can contain a literal reader, `public`, `self`, `internal`, or an `@` mention. An unresolved dynamic mention stops the call with an operational error.

#### Configure audience membership

Membership answers: who belongs to this audience? OpenAPPA asks an external membership service for the members. For example, that service can read the members of a Slack group.

Each `from` entry has the form `provider:selector`. The provider identifies the service configured under `[externals.audience.<provider>]`. The selector tells that service which identity or group to read.

The example below uses a Google Workspace membership service to define `self`, `internal`, and `@finance`:

```toml
# Use the Google Workspace viewer as self.
[policy.audience.self]
from = ["google-workspace:viewer"]

# Use Google Workspace organization members as internal.
[policy.audience.internal]
from = ["google-workspace:full-members"]

# Define @finance from a Workspace group and declare it part of internal.
[[policy.audience.group]]
name = "finance"
within = "internal"
from = ["google-workspace:group/finance@corp.com"]

# Set the service that supplies Google Workspace membership.
[externals.audience.google-workspace]
url = "https://audience.corp/google-workspace"
```

For `google-workspace:group/finance@corp.com`, OpenAPPA sends `group/finance@corp.com` as the selector to the membership service configured under `[externals.audience.google-workspace]`. The service reads the group's members and returns them to OpenAPPA.

OpenAPPA defines the selector formats below. The external service must understand these formats and perform the membership lookup. Configuring a provider does not connect OpenAPPA directly to Google Workspace, Slack, or GitHub; you must supply the service that makes that connection.

The current implementation has a fixed set of providers and selector formats. You cannot add a new provider name or selector format through the policy file. Replace values in angle brackets with the group address, handle, organization, or team to read.

| Provider | Selector formats understood by its service |
|---|---|
| `google-workspace` | `viewer`, `full-members`, `group/<group-address>` |
| `slack` | `viewer`, `full-members`, `user-group/<handle>` |
| `github` | `viewer`, `org/<org>/members`, `org/<org>/team/<team>` |

Choose a selector based on the audience you configure:

| Audience section | Selectors you can use in `from` |
|---|---|
| `[policy.audience.self]` | `viewer`: the identity OpenAPPA acts for. |
| `[policy.audience.internal]` | `full-members` for Google Workspace or Slack; `org/<org>/members` for GitHub. For example, `github:org/acme/members` makes members of `acme` internal. Members of other GitHub organizations are not included by this source. |
| `[[policy.audience.group]]` | A specific group or an organization's members, using any of the formats above except `viewer`. |

OpenAPPA rejects the configuration if you use a selector in the wrong section. For example, `slack:viewer` cannot define `internal`.

If `from` contains several sources, the audience includes members from any of them. For example, `from = ["google-workspace:full-members", "slack:full-members"]` includes members returned by either service.

In the `finance` example, `within = "internal"` declares that every member of `finance` is internal. OpenAPPA trusts this declaration; it does not check each member against the sources for `internal` or check their email domain. A group can declare `within = "self"` or `within = "internal"`.

You can use `"@slack:user-group/oncall"` directly instead of declaring a named group such as `@oncall` in `[[policy.audience.group]]`. It refers to the Slack group `oncall`. To use this reference, at least one `from` entry must use the `slack` provider:

```toml
# Use Slack workspace members as internal.
[policy.audience.internal]
from = ["slack:full-members"]

# Restrict incident details to the Slack oncall group.
[[policy.tool]]
name = "get_incident"
delta = { audience = ["@slack:user-group/oncall"] }

# Set the service that supplies Slack membership.
[externals.audience.slack]
url = "https://audience.corp/slack"
```

OpenAPPA rejects policy references to undeclared named audiences, providers not used in `from`, or unsupported selector formats.

##### Membership request protocol

Audience providers support HTTP endpoints and local commands.

OpenAPPA can ask the membership service for a group's members or for details about one member. The examples below show the request data and response data. For the complete JSON request and response format, see [The consult request](#the-consult-request).

To read the members of the Slack `oncall` group, OpenAPPA sends:

```json
{"selector": "user-group/oncall"}
```

The service returns the members and their identity details:

```json
{
  "members": [
    {"id": "slack:U1", "verified_email": "a@corp.com"}
  ]
}
```

To look up one member, OpenAPPA sends:

```json
{"member": "slack:U1"}
```

The service returns that member's identity details under `claims`:

```json
{
  "claims": {
    "id": "slack:U1",
    "verified_email": "a@corp.com"
  }
}
```

If the service cannot find the member, it returns `{"claims": null}`.

The service returns `{"members": []}` when a group has no members. This is a successful response. If the service fails or takes too long to respond, OpenAPPA cannot complete the audience check and stops the operation.

#### Identity resolution

Membership services can identify the same person as `google-workspace:alice@corp.com`, `slack:U012345`, or `github:alice`. Identity resolution maps these identities to a common reader ID, such as `alice@corp.com`, which OpenAPPA uses to check audience conditions.

This applies only to members returned by services for `self`, `internal`, and groups, including directly referenced groups. Audience names remain unchanged. Literal reader IDs in the policy or tool arguments are compared directly, without identity resolution.

##### Default implementation

`verified-email` is OpenAPPA's default identity implementation. OpenAPPA uses it automatically if you omit `[policy.identity]`.

It reads the membership service's `verified_email` field, checks that it has a valid email format, and uses that address as the reader ID. If the field is absent, it keeps the provider ID.

The membership service is responsible for verifying who owns the email address. OpenAPPA trusts that service's claim; it does not verify ownership itself. A value such as `"finance"` or `"id63234"` in `verified_email` causes an error because it is not an email address. OpenAPPA does not fall back to the provider ID when this field is invalid.

The following configuration explicitly selects this default behavior. You do not need to add it to your file:

```toml
[policy.identity]
implementation = "verified-email"
```

For example, a Slack member and a GitHub member with the verified email `alice@corp.com` become the same reader. A Slack member without a verified email keeps an ID such as `slack:U012345`. OpenAPPA makes no additional network requests for this step.

OpenAPPA converts the email domain to lowercase. It leaves the part before `@` unchanged, including dots and `+suffix` values. It does not merge aliases or treat personal and corporate addresses as the same identity.

##### Custom implementation

To apply your own identity rules, configure an external identity service. For example, your service could map `github:alice` and `slack:U012345` to the same reader ID, `alice@corp.com`:

```toml
# Use the corp-identity service to resolve member identities.
[policy.identity]
implementation = "corp-identity"

# Set the service endpoint.
[externals.identity.corp-identity]
url = "https://identity.corp/resolve"
```

The `implementation` name must match the name under `[externals.identity.<name>]`. Identity services support HTTP endpoints and local commands.

##### Identity resolution protocol

OpenAPPA sends one member's identity details to the service configured under `[externals.identity.corp-identity]`. The service returns one reader ID. The examples below show the request data and response data. For the complete JSON format, see [The consult request](#the-consult-request).

For a Slack member, OpenAPPA sends:

```json
{
  "id": "slack:U012345",
  "verified_email": "alice@corp.com"
}
```

The service returns the reader ID in the `principal` field:

```json
{"principal": "alice@corp.com"}
```

The `verified_email` field is omitted when the member has no verified email. The service must return one valid reader ID for each member and must return the same result for the same input. OpenAPPA saves the response with the decision that requested it.

If the service fails, takes too long to respond, or returns an invalid answer, OpenAPPA cannot complete the identity check and stops the operation.

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

Reading a later result marked `trusted` does not undo the earlier drop to `suspicious`. A tool's `delta.trust` can lower the trajectory's trust, but cannot raise it. To allow a blocked call, configure a [remedy plan](#remedy-plans-and-child-returns) with permission to address its trust requirement.

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

The mark `sre-signoff` routes the request to authorities whose `permits.attention` lists it. Authority tags do not restrict this routing. See [Authorities](#authorities) for other approval permissions.

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

Without `inputs`, the annotator receives the complete tool call under `args`:

```json
{
  "args": {
    "name": "Bash",
    "description": "Runs one shell command and returns its output.",
    "arguments": { "command": "cargo test" }
  }
}
```

The complete-call form does not require a parameter schema. If the tool has no description, OpenAPPA omits `description` from the artifact.

### Inputs

Use `inputs` to select call data and assign input names:

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

The annotator receives `customer_id` under `subject`. It still returns the complete annotation.

| Input value | Selected data |
|---|---|
| `$tool_call` | Complete call: name, optional description, and arguments. |
| `$tool_call.name` | Tool name. |
| `$tool_call.description` | Description declared on the tool. Requires a description. |
| `$tool_call.arguments` | Complete argument object. |
| `$tool_call.arguments.<name>` | One required top-level argument. Requires a parameter schema. |

`$tool_call` is the only input source. A selected argument can contain any JSON value permitted by its schema.

### Permits and hint

An annotator's permits limit the values it can use in its answers. The following fields define these limits:

| Field | Allowed values in an answer | If omitted |
|---|---|---|
| `ranks` | Ranks used in `delta.trust` or `requires.trust`. | Every rank in the trust chain. |
| `audiences` | Built-in audiences, `@` mentions, or literal readers used in restricted audiences. | Every audience in the policy vocabulary. |
| `marks` | Required attention marks. | Every mark declared in an authority's `permits.attention`. |
| `effects` | Effects emitted or checked in history. | Every effect kind declared by the policy. |

`public` is always an allowed audience answer. Do not list it in `audiences`. An empty `audiences` list permits only public answers. The default audience vocabulary includes `self`, `internal`, named groups, and reader IDs from declarations.

Write explicit bounds for each annotator. An omitted bound does not prohibit values. It admits the corresponding policy vocabulary.

The optional `hint` explains how to select values. It can define terms, evidence requirements, and examples. It cannot expand the permits and cannot exceed 512 characters. An annotator name must be non-empty and can contain dots.

A root annotator replaces the complete included declaration with the same name. When changing only a hint, repeat the original implementation, inputs, and fields that define its permits.

### Implementing an annotator

Bind an annotator to an HTTP endpoint or Unix command under `[externals.annotators.<name>]`.

Alternatively, use a built-in annotator. The available options are:

- `builtin = "claude-code"`: uses Claude Code to classify tool calls.
- `builtin = "llm"`: uses the model configured under `[externals.llm]` to classify tool calls.

```toml
[[policy.annotator]]
name = "classify-call"
builtin = "claude-code"
ranks = ["suspicious"]
audiences = ["internal"]
marks = []
effects = []
hint = "Treat output as suspicious. Restrict private results to internal."
```

Do not add an external binding for an annotator with a builtin. A `builtin` field under `[externals.annotators.<name>]` is invalid.

The deployment refuses to load an unbound annotator, an unknown binding, or an unavailable builtin. `llm` requires `[externals.llm]`. `claude-code` requires a supported Unix host.

### Annotator protocol

A consult request to an annotator uses `kind = "annotation"`. For the customer example, the request is:

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

`declaration.inputs` is empty for the complete-call form. The other declaration fields contain the permitted values. The request excludes the current trajectory label and history.

An endpoint or command returns:

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
- Other leaves are optional. An omitted leaf adds no restriction or requirement.
- `requires.audience` can contain `contains`, `within`, or both.
- Each history entry is `{"contains":"<effect>"}` or `{"excludes":"<effect>"}`.
- JSON audience values use `"public"` or a list of permitted audiences. Do not put `public` inside a JSON audience list.
- A restricted list cannot repeat entries or contain both `self` and `internal`.

OpenAPPA rejects unknown keys, `null` values, empty audience objects, duplicate emitted effects, and values outside the permits. A model builtin returns the `answer` object without the envelope.

The accepted annotation is bound to the exact call. A recorded recheck or record replay reuses it. A rewritten call receives a new annotation. Symbolic audience membership uses the decision's recorded evidence.

If classification fails, the call does not run. The agent can propose the call again. Checking the permits limits the values in the answer; it does not establish that the classification is correct.

## Sanitizers

A sanitizer transforms data before the agent receives it or before a tool receives new arguments. Its `permits` table declares the label transition allowed for the transformed value.

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

Replace the example endpoint with a service that performs the stated transformation. This declaration permits public sharing of its output. The service must remove all information that cannot be shared publicly.

### Permitted transitions

A sanitizer permits a transition in one dimension. Declare either `audience` or `trust`, not both.

| Transition | Meaning of `from` | Meaning of `to` |
|---|---|---|
| `audience` | Readers that the source audience must contain. | Exact audience assigned to the transformed value. |
| `trust` | Minimum trust rank that the source must meet. | Trust rank assigned to the transformed value. |

For example, a sanitizer can validate or transform suspicious input into trusted output:

```toml
[[policy.sanitizer]]
name = "vouch-fetched-text"
on = ["tool_output"]

[policy.sanitizer.permits]
trust = { from = "suspicious", to = "trusted" }
```

This declaration needs a suitable implementation and an output point the deployment can control. The declaration alone does not establish that the output is safe to trust.

The optional `hint` states what the sanitizer removes or validates. It grants no additional permission. `permits` limits the output label; it does not prove that an implementation removed the required content.

### Tool outputs and inputs

| `on` value | Application point | Required behavior |
|---|---|---|
| `tool_output` | A tool result or child return. | The integration withholds the original value and delivers the transformed value. |
| `tool_input` | All arguments of one tool call. | The integration dispatches exactly the replacement arguments. |

For tool output, OpenAPPA offers a sanitizer only when its transition can reduce the additional restriction. If the agent selects it, the integration withholds the original result and runs the sanitizer.

The agent receives the transformed result. If restrictions remain, the agent can accept them or select another compatible sanitizer. Cleaning a new result does not remove restrictions from data already in the trajectory.

A `tool_input` rewrite can satisfy an unmet audience `contains` requirement. It cannot satisfy a `within` or trust requirement. Those requirements still apply to the trajectory and the decision to call the tool.

OpenAPPA selects a contract for the rewritten arguments. The replacement call must satisfy that contract's requirements, effects, and parameter schema. If the contract uses an annotator, OpenAPPA requests a new annotation. Membership checks reuse the decision's recorded evidence.

A sanitizer's `tags` restrict it to values from tools with a matching tag. For input rewrites, the tags must also match the selected replacement contract. A child return has no originating tool. Only a sanitizer without tags can transform that return.

### Implementing a sanitizer

A sanitizer implementation receives data and returns a transformed version. For example, a program can remove customer names and account numbers from a ticket before the agent reads it. The implementation must perform the transformation described by `hint` and make the result suitable for the audience or trust rank allowed by `permits`.

Configure the implementation under `[externals.sanitizers.<name>]`, using the name from `[[policy.sanitizer]]`. Use `url` for an HTTP service or `command` for a local program.

For example, this configuration runs a local program for the `remove_customer_details` sanitizer:

```toml
[externals.sanitizers.remove_customer_details]
command = ["python3", "./remove_customer_details.py"]
```

You can also select a built-in implementation with `builtin`. See [Externals](#externals) for implementation settings. The reserved `attest-schema` sanitizer has separate configuration for [structured child returns](#structured-child-returns).

### Sanitizer protocol

A consult request to a sanitizer contains these fields:

| Part | Fields |
|---|---|
| `declaration` | `hint`, `on`, and `permits`. For `tool_input`, also `parameters`. |
| `artifact` | `body`, and `tool` when the originating tool is known. |
| `answer` | `body`: the transformed value. |

The request's `on` is one string: `tool_input` or `tool_output`. OpenAPPA assigns the returned value's label from `permits`. See [The consult request](#the-consult-request) for the complete JSON format and failure rules.

## Authorities

An authority approves or denies a specific call with unmet requirements. Its `permits` table limits the requirements it can approve.

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

Add `tags = ["support"]` to the tools this reviewer should cover. This authority can approve sharing to any audience, including public sharing, for matching tools. Use narrower permissions where required.

Approval applies to one call. It does not change the trajectory's label or approve later calls. An offered remedy still requires the authority's decision.

### Permissions, tags, and hints

| `permits` field | What an approval can satisfy |
|---|---|
| `trust_below` | An unmet trust requirement, up to the specified rank. |
| `audience_missing` | Missing required readers, up to the specified audience. |
| `effects_containing` | An `excludes` requirement for a listed effect already present in history. |
| `attention` | The listed attention marks for this call. |

For example:

```toml
[policy.authority.permits]
trust_below = "trusted"
audience_missing = ["public"]
effects_containing = ["email.sent"]
attention = ["finance-signoff"]
```

Place this table after the authority it configures. The fields are independent permissions. Declare only those the authority needs.

Tags restrict which tools an authority covers for ordinary requirement gaps. If tags are omitted, it can cover all tools. Attention routing ignores tags. A mark routes to every authority whose `permits.attention` contains it.

The optional `hint` explains what the authority reviews. It does not expand `permits`.

A denial is recorded. The agent cannot repeat the same approval request for that specific call. An authority without an implementation returns no answer. This does not prevent deployment startup, but that authority cannot release a call.

### Authority implementation modes

| Implementation | Behavior |
|---|---|
| `builtin = "hitl"` | Asks a person to review the exact call and the requirements to be approved. |
| `builtin = "approve"` | Automatically approves every matching request within `permits`. |
| `builtin = "claude-code"` or `builtin = "llm"` | A model approves or denies using the declaration, call, and unmet requirements. |
| `builtin = "<module name>"` | Runs a trusted module loaded from `--modules-dir` with the runtime's privileges. |
| `url` or `command` | Requests a ruling from an external service or local program. |

Bind the implementation under `[externals.authorities.<name>]`. Every implementation has the same permission limits. A wide `permits` table gives an automatic approver wide approval power.

A consult request to an authority has `hint` and `permits` in `declaration`. Its artifact contains `tool`, canonical `arguments` (normalized JSON), and the unmet `requirements` the ruling would cover:

| Requirement | JSON form |
|---|---|
| Trust | `{"kind":"trust","required":"trusted"}` |
| Public audience | `{"kind":"audience","required":"public"}` |
| Restricted audience | `{"kind":"audience","required":2}`; the number is the required reader count. |
| Effect exclusion | `{"kind":"effect","excludes":"email.sent"}` |
| Attention | `{"kind":"attention","mark":"finance-signoff"}` |

The artifact does not contain the current label, actual rank, or actual reader set. The answer contains `ruling`, either `approve` or `deny`, and an optional `reason`. The reason is logged at debug level and is not persisted.

## Remedy plans and child returns

When a call fails its requirements, OpenAPPA blocks it and returns the remedy plans permitted by the policy. A plan can request approval or transform the proposed arguments.

When a result would add restrictions, a plan can accept those restrictions or transform the result before delivery. Available plans depend on the configured components and deployment capabilities.

The agent selects an offered plan. An authority can deny it, or a sanitizer can fail. Selecting a plan does not guarantee execution. If no permitted remedy succeeds, the call or result remains blocked.

### Subagent Returns

A child can read data in a separate context and return only what the parent permits. With `context_control`, OpenAPPA holds the spawn until the parent selects a return plan.

The first plan accepts the child's return without transformation. Later plans use registered `tool_output` sanitizers without tags, in registry order.

Each plan takes a `label`: the minimum label the parent accepts from the child return. Use `{}` for the parent's own label. A sanitized route permits child restrictions only as far as the sanitizer can transform them back to that limit.

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

The parent selects the corresponding offer:

```json
{ "offer_id": "<the sanitizer offer ID>", "label": {} }
```

The return declaration also limits what the child can read. The child receives the declared shape at startup and returns by ending its turn. If the return violates the declaration, OpenAPPA blocks it and gives the child the reason. The child can submit a revised return.

### Structured child returns

The reserved sanitizer `attest-schema` validates structured child returns. It does not change the returned bytes. It can raise trust from `suspicious` to `trusted` only when all these conditions hold:

1. Every returned field has a restricted shape, such as a number, boolean, fixed enum, or bounded format. Free text is not permitted.
2. The parent declares the schema before the child reads untrusted data.
3. The parent is trusted when it starts the child.

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

The parent supplies `return_schema` when it selects the plan:

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

OpenAPPA applies `attest-schema` directly. Do not add `[externals.sanitizers.attest-schema]`; that binding causes a load error. Schema validation establishes the permitted structure, not the factual accuracy of the returned values.

### Example: Customer Ticket Policy

This complete configuration accompanies the [customer-ticket example](/how-it-works#example-sharing-information-from-a-private-customer-ticket).

The integration must withhold original ticket results and support separate child contexts. Replace the example service URLs and set `APPA_PII_TOKEN` before loading the configuration.

```toml
[policy]
version = 2

[policy.deployment]
context_control = true
confined_results = ["get_ticket_from_crm"]

[policy.audience.internal]
from = ["google-workspace:full-members"]

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
```

After the agent reads the original ticket, it can email verified company members. External email and public issue creation require approval. Approval permits one call and leaves the trajectory internal.

If the agent receives only a public sanitized result, that result does not add an internal restriction. The same principle applies to a sanitized child return.

The example authority can approve audience gaps across all tools. It has no tags and permits public sharing. Narrow these permissions when the reviewer should cover fewer releases.

Use [Validation](/validation) to check allowed calls, blocked calls, and remedy selection. Test the actual services separately. Replay stand-ins do not verify a sanitizer's data removal or a human review process.

## Include policy files

Use `include` at the file root to load other configuration files:

```toml
include = ["battery.toml"]

[policy]
version = 2

[externals]
timeout_ms = 2000
max_body_bytes = 65536
```

Root declarations come first. Included declarations follow in list order. The following rules apply:

- An included file cannot include another file.
- An included file cannot replace settings that apply to the whole deployment.
- A root `[[policy.annotator]]` replaces an included annotator with the same name. The replacement is complete. Repeat all fields that you want to keep.
- Two included files cannot declare the same annotator.
- Duplicate external names within one component kind are errors.

See [Batteries](/batteries) for reusable policy files.

## Deployment coverage

The deployment must support each configured application point:

```toml
[policy.deployment]
confined_results = ["get_ticket_from_crm"]
context_control = true
```

`confined_results` lists tools whose original results the integration can withhold. `context_control` requires separate child contexts and control over their returns. Setting these fields does not add those capabilities to an integration.

- A `tool_output` sanitizer needs a confined tool result or, with `context_control`, a child return.
- Each `confined_results` entry must name a covered tool. A wildcard covers any tool name for this check.
- A provider-run tool executes inside the model provider's inference call. Its result cannot be withheld by the host.
- Provider-run tools can declare only static `delta` semantics. They cannot declare requirements, annotators, or argument selectors, and cannot appear in `confined_results`.

Unsupported constructs cause a load error. See [integration configuration](/writing-an-integration) for deployment capabilities.

## Externals

An external binding connects a declared component to its implementation. Use `[externals.<kind>.<name>]`:

```toml
[externals]
timeout_ms = 2000
max_body_bytes = 65536

[externals.authorities.support-reviewer]
url = "https://approver.corp/rule"
token_env = "APPA_APPROVER_TOKEN"
```

`timeout_ms` limits the time an endpoint or command has to answer one request. `max_body_bytes` limits the accepted response size. These settings apply to the whole deployment.

| Component kind | Binding | Requirement |
|---|---|---|
| `authorities` | Exactly one of `url`, `command`, or `builtin`. | Optional. Without a binding, the authority returns no answer. |
| `sanitizers` | Exactly one of `url`, `command`, or `builtin`. | Required, except for `attest-schema`. |
| `annotators` | Exactly one of `url` or `command`. | Required unless the declaration specifies a builtin. |
| `audience` | Exactly one of `url` or `command`. | Required for each referenced provider. |
| `identity` | Exactly one of `url` or `command`. | Required for custom implementations. No binding for `verified-email`. |

A binding for an unregistered name causes a load error. A missing required binding also causes a load error. An annotator builtin belongs on `[[policy.annotator]]`, not under `[externals]`.

Included files can add bindings and annotator builtins. They cannot replace root settings: `timeout_ms`, `max_body_bytes`, `review_timeout_ms`, `[externals.claude_code]`, or `[externals.llm]`.

### HTTP services

Set `url` to the service endpoint. Use HTTPS for remote services. Plain HTTP is allowed only for a service on the same machine, at a loopback address such as `127.0.0.1`. Do not put a username or password in the URL.

If the service requires authentication, set `token_env` to an environment variable such as `APPA_SANITIZER_TOKEN`. OpenAPPA reads its value and sends it as a bearer token. The variable name must start with `APPA_`, but cannot start with `APPA_PROVIDER_`.

### Local programs

Set `command` to a list containing the executable and its arguments, such as `command = ["python3", "./sanitize.py"]`. OpenAPPA runs the program on the same Unix machine, without a shell. It starts the program in the directory containing the configuration file.

The program reads one JSON consult request from standard input and writes one JSON response to standard output. It must respond within `timeout_ms`, and its response must fit within `max_body_bytes`. Each OpenAPPA instance runs at most eight such programs at once.

If the program needs a credential, set `token_env` to an environment variable whose name starts with `APPA_PROVIDER_`. OpenAPPA passes that variable to the program. It does not pass other `APPA_*` variables, including its own credentials.

The variable can be absent when OpenAPPA loads the policy, but a program that needs it may fail when called. Set it before running that program.

### The consult request

A consult request is a JSON request that OpenAPPA sends to an external component. HTTP endpoints and local commands receive the same request format:

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
| `kind` | `authority`, `sanitizer`, `annotation`, `audience`, or `identity`. |
| `name` | Registered component name. |
| `declaration` | Policy instructions and limits for the component. The agent does not supply them. |
| `artifact` | The call, value, or membership data the component must process. |

| Kind | `declaration` | `artifact` | `answer` |
|---|---|---|---|
| `authority` | `hint`, `permits` | `tool`, `arguments`, `requirements` | `ruling`, optional `reason` |
| `sanitizer` | `hint`, `on`, `permits`; `parameters` for input rewrites | `tool` when known, `body` | `body` |
| `annotation` | `hint`, `inputs`, `trust_ranks`, `audiences`, `attention_marks`, `effects` | `args` | `delta`, `requires`, `emits` |
| `audience` | `templates` | `selector` or `member` | `members` or `claims` |
| `identity` | Empty | Member claims: `id`, optional `verified_email` | `principal` |

For an audience request, `declaration.templates` lists the selector formats that OpenAPPA registers for the provider, such as `viewer` and `user-group/<handle>`. The service reads the requested selector or member ID from `artifact` and returns its result under `answer`.

OpenAPPA records membership responses with the decision that requested them. If that decision requires an approval or remedy, OpenAPPA reuses those responses when it continues the decision. A new decision can request updated membership. Replaying a recorded decision uses its saved responses without calling the membership service. Responses from unrelated decisions cannot be substituted.

A consult request does not provide the current trajectory label, reader set, history, or user turn. The component evaluates its artifact against its declaration.

An endpoint or command returns `{"version":1,"answer":{...}}`. `answer` must contain exactly the fields defined for the component kind. Unknown envelope fields are invalid.

An error status, non-zero exit, timeout, oversized response, or malformed answer counts as no answer. A blocked call stays blocked, a withheld result stays withheld, and an unannotated call does not run. A failed request is not a denial.

### Model transports

`claude-code` and `llm` use the same model request structure. The system prompt contains fixed instructions and the declaration JSON. The artifact JSON is the only user turn and must be treated as data.

The output schema comes from the declaration. The model returns the component's answer object without the envelope. OpenAPPA persists only the validated answer, not the prompt or raw model output.

Authorities and annotators remain limited by their permits. Sanitizer output receives the declared transition. These checks do not prove that a model made the correct judgment or removed all private content.

`[externals.claude_code]` configures the local Claude Code implementation:

| Field | Purpose |
|---|---|
| `command` | Selects the executable. |
| `model` | Selects the model. |
| `timeout_ms` | Sets the timeout for one request. |

Each consult request starts one isolated `claude -p` process. It has no tools, project settings, or session persistence. It uses a fresh temporary directory, disables optional background traffic, and receives no `APPA_*` variables. At most four requests run concurrently per runtime.

`[externals.llm]` configures one API profile per deployment:

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

Supported providers are `anthropic`, `openai`, `gemini`, and `ollama`. `token_env` is required except for `ollama`. Endpoint overrides follow the same URL rules as HTTP bindings.

`openai` uses the Chat Completions API. An OpenAI-compatible endpoint can use the same profile. `ollama` defaults to `http://localhost:11434` and requires no token.
