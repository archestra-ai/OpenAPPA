---
title: Validation
category: Operations
order: 8
description: Test policy decisions before merging changes, without running your agent's tools.
---

The [APPA CLI](https://openappa.com/install.sh) provides two commands for validation:

- `appa describe --check` checks that your configuration loads.
- `appa replay` checks scripted tool calls against the decisions you expect, without running your agent's tools.

Run them locally or in continuous integration (CI) to catch configuration errors and unexpected policy decisions before deployment.

Install the CLI on Linux or macOS:

```sh
curl -fsSL https://openappa.com/install.sh | sh
```

## Make policy tests a required CI check

Keep the workflow, policy, and tests in the same repository:

```text
.
|-- .github/
|   `-- workflows/
|       `-- policy-check.yml
|-- appa.toml
`-- policy-tests/
    `-- hr-email.appa
```

In `policy-check.yml`, add this step to a pull-request job after checkout and installation of your agent's APPA version:

```yaml
- name: Check policy decisions
  shell: bash
  run: |
    appa describe --config appa.toml --check
    appa replay --config appa.toml policy-tests/
```

Make the job a required check in GitHub to block merges when validation fails. The example below supplies the policy and test.

## Example: HR files can only be emailed to HR

Consider an agent that reads files and sends email. After it reads an HR file, the policy must block email to an outside recipient while still allowing email to HR.

Save this configuration as `appa.toml`. The read's `delta` restricts the audience to `hr@archestra.ai`. The send's `requires` checks that its recipient belongs to that audience.

```toml
[externals]
timeout_ms = 2000
max_body_bytes = 65536

[policy]
version = 2

[[policy.tool]]
name = "mcp/files/read(path:/hr/*)"
delta = { audience = ["hr@archestra.ai"] }

[[policy.tool]]
name = "mcp/mail/send"
requires = { audience = { contains = ["$to"] } }
delta = {}
```

Create a `policy-tests/` directory and save this sequence as `policy-tests/hr-email.appa`:

```text
mcp/files/read {
  path: "/hr/salaries.csv"
}
expect allow

mcp/mail/send {
  to: "x@other.com"
}
expect deny

mcp/mail/send {
  to: "hr@archestra.ai"
}
expect allow
```

The calls share one trajectory. After the read, only HR remains in the audience, so the outside recipient is denied and HR is allowed. Replay supplies an empty result for the read. No CSV file or email account is needed.

Run it with APPA installed:

```sh
appa replay --config appa.toml policy-tests/
```

```text
ok    policy-tests/hr-email.appa
1 file: 1 ok, 0 failed, 0 could not run
```

If a change permits the outside recipient or blocks HR, this test fails. It checks both confidentiality and permitted work.

## What to test

Start by checking your configuration and listing the tools named in your policy:

```sh
appa describe --check
```

The output shows your batteries, policy tools, and validation results. For example, with the tool list and other details shortened:

```text
OpenAPPA world
Adapter: claude-code
Config: /path/to/appa.toml (loadable)
Batteries: claude-code, slack, github, ...
Policy tools: *, Agent, ..., host/claude-code/Read, ...
...
Session integrations/tools/accounts: unavailable to this command
Validation: tools: 0 valid, 0 invalid, 522 unknown; inventory: partial or unavailable; new tools: possible; wildcard: present (ordinary tools only)
```

Use the policy tool list to choose what to test. Check that your rules allow intended work and block forbidden actions, including after the agent reads sensitive data. Test remedies where your rules offer them.

Replay checks policy decisions. Integration tests check that your agent follows those decisions and that the connected tools and services work.
