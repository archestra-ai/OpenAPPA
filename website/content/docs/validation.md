---
title: Validation
category: Operations
order: 8
description: Check that policy changes keep your security rules.
---

To help you make sure your configuration behaves as intended, OpenAPPA ships the
`appa replay` CLI tool. It tests a [policy file](/contracts) against a trace file. The
trace contains a sequence of tool calls and the expected decision for each call. Replay
checks these decisions but does not run the tools.

## Start with an example

This trace reads an HR file. It then checks that the data cannot go to an external email
address. The data can still go to the HR email address.

Create `secret-stays-inside.appa`:

```text
Read {
  path: "/hr/salaries.csv"
}
expect allow

Email {
  to: "x@other.com"
}
expect deny

Email {
  to: "hr@archestra.ai"
}
expect allow
```

Create `appa.toml` in the same directory:

```toml
[policy]
version = 2

[[policy.tool]]
name = "Read(path:/hr/*)"
delta = { audience = ["hr@archestra.ai"] }

[[policy.tool]]
name = "Email"
parameters = { type = "object", properties = { to = { type = "string" } }, required = ["to"] }
requires = { audience = { contains = ["$to"] } }
delta = {}

[externals]
timeout_ms = 2000
max_body_bytes = 65536
```

The `Read` rule limits the data to `hr@archestra.ai`. The `Email` rule uses the `to`
value from each call. It allows the email only if the recipient can receive the data.

These rules use an [audience](/contracts#audiences). An audience states who can receive
data. See [Tools](/contracts#tools) for all tool rule options.

Run the test:

```sh
appa replay --config appa.toml secret-stays-inside.appa
```

The command exits with code `0` when all three decisions match the trace.

## Configuration

Use `--config` to select the policy file. Then add one or more trace files or directories:

```sh
appa replay --config appa.toml first.appa second.appa
```

A directory adds the `.appa` files directly inside that directory:

```sh
appa replay --config appa.toml traces/
```

Each trace file is one test case. Calls in one file run in order and share the same data
limits. A new file starts a new test case.

Add `-v` to show every call:

```sh
appa replay -v --config appa.toml traces/
```

## Trace syntax

A call starts with the tool name. Put its arguments between `{` and `}`. Put one argument
on each line. Each value must be valid JSON. This means that a text value needs double
quotes.

```text
Email {
  to: "person@example.com"
  subject: "Status report"
}
expect allow
```

Use `{}` when a call has no arguments:

```text
Backup {}
expect allow
```

Write the expected result immediately after each call. A line that starts with `#` is a
comment.

Replay reports a syntax error when:

- an argument occurs more than one time;
- a value is not valid JSON;
- a call has no `expect` line.

The error includes the file name and line number.

## Expected results

### `allow`

Use `allow` when the policy must allow the call.

```text
Backup {}
expect allow
```

A read can reduce the trust level or the audience for later calls. APPA normally asks the
model to accept this change. Replay accepts the change for an `allow` step. See
[Labels only move one way](/how-it-works#labels-only-move-one-way) for an explanation of
these data limits.

### `deny`

Use `deny` when the policy must block the call and must not offer another way to run it.

```text
Deploy {}
expect deny
```

### `authority`

Use `authority` when the policy must require approval from an
[authority](/contracts#authorities).

```text
Publish {}
expect authority
```

You can require one named authority:

```text
Publish {}
expect authority security-team
```

Replay does not contact the configured authority. It gives approval and continues the
test case.

### `sanitizer`

Use `sanitizer` when the policy must require a [sanitizer](/contracts#sanitizers).

```text
Email {
  to: "person@example.com"
}
expect sanitizer
```

You can require one named sanitizer:

```text
Email {
  to: "person@example.com"
}
expect sanitizer remove-secrets
```

Replay does not call the configured sanitizer. It continues with the same value. This
checks that the policy requires the sanitizer. It does not test how the sanitizer changes
the value.

## Calls during replay

Replay never runs a tool from the trace. When the policy allows a call, replay records an
empty successful result. It also records the effects from the
[tool rule](/contracts#tools). The next call can then check those effects.

Replay does not call [authorities](/contracts#authorities) or
[sanitizers](/contracts#sanitizers). It uses the test behavior described above.

Replay does call configured [annotators](/contracts#annotators). An annotator provides a
rule for a proposed tool call, so replay needs its answer to make the real policy decision.
Replay also calls configured [audience services](/contracts#audience-sources) and
[identity services](/contracts#identity) when a decision needs them.

If one of these services fails, replay reports that the call could not run. It does not
report a policy denial.

## Results and exit codes

Replay prints `ok` for a test case when all decisions match. If a decision does not match,
replay prints the file name, line number, actual result, and expected result.

```text
secret-stays-inside.appa:6: Email: got allow, want deny
FAIL  secret-stays-inside.appa
1 file: 0 ok, 1 failed, 0 could not run
```

A wrong result stops the current test case. Other test cases continue.

| Exit code | Meaning |
|---|---|
| `0` | All expected results matched. |
| `1` | One or more results did not match. |
| `2` | Replay could not complete one or more calls. |

## More examples

See the [replay examples on GitHub](https://github.com/archestra-ai/OpenAPPA/tree/main/examples/tests)
for policies that test audiences, trust levels, approval, sanitizers, and recorded effects.
