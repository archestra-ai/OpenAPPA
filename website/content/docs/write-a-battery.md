---
title: Create your own battery
category: Batteries
order: 6.7
description: Create, test, and submit a battery for your MCP server.
---

This guide shows MCP server authors how to create and publish a battery for their server.

Define tool contracts in TOML, using argument matching where appropriate. Add an
annotator when determining a contract requires additional logic or information
from the provider.

For example, a push to a public GitHub repository requires data that may be shared
publicly. A push to a private repository requires data that may be shared with
that repository's readers. Determining the repository's visibility and readers
may require a GitHub API lookup.

> **Ask your coding agent**
>
> Copy this prompt into your coding agent:
>
> ```text
> Read https://openappa.com/write-a-battery and follow the guide.
>
> Inspect the MCP server in this repository.
> Create a battery for it.
> Add its documentation and catalogue card.
> Run every available check.
> Do not guess when the server code does not answer a question.
> Tell me what you created and what you verified.
> Tell me what I still need to review.
> ```

## What happens next

1. **Read.** Your agent reads the server code and docs. It lists every tool, its inputs and results, and anything it can change.
2. **Build.** It creates the battery, tests, documentation page, and catalogue card. It records the exact server version and links each rule to the source that supports it.
3. **Review.** You open those links, check the rules, resolve any unanswered questions, and submit the pull request.

:::battery-review-checklist:::

## Add the battery files

Create a folder under `marketplace/batteries/`:

```text
marketplace/batteries/
`-- your-server/
    |-- README.md
    |-- appa-package.toml
    |-- appa.toml
    |-- annotator.py       # optional
    `-- test_annotator.py  # when an annotator is included
```

`appa-package.toml` is the package manifest: the battery's name and
description, the policy file, the hosts it is composed with, and the helper
scripts its bindings name. Run `bash scripts/appa-marketplace.sh` to generate
the catalog entry and content digest, then commit `marketplace/marketplace.toml`
with the package. CI checks that the generated catalog is current.

`appa.toml` contains the tool contracts. An annotator determines contracts that static rules cannot express. An audience source supplies the members of the provider's collections: the viewer, the full membership, groups, and per-resource readers such as one channel's members. The battery binds it under `[externals.audience.<provider>]` with `command`, `token_env`, and the `selectors` it serves. Contracts in the battery may then name those collections with selector placeholders, such as `@slack:channel/$channel_id`.

The battery `README.md` must name the server version and list the covered tools. It must also explain each contract, script, test, and known limit.

Declare in the battery's `README.md` what a root config adds around it: the authority its rules require, the `[policy.audience]` mapping onto its source, and the credential variable. [`examples/README.md`](https://github.com/archestra-ai/OpenAPPA/tree/main/examples/README.md) shows the install steps a root follows.

The test suite composes every battery into each host it declares to make sure the shipped batteries work together.

## Test the battery

### Unit tests

Write tests for every annotator and audience source that need no credential: the consult envelope, the template check against `declaration.templates`, argument and selector handling, and every refusal path. An audience source must refuse a declaration that lists templates it does not serve, before it reads its token; test that refusal. Check the service against the real provider with a replay trace and a real token, as described below.

For example, to test a Python battery:

```sh
python3 -m unittest discover -s marketplace/batteries/your-server -p 'test_*.py'
```

Then check that the battery composes with the shipped ones:

```sh
cargo test -p appa --test marketplace
```

This test detects invalid battery config and conflicts with other batteries. CI also runs Python tests under `marketplace/batteries/*/test_*.py`.

### Integration test

Use [`appa replay` validation](/validation) to test the battery end to end. Replay proposes tool calls and checks the decisions from OpenAPPA. It does not run the tools.

In this example, the marketplace GitHub battery uses a `github.repository-visibility` annotator for reads. The annotator reads the repository owner and name from the call, asks the GitHub API whether the repository is private, and returns a complete contract for the call:

- A public repository gives the result `suspicious` trust and a `public` audience.
- A private repository gives the result `suspicious` trust and limits the audience to the repository's collaborators, the collection `@github:repo/<owner>/<repo>/collaborators` that the battery's audience source resolves.

The battery declares the annotator with a selector placeholder in its mandate, so each call's consult admits exactly the repository that call names; `owner` and `repo` become required string arguments of every tool that uses it:

```toml
# marketplace/batteries/github/appa.toml
[policy]
version = 2

[[policy.annotator]]
name = "github.repository-visibility"
ranks = ["suspicious"]
audiences = ["@github:repo/$owner/$repo/collaborators"]
marks = []

[[policy.tool]]
name = "mcp/github/get_file_contents"
annotator = "github.repository-visibility"

[externals.annotators."github.repository-visibility"]
command = ["python3", "repository-visibility.py"]
token_env = "APPA_PROVIDER_GITHUB_TOKEN"

[externals.audience.github]
command = ["python3", "audience-source.py"]
token_env = "APPA_PROVIDER_GITHUB_TOKEN"
selectors = [
  { template = "viewer", feeds = "self" },
  { template = "repo/<owner>/<repo>/collaborators" },
]
```

The battery implementation is in [`marketplace/batteries/github`](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/github) and the live replay in [`examples/live-replays/github`](https://github.com/archestra-ai/OpenAPPA/tree/main/examples/live-replays/github).

The replay's `appa.toml` includes the battery and adds two tools that are not part of it. These tools make the trust and audience changes observable.

```toml
include = ["../../../marketplace/batteries/github/appa.toml"]

[policy]
version = 2
trust_chain = ["suspicious", "trusted"]

# This tool tests the trust change from the GitHub result.
[[policy.tool]]
name = "mcp/shell/run_command"
requires = { trust = "trusted" }
delta = {}

# This tool tests who can receive the GitHub result.
[[policy.tool]]
name = "mcp/mail/send"
requires = { trust = "suspicious", audience = { contains = ["$to"] } }
delta = {}

[externals]
timeout_ms = 30000
max_body_bytes = 1048576
```

The `github-battery.appa` trace uses one public repository and one private repository that the GitHub token can read:

```appa
# Public repository content is suspicious, but it can remain public.
mcp/github/get_file_contents {
  owner: "your-org"
  repo: "your-public-repo"
  path: "README.md"
}
expect allow

# Suspicious content cannot enter a tool that requires trusted input.
mcp/shell/run_command {
  command: "deploy"
}
expect deny

# Public repository content can go to a public destination.
mcp/mail/send {
  to: "public"
}
expect allow

# Private repository content narrows the audience.
mcp/github/get_file_contents {
  owner: "your-org"
  repo: "your-private-repo"
  path: "README.md"
}
expect allow

# Private repository content cannot go to a public destination.
mcp/mail/send {
  to: "public"
}
expect deny

# The repository's collaborators can receive the content.
mcp/mail/send {
  to: "@github:repo/your-org/your-private-repo/collaborators"
}
expect allow
```

Run the replay with a GitHub token that can read both repositories and list the private one's collaborators:

```sh
APPA_PROVIDER_GITHUB_TOKEN=... appa replay \
  --config examples/live-replays/github/appa.toml \
  examples/live-replays/github/github-battery.appa
```

Replay calls the configured annotator for each GitHub tool call. The annotator can call the GitHub API. The GitHub MCP tool and the other tools do not run.

## Open the pull request

Open a pull request against `archestra-ai/OpenAPPA` `main`. Include:

- the new battery folder;
- its `README.md`, scripts, and tests;
- a battery documentation page under `website/content/docs/`;
- a card in `website/components/BatteryCatalog.tsx`; and
- the server repository, exact version or commit, covered tools, and test results in the pull request description.

## Verify the pull request

The pull request is ready for review when:

1. Repository CI passes.
2. Every contract uses the canonical tool id (`mcp/<server>/<tool>`) and the exact arguments from that server version.
3. Every script passes its tests and refuses invalid input.
4. The test that loads all batteries passes.
5. The catalogue card opens the battery documentation page.
