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

How to install the APPA CLI on Linux or macOS:

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

## Expected decisions

Each call in a test file can be followed by one `expect` line:

| Line | The call passes when |
|---|---|
| `expect allow` | The call runs. A plain narrowing acceptance is taken as the model would take it. |
| `expect deny` | The call is blocked and no remedy is offered. |
| `expect withhold` | The call is blocked with an offer to run it without returning its result. Replay takes the offer. |
| `expect authority [name]` | The call is blocked with an offer to ask an authority. Replay takes the offer, and a stand-in approves. |
| `expect sanitizer [name]` | The call is blocked with an offer to apply a sanitizer. Replay takes the offer, and the stand-in returns the value unchanged. |

With a name, the offer must involve that authority or sanitizer. Without a name, any authority or sanitizer passes. After a taken offer, the trajectory continues from the remedied call.

## Observe decisions without expectations

A call may omit its `expect` line. Replay then reports the decision and does not check it. Use this to ask what the policy decides on a real trajectory before you write expectations. A file may mix checked and observed calls.

```text
mcp/files/read {
  path: "/hr/salaries.csv"
}

mcp/mail/send {
  to: "x@other.com"
}
```

```text
obs   policy-tests/hr-email.appa:1 mcp/files/read got allow (after accepting the narrowing)
obs   policy-tests/hr-email.appa:5 mcp/mail/send got deny
ok    policy-tests/hr-email.appa
1 file: 1 ok, 0 failed, 0 could not run
```

An observed call never fails its file. When the call is allowed, it runs. When the call is blocked with a plain narrowing acceptance, replay takes it, as for `expect allow`. When the call is blocked otherwise, replay takes no offer. The call did not run, so its `delta` does not change the trajectory. An observed `appa/do_something` call never takes an offer.

## JSON output

`appa replay --json` writes one JSON object per line instead of text. Each call gets one object:

| Field | Value |
|---|---|
| `file` | The test file path. |
| `line` | The line of the call, or `null` when the trajectory could not open. |
| `tool` | The proposed tool. |
| `check` | The `expect` text, such as `allow` or `authority hitl`, or `observe`. |
| `got` | The decision in `expect` words, or `null` when the call could not run. |
| `offers` | The offers of a blocked call, in `expect` words. |
| `taken` | The offer that replay took, or `null`. |
| `outcome` | `passed`, `mismatch`, `observed`, or `cannot_run`. |
| `detail` | Why the call or trajectory could not run, or `null`. |

The last line is `{"summary": {"ok": …, "failed": …, "cannot_run": …}}`. The exit code is the same as for text output.

## Check a restriction without naming a tool

Some requirements name the data but no tool that could leak it, such as "Slack data stays private". Use the call `appa/do_something` to check them. Only `appa replay` serves it. Every other deployment refuses it, and a policy cannot declare it.

Its one argument, `requires`, takes the same fields as a tool contract's `requires`, written as JSON. Replay checks the call against those requirements only. The call has no `delta` and no `effects`, so it never changes the trajectory. When the call is blocked with an offer that the `expect` line names, the step passes and replay does not take the offer.

```toml
[[policy.tool]]
name = "mcp/slack/read_channel"
delta = { audience = ["@slack:channel/$channel_id"] }
```

```text
# A fresh trajectory is public.
appa/do_something {
  requires: {"audience": {"contains": ["public"]}}
}
expect allow

mcp/slack/read_channel {
  channel_id: "C0123456789"
}
expect allow

# Channel content cannot reach the public.
appa/do_something {
  requires: {"audience": {"contains": ["public"]}}
}
expect deny
```

The same form checks trust. `requires: {"trust": "trusted"}` is denied once the trajectory has read `suspicious` content.

`requires` can name `public`, `self`, `internal`, and the audiences, attention marks, effects, and trust ranks that the policy names. It cannot read a call argument such as `$to`. A requirement outside these limits makes the step report `could not run`, never `deny`.

`appa/do_something` checks what the trajectory allows at that step. It does not check that a real tool enforces the restriction: a tool without `requires` still sends the content anywhere. Prefer a real tool from your policy when the requirement names one.

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
Session tools: unavailable to this command; pass the names this session sees with --session-tools
Connector accounts: unavailable to this command
Validation: tools: 0 valid, 0 invalid, 522 unknown; inventory: partial or unavailable; new tools: possible; wildcard: present (ordinary tools only)
```

Use the policy tool list to choose what to test. Check that your rules allow intended work and block forbidden actions, including after the agent reads sensitive data. Test remedies where your rules offer them.

Replay checks policy decisions. Integration tests check that your agent follows those decisions and that the connected tools and services work.
