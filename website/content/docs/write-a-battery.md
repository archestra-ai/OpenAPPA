---
title: Create your own battery
category: Batteries
order: 6.7
description: Create, test, and submit a battery for your MCP server.
---

This guide shows MCP server authors how to create and publish a battery for their server.

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

Create a folder under `batteries/`:

```text
batteries/
`-- your-server/
    |-- README.md
    |-- appa.toml
    |-- annotator.py
    `-- test_annotator.py
```

`appa.toml` contains the tool contracts. Add an annotator when a contract depends on the call's arguments. Add an audience source when the service's users or groups define who can receive data.

The battery `README.md` must name the server version and list the covered tools. It must also explain each contract, script, test, and known limit.

Add the battery config to `include` in `examples/claude-code-battery/appa.toml`. Add the Authority and audience source settings it needs.

The test suite loads this example to make sure all included batteries work together.

## Test the battery

### Unit tests

Write tests for every annotator and audience source. Use saved API responses instead of calling the real service.

Test expected input, invalid input, provider errors, and missing data.

For example, to test a Python battery:

```sh
python3 -m unittest discover -s batteries/your-server -p 'test_*.py'
```

After you add the battery to `examples/claude-code-battery/appa.toml`, check that the complete config still loads:

```sh
cargo test -p appa --test examples_load
```

This test detects invalid battery config and conflicts with other included batteries. CI also runs Python tests under `batteries/*/test_*.py`.

### Integration test

Use [`appa replay` validation](/validation) to test the battery end to end. Replay proposes tool calls and checks the decisions from OpenAPPA. It does not run the tools.

In this example, a GitHub MCP battery uses a `github.repository-visibility` annotator. The annotator reads the repository owner and name, calls the GitHub API, and returns a complete contract for the call:

- A public repository gives the result `suspicious` trust and a `public` audience.
- A private repository gives the result `suspicious` trust and limits the audience to `internal`.

The battery defines the GitHub tool contracts and the Python annotator that resolves repository visibility:

```toml
# examples/test-github-battery/github-battery.toml
[policy]
version = 2

[[policy.annotator]]
name = "github.repository-visibility"
ranks = ["suspicious"]
audiences = ["github:internal"]

[[policy.tool]]
name = "mcp__github__get_file_contents"
annotator = "github.repository-visibility"

[externals.annotators."github.repository-visibility"]
command = ["python3", "repository-visibility.py"]
token_env = "APPA_PROVIDER_GITHUB_TOKEN"
```

The documentation omits the implementation of `repository-visibility.py`. The complete implementation is in the [`examples/test-github-battery`](https://github.com/archestra-ai/OpenAPPA/tree/main/examples/test-github-battery) example.

The complete `github-battery-test.toml` config includes the battery and adds two tools that are not part of it. These tools make the trust and audience changes observable.

```toml
include = ["github-battery.toml"]

[policy]
version = 2
trust_chain = ["suspicious", "trusted"]

# This tool tests the trust change from the GitHub result.
[[policy.tool]]
name = "RunCommand"
requires = { trust = "trusted" }
delta = {}

# This tool tests who can receive the GitHub result.
[[policy.tool]]
name = "Send"
requires = { trust = "suspicious", audience = { contains = ["$to"] } }
delta = {}

[policy.tool.parameters]
type = "object"
required = ["to"]

[policy.tool.parameters.properties.to]
type = "string"

[externals]
timeout_ms = 30000
max_body_bytes = 65536
```

The `github-repository-visibility.appa` trace uses one public repository and one private repository that the GitHub token can read:

```appa
# Public repository content is suspicious, but it can remain public.
mcp__github__get_file_contents {
  owner: "your-org"
  repo: "your-public-repo"
  path: "README.md"
}
expect allow

# Suspicious content cannot enter a tool that requires trusted input.
RunCommand {
  command: "deploy"
}
expect deny

# Public repository content can go to a public destination.
Send {
  to: "public"
}
expect allow

# Private repository content narrows the audience.
mcp__github__get_file_contents {
  owner: "your-org"
  repo: "your-private-repo"
  path: "README.md"
}
expect allow

# Private repository content cannot go to a public destination.
Send {
  to: "public"
}
expect deny

# The configured private-repository audience can receive the content.
Send {
  to: "github:internal"
}
expect allow
```

Run the replay with a GitHub token that can read both repositories:

```sh
APPA_PROVIDER_GITHUB_TOKEN=... appa replay \
  --config examples/test-github-battery/github-battery-test.toml \
  examples/test-github-battery/github-repository-visibility.appa
```

Replay calls the configured annotator for each GitHub tool call. The annotator can call the GitHub API. The GitHub MCP tool and the other tools do not run.

## Open the pull request

Open a pull request against `archestra-ai/OpenAPPA` `main`. Include:

- the new battery folder;
- its `README.md`, scripts, and tests;
- the updated `include` list in `examples/claude-code-battery/appa.toml`;
- a battery documentation page under `website/content/docs/`;
- a card in `website/components/BatteryCatalog.tsx`; and
- the server repository, exact version or commit, covered tools, and test results in the pull request description.

## Verify the pull request

The pull request is ready for review when:

1. Repository CI passes.
2. Every contract uses the exact tool name and arguments from that server version.
3. Every script passes its tests and refuses invalid input.
4. The test that loads all batteries passes.
5. The catalogue card opens the battery documentation page.
