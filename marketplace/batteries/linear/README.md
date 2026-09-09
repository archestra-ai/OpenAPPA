# Linear battery

Contracts for all **65 tools** captured from Linear's official read/write MCP
endpoint and all **36 tools** from its read-only endpoint. Per-tool definition hashes are in
`schema-lock.json`; the reviewed operation and argument classification is in
`operations.json`. Python 3.10+ is required. The command annotator is deterministic,
uses no model or network, and refuses unknown tools, unreviewed arguments, missing
resource mappings, and ambiguous mappings.

Installing this battery adds policy; it does not connect Linear, enable tools,
or grant provider permissions. It supports the existing Claude Code and kagent
adapters. Keep the host connection name mapped to the `linear` namespace:

```sh
appa battery install linear --config /path/to/appa.toml --server <host-server-id>
appa battery remove linear --config /path/to/appa.toml
```

Until the package is released, use the source includes shown in
[`examples/linear-battery`](../../../examples/linear-battery). Do not install a
released generation expecting this unreleased package to be present.

## Profiles

Include exactly one profile. The package installer selects `appa.toml`.
Alternate profiles can be selected through source includes; the installer has
no profile flag. Do not stack profiles or edit an installed immutable generation.

| File | Behavior |
| --- | --- |
| `appa.toml` | Approved writes (default): all mutations require `linear-review` |
| `read-only.toml` | Only the 36 captured read tools are registered |
| `team-use.toml` | Routine edits omit the extra review mark; sensitive edits retain it |
| `production-lockdown.toml` | Mutations additionally require `production: true` on their exact resource rule |

All content, including mutation responses, enters as suspicious. All writes
require trusted input. APPA checks this floor **after** applying the output
label, so even a fresh-session routine write requires a trust authority.
Team-use removes the additional attention mark, not this trust protection.
The examples bind the existing `hitl` backend with both permits. Without a
bound authority, writes stay blocked. Approval is for the exact call and does
not make subsequent content trusted or grant wider disclosure rights.

Routine operations are `save_issue`, `save_comment`, `save_document`,
`save_milestone`, and `mark_notification`. Access, ownership, project/team,
delegation and template changes require explicit matching; sensitive fields
also add review in team-use. Other mutations—including sharing, deletion,
attachment upload and code-review/merge operations—require review in every
write profile. Successful mutations record `linear.changed` or
`linear.sensitive` in the existing effect history; refused calls record neither.

## Resource audiences and argument matching

Override the profile's annotator declaration in the root config. Root
declarations replace the whole declaration, so retain `ranks`, `marks`, and
`effects`, and declare the audiences the helper is permitted to use:

```toml
[[policy.annotator]]
name = "linear.approved-writes"
ranks = ["suspicious", "trusted"]
marks = ["linear-review"]
effects = ["linear.changed", "linear.sensitive"]
audiences = ["alice@corp.example"]
hint = '{"rules":{"get_issue":[{"match":{"id":"ENG-1"},"audience":["alice@corp.example"]}],"save_comment":[{"match":{"issueId":"ENG-1"},"audience":["alice@corp.example"]}]}}'
```

Each rule has `match`, `audience`, and optionally boolean `production`. Audience
is `"public"` or a nonempty list of reader IDs / declared audience symbols.
Exactly one rule must match. Every supplied argument listed in
`scope_arguments` must be present in the rule and equal, including complete
arrays, booleans, filters, cursors, and related-content flags. There are no
wildcards, name-based guesses, or implicit directory-to-resource ACL conversions.
Omitted arguments use the provider's defaults; the operator must verify that
those defaults stay inside the declared audience. Broad searches need the
common allowed readers of every result they could return.

Only the explicitly reviewed `variable_arguments` (such as comment body) may
vary without a new rule. Rules may constrain those too. Reparenting, sharing,
assignee changes, URL destinations and expanded related content cannot reuse
an issue-only mapping. Mappings must account for existing content returned,
watchers, notifications, guest access, linked repositories, and secondary
effects. Mapping a team roster is safe only when that roster actually describes
the resource's readers. Use stable IDs where the tool accepts them.

The runtime resolves the configured server alias before invoking the helper.
The helper classifies the selected operation; it does not infer provider identity
from the physical connection name. Unbound servers remain refused by APPA.

Both reads and writes check disclosure to their mapped audience: query strings
and resource IDs can themselves contain private data. Results carry that same
audience. `extract_images` can contact external URLs and therefore additionally
requires public input and review, even in read-only mode.
`prepare_attachment_upload` keeps its signed URL result `self`. The subsequent
external PUT is outside Linear MCP and needs the host's shell/HTTP policy.

`operations.json` is the reviewed policy contract: operation kinds, required
arguments, and the partition between scope and variable arguments. Scope values,
including nested objects, must match an operator rule exactly. Variable values
are scalars except for the reviewed text-edit `patch` operations; `contract.py`
rejects unknown patch fields and structured values hidden in scalar content.
Linear validates API types, formats and bounds. The battery does not ship or
implement a general provider schema validator. Capture annotations never grant access.

## Audience source

`audience-source.py` implements the #262 external audience consult protocol.
It uses Linear's GraphQL API and supports:

| Selector | Meaning |
| --- | --- |
| `linear:viewer` | The active authenticated user |
| `linear:workspace/<workspace-uuid>/members` | Active non-guest users of the explicitly selected workspace |
| `linear:team/<team-uuid>/members` | Active members of the selected team, including its guests |

Membership is not a resource ACL. In particular, workspace membership does not
grant access to every private team, and a public team's explicit membership
does not necessarily enumerate everyone who can read its issues. Tool
contracts therefore require deployment-owned resource audience rules.

All returned members are `linear:<user-uuid>`. The helper does not treat profile
email as an identity attestation. Member lookups return the Linear-qualified
principal with a lowercase UUID, matching collection results even when the lookup
uses uppercase or mixed-case hex. A deployment can explicitly map those readers
to shared email principals through #262's lookup routing:

```toml
[policy]
version = 2

[policy.audience]
self = ["linear:viewer"]
internal = ["linear:workspace/00000000-0000-0000-0000-000000000001/members"]

[policy.audience.group.delivery]
from = ["linear:team/00000000-0000-0000-0000-000000000003/members"]

[externals]
timeout_ms = 30000
max_body_bytes = 1048576

[externals.audience.linear]
command = ["python3", "marketplace/batteries/linear/audience-source.py"]
token_env = "APPA_PROVIDER_LINEAR_TOKEN"
lookup = "people"

[externals.audience.people]
readers = { "linear:00000000-0000-0000-0000-000000000002" = "alice@corp.example" }
```

Replace the example UUIDs and reader mapping. The command path is relative to
the root configuration; this example assumes a config at the repository root.
Omit `lookup` and the `people` entry to retain Linear-qualified readers. A team
can include guests, so it is deliberately not constrained `within = "internal"`
in this example. Add that cap only when its meaning is intended.

The helper uses one token for its API calls, rejects redirects, bounds input
and responses, and refuses duplicate members, incomplete flags, failed pages,
repeated cursors, and unexpected workspace/team IDs. It never returns a
partially read directory. There is no automatic retry: an unavailable source
fails the runtime's probe or consult. The deployment controls the overall
consult timeout; very large directories may need an increased budget.

## Refresh and schema drift

Load `APPA_PROVIDER_LINEAR_TOKEN` into the environment from your secret store.
Never put it in arguments or repository files.

```sh
python3 marketplace/batteries/linear/capture.py --compare marketplace/batteries/linear/schema-lock.json > /tmp/linear-candidate-lock.json
# For reviewing changed definitions locally, without committing the capture:
python3 marketplace/batteries/linear/capture.py --full --compare marketplace/batteries/linear/schema-lock.json > /tmp/linear-candidate-full.json
```

The utility initializes both official endpoints, paginates `tools/list`, and
never calls `tools/call`. By default it outputs a lockfile containing per-tool
SHA-256 hashes of canonical complete definitions (including input schemas,
descriptions and annotations), plus endpoint provenance. `--full` emits the
observed definitions for local review instead. Neither mode changes policy files.
Output is produced only after both captures succeed. Exit codes: 0 unchanged/success,
1 failure, 2 usage, 3 drift. A redirected file may be empty on failure.

A changed hash requires reviewing the current definition and **every argument**
against `operations.json`. Update that contract before accepting the candidate
lockfile and running `build.py`. Do not commit full captures or copy provider
descriptions into policy files. The offline build checks tool coverage and the
argument partition's internal consistency; it cannot compare fields against hashes.
Hashes detect drift when this command runs, not during annotation, and do not
identify a hosted deployment version. Unknown tools and arguments still refuse
at runtime, but semantic changes to existing fields require the drift check.

```sh
python3 marketplace/batteries/linear/build.py --check
python3 -m unittest discover -s marketplace/batteries/linear -p 'test_*.py'
cargo test --locked -p appa --test linear_policy --test marketplace
bash scripts/appa-marketplace.sh --check
```

See `verification.json` for live read evidence and its limits. Mutation tests
use fixtures. The captured surface is account-visible; a future deployment may
expose different tools and must run the drift check.

## Upstream evidence

- [Official MCP endpoints and authentication](https://linear.app/docs/mcp).
- [GraphQL API](https://linear.app/developers/graphql).
- [Membership and guest roles](https://linear.app/docs/members-roles).
- Audience query fields checked against the [official SDK schema at
  `716871f2042cee9495220276b8ca28b0c35343f4`](https://github.com/linear/linear/blob/716871f2042cee9495220276b8ca28b0c35343f4/packages/sdk/src/schema.graphql).
